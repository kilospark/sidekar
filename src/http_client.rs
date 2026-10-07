//! Shared reqwest client construction.
//!
//! Behind a TLS-intercepting proxy, reqwest's rustls backend is the odd one
//! out: it trusts only its bundled webpki roots, ignoring `SSL_CERT_FILE` and
//! the system trust store that `curl`, Python, and everything else sidekar
//! shells out to already read. That's why `sidekar gmail` fails with
//! "invalid peer certificate: UnknownIssuer" in exactly the environments
//! where plain `curl` works fine. Loading `SSL_CERT_FILE`, when set, as an
//! extra trust anchor alongside the built-in roots closes that gap without
//! changing anything on hosts where the variable isn't set.
//!
//! Every client that reaches a remote host is built here, async or blocking.
//! The exceptions are clients that only ever talk to localhost (Chrome's
//! debugging port), where there is no proxy to intercept anything.

use anyhow::{Context, Result};
use std::path::Path;
use std::sync::OnceLock;

/// The certificates in a PEM bundle, or why there are none.
pub(crate) fn certs_from_pem(pem: &[u8], origin: &Path) -> Result<Vec<reqwest::Certificate>> {
    let certs = reqwest::Certificate::from_pem_bundle(pem)
        .with_context(|| format!("{} is not a PEM certificate bundle", origin.display()))?;
    if certs.is_empty() {
        anyhow::bail!("{} contains no certificates", origin.display());
    }
    Ok(certs)
}

/// The DER form of each certificate in a PEM bundle, for code that builds a
/// rustls config itself rather than going through reqwest.
pub(crate) fn pem_certs_der(pem: &[u8]) -> Vec<rustls::pki_types::CertificateDer<'static>> {
    use base64::Engine;
    let text = String::from_utf8_lossy(pem);
    let mut certs = Vec::new();
    let mut in_cert = false;
    let mut b64 = String::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed == "-----BEGIN CERTIFICATE-----" {
            in_cert = true;
            b64.clear();
        } else if trimmed == "-----END CERTIFICATE-----" {
            in_cert = false;
            if let Ok(der) = base64::engine::general_purpose::STANDARD.decode(&b64) {
                certs.push(rustls::pki_types::CertificateDer::from(der));
            }
        } else if in_cert {
            b64.push_str(trimmed);
        }
    }
    certs
}

/// `SSL_CERT_FILE`, read and checked once per process.
struct ExtraRoots {
    reqwest: Vec<reqwest::Certificate>,
    der: Vec<rustls::pki_types::CertificateDer<'static>>,
}

fn load_extra_roots(path: &Path) -> Result<ExtraRoots> {
    let pem = std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    Ok(ExtraRoots {
        reqwest: certs_from_pem(&pem, path)?,
        der: pem_certs_der(&pem),
    })
}

fn extra_roots() -> &'static ExtraRoots {
    static ROOTS: OnceLock<ExtraRoots> = OnceLock::new();
    ROOTS.get_or_init(|| {
        let none = || ExtraRoots {
            reqwest: Vec::new(),
            der: Vec::new(),
        };
        let Some(path) = std::env::var_os("SSL_CERT_FILE").filter(|p| !p.is_empty()) else {
            return none();
        };
        match load_extra_roots(Path::new(&path)) {
            Ok(roots) => roots,
            Err(e) => {
                // Said once, here, because the failure it causes shows up
                // somewhere else: a TLS "UnknownIssuer" on a request that
                // gives no hint the bundle meant to fix it was never loaded.
                eprintln!("sidekar: ignoring SSL_CERT_FILE: {e:#}");
                none()
            }
        }
    })
}

fn extra_root_certs() -> &'static [reqwest::Certificate] {
    &extra_roots().reqwest
}

/// The built-in web roots plus `SSL_CERT_FILE`'s, for code that configures
/// rustls directly: sidekar's proxy connecting upstream, and WebSockets.
pub fn web_root_store() -> rustls::RootCertStore {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    for cert in &extra_roots().der {
        let _ = roots.add(cert.clone());
    }
    roots
}

/// A WebSocket TLS connector trusting [`web_root_store`]. `connect_async`
/// on its own uses only the built-in roots.
pub fn ws_connector() -> tokio_tungstenite::Connector {
    ws_connector_trusting(web_root_store())
}

pub(crate) fn ws_connector_trusting(roots: rustls::RootCertStore) -> tokio_tungstenite::Connector {
    let config = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("ring supports the default TLS versions")
    .with_root_certificates(roots)
    .with_no_client_auth();
    tokio_tungstenite::Connector::Rustls(std::sync::Arc::new(config))
}

/// A WebSocket over TLS, as `ws_connect` opens it.
pub type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Open a WebSocket to `request`'s URL with `connector`, through the HTTP
/// proxy `HTTPS_PROXY` names when one is set, as reqwest does for sidekar's
/// HTTP requests.
///
/// A direct dial on a network that only lets traffic out through its proxy
/// reaches something that is not the relay: the proxy itself, or a firewall,
/// answering in plain HTTP. rustls reads that as "received corrupt message
/// of type InvalidContentType", which is all the relay tunnel used to say.
pub async fn ws_connect(
    request: tokio_tungstenite::tungstenite::handshake::client::Request,
    connector: tokio_tungstenite::Connector,
) -> Result<(
    WsStream,
    tokio_tungstenite::tungstenite::handshake::client::Response,
)> {
    let uri = request.uri();
    let host = uri.host().context("WebSocket URL has no host")?.to_string();
    let port = uri.port_u16().unwrap_or(if uri.scheme_str() == Some("ws") {
        80
    } else {
        443
    });
    let Some(proxy) = connect_proxy_for(&host, |k| std::env::var(k).ok())? else {
        return Ok(tokio_tungstenite::connect_async_tls_with_config(
            request,
            None,
            false,
            Some(connector),
        )
        .await?);
    };
    let tcp = tokio::net::TcpStream::connect(&proxy.addr)
        .await
        .with_context(|| format!("could not reach the proxy at {} (HTTPS_PROXY)", proxy.addr))?;
    let tcp = connect_through(tcp, &host, port, proxy.auth.as_deref()).await?;
    Ok(
        tokio_tungstenite::client_async_tls_with_config(request, tcp, None, Some(connector))
            .await?,
    )
}

/// An HTTP proxy to tunnel through with CONNECT.
#[derive(Debug, PartialEq)]
pub(crate) struct ConnectProxy {
    /// The proxy's `host:port`.
    addr: String,
    /// `Proxy-Authorization`, from the user and password in the proxy URL.
    auth: Option<String>,
}

/// The proxy that `HTTPS_PROXY` (or `ALL_PROXY`) names for `host`, unless
/// `NO_PROXY` exempts it. `var` reads the environment, so tests need none.
pub(crate) fn connect_proxy_for(
    host: &str,
    var: impl Fn(&str) -> Option<String>,
) -> Result<Option<ConnectProxy>> {
    let first = |names: &[&str]| {
        names.iter().find_map(|n| {
            var(n)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        })
    };
    let Some(raw) = first(&["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"]) else {
        return Ok(None);
    };
    if let Some(no_proxy) = first(&["NO_PROXY", "no_proxy"]) {
        let host = host.to_ascii_lowercase();
        let exempt = no_proxy.split(',').map(str::trim).any(|entry| {
            let entry = entry.trim_start_matches('.').to_ascii_lowercase();
            entry == "*"
                || (!entry.is_empty() && (host == entry || host.ends_with(&format!(".{entry}"))))
        });
        if exempt {
            return Ok(None);
        }
    }
    let with_scheme = if raw.contains("://") {
        raw.clone()
    } else {
        format!("http://{raw}")
    };
    let url = url::Url::parse(&with_scheme).with_context(|| format!("HTTPS_PROXY {raw:?}"))?;
    if url.scheme() != "http" {
        anyhow::bail!(
            "HTTPS_PROXY {raw:?}: sidekar's WebSockets reach a proxy over plain http://, not {}://",
            url.scheme()
        );
    }
    let proxy_host = url
        .host_str()
        .with_context(|| format!("HTTPS_PROXY {raw:?} has no host"))?;
    let port = url.port_or_known_default().unwrap_or(80);
    let auth = (!url.username().is_empty()).then(|| {
        let decode = |s: &str| {
            urlencoding::decode(s)
                .map(|d| d.into_owned())
                .unwrap_or_default()
        };
        let pair = format!(
            "{}:{}",
            decode(url.username()),
            decode(url.password().unwrap_or_default())
        );
        use base64::Engine;
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(pair)
        )
    });
    Ok(Some(ConnectProxy {
        addr: format!("{proxy_host}:{port}"),
        auth,
    }))
}

/// Ask the proxy on `tcp` for a tunnel to `host:port`, and hand the stream
/// back once it is open.
async fn connect_through(
    mut tcp: tokio::net::TcpStream,
    host: &str,
    port: u16,
    auth: Option<&str>,
) -> Result<tokio::net::TcpStream> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut request = format!("CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n");
    if let Some(auth) = auth {
        request.push_str(&format!("Proxy-Authorization: {auth}\r\n"));
    }
    request.push_str("\r\n");
    tcp.write_all(request.as_bytes()).await?;
    // A byte at a time, so nothing past the response head is read: after it
    // the stream belongs to TLS.
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > 16 * 1024 {
            anyhow::bail!("the proxy's answer to CONNECT {host}:{port} never ended");
        }
        let mut byte = [0u8; 1];
        if tcp.read(&mut byte).await? == 0 {
            anyhow::bail!("the proxy closed the connection when asked to CONNECT {host}:{port}");
        }
        head.push(byte[0]);
    }
    let status_line = String::from_utf8_lossy(&head)
        .lines()
        .next()
        .unwrap_or_default()
        .to_string();
    if status_line.split_whitespace().nth(1) != Some("200") {
        anyhow::bail!("the proxy refused CONNECT {host}:{port}: {status_line}");
    }
    Ok(tcp)
}

/// What usually causes a TLS failure reaching `host`, when `err` is one of
/// the failures with a usual cause.
pub fn explain_tls_failure(err: &anyhow::Error, host: &str) -> Option<String> {
    let chain = format!("{err:#}");
    if chain.contains("InvalidContentType") || chain.contains("corrupt message") {
        return Some(format!(
            "{host} was answered by something that does not speak TLS, as an HTTP proxy \
             or a filtering firewall does. If this network reaches the internet through a \
             proxy, set HTTPS_PROXY to it"
        ));
    }
    if chain.contains("UnknownIssuer") || chain.contains("invalid peer certificate") {
        return Some(format!(
            "{host} presented a certificate from an issuer sidekar does not trust, as a \
             proxy that intercepts TLS does. Point SSL_CERT_FILE at the proxy's CA \
             certificate (PEM)"
        ));
    }
    None
}

/// A [`reqwest::ClientBuilder`] that also trusts `SSL_CERT_FILE`'s
/// certificates, if the environment variable is set, in addition to the
/// built-in roots.
pub fn client_builder() -> reqwest::ClientBuilder {
    let mut builder = reqwest::Client::builder();
    for cert in extra_root_certs() {
        builder = builder.add_root_certificate(cert.clone());
    }
    builder
}

/// [`client_builder`] for the blocking client.
pub fn blocking_client_builder() -> reqwest::blocking::ClientBuilder {
    let mut builder = reqwest::blocking::Client::builder();
    for cert in extra_root_certs() {
        builder = builder.add_root_certificate(cert.clone());
    }
    builder
}

/// A shared client with no configuration beyond [`client_builder`]'s trust
/// anchors, for call sites that used to reach for `reqwest::Client::new()`.
pub fn client() -> reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            client_builder()
                .build()
                .expect("failed to build HTTP client")
        })
        .clone()
}

#[cfg(test)]
mod tests;
