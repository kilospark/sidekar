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

/// A reply: status, content type, body bytes.
pub(crate) type Reply = (u16, String, Vec<u8>);

type Responder = dyn Fn(&Request) -> Reply + Send + Sync;

impl MockServer {
    /// Serve every request with `respond`, recording each one.
    pub fn start(respond: impl Fn(&Request) -> (u16, String) + Send + Sync + 'static) -> Self {
        Self::start_raw(move |req| {
            let (status, body) = respond(req);
            (status, "application/json".into(), body.into_bytes())
        })
    }

    /// Like [`start`](Self::start), choosing the content type and raw bytes:
    /// for file downloads and upload endpoints that are not JSON.
    pub fn start_raw(respond: impl Fn(&Request) -> Reply + Send + Sync + 'static) -> Self {
        Self::serve(Arc::new(respond), false)
    }

    /// Like [`start`](Self::start), but each connection is answered on its own
    /// thread, so a test can tell whether the client really ran calls in
    /// parallel. Requests are recorded in completion order.
    pub fn start_concurrent(
        respond: impl Fn(&Request) -> (u16, String) + Send + Sync + 'static,
    ) -> Self {
        Self::serve(
            Arc::new(move |req: &Request| {
                let (status, body) = respond(req);
                (status, "application/json".into(), body.into_bytes())
            }),
            true,
        )
    }

    fn serve(respond: Arc<Responder>, concurrent: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen: Arc<Mutex<Vec<Request>>> = Arc::default();
        let record = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let (respond, record) = (respond.clone(), record.clone());
                let answer = move || answer(stream, &*respond, &record);
                if concurrent {
                    std::thread::spawn(answer);
                } else {
                    answer();
                }
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

fn answer(mut stream: std::net::TcpStream, respond: &Responder, record: &Mutex<Vec<Request>>) {
    let Some(req) = read_request(&mut stream) else {
        return;
    };
    let (status, content_type, body) = respond(&req);
    record.lock().unwrap().push(req);
    let head = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: {content_type}\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&body);
    let _ = stream.flush();
}

/// A listener standing in for evil.example: it counts connections, so a test
/// can show the download never reached it (not just that it failed).
pub fn decoy() -> (u16, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let h = hits.clone();
    std::thread::spawn(move || {
        for s in l.incoming() {
            h.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            drop(s);
        }
    });
    (port, hits)
}
