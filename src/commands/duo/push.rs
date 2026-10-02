//! Answering a Duo push from the enrolled device's key, scoped to one login.
//!
//! Approval is bound to a login you just started: `duo approve` opens a short
//! window, polls Duo's device-transaction endpoint signed with the device's
//! RSA key, and approves **exactly one** pending transaction. If two are
//! pending it approves neither and says so — one of them may be an attacker
//! signing in as you at that moment, and from the device API they look alike.
//! Outside the window nothing is polled, so this is never a standing
//! yes-to-everything. See `context`-free reasoning in the command help.
//!
//! The device API request signing (the canonical string, the `Authorization:
//! Basic <pkey>:<sig>` header) follows Duo Mobile's documented scheme. The
//! exact endpoint paths and transaction field names can only be confirmed
//! against a live enrolled device; they are kept in one place here so they
//! are easy to adjust after enrollment. Nothing here consumes an activation
//! or weakens anything until `duo approve` is actually run.

use crate::*;
use rsa::RsaPrivateKey;
use rsa::pkcs1v15::SigningKey;
use rsa::pkcs8::{DecodePrivateKey, EncodePrivateKey, EncodePublicKey, LineEnding};
use rsa::signature::{SignatureEncoding, Signer};
use serde_json::Value;
use sha2::Sha512;
use std::time::{Duration, Instant};

/// The device key is stored in KV beside pkey/akey.
pub(crate) fn privkey_kv(service: &str, account: &str) -> String {
    format!("duo:{service}:{account}:device_key")
}
pub(crate) fn pkey_kv(service: &str, account: &str) -> String {
    format!("duo:{service}:{account}:pkey")
}

/// A fresh RSA-2048 device keypair: (private PKCS#8 PEM, public SPKI PEM).
/// The public half goes to Duo at activation; the private half stays here.
pub(crate) fn generate_device_keypair() -> Result<(String, String)> {
    let mut rng = rsa::rand_core::OsRng;
    let sk = RsaPrivateKey::new(&mut rng, 2048).context("generating the Duo device key")?;
    let priv_pem = sk
        .to_pkcs8_pem(LineEnding::LF)
        .context("encoding the device private key")?
        .to_string();
    let pub_pem = sk
        .to_public_key()
        .to_public_key_pem(LineEnding::LF)
        .context("encoding the device public key")?;
    Ok((priv_pem, pub_pem))
}

/// Duo's device-API signature. The canonical string is the date, method,
/// host, path and the sorted form-encoded params, newline-joined; it is
/// signed RSA-SHA512 and carried as `Authorization: Basic base64(pkey:sig)`.
fn sign_request(
    priv_pem: &str,
    pkey: &str,
    date: &str,
    method: &str,
    host: &str,
    path: &str,
    params: &[(String, String)],
) -> Result<String> {
    let sk = RsaPrivateKey::from_pkcs8_pem(priv_pem).context("reading the Duo device key")?;
    let signing_key = SigningKey::<Sha512>::new(sk);

    let mut sorted = params.to_vec();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let encoded = sorted
        .iter()
        .map(|(k, v)| format!("{}={}", urlencoding::encode(k), urlencoding::encode(v)))
        .collect::<Vec<_>>()
        .join("&");

    let canonical = format!("{date}\n{method}\n{host}\n{path}\n{encoded}");
    let sig = signing_key.sign(canonical.as_bytes());
    use base64::Engine;
    let sig_b64 = base64::engine::general_purpose::STANDARD.encode(sig.to_bytes());
    let basic = base64::engine::general_purpose::STANDARD.encode(format!("{pkey}:{sig_b64}"));
    Ok(format!("Basic {basic}"))
}

/// RFC 2822-ish date in the form Duo signs over.
fn duo_date() -> String {
    // e.g. "Tue, 01 Oct 2026 20:00:00 -0000"
    chrono_like_now()
}

fn chrono_like_now() -> String {
    // Avoid a chrono dependency: format from the system clock in UTC.
    let secs = crate::message::epoch_secs();
    // days/seconds since epoch
    let days = secs / 86400;
    let tod = secs % 86400;
    let (h, m, s) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    // civil-from-days (Howard Hinnant's algorithm)
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    let wd = ((days as i64 + 4) % 7 + 7) % 7; // 0=Sun
    const WD: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MO: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!(
        "{}, {:02} {} {:04} {:02}:{:02}:{:02} -0000",
        WD[wd as usize],
        d,
        MO[(month - 1) as usize],
        year,
        h,
        m,
        s
    )
}

struct Device {
    host: String,
    pkey: String,
    priv_pem: String,
}

impl Device {
    fn load(service: &str, account: &str) -> Result<Self> {
        let pkey = crate::broker::kv_get(&pkey_kv(service, account))?
            .map(|e| e.value)
            .ok_or_else(|| anyhow!("no Duo device for {service} ({account}); run `sidekar duo enroll` with push enabled"))?;
        let priv_pem = crate::broker::kv_get(&privkey_kv(service, account))?
            .map(|e| e.value)
            .ok_or_else(|| anyhow!("{service} ({account}) was enrolled without a push key; re-enroll to answer pushes"))?;
        // The API host is recorded at enrollment.
        let host = crate::broker::kv_get(&format!("duo:{service}:{account}:host"))?
            .map(|e| e.value)
            .ok_or_else(|| anyhow!("no Duo API host stored for {service} ({account}); re-enroll"))?;
        Ok(Self { host, pkey, priv_pem })
    }

    async fn get(&self, path: &str) -> Result<Value> {
        let date = duo_date();
        let auth = sign_request(&self.priv_pem, &self.pkey, &date, "GET", &self.host, path, &[])?;
        let resp = crate::http_client::client()
            .get(format!("https://{}{}", self.host, path))
            .header("Authorization", auth)
            .header("x-duo-date", date)
            .send()
            .await
            .with_context(|| format!("GET {path}"))?;
        resp.json().await.context("Duo device response was not JSON")
    }

    async fn post(&self, path: &str, params: &[(String, String)]) -> Result<Value> {
        let date = duo_date();
        let auth = sign_request(
            &self.priv_pem,
            &self.pkey,
            &date,
            "POST",
            &self.host,
            path,
            params,
        )?;
        let form: Vec<(&str, &str)> = params.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let resp = crate::http_client::client()
            .post(format!("https://{}{}", self.host, path))
            .header("Authorization", auth)
            .header("x-duo-date", date)
            .form(&form)
            .send()
            .await
            .with_context(|| format!("POST {path}"))?;
        resp.json().await.context("Duo device response was not JSON")
    }
}

const TRANSACTIONS_PATH: &str = "/push/v2/device/transactions";

/// A pending push, reduced to what the decision needs.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Transaction {
    pub id: String,
    /// A human description Duo provides (app, location) for the refusal notice.
    pub summary: String,
}

/// Pull the `transactions` array out of a device GET response.
pub(crate) fn parse_transactions(body: &Value) -> Vec<Transaction> {
    body.get("response")
        .and_then(|r| r.get("transactions"))
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|t| Transaction {
                    id: t
                        .get("urgid")
                        .or_else(|| t.get("txid"))
                        .or_else(|| t.get("id"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    summary: summarize(t),
                })
                .filter(|t| !t.id.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn summarize(t: &Value) -> String {
    let get = |k: &str| t.get(k).and_then(Value::as_str).unwrap_or("");
    let app = [get("integration_name"), get("application_name"), get("service")]
        .into_iter()
        .find(|s| !s.is_empty())
        .unwrap_or("a login");
    let where_ = [get("location"), get("ip_address"), get("near")]
        .into_iter()
        .find(|s| !s.is_empty())
        .unwrap_or("");
    if where_.is_empty() {
        app.to_string()
    } else {
        format!("{app} from {where_}")
    }
}

/// What to do with the transactions pending in one poll.
#[derive(Debug, PartialEq)]
pub(crate) enum Decision {
    /// Nothing pending yet — keep waiting until the window ends.
    Wait,
    /// Exactly one — approve it.
    Approve(Transaction),
    /// More than one — approve none.
    Refuse(Vec<String>),
}

/// The single-transaction rule, pure so it can be tested without a network:
/// approve one, refuse when two or more could be anyone's, else wait.
pub(crate) fn decide(pending: Vec<Transaction>) -> Decision {
    match pending.len() {
        0 => Decision::Wait,
        1 => Decision::Approve(pending.into_iter().next().unwrap()),
        _ => Decision::Refuse(pending.into_iter().map(|t| t.summary).collect()),
    }
}

/// What a scoped approve did.
#[derive(Debug, PartialEq)]
pub(crate) enum Outcome {
    Approved { summary: String },
    /// No push arrived in the window.
    NonePending,
    /// More than one push was pending — approved none.
    Ambiguous(Vec<String>),
}

pub async fn cmd_approve(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let positional: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    let [service, account] = positional[..] else {
        bail!(
            "Usage: sidekar duo approve <service> <account> [--window <secs>]\n\
             Run this right after you start the login that sends the push."
        );
    };
    let window = args
        .iter()
        .find_map(|a| a.strip_prefix("--window="))
        .map(|v| v.parse::<u64>())
        .transpose()
        .context("--window takes whole seconds")?
        .unwrap_or(30);

    let device = Device::load(service, account)?;
    let outcome = approve_one(&device, Duration::from_secs(window)).await?;
    let text = match outcome {
        Outcome::Approved { summary } => format!("Approved the Duo push for {summary}."),
        Outcome::NonePending => {
            bail!(
                "No Duo push arrived within {window}s. Start the login first, then run this; \
                 raise --window if the push is slow."
            )
        }
        Outcome::Ambiguous(summaries) => {
            // Exit non-zero without approving: refusing is the safe answer.
            bail!(
                "{} Duo pushes were pending at once, so none were approved — one may not be \
                 your login:\n  - {}\nApprove the right one in Duo Mobile, or retry when only \
                 yours is pending.",
                summaries.len(),
                summaries.join("\n  - ")
            )
        }
    };
    out!(
        ctx,
        "{}",
        crate::output::to_string(&crate::output::PlainOutput::new(text))?
    );
    Ok(())
}

/// Poll until a transaction is pending, then apply the single-transaction rule.
async fn approve_one(device: &Device, window: Duration) -> Result<Outcome> {
    let deadline = Instant::now() + window;
    loop {
        let body = device.get(TRANSACTIONS_PATH).await?;
        match decide(parse_transactions(&body)) {
            Decision::Wait => {
                if Instant::now() >= deadline {
                    return Ok(Outcome::NonePending);
                }
                tokio::time::sleep(Duration::from_millis(1500)).await;
            }
            Decision::Approve(t) => {
                let answer = device
                    .post(
                        &format!("{TRANSACTIONS_PATH}/{}", t.id),
                        &[
                            ("answer".into(), "approve".into()),
                            ("txid".into(), t.id.clone()),
                        ],
                    )
                    .await?;
                if answer.get("stat").and_then(Value::as_str) != Some("OK") {
                    bail!(
                        "Duo rejected the approval: {}",
                        answer
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown error")
                    );
                }
                return Ok(Outcome::Approved { summary: t.summary });
            }
            Decision::Refuse(summaries) => return Ok(Outcome::Ambiguous(summaries)),
        }
    }
}

#[cfg(test)]
mod tests;
