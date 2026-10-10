use super::*;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Mirrors `src/broker/tests.rs` `with_test_db`: swaps HOME for a fresh temp
/// dir for the duration of `f`, serialized against every other test that
/// touches process-global broker state (HOME, the encryption key, the
/// current user id). HOME is restored and the directory removed even if `f`
/// panics.
fn with_test_db<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    let _home = crate::ScratchHome::new();
    f()
}

fn reset_encryption_state() {
    clear_encryption_key();
    clear_current_user_id();
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
    // The key is process-wide: hold the test lock, or this clears another
    // test's key mid-run.
    let _home = crate::ScratchHome::new();
    let key = vec![3u8; 32];
    let local_blob = encrypt_with_key_for_test(&key, "hello local")?;
    assert!(local_blob.starts_with("$encrypted$"));
    assert!(super::super::encryption::sync_decrypt(&key, &local_blob).is_err());
    Ok(())
}

#[test]
fn local_decrypt_rejects_sync_prefix() -> Result<()> {
    let _home = crate::ScratchHome::new();
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
    /// The `?channel=bus` collection, apart from the secrets one as on the
    /// real server.
    bus: Arc<Mutex<Vec<ServerDoc>>>,
}

impl FakeSyncServer {
    async fn start(store: Arc<Mutex<Vec<ServerDoc>>>) -> Result<Self> {
        let bus: Arc<Mutex<Vec<ServerDoc>>> = Arc::new(Mutex::new(Vec::new()));
        let bus_for_server = bus.clone();
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
                            let bus = bus_for_server.clone();
                            tokio::spawn(async move {
                                let _ = handle_conn(stream, store, bus).await;
                            });
                        }
                    }
                }
            }
        });

        Ok(Self {
            addr,
            shutdown: Some(tx),
            bus,
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
    secrets: Arc<Mutex<Vec<ServerDoc>>>,
    bus: Arc<Mutex<Vec<ServerDoc>>>,
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
    let bus_channel = query_param(query, "channel").as_deref() == Some("bus");
    let store = if bus_channel { bus } else { secrets };
    // Each channel takes only its own kinds, as `validateRecord` does.
    let wrong_kind = method == "PUT"
        && if bus_channel {
            body_str.contains("\"kind\":\"kv\"") || body_str.contains("\"kind\":\"memory\"")
        } else {
            body_str.contains("\"kind\":\"agent\"") || body_str.contains("\"kind\":\"bus\"")
        };

    let (status, response_body) = match method.as_str() {
        "PUT" if bus_channel
            && FAKE_REJECTS_LEASE.load(Ordering::SeqCst)
            && body_str.contains("\"kind\":\"lease\"") =>
        {
            (
                "400 Bad Request",
                json!({ "error": "kind must be one of: agent, bus" }),
            )
        }
        "PUT" if wrong_kind => (
            "400 Bad Request",
            json!({ "error": "kind not on this channel" }),
        ),
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

/// Act like a server from before refresh leases, which refuses the kind.
static FAKE_REJECTS_LEASE: AtomicBool = AtomicBool::new(false);

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
    // Holds the HOME lock, and restores HOME however the test ends. The
    // device homes below are removed the same way.
    let _home = crate::ScratchHome::new();

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let store: Arc<Mutex<Vec<ServerDoc>>> = Arc::new(Mutex::new(Vec::new()));
        let server = FakeSyncServer::start(store).await?;

        let old_api_url = env::var_os("SIDEKAR_API_URL");
        unsafe {
            env::set_var("SIDEKAR_API_URL", format!("http://{}", server.addr));
        }

        let uid = "shared-account";
        let account_key = vec![11u8; 32];

        let home_a = crate::ScratchDir::new("sync-test-a");
        let home_b = crate::ScratchDir::new("sync-test-b");

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
        switch_to(home_a.path());
        kv_set("shared-key", "value-from-a", None)?;
        let push_a = push_dirty(uid, Duration::from_secs(5)).await?;
        assert_eq!(push_a.pushed, 1);
        assert_eq!(push_a.failed, 0);

        // Device B: pull, must see A's value.
        switch_to(home_b.path());
        let pull_b = pull_merge(uid).await?;
        assert_eq!(pull_b.applied, 1);
        let seen = kv_get("shared-key")?.expect("device B should see device A's kv row");
        assert_eq!(seen.value, "value-from-a");

        // Device B deletes it and pushes the tombstone.
        kv_delete("shared-key")?;
        let push_b = push_dirty(uid, Duration::from_secs(5)).await?;
        assert_eq!(push_b.pushed, 1);

        // Device A pulls again: the key must be gone.
        switch_to(home_a.path());
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
    // Holds the HOME lock, and restores HOME however the test ends. The
    // device homes below are removed the same way.
    let _home = crate::ScratchHome::new();

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let store: Arc<Mutex<Vec<ServerDoc>>> = Arc::new(Mutex::new(Vec::new()));
        let server = FakeSyncServer::start(store.clone()).await?;

        let old_api_url = env::var_os("SIDEKAR_API_URL");
        unsafe {
            env::set_var("SIDEKAR_API_URL", format!("http://{}", server.addr));
        }

        let uid = "memory-account";
        let account_key = vec![13u8; 32];
        let home_a = crate::ScratchDir::new("memsync-a");
        let home_b = crate::ScratchDir::new("memsync-b");

        let switch_to = |home: &std::path::Path| {
            unsafe { env::set_var("HOME", home) };
            reset_encryption_state();
            set_encryption_key(account_key.clone());
            set_current_user_id(uid.to_string());
            auth_set("token", uid).expect("auth_set should persist the fake device token");
        };

        // Device A: five memories, pushed.
        switch_to(home_a.path());
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
        switch_to(home_b.path());
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

        switch_to(home_a.path());
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
        Ok(())
    })
}

#[test]
fn a_server_without_memory_sync_does_not_hold_up_kv() -> Result<()> {
    // Holds the HOME lock, and restores HOME however the test ends. The
    // device homes below are removed the same way.
    let _home = crate::ScratchHome::new();

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let store: Arc<Mutex<Vec<ServerDoc>>> = Arc::new(Mutex::new(Vec::new()));
        let server = FakeSyncServer::start(store).await?;
        let old_api_url = env::var_os("SIDEKAR_API_URL");
        let home = crate::ScratchDir::new("memsync-old");
        unsafe {
            env::set_var("SIDEKAR_API_URL", format!("http://{}", server.addr));
            env::set_var("HOME", home.path());
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
    // Holds the HOME lock, and restores HOME however the test ends. The
    // device homes below are removed the same way.
    let _home = crate::ScratchHome::new();

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let store: Arc<Mutex<Vec<ServerDoc>>> = Arc::new(Mutex::new(Vec::new()));
        let server = FakeSyncServer::start(store).await?;
        let old_api_url = env::var_os("SIDEKAR_API_URL");
        unsafe {
            env::set_var("SIDEKAR_API_URL", format!("http://{}", server.addr));
        }

        let uid = "conflict-account";
        let account_key = vec![23u8; 32];
        let home_a = crate::ScratchDir::new("conflict-a");
        let home_b = crate::ScratchDir::new("conflict-b");
        let switch_to = |home: &std::path::Path| {
            unsafe { env::set_var("HOME", home) };
            reset_encryption_state();
            set_encryption_key(account_key.clone());
            set_current_user_id(uid.to_string());
            auth_set("token", uid).expect("auth_set should persist the fake device token");
        };

        // Each device sets the same key on its own, so both reach version 1.
        switch_to(home_a.path());
        kv_set("shared", "from-a", None)?;
        switch_to(home_b.path());
        kv_set("shared", "from-b", None)?;

        switch_to(home_a.path());
        assert_eq!(push_dirty(uid, Duration::from_secs(5)).await?.pushed, 1);

        // B's version-1 push is refused. B moves past the server's version and
        // re-pushes in the same call, instead of staying refused forever.
        switch_to(home_b.path());
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
        switch_to(home_a.path());
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
        Ok(())
    })
}

// ---------------------------------------------------------------------------
// Bus across machines (context/bus-sync.md)
// ---------------------------------------------------------------------------

#[test]
fn two_machines_see_each_others_agents_and_exchange_a_request_and_answer() -> Result<()> {
    let _home = crate::ScratchHome::new();
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let store: Arc<Mutex<Vec<ServerDoc>>> = Arc::new(Mutex::new(Vec::new()));
        let server = FakeSyncServer::start(store.clone()).await?;
        let old_api_url = env::var_os("SIDEKAR_API_URL");
        unsafe { env::set_var("SIDEKAR_API_URL", format!("http://{}", server.addr)) };

        let uid = "shared-account";
        let home_a = crate::ScratchDir::new("bus-sync-a");
        let home_b = crate::ScratchDir::new("bus-sync-b");
        let switch_to = |home: &std::path::Path| {
            unsafe { env::set_var("HOME", home) };
            reset_encryption_state();
            set_encryption_key(vec![21u8; 32]);
            set_current_user_id(uid.to_string());
            auth_set("token", uid).expect("auth_set should persist the fake device token");
        };

        // Machine A runs an agent; its round publishes it.
        switch_to(home_a.path());
        let agent = crate::message::AgentId {
            name: "claude-app-1".into(),
            nick: Some("otter".into()),
            session: Some("/src/app".into()),
            pane: Some("pty-424242".into()),
            agent_type: Some("claude".into()),
        };
        register_agent(&agent, Some("pty-424242"))?;
        bus_sync_round(uid).await?;
        assert!(
            store.lock().unwrap().is_empty(),
            "nothing on the secrets channel"
        );
        assert_eq!(server.bus.lock().unwrap().len(), 1);
        let device_a = device_id(&open()?)?;

        // Machine B sees it, by name or nick.
        switch_to(home_b.path());
        pull_bus(uid).await?;
        let conn = open()?;
        let found = crate::broker::bus_sync::find_remote_agent(&conn, uid, "otter")?
            .expect("machine A's agent is visible on B");
        assert_eq!(found.device_id, device_a);

        // B's one-shot shell asks it something.
        let asker = crate::message::AgentId::new("cli-app-7");
        let request =
            crate::message::Envelope::new_request(asker, "claude-app-1", "review the diff");
        set_outbound_request(&request, "cli-app-7", "bus_sync", "x", None, None)?;
        let device_b = device_id(&conn)?;
        crate::broker::bus_sync::queue_remote_message(
            &conn,
            uid,
            &device_b,
            &device_a,
            "claude-app-1",
            "cli-app-7",
            "[from cli-app-7] review the diff",
            Some(&request),
        )?;
        drop(conn);
        push_bus(uid, Duration::from_secs(5)).await?;

        // A's next round delivers it into the agent's queue.
        switch_to(home_a.path());
        bus_sync_round(uid).await?;
        let queued = list_queued_messages("claude-app-1")?;
        assert_eq!(queued.len(), 1);
        assert!(queued[0].body.contains("review the diff"));
        assert_eq!(
            crate::broker::bus_sync::origin_device(&open()?, &request.id)?.as_deref(),
            Some(device_b.as_str())
        );

        // Pulling the same window again, as the overlap and other pullers do,
        // delivers nothing twice.
        pull_bus(uid).await?;
        bus_sync_round(uid).await?;
        assert_eq!(list_queued_messages("claude-app-1")?.len(), 1);

        // A message the server stamped at or below A's watermark (another
        // instance's clock, or a commit after A's read) still arrives.
        let watermark: i64 = open()?.query_row(
            "SELECT last_bus_pull_at FROM sync_meta WHERE user_id = ?1",
            params![uid],
            |r| r.get(0),
        )?;
        switch_to(home_b.path());
        let late = crate::message::Envelope::new_fyi(
            crate::message::AgentId::new("cli-app-7"),
            "claude-app-1",
            "one more thing".to_string(),
        );
        crate::broker::bus_sync::queue_remote_message(
            &open()?,
            uid,
            &device_b,
            &device_a,
            "claude-app-1",
            "cli-app-7",
            "one more thing",
            Some(&late),
        )?;
        push_bus(uid, Duration::from_secs(5)).await?;
        for doc in server.bus.lock().unwrap().iter_mut() {
            if doc.record_id == late.id {
                doc.updated_at = watermark;
            }
        }
        switch_to(home_a.path());
        pull_bus(uid).await?;
        assert_eq!(
            list_queued_messages("claude-app-1")?.len(),
            2,
            "the late-stamped one arrived"
        );

        // The agent answers; the asker has left, so it goes to the machine that asked.
        let answer = crate::message::Envelope::new_response(
            agent.clone(),
            "cli-app-7",
            "looks fine",
            request.id.clone(),
        );
        crate::broker::bus_sync::queue_remote_message(
            &open()?,
            uid,
            &device_a,
            &device_b,
            "cli-app-7",
            "claude-app-1",
            "looks fine",
            Some(&answer),
        )?;
        push_bus(uid, Duration::from_secs(5)).await?;

        // B pulls (as `bus await` does) and has the answer for the request.
        switch_to(home_b.path());
        pull_bus(uid).await?;
        let replies = replies_for_request(&request.id)?;
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].message, "looks fine");

        // Delivered messages were tombstoned on the server.
        push_bus(uid, Duration::from_secs(5)).await?;
        let live_messages = server
            .bus
            .lock()
            .unwrap()
            .iter()
            .filter(|d| d.kind == "bus" && !d.deleted)
            .count();
        assert_eq!(live_messages, 0, "both messages delivered and let go");

        // A's agent leaves: B stops listing it after A's next round.
        switch_to(home_a.path());
        unregister_agent("claude-app-1")?;
        bus_sync_round(uid).await?;
        switch_to(home_b.path());
        pull_bus(uid).await?;
        assert!(crate::broker::bus_sync::live_remote_agents(&open()?, uid)?.is_empty());

        server.stop();
        reset_encryption_state();
        match old_api_url {
            Some(v) => unsafe { env::set_var("SIDEKAR_API_URL", v) },
            None => unsafe { env::remove_var("SIDEKAR_API_URL") },
        }
        Ok(())
    })
}

// ---------------------------------------------------------------------------
// Per-device kv keys that used to sync (`_nick:`, `gemini_cache:`)
// ---------------------------------------------------------------------------

#[test]
fn legacy_per_device_keys_move_under_internal_and_their_synced_copies_are_tombstoned() -> Result<()> {
    with_test_db(|| {
        kv_set("_nick:/tmp/p", "borzoi", None)?;
        kv_set("gemini_cache:abc", "{}", Some(&["gemini_cache".to_string()]))?;
        // Already set under the new name: kept, the legacy value dropped.
        kv_set("internal:nick:/tmp/q", "corgi", None)?;
        kv_set("_nick:/tmp/q", "stale", None)?;
        kv_set("shared", "v", None)?;
        let conn = open()?;
        let uid = current_user_id().unwrap_or_default();
        // As a build from before the move left them: synced, and one
        // tombstone already pushed.
        conn.execute_batch(&format!(
            "DELETE FROM sync_state;
             INSERT INTO sync_state (user_id, kind, record_id, version, deleted, dirty, updated_at) VALUES
               ('{uid}', 'kv', '_nick:/tmp/p', 3, 0, 0, 0),
               ('{uid}', 'kv', 'gemini_cache:abc', 1, 0, 1, 0),
               ('{uid}', 'kv', 'gemini_cache:old', 2, 1, 0, 0),
               ('{uid}', 'kv', 'shared', 1, 0, 0, 0);"
        ))?;
        drop(conn);

        let conn = open()?;
        assert_eq!(kv_get("internal:nick:/tmp/p")?.unwrap().value, "borzoi");
        assert_eq!(kv_get("internal:nick:/tmp/q")?.unwrap().value, "corgi");
        let cache = kv_get("internal:gemini_cache:abc")?.unwrap();
        assert_eq!(cache.tags, vec!["gemini_cache".to_string()], "tags move with it");
        for gone in ["_nick:/tmp/p", "_nick:/tmp/q", "gemini_cache:abc"] {
            assert!(kv_get(gone)?.is_none(), "{gone} should be renamed");
        }
        let rows: Vec<(String, i64, bool, bool)> = conn
            .prepare("SELECT record_id, version, deleted, dirty FROM sync_state ORDER BY record_id")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<rusqlite::Result<_>>()?;
        assert_eq!(
            rows,
            vec![
                ("_nick:/tmp/p".to_string(), 4, true, true),
                ("gemini_cache:abc".to_string(), 2, true, true),
                ("shared".to_string(), 1, false, false),
            ],
            "synced copies get a tombstone to push; a pushed tombstone and new keys leave no row"
        );

        // Once the tombstones are pushed, the rows go on the next open.
        conn.execute("UPDATE sync_state SET dirty = 0", [])?;
        drop(conn);
        let conn = open()?;
        let left: Vec<String> = conn
            .prepare("SELECT record_id FROM sync_state ORDER BY record_id")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        assert_eq!(left, vec!["shared"]);
        Ok(())
    })
}

#[test]
fn a_legacy_per_device_key_from_an_older_build_is_not_adopted() -> Result<()> {
    with_test_db(|| {
        let conn = open()?;
        let uid = current_user_id().unwrap_or_default();
        for key in ["_nick:/tmp/p", "gemini_cache:abc"] {
            assert!(!super::super::kv_store::kv_key_syncs(key));
            assert!(!apply_remote_record(&conn, &uid, "kv", key, "x", 99, false)?);
        }
        assert!(kv_get("_nick:/tmp/p")?.is_none());
        assert!(kv_get("internal:nick:/tmp/p")?.is_none());
        Ok(())
    })
}

// ---------------------------------------------------------------------------
// Refresh leases
// ---------------------------------------------------------------------------

#[test]
fn one_machine_at_a_time_holds_a_refresh_lease() -> Result<()> {
    let _home = crate::ScratchHome::new();
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let store: Arc<Mutex<Vec<ServerDoc>>> = Arc::new(Mutex::new(Vec::new()));
        let server = FakeSyncServer::start(store).await?;
        let old_api_url = env::var_os("SIDEKAR_API_URL");
        unsafe { env::set_var("SIDEKAR_API_URL", format!("http://{}", server.addr)) };

        let uid = "shared-account";
        let account_key = vec![11u8; 32];
        let home_a = crate::ScratchDir::new("lease-a");
        let home_b = crate::ScratchDir::new("lease-b");
        let switch_to = |home: &std::path::Path| {
            unsafe { env::set_var("HOME", home) };
            reset_encryption_state();
            set_encryption_key(account_key.clone());
            set_current_user_id(uid.to_string());
            auth_set("token", uid).expect("auth_set should persist the fake device token");
        };

        switch_to(home_a.path());
        let a = claim_lease("oauth:anthropic").await?;
        assert!(matches!(a, LeaseClaim::Held { .. }), "first claim wins: {a:?}");
        let LeaseClaim::Held { until } = a else { unreachable!() };
        let now = crate::message::epoch_secs();
        assert!(until >= now + LEASE_SLOT_SECS, "held for at least one full slot");

        switch_to(home_b.path());
        let b = claim_lease("oauth:anthropic").await?;
        assert!(matches!(b, LeaseClaim::HeldElsewhere { .. }), "second machine is refused: {b:?}");
        // A different credential has its own lease.
        assert!(matches!(claim_lease("oauth:codex").await?, LeaseClaim::Held { .. }));

        // The holder can claim again while it holds it.
        switch_to(home_a.path());
        assert!(matches!(claim_lease("oauth:anthropic").await?, LeaseClaim::Held { .. }));

        // A server from before leases: go ahead without one.
        FAKE_REJECTS_LEASE.store(true, Ordering::SeqCst);
        switch_to(home_b.path());
        let old = claim_lease("oauth:grok").await;
        FAKE_REJECTS_LEASE.store(false, Ordering::SeqCst);
        assert_eq!(old?, LeaseClaim::Unavailable);

        // Leases stay out of the bus pull.
        let pulled = pull_bus(uid).await?;
        assert_eq!(pulled.applied, 0);

        server.stop();
        reset_encryption_state();
        match old_api_url {
            Some(v) => unsafe { env::set_var("SIDEKAR_API_URL", v) },
            None => unsafe { env::remove_var("SIDEKAR_API_URL") },
        }
        Ok(())
    })
}

#[test]
fn another_machines_agent_activity_is_seen_and_a_message_it_missed_bounces_back() -> Result<()> {
    let _home = crate::ScratchHome::new();
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let store: Arc<Mutex<Vec<ServerDoc>>> = Arc::new(Mutex::new(Vec::new()));
        let server = FakeSyncServer::start(store.clone()).await?;
        let old_api_url = env::var_os("SIDEKAR_API_URL");
        unsafe { env::set_var("SIDEKAR_API_URL", format!("http://{}", server.addr)) };

        let uid = "shared-account";
        let home_a = crate::ScratchDir::new("bus-gaps-a");
        let home_b = crate::ScratchDir::new("bus-gaps-b");
        let switch_to = |home: &std::path::Path| {
            unsafe { env::set_var("HOME", home) };
            reset_encryption_state();
            set_encryption_key(vec![23u8; 32]);
            set_current_user_id(uid.to_string());
            auth_set("token", uid).expect("auth_set should persist the fake device token");
        };
        let remote_state = |name: &str| -> Result<Option<String>> {
            let agents = crate::broker::bus_sync::live_remote_agents(&open()?, uid)?;
            Ok(agents
                .into_iter()
                .find(|a| a.name == name)
                .and_then(|a| a.activity)
                .map(|a| a.state))
        };

        // Machine A: an agent at work.
        switch_to(home_a.path());
        let agent = crate::message::AgentId {
            name: "claude-app-1".into(),
            nick: Some("otter".into()),
            session: Some("/src/app".into()),
            pane: Some("pty-525252".into()),
            agent_type: Some("claude".into()),
        };
        register_agent(&agent, Some("pty-525252"))?;
        let now = crate::message::epoch_secs();
        update_agent_activity("claude-app-1", crate::activity::ActivityState::AgentWorking, now)?;
        bus_sync_round(uid).await?;
        let device_a = device_id(&open()?)?;

        // Machine B sees it working, in `agents` too.
        switch_to(home_b.path());
        pull_bus(uid).await?;
        assert_eq!(remote_state("claude-app-1")?.as_deref(), Some("agent_working"));
        let (_, found) = crate::broker::bus_sync::lookup_remote_agent("otter")?
            .expect("found by nick from another machine");
        let row = crate::commands::agents::remote_row(&found, crate::message::epoch_secs());
        assert_eq!(row.attention, crate::commands::agents::Attention::Working);
        assert!(row.host.is_some());

        // A's agent finishes; its next round says so at once, not at the
        // two-minute heartbeat.
        switch_to(home_a.path());
        update_agent_activity("claude-app-1", crate::activity::ActivityState::Idle, now + 1)?;
        bus_sync_round(uid).await?;
        switch_to(home_b.path());
        pull_bus(uid).await?;
        assert_eq!(remote_state("claude-app-1")?.as_deref(), Some("idle"));

        // B asks it something; A's agent leaves before A's next round.
        let conn = open()?;
        let asker = crate::message::AgentId::new("cli-app-9");
        let request = crate::message::Envelope::new_request(asker, "claude-app-1", "review");
        set_outbound_request(
            &request,
            "cli-app-9",
            crate::bus::BUS_SYNC_TRANSPORT,
            &found.record_id(),
            None,
            None,
        )?;
        let device_b = device_id(&conn)?;
        crate::broker::bus_sync::queue_remote_message(
            &conn,
            uid,
            &device_b,
            &device_a,
            "claude-app-1",
            "cli-app-9",
            "[from cli-app-9] review",
            Some(&request),
        )?;
        drop(conn);
        push_bus(uid, Duration::from_secs(5)).await?;

        switch_to(home_a.path());
        unregister_agent("claude-app-1")?;
        bus_sync_round(uid).await?;
        assert!(list_queued_messages("claude-app-1")?.is_empty());

        // B's pull brings the bounce: the request is closed, so `bus await`
        // fails at once instead of waiting.
        switch_to(home_b.path());
        let outcome = crate::bus::await_reply::await_reply(
            &request.id,
            None,
            Duration::from_secs(10),
        )
        .await?;
        assert!(
            matches!(outcome, crate::bus::await_reply::AwaitOutcome::RecipientGone { .. }),
            "undeliverable request reported: {outcome:?}"
        );
        assert!(crate::broker::bus_sync::live_remote_agents(&open()?, uid)?.is_empty());

        server.stop();
        reset_encryption_state();
        match old_api_url {
            Some(v) => unsafe { env::set_var("SIDEKAR_API_URL", v) },
            None => unsafe { env::remove_var("SIDEKAR_API_URL") },
        }
        Ok(())
    })
}
