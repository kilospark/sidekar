use super::*;

/// Resolve an agent name to its binary path via `which`.
pub(crate) fn resolve_agent(agent: &str) -> Result<(String, std::ffi::CString)> {
    let output = std::process::Command::new("which")
        .arg(agent)
        .output()
        .with_context(|| format!("failed to look up \"{agent}\""))?;
    if output.status.success() {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let c_path = std::ffi::CString::new(path.as_str()).context("invalid binary path")?;
        return Ok((path, c_path));
    }
    bail!("\"{agent}\" not found on PATH. Is it installed?");
}

/// Build CString args for execvp (must happen before fork).
pub(crate) fn prepare_args(
    bin: &std::ffi::CString,
    args: &[String],
) -> Result<Vec<std::ffi::CString>> {
    let mut c_args: Vec<std::ffi::CString> = vec![bin.clone()];
    for arg in args {
        c_args.push(std::ffi::CString::new(arg.as_str()).context("invalid arg")?);
    }
    Ok(c_args)
}

/// Detect a channel name. Priority: $PWD → git repo name → hostname.
pub(crate) fn detect_channel() -> String {
    // 1. Full path ($PWD) — agents in the same directory are on the same channel
    if let Ok(cwd) = std::env::current_dir() {
        return cwd.to_string_lossy().to_string();
    }
    // 2. Git repo name
    if let Some(name) = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .and_then(|p| {
            p.rsplit('/')
                .next()
                .filter(|n| !n.is_empty())
                .map(|n| n.to_lowercase())
        })
    {
        return name;
    }
    // 3. Hostname
    std::process::Command::new("hostname")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "local".into())
}

/// Pick a unique agent name like `{agent}-{channel}-{n}`, checking the broker
/// for existing names to avoid collisions.
pub(crate) fn unique_agent_name(agent: &str, channel: &str) -> String {
    let mut existing: HashSet<String> = HashSet::new();
    if let Ok(agents) = broker::list_agents(None) {
        for a in agents {
            existing.insert(a.id.name);
        }
    }
    let mut n = 1u32;
    loop {
        let candidate = format!("{agent}-{channel}-{n}");
        if !existing.contains(&candidate) {
            return candidate;
        }
        n += 1;
    }
}
