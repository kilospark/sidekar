//! `sidekar duo enroll`: capture a Duo Mobile activation into the HOTP store.
//!
//! A Duo Mobile enrollment QR encodes an activation URL,
//! `https://api-<host>.duosecurity.com/push/v2/activation/<code>`. POSTing to
//! it returns the device's `hotp_secret` (plus `pkey`/`akey`) and burns the
//! code. So this command — not Duo Mobile on a phone — has to be what
//! consumes the code: an activation scanned into the phone first leaves
//! nothing to capture here.
//!
//! What this builds is a passcode source, the same thing storing Duo in any
//! authenticator app gives you: `hotp get <service> <account>` then yields
//! Duo passcodes. `pkey`/`akey` are kept in KV for a possible later
//! push-signing feature; this command does not use them to answer pushes.

use crate::*;
use serde_json::Value;

pub(crate) mod push;

const USAGE: &str = "\
Usage: sidekar duo enroll   <service> <account> <activation-url-or-code>
       sidekar duo approve  <service> <account> [--window <secs>]

  enroll consumes a Duo Mobile activation and stores its HOTP secret (so
  `sidekar hotp get <service> <account>` yields Duo passcodes) and a device
  key (so `duo approve` can answer pushes).

  The activation code is SINGLE USE. Run enroll BEFORE scanning the QR in
  Duo Mobile — once the phone claims it, there is nothing left to capture.
  Pass the full https://api-<host>.duosecurity.com/push/v2/activation/<code>
  URL the QR encodes (decode it with any QR reader), or <host>:<code>.

  approve waits up to --window seconds (default 30) for ONE pending push and
  approves it. Run it right after you start the login that sends the push.
  If two pushes are pending it approves neither (one may not be yours).";

pub async fn cmd_duo(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("enroll") => cmd_enroll(ctx, args.get(1..).unwrap_or(&[])).await,
        Some("approve") => push::cmd_approve(ctx, args.get(1..).unwrap_or(&[])).await,
        _ => bail!("{USAGE}"),
    }
}

/// `api-xxxx.duosecurity.com` and the activation code, from the QR's URL or a
/// `host:code` pair.
pub(crate) fn parse_activation(input: &str) -> Result<(String, String)> {
    if let Some(rest) = input
        .strip_prefix("https://")
        .or_else(|| input.strip_prefix("http://"))
    {
        // <host>/push/v2/activation/<code>[?...]
        let (host, tail) = rest
            .split_once('/')
            .ok_or_else(|| anyhow!("not a Duo activation URL: {input}"))?;
        let code = tail
            .rsplit('/')
            .next()
            .map(|c| c.split(['?', '&']).next().unwrap_or(c))
            .filter(|c| !c.is_empty())
            .ok_or_else(|| anyhow!("no activation code in URL: {input}"))?;
        return Ok((host.to_string(), code.to_string()));
    }
    // host:code — the host as the QR gives it (api-xxxx.duosecurity.com).
    let (host, code) = input
        .split_once(':')
        .ok_or_else(|| anyhow!("{USAGE}"))?;
    if host.is_empty() || code.is_empty() {
        bail!("{USAGE}");
    }
    Ok((host.to_string(), code.to_string()))
}

/// Guard against being pointed at an arbitrary host: this POST carries an
/// activation secret and trusts the response.
pub(crate) fn is_duo_host(host: &str) -> bool {
    let host = host.split(':').next().unwrap_or(host);
    host.ends_with(".duosecurity.com") && !host.contains('/')
}

async fn cmd_enroll(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let [service, account, activation] = args else {
        bail!("{USAGE}");
    };
    let (host, code) = parse_activation(activation)?;
    if !is_duo_host(&host) {
        bail!("{host} is not a *.duosecurity.com host; refusing to post the activation there");
    }
    let url = format!("https://{host}/push/v2/activation/{code}");

    // A device keypair, so this enrollment can also answer pushes later. The
    // public half goes to Duo now; the private half is stored below. If the
    // pubkey is somehow not accepted, passcodes still work — only push-approve
    // would need a fresh enrollment.
    let (device_priv_pem, device_pub_pem) = push::generate_device_keypair()?;

    // Duo's app sends a platform; without it the activation can be rejected.
    let resp = crate::http_client::client()
        .post(&url)
        .form(&[
            ("pkpush", "rsa-sha512"),
            ("pubkey", device_pub_pem.as_str()),
            ("platform", "Android"),
            ("app_id", "com.duosecurity.duomobile"),
            ("app_version", "4.57.0"),
            ("version", "4.57.0"),
            ("manufacturer", "unknown"),
            ("model", "sidekar"),
        ])
        .send()
        .await
        .with_context(|| format!("posting the activation to {host}"))?;

    let status = resp.status();
    let body: Value = resp
        .json()
        .await
        .context("Duo activation response was not JSON")?;
    if body.get("stat").and_then(Value::as_str) != Some("OK") {
        let msg = body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("activation failed");
        // A spent code lands here: say so, since it is the likeliest cause.
        bail!(
            "Duo activation failed (HTTP {status}): {msg}. \
             If the code was already scanned in Duo Mobile it is spent — \
             a new enrollment QR is needed."
        );
    }
    let data = body
        .get("response")
        .ok_or_else(|| anyhow!("Duo activation response had no `response` object"))?;
    let hotp_secret = data
        .get("hotp_secret")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Duo activation returned no hotp_secret"))?;

    // Duo's hotp_secret is already base32. Store it exactly as a 6-digit
    // SHA1 HOTP from counter 0, which is how Duo passcodes are computed.
    let secret = crate::secrets::normalize_totp_secret(hotp_secret)?;
    crate::secrets::hotp_code(&secret, "SHA1", 6, 0)?;
    crate::broker::hotp_add(service, account, &secret, "SHA1", 6, 0)?;

    // Keep pkey/akey, the device private key, and the API host beside the
    // secret, so `duo approve` can answer a push from this device later.
    for field in ["pkey", "akey"] {
        if let Some(v) = data.get(field).and_then(Value::as_str) {
            crate::broker::kv_set(&format!("duo:{service}:{account}:{field}"), v, None)?;
        }
    }
    crate::broker::kv_set(&push::privkey_kv(service, account), &device_priv_pem, None)?;
    crate::broker::kv_set(&format!("duo:{service}:{account}:host"), &host, None)?;
    crate::commands::push_sync_after_mutation();

    out!(
        ctx,
        "{}",
        crate::output::to_string(&crate::output::PlainOutput::new(format!(
            "Enrolled Duo for {service} ({account}).\n\
             Passcode:  sidekar hotp get {service} {account}\n\
             Approve a push you just triggered:  sidekar duo approve {service} {account}"
        )))?
    );
    Ok(())
}
