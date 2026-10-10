//! A tiny HTTP server for tests that need to see what a client sends.
//!
//! The Slack and Linear clients take their base URL as a parameter, so a test
//! points one at this, answers with a canned body, and then asserts on the
//! exact method, path, headers and body that went out. One thread, one request
//! per connection, `Connection: close`: enough for a reqwest client and nothing
//! more.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub(crate) struct Request {
    pub method: String,
    /// Path and query, as sent.
    pub target: String,
    /// Header names lower-cased.
    pub headers: HashMap<String, String>,
    pub body: String,
}

impl Request {
    pub fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or("")
    }

    pub fn query(&self) -> HashMap<String, String> {
        crate::oauth_loopback::query_params(&self.target)
    }

    pub fn form(&self) -> HashMap<String, String> {
        crate::oauth_loopback::query_params(&format!("?{}", self.body))
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).unwrap_or(serde_json::Value::Null)
    }
}

pub(crate) struct MockServer {
    pub base: String,
    seen: Arc<Mutex<Vec<Request>>>,
}

type Responder = dyn Fn(&Request) -> (u16, String) + Send + Sync;

impl MockServer {
    /// Serve every request with `respond`, recording each one.
    pub fn start(respond: impl Fn(&Request) -> (u16, String) + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen: Arc<Mutex<Vec<Request>>> = Arc::default();
        let record = seen.clone();
        let respond: Arc<Responder> = Arc::new(respond);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let Some(req) = read_request(&mut stream) else {
                    continue;
                };
                let (status, body) = respond(&req);
                record.lock().unwrap().push(req);
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
                let _ = stream.flush();
            }
        });
        Self { base, seen }
    }

    /// Answer each request with the next body in order, all `200`.
    pub fn sequence(bodies: Vec<serde_json::Value>) -> Self {
        let queue = Mutex::new(std::collections::VecDeque::from(bodies));
        Self::start(move |_| {
            let next = queue
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(serde_json::json!({"error": "mock server ran out of responses"}));
            (200, next.to_string())
        })
    }

    pub fn requests(&self) -> Vec<Request> {
        self.seen.lock().unwrap().clone()
    }

    /// A client that never routes through a proxy, so loopback stays loopback.
    pub fn client() -> reqwest::Client {
        crate::http_client::client_builder()
            .no_proxy()
            .build()
            .unwrap()
    }
}

fn read_request(stream: &mut std::net::TcpStream) -> Option<Request> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    let mut headers = HashMap::new();
    loop {
        let mut h = String::new();
        reader.read_line(&mut h).ok()?;
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let len: usize = headers
        .get("content-length")
        .and_then(|l| l.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body).ok()?;
    Some(Request {
        method,
        target,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}
