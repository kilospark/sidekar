use super::*;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

const KEY: &str = "oauth:anthropic";

fn creds(access: &str, refresh: &str, expires_in: i64) -> OAuthCredentials {
    OAuthCredentials {
        access_token: access.to_string(),
        refresh_token: refresh.to_string(),
        expires_at: (crate::message::epoch_secs() as i64 + expires_in).max(0) as u64,
        metadata: serde_json::Value::Null,
    }
}

/// Another machine, as the server shows it: what a pull brings in, and how
/// the lease answers.
#[derive(Default)]
struct FakeSync {
    /// Credentials the next pulls bring in, one per pull; a pull with none
    /// left brings nothing.
    arriving: Mutex<Vec<Option<OAuthCredentials>>>,
    /// Lease answers, one per claim; the last repeats.
    claims: Mutex<Vec<LeaseClaim>>,
    pulls: AtomicUsize,
    pushes: AtomicUsize,
}

impl FakeSync {
    fn new(claims: Vec<LeaseClaim>, arriving: Vec<Option<OAuthCredentials>>) -> Self {
        Self {
            arriving: Mutex::new(arriving),
            claims: Mutex::new(claims),
            ..Default::default()
        }
    }
}

impl RefreshSync for FakeSync {
    async fn pull(&self) {
        self.pulls.fetch_add(1, Ordering::SeqCst);
        let mut arriving = self.arriving.lock().unwrap();
        if !arriving.is_empty()
            && let Some(c) = arriving.remove(0)
        {
            save_credentials(KEY, &c).unwrap();
        }
    }
    async fn claim(&self, _lease_id: &str) -> LeaseClaim {
        let mut claims = self.claims.lock().unwrap();
        if claims.len() > 1 {
            claims.remove(0)
        } else {
            claims.first().copied().unwrap_or(LeaseClaim::Unavailable)
        }
    }
    async fn push(&self) {
        self.pushes.fetch_add(1, Ordering::SeqCst);
    }
}

const FAST: Timing = Timing {
    poll: Duration::from_millis(5),
    max_wait: Duration::from_millis(200),
};

const HELD: LeaseClaim = LeaseClaim::Held { until: u64::MAX };
const ELSEWHERE: LeaseClaim = LeaseClaim::HeldElsewhere { until: u64::MAX };

/// Run `f` in a scratch home with a database, on a runtime.
fn run<T>(f: impl Future<Output = Result<T>>) -> Result<T> {
    let _home = crate::ScratchHome::new();
    crate::broker::init_db()?;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(f)
}

/// A refresh that records which refresh token it was given and answers with
/// `result(refresh_token)`.
fn recording(
    seen: &Mutex<Vec<String>>,
    result: impl Fn(&str) -> Result<OAuthCredentials>,
) -> impl Fn(OAuthCredentials) -> std::future::Ready<Result<OAuthCredentials>> {
    move |c: OAuthCredentials| {
        seen.lock().unwrap().push(c.refresh_token.clone());
        std::future::ready(result(&c.refresh_token))
    }
}

#[test]
fn a_token_another_machine_refreshed_is_used_without_refreshing() -> Result<()> {
    run(async {
        save_credentials(KEY, &creds("old", "rt1", -10))?;
        let sync = FakeSync::new(vec![HELD], vec![Some(creds("theirs", "rt2", 3600))]);
        let seen = Mutex::new(Vec::new());
        let got = refresh_shared(&sync, KEY, None, FAST, recording(&seen, |_| unreachable!())).await?;
        assert_eq!(got.access_token, "theirs");
        assert!(seen.lock().unwrap().is_empty(), "no refresh when a fresh token was pulled");
        Ok(())
    })
}

#[test]
fn the_lease_holder_refreshes_saves_and_pushes() -> Result<()> {
    run(async {
        save_credentials(KEY, &creds("old", "rt1", -10))?;
        let sync = FakeSync::new(vec![HELD], vec![]);
        let seen = Mutex::new(Vec::new());
        let got = refresh_shared(
            &sync,
            KEY,
            None,
            FAST,
            recording(&seen, |_| Ok(creds("new", "rt2", 3600))),
        )
        .await?;
        assert_eq!(got.access_token, "new");
        assert_eq!(*seen.lock().unwrap(), vec!["rt1".to_string()]);
        assert_eq!(load(KEY)?.refresh_token, "rt2", "the rotated token is saved");
        assert_eq!(sync.pushes.load(Ordering::SeqCst), 1, "and pushed at once");
        Ok(())
    })
}

#[test]
fn while_another_machine_holds_the_lease_its_token_is_awaited() -> Result<()> {
    run(async {
        save_credentials(KEY, &creds("old", "rt1", -10))?;
        // Nothing on the first two pulls, then the holder's token.
        let sync = FakeSync::new(
            vec![ELSEWHERE],
            vec![None, None, Some(creds("theirs", "rt2", 3600))],
        );
        let seen = Mutex::new(Vec::new());
        let got = refresh_shared(&sync, KEY, None, FAST, recording(&seen, |_| unreachable!())).await?;
        assert_eq!(got.access_token, "theirs");
        assert!(seen.lock().unwrap().is_empty(), "the waiting machine never spends rt1");
        assert!(sync.pulls.load(Ordering::SeqCst) >= 3);
        Ok(())
    })
}

#[test]
fn a_holder_that_never_refreshes_is_passed_over_once_the_wait_runs_out() -> Result<()> {
    run(async {
        save_credentials(KEY, &creds("old", "rt1", -10))?;
        let sync = FakeSync::new(vec![ELSEWHERE], vec![]);
        let seen = Mutex::new(Vec::new());
        let got = refresh_shared(
            &sync,
            KEY,
            None,
            FAST,
            recording(&seen, |_| Ok(creds("mine", "rt2", 3600))),
        )
        .await?;
        assert_eq!(got.access_token, "mine");
        assert_eq!(seen.lock().unwrap().len(), 1);
        Ok(())
    })
}

#[test]
fn a_spent_refresh_token_is_retried_with_the_newer_one_another_machine_pushed() -> Result<()> {
    run(async {
        save_credentials(KEY, &creds("old", "rt1", -10))?;
        // The first pull brings nothing; after the failure, the other
        // machine's rotation has arrived, its access token already expired.
        let sync = FakeSync::new(vec![HELD], vec![None, Some(creds("theirs", "rt2", -10))]);
        let seen = Mutex::new(Vec::new());
        let got = refresh_shared(
            &sync,
            KEY,
            None,
            FAST,
            recording(&seen, |rt| match rt {
                "rt1" => Err(anyhow!("invalid_grant")),
                _ => Ok(creds("new", "rt3", 3600)),
            }),
        )
        .await?;
        assert_eq!(got.access_token, "new");
        assert_eq!(*seen.lock().unwrap(), vec!["rt1".to_string(), "rt2".to_string()]);
        assert_eq!(load(KEY)?.refresh_token, "rt3");
        Ok(())
    })
}

#[test]
fn a_failed_refresh_uses_a_fresh_token_that_arrived_meanwhile() -> Result<()> {
    run(async {
        save_credentials(KEY, &creds("old", "rt1", -10))?;
        let sync = FakeSync::new(vec![HELD], vec![None, Some(creds("theirs", "rt2", 3600))]);
        let seen = Mutex::new(Vec::new());
        let got = refresh_shared(
            &sync,
            KEY,
            None,
            FAST,
            recording(&seen, |_| Err(anyhow!("invalid_grant"))),
        )
        .await?;
        assert_eq!(got.access_token, "theirs");
        assert_eq!(seen.lock().unwrap().len(), 1, "no second refresh needed");
        Ok(())
    })
}

#[test]
fn a_failed_refresh_with_nothing_newer_is_the_error() -> Result<()> {
    run(async {
        save_credentials(KEY, &creds("old", "rt1", -10))?;
        let sync = FakeSync::new(vec![HELD], vec![]);
        let seen = Mutex::new(Vec::new());
        let err = refresh_shared(
            &sync,
            KEY,
            None,
            FAST,
            recording(&seen, |_| Err(anyhow!("invalid_grant"))),
        )
        .await
        .unwrap_err();
        assert!(format!("{err:#}").contains("invalid_grant"));
        assert_eq!(seen.lock().unwrap().len(), 1);
        Ok(())
    })
}

#[test]
fn a_rejected_access_token_is_replaced_even_before_it_expires() -> Result<()> {
    run(async {
        save_credentials(KEY, &creds("turned-down", "rt1", 3600))?;
        let sync = FakeSync::new(vec![HELD], vec![]);
        let seen = Mutex::new(Vec::new());
        let got = refresh_shared(
            &sync,
            KEY,
            Some("turned-down"),
            FAST,
            recording(&seen, |_| Ok(creds("new", "rt2", 3600))),
        )
        .await?;
        assert_eq!(got.access_token, "new");
        assert_eq!(seen.lock().unwrap().len(), 1);
        Ok(())
    })
}

#[test]
fn a_valid_token_is_returned_without_touching_sync() -> Result<()> {
    run(async {
        save_credentials(KEY, &creds("good", "rt1", 3600))?;
        let sync = FakeSync::new(vec![HELD], vec![]);
        let seen = Mutex::new(Vec::new());
        let got = refresh_shared(&sync, KEY, None, FAST, recording(&seen, |_| unreachable!())).await?;
        assert_eq!(got.access_token, "good");
        assert_eq!(sync.pulls.load(Ordering::SeqCst), 0);
        Ok(())
    })
}

#[test]
fn processes_on_one_machine_refresh_one_at_a_time() -> Result<()> {
    // The second caller waits on the file lock, then finds the first one's
    // token and doesn't refresh.
    let _home = crate::ScratchHome::new();
    crate::broker::init_db()?;
    save_credentials(KEY, &creds("old", "rt1", -10))?;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let refreshes = std::sync::Arc::new(AtomicUsize::new(0));
    rt.block_on(async {
        let mut handles = Vec::new();
        for _ in 0..2 {
            let refreshes = refreshes.clone();
            handles.push(tokio::spawn(async move {
                let sync = FakeSync::new(vec![HELD], vec![]);
                refresh_shared(&sync, KEY, None, FAST, move |_c| {
                    let refreshes = refreshes.clone();
                    async move {
                        refreshes.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        Ok(creds("new", "rt2", 3600))
                    }
                })
                .await
                .map(|c| c.access_token)
            }));
        }
        for h in handles {
            assert_eq!(h.await??, "new");
        }
        Ok::<_, anyhow::Error>(())
    })?;
    assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    Ok(())
}
