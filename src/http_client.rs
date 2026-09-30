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

use std::sync::OnceLock;

fn extra_root_certs() -> &'static [reqwest::Certificate] {
    static CERTS: OnceLock<Vec<reqwest::Certificate>> = OnceLock::new();
    CERTS.get_or_init(|| {
        let Ok(path) = std::env::var("SSL_CERT_FILE") else {
            return Vec::new();
        };
        let Ok(pem) = std::fs::read(&path) else {
            return Vec::new();
        };
        reqwest::Certificate::from_pem_bundle(&pem).unwrap_or_default()
    })
}

/// A [`reqwest::ClientBuilder`] that also trusts `SSL_CERT_FILE`'s
/// certificates, if the environment variable is set, in addition to the
/// platform's built-in roots.
pub fn client_builder() -> reqwest::ClientBuilder {
    let mut builder = reqwest::Client::builder();
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
