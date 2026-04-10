use super::*;

// ---------------------------------------------------------------------------
// WebSocket frame parser tests
// ---------------------------------------------------------------------------

fn mask(payload: &[u8], key: [u8; 4]) -> Vec<u8> {
    payload
        .iter()
        .enumerate()
        .map(|(i, b)| b ^ key[i % 4])
        .collect()
}

#[tokio::test]
async fn ws_small_text_frame_unmasked() {
    // FIN=1, opcode=TEXT, len=5, "hello"
    let bytes: Vec<u8> = vec![0x81, 0x05, b'h', b'e', b'l', b'l', b'o'];
    let mut r = &bytes[..];
    let (raw, frame) = ws::read_frame(&mut r).await.unwrap().expect("frame");
    assert_eq!(raw, bytes);
    assert!(frame.fin);
    assert!(!frame.rsv1);
    assert_eq!(frame.opcode, ws::OP_TEXT);
    assert_eq!(frame.payload, b"hello");
}

#[tokio::test]
async fn ws_masked_client_frame() {
    let key = [0xAA, 0xBB, 0xCC, 0xDD];
    let payload = b"sidekar rules";
    let masked = mask(payload, key);
    let mut bytes: Vec<u8> = vec![0x81, 0x80 | (payload.len() as u8)];
    bytes.extend_from_slice(&key);
    bytes.extend_from_slice(&masked);
    let mut r = &bytes[..];
    let (raw, frame) = ws::read_frame(&mut r).await.unwrap().expect("frame");
    assert_eq!(raw, bytes, "raw bytes must echo the wire format");
    assert!(frame.fin);
    assert_eq!(frame.opcode, ws::OP_TEXT);
    assert_eq!(frame.payload, payload);
}

#[tokio::test]
async fn ws_extended_16bit_length() {
    let payload = vec![b'x'; 300];
    let len = payload.len() as u16;
    let mut bytes = vec![0x82, 126];
    bytes.extend_from_slice(&len.to_be_bytes());
    bytes.extend_from_slice(&payload);
    let mut r = &bytes[..];
    let (_, frame) = ws::read_frame(&mut r).await.unwrap().expect("frame");
    assert_eq!(frame.opcode, ws::OP_BINARY);
    assert_eq!(frame.payload.len(), 300);
}

#[tokio::test]
async fn ws_fragmented_message_reassembly() {
    // Frame 1: FIN=0, TEXT, "hel"
    let f1: Vec<u8> = vec![0x01, 0x03, b'h', b'e', b'l'];
    // Frame 2: FIN=0, CONTINUATION, "lo "
    let f2: Vec<u8> = vec![0x00, 0x03, b'l', b'o', b' '];
    // Frame 3: FIN=1, CONTINUATION, "world"
    let f3: Vec<u8> = vec![0x80, 0x05, b'w', b'o', b'r', b'l', b'd'];
    let mut all = Vec::new();
    all.extend_from_slice(&f1);
    all.extend_from_slice(&f2);
    all.extend_from_slice(&f3);
    let mut r = &all[..];

    let mut acc = ws::MessageAcc::new();
    let mut seen: Option<Vec<u8>> = None;
    while let Some((_, frame)) = ws::read_frame(&mut r).await.unwrap() {
        if let Some(msg) = acc.push(frame) {
            if let ws::Message::Data { opcode, payload, .. } = msg {
                assert_eq!(opcode, ws::OP_TEXT);
                seen = Some(payload);
                break;
            }
        }
    }
    assert_eq!(seen.as_deref(), Some(&b"hello world"[..]));
}

#[tokio::test]
async fn ws_control_frames_classified() {
    // ping with no payload
    let ping: Vec<u8> = vec![0x89, 0x00];
    let mut r = &ping[..];
    let (_, frame) = ws::read_frame(&mut r).await.unwrap().expect("frame");
    let mut acc = ws::MessageAcc::new();
    assert!(matches!(acc.push(frame), Some(ws::Message::Ping)));

    // close with 2-byte status code
    let close: Vec<u8> = vec![0x88, 0x02, 0x03, 0xe8];
    let mut r = &close[..];
    let (_, frame) = ws::read_frame(&mut r).await.unwrap().expect("frame");
    assert!(matches!(acc.push(frame), Some(ws::Message::Close)));
}

/// End-to-end check: drive a real WebSocket handshake + echo round-trip
/// through the MITM proxy. Validates that our frame reader forwards raw
/// bytes unchanged and that the 101 response is parsed without corrupting
/// the upstream stream.
#[tokio::test]
async fn mitm_websocket_echo_roundtrip() {
    use base64::Engine as _;
    use futures_util::{SinkExt as _, StreamExt as _};
    use rustls::pki_types::CertificateDer;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio_tungstenite::{
        Connector, client_async_tls_with_config, tungstenite::protocol::Message,
    };

    let (port, ca_path) = start(true).await.expect("proxy start");

    // Open a raw TCP connection to the proxy and speak CONNECT ourselves —
    // tokio-tungstenite doesn't honor HTTPS_PROXY, and we want to exercise
    // the MITM CONNECT path under test.
    let mut tcp = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))
        .await
        .expect("connect to proxy");
    tcp.write_all(
        b"CONNECT ws.postman-echo.com:443 HTTP/1.1\r\nHost: ws.postman-echo.com:443\r\n\r\n",
    )
    .await
    .expect("write CONNECT");
    tcp.flush().await.unwrap();

    // Read CONNECT response until CRLFCRLF.
    let mut resp: Vec<u8> = Vec::new();
    let mut byte = [0u8; 1];
    while !resp.ends_with(b"\r\n\r\n") {
        match tcp.read(&mut byte).await {
            Ok(0) | Err(_) => break,
            Ok(_) => resp.push(byte[0]),
        }
    }
    assert!(
        resp.starts_with(b"HTTP/1.1 200"),
        "CONNECT failed: {}",
        String::from_utf8_lossy(&resp)
    );

    // Build a rustls ClientConfig that trusts only the proxy's MITM CA.
    // Parse the PEM manually (rustls-pemfile isn't a direct dep).
    let pem = std::fs::read_to_string(&ca_path).expect("read CA pem");
    let b64: String = pem
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .collect::<Vec<_>>()
        .join("");
    let der = base64::prelude::BASE64_STANDARD
        .decode(b64.as_bytes())
        .expect("decode CA base64");
    let mut root_store = rustls::RootCertStore::empty();
    root_store.add(CertificateDer::from(der)).expect("add CA");
    let client_config = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();
    let connector = Some(Connector::Rustls(std::sync::Arc::new(client_config)));

    // Do the WebSocket handshake over the MITM'd TLS. The proxy should
    // present a leaf cert signed by its own CA, which we trust above.
    let (mut ws, response) =
        client_async_tls_with_config("wss://ws.postman-echo.com/raw", tcp, None, connector)
            .await
            .expect("WebSocket handshake through MITM proxy");
    assert_eq!(response.status().as_u16(), 101);

    // ws.postman-echo.com sends a welcome message before echoing. Send
    // a tagged payload and drain until we see it returned.
    let payload = "hello-from-sidekar-mitm-test";
    ws.send(Message::Text(payload.to_string().into()))
        .await
        .expect("send text");

    let mut saw_echo = false;
    for _ in 0..8 {
        match tokio::time::timeout(std::time::Duration::from_secs(10), ws.next()).await {
            Ok(Some(Ok(Message::Text(t)))) => {
                if t == payload {
                    saw_echo = true;
                    break;
                }
            }
            Ok(Some(Ok(_))) => {}
            Ok(Some(Err(e))) => panic!("ws error: {e}"),
            Ok(None) => break,
            Err(_) => panic!("timed out waiting for echo"),
        }
    }
    assert!(saw_echo, "echo server did not return our payload");
    let _ = ws.close(None).await;

    cleanup_ca_file(&ca_path);
}

#[tokio::test]
async fn ws_permessage_deflate_roundtrip() {
    use flate2::Compression;
    use flate2::write::DeflateEncoder;
    use std::io::Write;

    let original = br#"{"type":"response.output_text.delta","delta":"hello"}"#;
    // Compress raw deflate, then strip trailing 0x00 0x00 0xff 0xff per RFC 7692.
    let mut enc = DeflateEncoder::new(Vec::new(), Compression::default());
    enc.write_all(original).unwrap();
    let mut compressed = enc.finish().unwrap();
    if compressed.ends_with(&[0x00, 0x00, 0xff, 0xff]) {
        compressed.truncate(compressed.len() - 4);
    }

    let mut decomp = flate2::Decompress::new(false);
    let inflated = ws::inflate_with(&mut decomp, &compressed, 1024 * 1024).unwrap();
    assert_eq!(inflated, original);
}

/// Context-takeover round-trip: encode N separate messages with a shared
/// compressor (the RFC 7692 default), strip the sync-flush trailer from each,
/// then decode through a shared `Decompress` instance. Each decoded message
/// must match its original — this is exactly how codex's permessage-deflate
/// stream behaves on the wire.
#[tokio::test]
async fn ws_permessage_deflate_context_takeover_multi() {
    use flate2::Compress;
    use flate2::Compression;
    use flate2::FlushCompress;

    let messages: Vec<&[u8]> = vec![
        br#"{"type":"response.create","input":[{"type":"function_call_output","call_id":"c1","output":"hello world"}]}"#,
        br#"{"type":"response.create","input":[{"type":"function_call_output","call_id":"c2","output":"hello again"}]}"#,
        br#"{"type":"response.create","input":[{"type":"function_call_output","call_id":"c3","output":"still hello"}]}"#,
        br#"{"type":"response.create","input":[{"type":"function_call_output","call_id":"c4","output":"final hello"}]}"#,
    ];

    fn encode_one(comp: &mut Compress, msg: &[u8]) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::new();
        let mut scratch = [0u8; 8192];
        // Phase 1: feed all payload bytes with FlushCompress::None.
        let mut in_pos = 0usize;
        while in_pos < msg.len() {
            let before_in = comp.total_in();
            let before_out = comp.total_out();
            comp.compress(&msg[in_pos..], &mut scratch, FlushCompress::None)
                .unwrap();
            let consumed = (comp.total_in() - before_in) as usize;
            let produced = (comp.total_out() - before_out) as usize;
            in_pos += consumed;
            out.extend_from_slice(&scratch[..produced]);
        }
        // Phase 2: one Sync flush. zlib emits all pending bytes followed by
        // the 0x00 0x00 0xff 0xff sync marker. If it does not fit in one
        // scratch buffer, loop until we actually see the trailer.
        loop {
            let before_out = comp.total_out();
            comp.compress(&[], &mut scratch, FlushCompress::Sync).unwrap();
            let produced = (comp.total_out() - before_out) as usize;
            out.extend_from_slice(&scratch[..produced]);
            if out.ends_with(&[0x00, 0x00, 0xff, 0xff]) {
                break;
            }
            if produced == 0 {
                break;
            }
        }
        // Strip the 0x00 0x00 0xff 0xff sync trailer per RFC 7692 §7.2.1.
        if out.ends_with(&[0x00, 0x00, 0xff, 0xff]) {
            out.truncate(out.len() - 4);
        }
        out
    }

    // One compressor, shared across all messages (context takeover).
    let mut comp = Compress::new(Compression::default(), false);
    let encoded: Vec<Vec<u8>> =
        messages.iter().map(|m| encode_one(&mut comp, m)).collect();

    // Now decode with a shared Decompress (context takeover on receive side).
    let mut decomp = flate2::Decompress::new(false);
    for (i, enc) in encoded.iter().enumerate() {
        let inflated = ws::inflate_with(&mut decomp, enc, 1024 * 1024).unwrap();
        assert_eq!(
            inflated,
            messages[i],
            "message {i} decoded incorrectly: got {:?}",
            String::from_utf8_lossy(&inflated)
        );
    }
}

/// Regression: codex request envelopes are ~35 KB each. With a 16 KB scratch
/// buffer, `inflate_with` used to stop as soon as total_in caught up with the
/// input length — leaving bytes buffered inside `Decompress` that then leaked
/// into the *next* message's decoded output on the next call.
///
/// This test feeds 4 large envelopes through shared compress/decompress and
/// requires every message to decode to exactly its original bytes.
#[tokio::test]
async fn ws_permessage_deflate_large_context_takeover() {
    use flate2::Compress;
    use flate2::Compression;
    use flate2::FlushCompress;

    fn make_envelope(seq: usize) -> Vec<u8> {
        // Build a ~35 KB JSON-ish envelope whose content is non-repeating
        // enough to push the 16 KB scratch boundary but still compresses well.
        let mut body = String::with_capacity(40 * 1024);
        body.push_str(&format!(
            "{{\"type\":\"response.create\",\"seq\":{seq},\"instructions\":\""
        ));
        for i in 0..900 {
            body.push_str(&format!(
                "line {seq}-{i}: the quick brown fox jumps over the lazy dog; \
                 pack my box with five dozen liquor jugs -- sphinx of black quartz, \
                 judge my vow. "
            ));
        }
        body.push_str("\"}");
        body.into_bytes()
    }

    fn encode_one(comp: &mut Compress, msg: &[u8]) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::new();
        let mut scratch = [0u8; 8192];
        let mut in_pos = 0usize;
        while in_pos < msg.len() {
            let before_in = comp.total_in();
            let before_out = comp.total_out();
            comp.compress(&msg[in_pos..], &mut scratch, FlushCompress::None)
                .unwrap();
            in_pos += (comp.total_in() - before_in) as usize;
            let produced = (comp.total_out() - before_out) as usize;
            out.extend_from_slice(&scratch[..produced]);
        }
        loop {
            let before_out = comp.total_out();
            comp.compress(&[], &mut scratch, FlushCompress::Sync).unwrap();
            let produced = (comp.total_out() - before_out) as usize;
            out.extend_from_slice(&scratch[..produced]);
            if out.ends_with(&[0x00, 0x00, 0xff, 0xff]) || produced == 0 {
                break;
            }
        }
        if out.ends_with(&[0x00, 0x00, 0xff, 0xff]) {
            out.truncate(out.len() - 4);
        }
        out
    }

    let messages: Vec<Vec<u8>> = (0..4).map(make_envelope).collect();
    assert!(
        messages[0].len() > 16 * 1024,
        "envelope must exceed scratch size ({}b)",
        messages[0].len()
    );

    let mut comp = Compress::new(Compression::default(), false);
    let encoded: Vec<Vec<u8>> = messages.iter().map(|m| encode_one(&mut comp, m)).collect();

    let mut decomp = flate2::Decompress::new(false);
    for (i, enc) in encoded.iter().enumerate() {
        let inflated = ws::inflate_with(&mut decomp, enc, 10 * 1024 * 1024).unwrap();
        assert_eq!(
            inflated.len(),
            messages[i].len(),
            "message {i} length mismatch: got {}, want {}",
            inflated.len(),
            messages[i].len()
        );
        assert_eq!(
            inflated, messages[i],
            "message {i} decoded incorrectly (first 80 inflated bytes: {:?})",
            String::from_utf8_lossy(&inflated[..inflated.len().min(80)])
        );
    }
}

#[test]
fn proxy_dir_does_not_depend_on_home() {
    let _guard = crate::test_home_lock()
        .lock()
        .unwrap_or_else(|_| panic!("failed to lock test HOME mutex"));
    let old_home = std::env::var_os("HOME");
    let fake_home =
        std::env::temp_dir().join(format!("sidekar-proxy-home-test-{}", std::process::id()));
    std::fs::create_dir_all(&fake_home).expect("create fake home");
    unsafe { std::env::set_var("HOME", &fake_home) };

    let dir = proxy_dir();

    match old_home {
        Some(home) => unsafe { std::env::set_var("HOME", home) },
        None => unsafe { std::env::remove_var("HOME") },
    }
    let _ = std::fs::remove_dir_all(&fake_home);

    assert!(dir.starts_with(std::env::temp_dir()));
    assert!(!dir.starts_with(&fake_home));
}

/// Build a client that uses CONNECT proxy (MITM mode).
async fn mitm_client(port: u16, ca_path: &std::path::Path) -> reqwest::Client {
    let ca_pem =
        std::fs::read(ca_path).unwrap_or_else(|e| panic!("read {}: {e}", ca_path.display()));
    let ca_cert = reqwest::Certificate::from_pem(&ca_pem).expect("parse ca cert");
    reqwest::Client::builder()
        .proxy(reqwest::Proxy::https(format!("http://127.0.0.1:{port}")).unwrap())
        .add_root_certificate(ca_cert)
        .build()
        .expect("build client")
}

/// Build a plain HTTP client (reverse proxy mode — no TLS on local leg).
fn reverse_client() -> reqwest::Client {
    reqwest::Client::builder().build().expect("build client")
}

#[tokio::test]
async fn mitm_passthrough() {
    let (port, ca_path) = start(true).await.expect("proxy start");
    let client = mitm_client(port, &ca_path).await;

    let resp = client
        .get("https://httpbin.org/get")
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .expect("request through MITM proxy");

    assert!(resp.status().is_success());
    cleanup_ca_file(&ca_path);
}

#[tokio::test]
async fn mitm_anthropic() {
    let (port, ca_path) = start(true).await.expect("proxy start");
    let client = mitm_client(port, &ca_path).await;

    let resp = client
        .get("https://api.anthropic.com/v1/models")
        .header("x-api-key", "test-invalid")
        .header("anthropic-version", "2023-06-01")
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .expect("request through MITM proxy to anthropic");

    assert_eq!(resp.status().as_u16(), 401);
    cleanup_ca_file(&ca_path);
}

#[tokio::test]
async fn reverse_proxy_anthropic() {
    let (port, ca_path) = start(true).await.expect("proxy start");
    let client = reverse_client();

    // Simulate ANTHROPIC_BASE_URL=http://127.0.0.1:<port>
    let resp = client
        .get(format!("http://127.0.0.1:{port}/v1/models"))
        .header("x-api-key", "test-invalid")
        .header("anthropic-version", "2023-06-01")
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .expect("request through reverse proxy");

    // 401/403 = request reached upstream (403 if CDN rejects Host mismatch)
    let s = resp.status().as_u16();
    assert!(s == 401 || s == 403, "unexpected status: {s}");
    cleanup_ca_file(&ca_path);
}

#[tokio::test]
async fn reverse_proxy_openai() {
    let (port, ca_path) = start(true).await.expect("proxy start");
    let client = reverse_client();

    // Simulate OPENAI_BASE_URL=http://127.0.0.1:<port>/v1
    let resp = client
        .get(format!("http://127.0.0.1:{port}/v1/models"))
        .header("authorization", "Bearer test-invalid")
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .expect("request through reverse proxy to openai");

    let s = resp.status().as_u16();
    assert!(s == 401 || s == 403, "unexpected status: {s}");
    cleanup_ca_file(&ca_path);
}
