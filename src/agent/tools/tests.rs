use super::execute;
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

#[tokio::test]
async fn bash_tool_cancels_promptly() {
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_setter = cancel.clone();

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel_setter.store(true, Ordering::Relaxed);
    });

    let started = Instant::now();
    let result = execute(
        "bash",
        &json!({
            "command": "sleep 5",
            "timeout": 10
        }),
        Some(&cancel),
    )
    .await;

    assert!(result.is_err());
    assert!(
        result
            .expect_err("cancelled tool")
            .is::<crate::agent::Cancelled>()
    );
    assert!(started.elapsed() < Duration::from_secs(2));
}
