//! Viewer WebSocket: attach read-only-ish terminal to a tunnel session on the relay
//! (same path as `www/public/js/terminal.js`, but Bearer device-token auth).

use anyhow::{Context, Result, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::Message;

const CTRL_DETACH: u8 = 0x1d; // Ctrl+]

#[cfg(not(unix))]
pub async fn attach_remote_relay_terminal(_token: &str, _session_id: &str) -> Result<()> {
    bail!("remote relay terminal (/relay list) is only supported on Unix");
}

#[cfg(unix)]
pub async fn attach_remote_relay_terminal(device_token: &str, session_id: &str) -> Result<()> {
    attach_unix(device_token, session_id).await
}

#[derive(Deserialize)]
struct ResolveResp {
    owner_origin: String,
}

#[cfg(unix)]
async fn attach_unix(device_token: &str, session_id: &str) -> Result<()> {
    let base = crate::transport::relay_http_base();
    let base = base.trim_end_matches('/');
    let enc = urlencoding::encode(session_id);
    let resolve_url = format!("{base}/session/{enc}/resolve");

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(25))
        .build()
        .context("build HTTP client for relay resolve")?;

    let resp = client
        .get(&resolve_url)
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", device_token),
        )
        .send()
        .await
        .with_context(|| format!("GET {resolve_url}"))?;

    if !resp.status().is_success() {
        bail!(
            "relay resolve failed: HTTP {} for {}",
            resp.status(),
            resolve_url
        );
    }

    let resolve: ResolveResp = resp.json().await.context("parse relay resolve JSON")?;

    let ws_origin = origin_to_ws_origin(&resolve.owner_origin)
        .with_context(|| format!("bad owner_origin {:?}", resolve.owner_origin))?;
    let ws_url = format!("{}/session/{}", ws_origin.trim_end_matches('/'), enc);

    let mut request = ws_url
        .as_str()
        .into_client_request()
        .with_context(|| format!("invalid viewer WebSocket URL: {ws_url}"))?;
    request.headers_mut().insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {}", device_token)
            .parse()
            .context("invalid Authorization header value for viewer WS")?,
    );

    let (mut ws_write, mut ws_read) = tokio_tungstenite::connect_async(request)
        .await
        .with_context(|| format!("WebSocket viewer connect failed: {ws_url}"))?
        .0
        .split();

    struct RawRestore {
        saved: libc::termios,
        fd: libc::c_int,
    }

    impl RawRestore {
        fn enter() -> Result<Self> {
            let fd = libc::STDIN_FILENO;
            let mut saved: libc::termios = unsafe { std::mem::zeroed() };
            if unsafe { libc::tcgetattr(fd, &mut saved) } != 0 {
                bail!("tcgetattr: {}", std::io::Error::last_os_error());
            }
            let mut raw = saved;
            unsafe { libc::cfmakeraw(&mut raw) };
            if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
                bail!("tcsetattr: {}", std::io::Error::last_os_error());
            }
            Ok(Self { saved, fd })
        }
    }

    impl Drop for RawRestore {
        fn drop(&mut self) {
            unsafe { libc::tcsetattr(self.fd, libc::TCSANOW, &self.saved) };
        }
    }

    let stdout = std::io::stdout();

    let _raw = RawRestore::enter().context("enter terminal raw mode (need a TTY?)")?;

    eprintln!(
        "\r\n\x1b[2mrelay attach: {}; Ctrl+] to detach\x1b[0m\r",
        &session_id[..session_id.len().min(12)]
    );
    let _ = std::io::stderr().flush();

    let (stdin_tx, mut stdin_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    let stdin_running = Arc::new(AtomicBool::new(true));
    let stdin_running_thread = stdin_running.clone();

    let reader = std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while stdin_running_thread.load(Ordering::Relaxed) {
            let mut pollfd = libc::pollfd {
                fd: libc::STDIN_FILENO,
                events: libc::POLLIN,
                revents: 0,
            };
            let ready = unsafe { libc::poll(&mut pollfd, 1, 50) };
            if ready <= 0 {
                continue;
            }
            if (pollfd.revents & libc::POLLIN) == 0 {
                continue;
            }
            let n = unsafe {
                libc::read(
                    libc::STDIN_FILENO,
                    buf.as_mut_ptr() as *mut libc::c_void,
                    buf.len(),
                )
            };
            if n <= 0 {
                break;
            }
            if stdin_tx.send(buf[..n as usize].to_vec()).is_err() {
                break;
            }
        }
    });

    let mut detach = false;
    let result: Result<()> = async {
        loop {
            if detach {
                break;
            }
            tokio::select! {
                msg = ws_read.next() => {
                    match msg {
                        Some(Ok(Message::Binary(data))) => {
                            let mut lock = stdout.lock();
                            lock.write_all(&data).context("write PTY bytes to stdout")?;
                            lock.flush().ok();
                        }
                        Some(Ok(Message::Text(_))) => {
                            // Session hello and control JSON — browser applies local layout; we stream binary PTY only.
                        }
                        Some(Ok(Message::Ping(p))) => {
                            let _ = ws_write.send(Message::Pong(p)).await;
                        }
                        Some(Ok(Message::Close(_))) | None => {
                            break;
                        }
                        Some(Ok(_)) => {}
                        Some(Err(e)) => bail!("viewer WebSocket error: {e}"),
                    }
                }
                Some(chunk) = stdin_rx.recv() => {
                    let (to_send, should_detach) = filter_detach(chunk);
                    if !to_send.is_empty() {
                        ws_write
                            .send(Message::Binary(to_send.into()))
                            .await
                            .context("send stdin to relay")?;
                    }
                    if should_detach {
                        detach = true;
                    }
                }
            }
        }
        Ok(())
    }
    .await;

    stdin_running.store(false, Ordering::Relaxed);
    drop(_raw);
    let _ = ws_write.close().await;

    let _ = reader.join();

    result?;
    eprintln!("\r\n\x1b[2mrelay attach: detached\x1b[0m");
    let _ = std::io::stderr().flush();

    Ok(())
}

fn origin_to_ws_origin(origin: &str) -> Result<String> {
    let o = origin.trim().trim_end_matches('/');
    if let Some(host) = o.strip_prefix("https://") {
        return Ok(format!("wss://{}", host));
    }
    if let Some(host) = o.strip_prefix("http://") {
        return Ok(format!("ws://{}", host));
    }
    if o.starts_with("wss://") || o.starts_with("ws://") {
        return Ok(o.to_string());
    }
    Err(anyhow!("unsupported relay origin scheme: {origin:?}"))
}

/// Send bytes before the first Ctrl+]; if that byte appears, stop forwarding and detach.
fn filter_detach(mut chunk: Vec<u8>) -> (Vec<u8>, bool) {
    if let Some(pos) = chunk.iter().position(|&b| b == CTRL_DETACH) {
        chunk.truncate(pos);
        (chunk, true)
    } else {
        (chunk, false)
    }
}
