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
