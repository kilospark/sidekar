use super::*;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Mirrors `src/broker/tests.rs` `with_test_db`: swaps HOME for a fresh temp
/// dir for the duration of `f`, serialized against every other test that
/// touches process-global broker state (HOME, the encryption key, the
/// current user id).
fn with_test_db<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    let _guard = crate::test_home_lock()
        .lock()
        .map_err(|_| anyhow!("failed to lock test HOME mutex"))?;
    let old_home = env::var_os("HOME");
    let temp_home = env::temp_dir().join(format!("sidekar-sync-test-home-{}", unique_suffix()));
    fs::create_dir_all(&temp_home)?;
    // Safety: tests run in-process and this helper restores HOME before returning.
    unsafe { env::set_var("HOME", &temp_home) };
    let result = f();
    match old_home {
        Some(home) => unsafe { env::set_var("HOME", home) },
        None => unsafe { env::remove_var("HOME") },
    }
    let _ = fs::remove_dir_all(&temp_home);
    result
}

fn reset_encryption_state() {
    clear_encryption_key();
    clear_current_user_id();
}

fn unique_suffix() -> String {
    static COUNTER: AtomicI64 = AtomicI64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos}-{n}")
}

// ---------------------------------------------------------------------------
// mark_dirty
// ---------------------------------------------------------------------------

#[test]
fn mark_dirty_versions_are_monotonic() -> Result<()> {
    with_test_db(|| {
        let conn = open()?;
        let uid = "u1";

        let v1 = mark_dirty(&conn, uid, "kv", "k", false)?;
        assert_eq!(v1, 1);
        let v2 = mark_dirty(&conn, uid, "kv", "k", false)?;
        assert_eq!(v2, 2);
        let v3 = mark_dirty(&conn, uid, "kv", "k", false)?;
        assert_eq!(v3, 3);

        let (dirty, deleted): (i64, i64) = conn.query_row(
            "SELECT dirty, deleted FROM sync_state WHERE user_id = ?1 AND kind = 'kv' AND record_id = 'k'",
            params![uid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        assert_eq!(dirty, 1);
        assert_eq!(deleted, 0);
        Ok(())
    })
}

#[test]
fn mark_dirty_deleted_sets_tombstone_and_bumps_version() -> Result<()> {
    with_test_db(|| {
        let conn = open()?;
        let uid = "u1";

        mark_dirty(&conn, uid, "kv", "k", false)?;
        let v = mark_dirty(&conn, uid, "kv", "k", true)?;
        assert_eq!(v, 2);

        let deleted: i64 = conn.query_row(
            "SELECT deleted FROM sync_state WHERE user_id = ?1 AND kind = 'kv' AND record_id = 'k'",
            params![uid],
            |r| r.get(0),
        )?;
        assert_eq!(deleted, 1);
        Ok(())
    })
}

// ---------------------------------------------------------------------------
// resolve()
// ---------------------------------------------------------------------------

#[test]
fn resolve_no_local_row_applies_remote() {
    let action = resolve(
        None,
        RemoteState {
            version: 1,
            deleted: false,
        },
    );
    assert_eq!(action, MergeAction::ApplyRemote);
}

#[test]
fn resolve_no_local_row_remote_deleted_still_applies() {
    // A tombstone with no local row is still "apply remote" -- the caller is
    // responsible for turning that into a no-op delete of a row that never
    // existed locally, not for skipping it here.
    let action = resolve(
        None,
        RemoteState {
            version: 1,
            deleted: true,
        },
    );
    assert_eq!(action, MergeAction::ApplyRemote);
}

#[test]
fn resolve_remote_newer_applies_even_if_local_dirty() {
    let local = Some(LocalState {
        version: 1,
        dirty: true,
    });
    let action = resolve(
        local,
        RemoteState {
            version: 2,
            deleted: false,
        },
    );
    assert_eq!(action, MergeAction::ApplyRemote);
}

#[test]
fn resolve_local_dirty_keeps_local_when_remote_not_newer() {
    let local = Some(LocalState {
        version: 3,
        dirty: true,
    });
    assert_eq!(
        resolve(
            local,
            RemoteState {
                version: 3,
                deleted: false
            }
        ),
        MergeAction::KeepLocal
    );
    assert_eq!(
        resolve(
            local,
            RemoteState {
                version: 2,
                deleted: false
            }
        ),
        MergeAction::KeepLocal
    );
}

#[test]
fn resolve_clean_equal_version_is_noop() {
    let local = Some(LocalState {
        version: 3,
        dirty: false,
    });
    assert_eq!(
        resolve(
            local,
            RemoteState {
                version: 3,
                deleted: false
            }
        ),
        MergeAction::NoOp
    );
}

#[test]
fn resolve_clean_remote_older_is_noop_not_panic() {
    let local = Some(LocalState {
        version: 5,
        dirty: false,
    });
    assert_eq!(
        resolve(
            local,
            RemoteState {
                version: 1,
                deleted: false
            }
        ),
        MergeAction::NoOp
    );
}

// ---------------------------------------------------------------------------
// $sync1$ envelope
// ---------------------------------------------------------------------------

#[test]
fn sync_encrypt_decrypt_round_trip() -> Result<()> {
    let key = vec![9u8; 32];
    let blob = super::super::encryption::sync_encrypt(&key, "hello sync")?;
    assert!(blob.starts_with("$sync1$"));
    let plain = super::super::encryption::sync_decrypt(&key, &blob)?;
    assert_eq!(plain, "hello sync");
    Ok(())
}

#[test]
fn sync_decrypt_wrong_key_fails_cleanly() -> Result<()> {
    let key = vec![1u8; 32];
    let other_key = vec![2u8; 32];
    let blob = super::super::encryption::sync_encrypt(&key, "hello sync")?;
    assert!(super::super::encryption::sync_decrypt(&other_key, &blob).is_err());
    Ok(())
}

#[test]
fn sync_decrypt_rejects_local_encrypted_prefix() -> Result<()> {
    let key = vec![3u8; 32];
    let local_blob = encrypt_with_key_for_test(&key, "hello local")?;
    assert!(local_blob.starts_with("$encrypted$"));
    assert!(super::super::encryption::sync_decrypt(&key, &local_blob).is_err());
    Ok(())
}

#[test]
fn local_decrypt_rejects_sync_prefix() -> Result<()> {
    let key = vec![4u8; 32];
    let sync_blob = super::super::encryption::sync_encrypt(&key, "hello sync")?;
    set_encryption_key(key);
    assert!(decrypt(&sync_blob).is_err());
    clear_encryption_key();
    Ok(())
}

/// `encrypt_with_key` itself is private to `encryption.rs`; go through the
/// public `encrypt()` with a temporarily-installed key instead, to get a
/// `$encrypted$...` blob for the negative-prefix tests above.
fn encrypt_with_key_for_test(key: &[u8], plaintext: &str) -> Result<String> {
    set_encryption_key(key.to_vec());
    let result = encrypt(plaintext);
    clear_encryption_key();
    result
}

// ---------------------------------------------------------------------------
// initial_upload seeding
// ---------------------------------------------------------------------------

#[test]
fn initial_upload_seeds_one_sync_state_row_per_existing_record() -> Result<()> {
    with_test_db(|| {
        reset_encryption_state();
        let uid = "seed-user";
        set_encryption_key(vec![5u8; 32]);
        set_current_user_id(uid.to_string());

        kv_set("alpha", "one", None)?;
        kv_set("beta", "two", None)?;
        totp_add("github", "me@example.com", "SEEDSECRET", "SHA1", 6, 30)?;

        // kv_set/totp_add already marked these dirty via mark_dirty (version
        // 1, dirty=1) -- clear that so this test exercises seed_sync_state's
        // own idempotent seeding path in isolation from the mutation hooks.
        let conn = open()?;
        conn.execute("DELETE FROM sync_state WHERE user_id = ?1", params![uid])?;

        seed_sync_state(&conn, uid)?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sync_state WHERE user_id = ?1",
            params![uid],
            |r| r.get(0),
        )?;
        assert_eq!(count, 3);

        let all_dirty_v1: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sync_state WHERE user_id = ?1 AND version = 1 AND dirty = 1",
            params![uid],
            |r| r.get(0),
        )?;
        assert_eq!(all_dirty_v1, 3);

        // Idempotent re-run: no duplicate rows, and an existing dirty=0 row
        // (simulating one that already pushed) must not be reset back to
        // dirty=1.
        conn.execute(
            "UPDATE sync_state SET dirty = 0 WHERE user_id = ?1 AND kind = 'kv' AND record_id = 'alpha'",
            params![uid],
        )?;
        seed_sync_state(&conn, uid)?;
        let count_again: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sync_state WHERE user_id = ?1",
            params![uid],
            |r| r.get(0),
        )?;
        assert_eq!(count_again, 3);
        let alpha_dirty: i64 = conn.query_row(
            "SELECT dirty FROM sync_state WHERE user_id = ?1 AND kind = 'kv' AND record_id = 'alpha'",
            params![uid],
            |r| r.get(0),
        )?;
        assert_eq!(alpha_dirty, 0);

        Ok(())
    })
}

// ---------------------------------------------------------------------------
// Two-device simulation against an in-process fake server
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct ServerDoc {
    user_id: String,
    kind: String,
    record_id: String,
    ciphertext: String,
    version: i64,
    deleted: bool,
    updated_at: i64,
}

static FAKE_CLOCK: AtomicI64 = AtomicI64::new(1);

fn next_tick() -> i64 {
    FAKE_CLOCK.fetch_add(1, Ordering::SeqCst)
}

struct FakeSyncServer {
    addr: std::net::SocketAddr,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl FakeSyncServer {
    async fn start(store: Arc<Mutex<Vec<ServerDoc>>>) -> Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let (tx, mut rx) = tokio::sync::oneshot::channel::<()>();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut rx => break,
                    accepted = listener.accept() => {
                        if let Ok((stream, _)) = accepted {
                            let store = store.clone();
                            tokio::spawn(async move {
                                let _ = handle_conn(stream, store).await;
                            });
                        }
                    }
                }
            }
        });

        Ok(Self {
            addr,
            shutdown: Some(tx),
        })
    }

    fn stop(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}

async fn handle_conn(
    mut stream: tokio::net::TcpStream,
    store: Arc<Mutex<Vec<ServerDoc>>>,
) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

    let (read_half, mut write_half) = stream.split();
    let mut reader = BufReader::new(read_half);

    let mut request_line = String::new();
    reader.read_line(&mut request_line).await?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("").to_string();

    let mut content_length: usize = 0;
    let mut auth_header = String::new();
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 || line == "\r\n" {
            break;
        }
        let lower = line.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        }
        if lower.starts_with("authorization:") {
            auth_header = line[("authorization:".len())..].trim().to_string();
        }
    }

    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body).await?;
    }
    let body_str = String::from_utf8_lossy(&body).to_string();
    let user_id = auth_header
        .strip_prefix("Bearer ")
        .unwrap_or("")
        .trim()
        .to_string();
    let (path, query) = target.split_once('?').unwrap_or((target.as_str(), ""));

    let (status, response_body) = match method.as_str() {
        // A server from before memory sync: one memory record fails the batch.
        "PUT" if path == "/api/v1/sync/secrets"
            && FAKE_REJECTS_MEMORY.load(Ordering::SeqCst)
            && body_str.contains("\"kind\":\"memory\"") =>
        {
            (
                "400 Bad Request",
                json!({ "error": "kind must be 'kv', 'totp' or 'hotp'" }),
            )
        }
        "PUT" if path == "/api/v1/sync/secrets" => {
            ("200 OK", handle_put(&store, &user_id, &body_str))
        }
        "GET" if path == "/api/v1/sync/secrets" => {
            ("200 OK", handle_get(&store, &user_id, query))
        }
        _ => ("404 Not Found", json!({ "error": "not found" })),
    };

    let bytes = serde_json::to_vec(&response_body)?;
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        bytes.len()
    );
    write_half.write_all(header.as_bytes()).await?;
    write_half.write_all(&bytes).await?;
    write_half.flush().await?;
    Ok(())
}

/// Fake-server counterpart of `www/api/v1/sync/secrets.js`'s PUT handler:
/// accept only if the incoming version is strictly greater than what's
/// stored (or nothing is stored yet).
fn handle_put(store: &Arc<Mutex<Vec<ServerDoc>>>, user_id: &str, body: &str) -> Value {
    #[derive(serde::Deserialize)]
    struct InRecord {
        kind: String,
        record_id: String,
        ciphertext: String,
        version: i64,
        #[serde(default)]
        deleted: bool,
    }
    #[derive(serde::Deserialize)]
    struct InBody {
        records: Vec<InRecord>,
    }
    let parsed: InBody = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return json!({ "results": [] }),
    };

    let mut guard = store.lock().unwrap();
    let mut results = Vec::new();
    for rec in parsed.records {
        let existing = guard
            .iter_mut()
            .find(|d| d.user_id == user_id && d.kind == rec.kind && d.record_id == rec.record_id);
        match existing {
            Some(doc) if rec.version > doc.version => {
                doc.ciphertext = rec.ciphertext;
                doc.version = rec.version;
                doc.deleted = rec.deleted;
                doc.updated_at = next_tick();
                results.push(json!({
                    "kind": rec.kind, "record_id": rec.record_id,
                    "accepted": true, "current_version": rec.version,
                }));
            }
            Some(doc) => {
                results.push(json!({
                    "kind": rec.kind, "record_id": rec.record_id,
                    "accepted": false, "current_version": doc.version,
                }));
            }
            None => {
                let version = rec.version;
                guard.push(ServerDoc {
                    user_id: user_id.to_string(),
                    kind: rec.kind.clone(),
                    record_id: rec.record_id.clone(),
                    ciphertext: rec.ciphertext,
                    version,
                    deleted: rec.deleted,
                    updated_at: next_tick(),
                });
                results.push(json!({
                    "kind": rec.kind, "record_id": rec.record_id,
                    "accepted": true, "current_version": version,
                }));
            }
        }
    }
    json!({ "results": results })
}

/// Records per page when a client asks for pages (`paged=1`). The default, no
/// limit, answers in one page, like the server before it paged.
static FAKE_PAGE_SIZE: AtomicUsize = AtomicUsize::new(usize::MAX);

/// Act like a server from before memory sync, which rejects any batch holding
/// a memory record.
static FAKE_REJECTS_MEMORY: AtomicBool = AtomicBool::new(false);

fn query_param(query: &str, name: &str) -> Option<String> {
    query.split('&').find_map(|kv| {
        kv.split_once('=')
            .filter(|(k, _)| *k == name)
            .map(|(_, v)| v.to_string())
    })
}

/// Fake-server counterpart of the GET handler, paging as `pagedPull` does:
/// oldest first by (updated_at, record_id), resuming after the cursor.
fn handle_get(store: &Arc<Mutex<Vec<ServerDoc>>>, user_id: &str, query: &str) -> Value {
    let since: i64 = query_param(query, "since")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let after_id = query_param(query, "after_id");
    let page_size = if query_param(query, "paged").is_some() {
        FAKE_PAGE_SIZE.load(Ordering::SeqCst)
    } else {
        usize::MAX
    };

    let guard = store.lock().unwrap();
    let mut records: Vec<&ServerDoc> = guard
        .iter()
        .filter(|d| {
            d.user_id == user_id
                && match &after_id {
                    Some(after) => {
                        d.updated_at > since || (d.updated_at == since && d.record_id > *after)
                    }
                    None => d.updated_at > since,
                }
        })
        .collect();
    records.sort_by(|a, b| (a.updated_at, &a.record_id).cmp(&(b.updated_at, &b.record_id)));
    let has_more = records.len() > page_size;
    records.truncate(page_size);
    let next = has_more.then(|| {
        let last = records.last().expect("a full page has a last record");
        json!({ "since": last.updated_at, "after_id": last.record_id })
    });

    let out: Vec<Value> = records
        .iter()
        .map(|d| {
            json!({
                "kind": d.kind,
                "record_id": d.record_id,
                "ciphertext": d.ciphertext,
                "version": d.version,
                "deleted": d.deleted,
                "device_id": "",
            })
        })
        .collect();
    json!({ "records": out, "server_time": next_tick(), "has_more": has_more, "next": next })
}

#[test]
fn two_device_kv_set_pull_delete_push_round_trip() -> Result<()> {
    let _guard = crate::test_home_lock()
        .lock()
        .map_err(|_| anyhow!("failed to lock test HOME mutex"))?;

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let store: Arc<Mutex<Vec<ServerDoc>>> = Arc::new(Mutex::new(Vec::new()));
        let server = FakeSyncServer::start(store).await?;

        let old_api_url = env::var_os("SIDEKAR_API_URL");
        let old_home = env::var_os("HOME");
        unsafe {
            env::set_var("SIDEKAR_API_URL", format!("http://{}", server.addr));
        }

        let uid = "shared-account";
        let account_key = vec![11u8; 32];

        let home_a = env::temp_dir().join(format!("sidekar-sync-test-a-{}", unique_suffix()));
        let home_b = env::temp_dir().join(format!("sidekar-sync-test-b-{}", unique_suffix()));
        fs::create_dir_all(&home_a)?;
        fs::create_dir_all(&home_b)?;

        // HOME first: resetting clears the persisted user id, and doing that
        // before the switch cleared the developer's real one.
        let switch_to = |home: &std::path::Path| {
            unsafe { env::set_var("HOME", home) };
            reset_encryption_state();
            set_encryption_key(account_key.clone());
            set_current_user_id(uid.to_string());
            auth_set("token", uid).expect("auth_set should persist the fake device token");
        };

        // Device A: create a key, then push it.
        switch_to(&home_a);
        kv_set("shared-key", "value-from-a", None)?;
        let push_a = push_dirty(uid, Duration::from_secs(5)).await?;
        assert_eq!(push_a.pushed, 1);
        assert_eq!(push_a.failed, 0);

        // Device B: pull, must see A's value.
        switch_to(&home_b);
        let pull_b = pull_merge(uid).await?;
        assert_eq!(pull_b.applied, 1);
        let seen = kv_get("shared-key")?.expect("device B should see device A's kv row");
        assert_eq!(seen.value, "value-from-a");

        // Device B deletes it and pushes the tombstone.
        kv_delete("shared-key")?;
        let push_b = push_dirty(uid, Duration::from_secs(5)).await?;
        assert_eq!(push_b.pushed, 1);

        // Device A pulls again: the key must be gone.
        switch_to(&home_a);
        let pull_a = pull_merge(uid).await?;
        assert_eq!(pull_a.applied, 1);
        assert!(kv_get("shared-key")?.is_none());

        server.stop();
        // Still inside device A's HOME, so this clears A's state, not the real one.
        reset_encryption_state();
        match old_api_url {
            Some(v) => unsafe { env::set_var("SIDEKAR_API_URL", v) },
            None => unsafe { env::remove_var("SIDEKAR_API_URL") },
        }
        match old_home {
            Some(v) => unsafe { env::set_var("HOME", v) },
            None => unsafe { env::remove_var("HOME") },
        }
        let _ = fs::remove_dir_all(&home_a);
        let _ = fs::remove_dir_all(&home_b);

        Ok(())
    })
}

// ---------------------------------------------------------------------------
// Background push retry
// ---------------------------------------------------------------------------

#[test]
fn a_dirty_backlog_is_retried_at_most_once_per_window() -> Result<()> {
    with_test_db(|| {
        let uid = "retry-user";
        let conn = open()?;
        conn.execute(
            "INSERT INTO sync_state (user_id, kind, record_id, version, deleted, dirty, updated_at) \
             VALUES (?1, 'kv', 'k', 1, 0, 1, 0)",
            params![uid],
        )?;
        let now = 10_000;
        assert!(claim_push_retry(uid, now)?, "dirty and never tried");
        assert!(!claim_push_retry(uid, now + 1)?, "just tried");
        assert!(!claim_push_retry(uid, now + PUSH_RETRY_SECS - 1)?);
        assert!(
            claim_push_retry(uid, now + PUSH_RETRY_SECS)?,
            "the window has passed"
        );
        conn.execute(
            "UPDATE sync_state SET dirty = 0 WHERE user_id = ?1",
            params![uid],
        )?;
        assert!(
            !claim_push_retry(uid, now + 10 * PUSH_RETRY_SECS)?,
            "nothing to push"
        );
        Ok(())
    })
}

#[test]
fn an_hotp_counter_merges_to_the_higher_of_the_two() {
    with_test_db(|| {
        reset_encryption_state();
        let uid = "hotp-merge";
        set_encryption_key(vec![7u8; 32]);
        set_current_user_id(uid.to_string());

        hotp_add("duo", "me", "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ", "SHA1", 6, 9)?;
        let rid = totp_record_id("duo", "me");
        let conn = open()?;
        let key = get_encryption_key().unwrap();

        // A remote record with a LOWER counter must not drag ours back:
        // that would let a server-accepted code be issued again.
        let lower = super::encryption::sync_encrypt(&key, &serde_json::json!({
            "secret": "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ",
            "algorithm": "SHA1", "digits": 6, "period": 30, "counter": 3,
        }).to_string())?;
        apply_remote_record(&conn, uid, "hotp", &rid, &lower, 99, false)?;
        assert_eq!(totp_get("duo", "me")?.unwrap().counter, 9, "kept the higher local counter");

        // A remote record with a HIGHER counter wins.
        let higher = super::encryption::sync_encrypt(&key, &serde_json::json!({
            "secret": "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ",
            "algorithm": "SHA1", "digits": 6, "period": 30, "counter": 40,
        }).to_string())?;
        apply_remote_record(&conn, uid, "hotp", &rid, &higher, 100, false)?;
        assert_eq!(totp_get("duo", "me")?.unwrap().counter, 40, "took the higher remote counter");
        assert_eq!(totp_get("duo", "me")?.unwrap().kind, "hotp");
        Ok::<(), anyhow::Error>(())
    })
    .unwrap();
}

// ---------------------------------------------------------------------------
// Memory sync (#31)
// ---------------------------------------------------------------------------

fn push_record(ciphertext_len: usize) -> PushRecord {
    PushRecord {
        kind: "memory".into(),
        record_id: "r".into(),
        ciphertext: "x".repeat(ciphertext_len),
        version: 1,
        device_id: "d".into(),
        deleted: false,
    }
}

fn batch_sizes(records: &[PushRecord], max_records: usize, max_bytes: usize) -> Vec<usize> {
    push_batches(records, max_records, max_bytes)
        .iter()
        .map(|b| b.len())
        .collect()
}

#[test]
fn push_batches_split_on_record_count_and_on_bytes() {
    let small: Vec<_> = (0..5).map(|_| push_record(1)).collect();
    assert_eq!(batch_sizes(&small, 2, 100), vec![2, 2, 1], "count limit");

    let forties: Vec<_> = (0..3).map(|_| push_record(40)).collect();
    assert_eq!(batch_sizes(&forties, 500, 100), vec![2, 1], "byte limit");

    let mixed = vec![push_record(10), push_record(500), push_record(10)];
    assert_eq!(
        batch_sizes(&mixed, 500, 100),
        vec![1, 1, 1],
        "an oversized record goes alone, with no empty batches around it"
    );

    assert!(push_batches(&[], 500, 100).is_empty());
}

#[test]
fn seeding_claims_unowned_memories_but_never_another_accounts() -> Result<()> {
    with_test_db(|| {
        reset_encryption_state();
        let conn = open()?;
        conn.execute_batch(
            "INSERT INTO memory_events (uid, project, event_type, scope, summary, summary_norm,
                                        created_at, updated_at)
             VALUES ('m-unowned', 'alpha', 'convention', 'project', 'unowned', 'unowned', 0, 0);
             INSERT INTO memory_events (uid, sync_owner, project, event_type, scope, summary,
                                        summary_norm, created_at, updated_at)
             VALUES ('m-theirs', 'someone-else', 'alpha', 'convention', 'project', 'theirs',
                     'theirs', 0, 0);",
        )?;

        seed_sync_state(&conn, "me")?;

        let seeded: Vec<String> = conn
            .prepare(
                "SELECT record_id FROM sync_state
                  WHERE user_id = 'me' AND kind = 'memory' AND dirty = 1",
            )?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        assert_eq!(seeded, vec!["m-unowned".to_string()]);
        let owner: String = conn.query_row(
            "SELECT sync_owner FROM memory_events WHERE uid = 'm-unowned'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(owner, "me");
        Ok(())
    })
}

#[test]
fn adding_the_memory_uid_column_rewinds_the_pull_watermark_once() -> Result<()> {
    with_test_db(|| {
        // A database from before memory sync: no uid column, and a pull
        // watermark already set.
        let conn = open()?;
        conn.execute_batch(
            "DROP INDEX idx_memory_events_uid;
             ALTER TABLE memory_events DROP COLUMN uid;
             INSERT INTO sync_meta (user_id, last_pull_at) VALUES ('u', 12345);",
        )?;
        drop(conn);

        let watermark = |conn: &Connection| -> Result<i64> {
            Ok(conn.query_row(
                "SELECT last_pull_at FROM sync_meta WHERE user_id = 'u'",
                [],
                |r| r.get(0),
            )?)
        };

        let conn = open()?; // the upgrade
        assert_eq!(watermark(&conn)?, 0, "rewound so the next pull re-reads the history");

        conn.execute("UPDATE sync_meta SET last_pull_at = 777 WHERE user_id = 'u'", [])?;
        drop(conn);
        let conn = open()?; // any later open
        assert_eq!(watermark(&conn)?, 777, "only the open that adds the column rewinds");
        Ok(())
    })
}

#[test]
fn two_device_memory_sync_follows_pages_and_carries_tombstones() -> Result<()> {
    let _guard = crate::test_home_lock()
        .lock()
        .map_err(|_| anyhow!("failed to lock test HOME mutex"))?;

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let store: Arc<Mutex<Vec<ServerDoc>>> = Arc::new(Mutex::new(Vec::new()));
        let server = FakeSyncServer::start(store.clone()).await?;

        let old_api_url = env::var_os("SIDEKAR_API_URL");
        let old_home = env::var_os("HOME");
        unsafe {
            env::set_var("SIDEKAR_API_URL", format!("http://{}", server.addr));
        }

        let uid = "memory-account";
        let account_key = vec![13u8; 32];
        let home_a = env::temp_dir().join(format!("sidekar-memsync-a-{}", unique_suffix()));
        let home_b = env::temp_dir().join(format!("sidekar-memsync-b-{}", unique_suffix()));
        fs::create_dir_all(&home_a)?;
        fs::create_dir_all(&home_b)?;

        let switch_to = |home: &std::path::Path| {
            unsafe { env::set_var("HOME", home) };
            reset_encryption_state();
            set_encryption_key(account_key.clone());
            set_current_user_id(uid.to_string());
            auth_set("token", uid).expect("auth_set should persist the fake device token");
        };

        // Device A: five memories, pushed.
        switch_to(&home_a);
        let conn = open()?;
        for i in 0..5 {
            let mem_uid = format!("mem-{i}");
            conn.execute(
                "INSERT INTO memory_events (uid, sync_owner, project, event_type, scope, summary,
                                            summary_norm, created_at, updated_at)
                 VALUES (?1, ?2, 'alpha', 'convention', 'project', ?3, ?3, 1, 1)",
                params![mem_uid, uid, format!("memory number {i}")],
            )?;
            mark_dirty(&conn, uid, "memory", &mem_uid, false)?;
        }
        drop(conn);
        assert_eq!(push_dirty(uid, Duration::from_secs(5)).await?.pushed, 5);
        assert!(
            store
                .lock()
                .unwrap()
                .iter()
                .all(|d| !d.ciphertext.contains("memory number")),
            "the server only ever holds ciphertext"
        );

        // Device B pulls two records a page and still gets all five.
        switch_to(&home_b);
        FAKE_PAGE_SIZE.store(2, Ordering::SeqCst);
        let pulled = pull_merge(uid).await;
        FAKE_PAGE_SIZE.store(usize::MAX, Ordering::SeqCst);
        assert_eq!(pulled?.applied, 5, "followed every page");
        let conn = open()?;
        let owned: i64 = conn.query_row(
            "SELECT COUNT(*) FROM memory_events WHERE sync_owner = ?1",
            [uid],
            |r| r.get(0),
        )?;
        assert_eq!(owned, 5);
        let indexed: i64 = conn.query_row(
            "SELECT COUNT(*) FROM memory_events_fts WHERE memory_events_fts MATCH 'number'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(indexed, 5, "searchable on arrival");

        // Device B deletes one and pushes the tombstone; device A drops it.
        conn.execute("DELETE FROM memory_events WHERE uid = 'mem-3'", [])?;
        mark_dirty(&conn, uid, "memory", "mem-3", true)?;
        drop(conn);
        assert_eq!(push_dirty(uid, Duration::from_secs(5)).await?.pushed, 1);

        switch_to(&home_a);
        assert_eq!(pull_merge(uid).await?.applied, 1, "only the tombstone is news to A");
        let conn = open()?;
        let left: Vec<String> = conn
            .prepare("SELECT uid FROM memory_events ORDER BY uid")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        assert_eq!(left, vec!["mem-0", "mem-1", "mem-2", "mem-4"]);
        drop(conn);

        server.stop();
        reset_encryption_state();
        match old_api_url {
            Some(v) => unsafe { env::set_var("SIDEKAR_API_URL", v) },
            None => unsafe { env::remove_var("SIDEKAR_API_URL") },
        }
        match old_home {
            Some(v) => unsafe { env::set_var("HOME", v) },
            None => unsafe { env::remove_var("HOME") },
        }
        let _ = fs::remove_dir_all(&home_a);
        let _ = fs::remove_dir_all(&home_b);
        Ok(())
    })
}

#[test]
fn a_server_without_memory_sync_does_not_hold_up_kv() -> Result<()> {
    let _guard = crate::test_home_lock()
        .lock()
        .map_err(|_| anyhow!("failed to lock test HOME mutex"))?;

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let store: Arc<Mutex<Vec<ServerDoc>>> = Arc::new(Mutex::new(Vec::new()));
        let server = FakeSyncServer::start(store).await?;
        let old_api_url = env::var_os("SIDEKAR_API_URL");
        let old_home = env::var_os("HOME");
        let home = env::temp_dir().join(format!("sidekar-memsync-old-{}", unique_suffix()));
        fs::create_dir_all(&home)?;
        unsafe {
            env::set_var("SIDEKAR_API_URL", format!("http://{}", server.addr));
            env::set_var("HOME", &home);
        }
        let uid = "old-server-account";
        reset_encryption_state();
        set_encryption_key(vec![17u8; 32]);
        set_current_user_id(uid.to_string());
        auth_set("token", uid).expect("auth_set should persist the fake device token");

        kv_set("k", "v", None)?;
        let conn = open()?;
        conn.execute(
            "INSERT INTO memory_events (uid, sync_owner, project, event_type, scope, summary,
                                        summary_norm, created_at, updated_at)
             VALUES ('mem-x', ?1, 'alpha', 'convention', 'project', 'x', 'x', 1, 1)",
            [uid],
        )?;
        mark_dirty(&conn, uid, "memory", "mem-x", false)?;

        FAKE_REJECTS_MEMORY.store(true, Ordering::SeqCst);
        let pushed = push_dirty(uid, Duration::from_secs(5)).await;
        FAKE_REJECTS_MEMORY.store(false, Ordering::SeqCst);
        let pushed = pushed?;
        assert_eq!(pushed.pushed, 1, "kv went through");
        assert_eq!(pushed.failed, 1, "memory waits for a server that takes it");

        let dirty = |kind: &str| -> Result<i64> {
            Ok(conn.query_row(
                "SELECT dirty FROM sync_state WHERE user_id = ?1 AND kind = ?2",
                params![uid, kind],
                |r| r.get(0),
            )?)
        };
        assert_eq!(dirty("kv")?, 0);
        assert_eq!(dirty("memory")?, 1, "still queued for the next push");
        drop(conn);

        server.stop();
        reset_encryption_state();
        match old_api_url {
            Some(v) => unsafe { env::set_var("SIDEKAR_API_URL", v) },
            None => unsafe { env::remove_var("SIDEKAR_API_URL") },
        }
        match old_home {
            Some(v) => unsafe { env::set_var("HOME", v) },
            None => unsafe { env::remove_var("HOME") },
        }
        let _ = fs::remove_dir_all(&home);
        Ok(())
    })
}

// ---------------------------------------------------------------------------
// Per-device keys and refused pushes
// ---------------------------------------------------------------------------

#[test]
fn per_device_kv_keys_never_queue_for_sync() -> Result<()> {
    with_test_db(|| {
        reset_encryption_state();
        set_encryption_key(vec![19u8; 32]);
        set_current_user_id("u".to_string());
        kv_set("internal:device_id", "abc", None)?;
        kv_set("shared", "v", None)?;
        let conn = open()?;
        seed_sync_state(&conn, "u")?;
        let queued: Vec<String> = conn
            .prepare("SELECT record_id FROM sync_state WHERE kind = 'kv' ORDER BY record_id")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        assert_eq!(queued, vec!["shared"], "neither a write nor seeding queues it");
        Ok(())
    })
}

#[test]
fn a_pulled_per_device_key_is_left_out() -> Result<()> {
    with_test_db(|| {
        reset_encryption_state();
        let uid = "u";
        set_encryption_key(vec![19u8; 32]);
        set_current_user_id(uid.to_string());
        kv_set("internal:device_id", "mine", None)?;
        let key = get_encryption_key().unwrap();
        let theirs = super::super::encryption::sync_encrypt(
            &key,
            &serde_json::json!({ "value": "theirs", "tags": [] }).to_string(),
        )?;
        let conn = open()?;
        assert!(!apply_remote_record(&conn, uid, "kv", "internal:device_id", &theirs, 99, false)?);
        assert_eq!(kv_get("internal:device_id")?.unwrap().value, "mine");
        Ok(())
    })
}

#[test]
fn leftover_sync_rows_for_per_device_keys_are_dropped_on_open() -> Result<()> {
    with_test_db(|| {
        let conn = open()?;
        conn.execute_batch(
            "INSERT INTO sync_state (user_id, kind, record_id, version, deleted, dirty, updated_at)
             VALUES ('u', 'kv', 'internal:device_id', 5, 0, 1, 0),
                    ('u', 'kv', 'shared', 1, 0, 1, 0);",
        )?;
        drop(conn);
        let conn = open()?;
        let left: Vec<String> = conn
            .prepare("SELECT record_id FROM sync_state ORDER BY record_id")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        assert_eq!(left, vec!["shared"], "the stuck per-device row goes, nothing else");
        Ok(())
    })
}

#[test]
fn two_devices_at_the_same_version_converge_instead_of_deadlocking() -> Result<()> {
    let _guard = crate::test_home_lock()
        .lock()
        .map_err(|_| anyhow!("failed to lock test HOME mutex"))?;

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let store: Arc<Mutex<Vec<ServerDoc>>> = Arc::new(Mutex::new(Vec::new()));
        let server = FakeSyncServer::start(store).await?;
        let old_api_url = env::var_os("SIDEKAR_API_URL");
        let old_home = env::var_os("HOME");
        unsafe {
            env::set_var("SIDEKAR_API_URL", format!("http://{}", server.addr));
        }

        let uid = "conflict-account";
        let account_key = vec![23u8; 32];
        let home_a = env::temp_dir().join(format!("sidekar-conflict-a-{}", unique_suffix()));
        let home_b = env::temp_dir().join(format!("sidekar-conflict-b-{}", unique_suffix()));
        fs::create_dir_all(&home_a)?;
        fs::create_dir_all(&home_b)?;
        let switch_to = |home: &std::path::Path| {
            unsafe { env::set_var("HOME", home) };
            reset_encryption_state();
            set_encryption_key(account_key.clone());
            set_current_user_id(uid.to_string());
            auth_set("token", uid).expect("auth_set should persist the fake device token");
        };

        // Each device sets the same key on its own, so both reach version 1.
        switch_to(&home_a);
        kv_set("shared", "from-a", None)?;
        switch_to(&home_b);
        kv_set("shared", "from-b", None)?;

        switch_to(&home_a);
        assert_eq!(push_dirty(uid, Duration::from_secs(5)).await?.pushed, 1);

        // B's version-1 push is refused. B moves past the server's version and
        // re-pushes in the same call, instead of staying refused forever.
        switch_to(&home_b);
        let pushed = push_dirty(uid, Duration::from_secs(5)).await?;
        assert_eq!((pushed.pushed, pushed.failed, pushed.bumped), (1, 0, 1));
        let conn = open()?;
        let state: (i64, i64) = conn.query_row(
            "SELECT version, dirty FROM sync_state
              WHERE user_id = ?1 AND kind = 'kv' AND record_id = 'shared'",
            [uid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        assert_eq!(state, (2, 0), "settled, not stuck dirty");
        drop(conn);

        // A adopts B's value and keeps its own in history.
        switch_to(&home_a);
        assert_eq!(pull_merge(uid).await?.applied, 1);
        assert_eq!(kv_get("shared")?.unwrap().value, "from-b");
        assert!(
            kv_history("shared")?
                .iter()
                .any(|h| h.value.as_deref() == Ok("from-a"))
        );

        server.stop();
        reset_encryption_state();
        match old_api_url {
            Some(v) => unsafe { env::set_var("SIDEKAR_API_URL", v) },
            None => unsafe { env::remove_var("SIDEKAR_API_URL") },
        }
        match old_home {
            Some(v) => unsafe { env::set_var("HOME", v) },
            None => unsafe { env::remove_var("HOME") },
        }
        let _ = fs::remove_dir_all(&home_a);
        let _ = fs::remove_dir_all(&home_b);
        Ok(())
    })
}
