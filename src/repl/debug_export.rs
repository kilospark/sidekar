//! `/debug` — paste-friendly diagnostics for bug reports (Bedrock, AWS, etc.).
//!
//! Deliberately excludes API keys and OAuth tokens. Bedrock only surfaces region +
//! optional AWS named profile from stored credential metadata.

/// Plain-text bundle suitable for GitHub issues or support threads.
pub(super) fn format_debug_bundle(
    cred_name: &str,
    model: &str,
    session_id: &str,
    cwd: &str,
) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!(
        "sidekar {}",
        env!("CARGO_PKG_VERSION") // compile-time crate version
    ));
    lines.push(format!(
        "host_os={} host_arch={}",
        std::env::consts::OS,
        std::env::consts::ARCH
    ));
    lines.push(format!(
        "verbose_api_logging={}",
        crate::providers::is_verbose()
    ));
    lines.push(format!(
        "mitm_proxy_port={:?}",
        crate::providers::shared_mitm_proxy_port()
    ));
    lines.push(format!("journal_background={}", crate::runtime::journal()));
    lines.push(format!("session_id={session_id}"));
    lines.push(format!("cwd={cwd}"));
    lines.push(format!("model={model:?}"));

    if cred_name.is_empty() {
        lines.push("credential=(not set)".to_string());
        lines.push("provider_type=(unknown — set /credential)".to_string());
    } else {
        lines.push(format!("credential={cred_name:?}"));
        match crate::providers::oauth::resolve_provider_type_for_credential(cred_name) {
            Some(pt) => {
                lines.push(format!("provider_type={pt}"));
                if pt == "bedrock" {
                    match crate::providers::oauth::load_bedrock_stored(cred_name) {
                        Ok(b) => {
                            lines.push(format!("bedrock_region={:?}", b.region));
                            lines.push(format!("bedrock_aws_profile={:?}", b.aws_profile));
                            lines.push(
                                "bedrock_hint=model ids must match InvokeModelWithResponseStream in this region; paste last Bedrock error block if invoke fails".into(),
                            );
                        }
                        Err(e) => lines.push(format!("bedrock_config_error={e:#}")),
                    }
                }
            }
            None => lines.push("provider_type=(unknown nickname)".to_string()),
        }
    }

    lines.push(String::new());
    lines.push(
        "# Paste everything above when reporting Sidekar + Bedrock/AWS issues. No secrets included."
            .into(),
    );
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::format_debug_bundle;

    #[test]
    fn bundle_includes_session_and_model() {
        let s = format_debug_bundle("", "(not set)", "sess-abc", "/tmp/foo");
        assert!(s.contains("sidekar "));
        assert!(s.contains("session_id=sess-abc"));
        assert!(s.contains("model=\"(not set)\""));
        assert!(s.contains("credential=(not set)"));
    }
}
