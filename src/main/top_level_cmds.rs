use sidekar::*;

#[derive(serde::Serialize)]
struct DeviceOut {
    hostname: String,
    os: String,
    arch: String,
    version: String,
    last_seen: String,
}

#[derive(serde::Serialize)]
struct DevicesOutput {
    items: Vec<DeviceOut>,
}

impl sidekar::output::CommandOutput for DevicesOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        if self.items.is_empty() {
            writeln!(w, "No devices registered.")?;
            return Ok(());
        }
        writeln!(
            w,
            "{:<20} {:<10} {:<8} {:<12} LAST SEEN",
            "HOSTNAME", "OS", "ARCH", "VERSION"
        )?;
        for d in &self.items {
            writeln!(
                w,
                "{:<20} {:<10} {:<8} {:<12} {}",
                d.hostname, d.os, d.arch, d.version, d.last_seen
            )?;
        }
        Ok(())
    }
}

/// Handle `sidekar device <login|logout|list>`.
pub async fn handle_device(args: &[String]) -> Result<()> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("");
    match sub {
        "login" => sidekar::auth::device_auth_flow().await,
        "logout" => {
            sidekar::auth::logout()?;
            sidekar::output::emit(&sidekar::output::PlainOutput::new(
                "Signed out. Device token removed.",
            ))?;
            Ok(())
        }
        "list" => {
            let data = sidekar::api_client::list_devices().await?;
            let items = data
                .get("devices")
                .and_then(|v| v.as_array())
                .map(|devices| {
                    devices
                        .iter()
                        .map(|d| DeviceOut {
                            hostname: d
                                .get("hostname")
                                .and_then(|v| v.as_str())
                                .unwrap_or("-")
                                .to_string(),
                            os: d
                                .get("os")
                                .and_then(|v| v.as_str())
                                .unwrap_or("-")
                                .to_string(),
                            arch: d
                                .get("arch")
                                .and_then(|v| v.as_str())
                                .unwrap_or("-")
                                .to_string(),
                            version: d
                                .get("sidekar_version")
                                .and_then(|v| v.as_str())
                                .unwrap_or("-")
                                .to_string(),
                            last_seen: d
                                .get("last_seen_at")
                                .and_then(|v| v.as_str())
                                .unwrap_or("-")
                                .to_string(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            sidekar::output::emit(&DevicesOutput { items })?;
            Ok(())
        }
        _ => {
            eprintln!("Usage: sidekar device <login|logout|list>");
            std::process::exit(1);
        }
    }
}

#[derive(serde::Serialize)]
struct RelaySessionOut {
    name: String,
    agent: String,
    hostname: String,
    cwd: String,
}

#[derive(serde::Serialize)]
struct RelaySessionsOutput {
    items: Vec<RelaySessionOut>,
}

impl sidekar::output::CommandOutput for RelaySessionsOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        if self.items.is_empty() {
            writeln!(w, "No active sessions.")?;
            return Ok(());
        }
        writeln!(w, "{:<20} {:<15} {:<12} CWD", "NAME", "AGENT", "HOSTNAME")?;
        for s in &self.items {
            writeln!(
                w,
                "{:<20} {:<15} {:<12} {}",
                s.name, s.agent, s.hostname, s.cwd
            )?;
        }
        Ok(())
    }
}

/// Handle `sidekar relay list`.
pub async fn handle_relay(args: &[String]) -> Result<()> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("list");
    match sub {
        "list" => {
            let data = sidekar::api_client::list_sessions().await?;
            let items = data
                .get("sessions")
                .and_then(|v| v.as_array())
                .map(|sessions| {
                    sessions
                        .iter()
                        .map(|s| RelaySessionOut {
                            name: s
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("-")
                                .to_string(),
                            agent: s
                                .get("agent_type")
                                .and_then(|v| v.as_str())
                                .unwrap_or("-")
                                .to_string(),
                            hostname: s
                                .get("hostname")
                                .and_then(|v| v.as_str())
                                .unwrap_or("-")
                                .to_string(),
                            cwd: s
                                .get("cwd")
                                .and_then(|v| v.as_str())
                                .unwrap_or("-")
                                .to_string(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            sidekar::output::emit(&RelaySessionsOutput { items })?;
            Ok(())
        }
        _ => {
            eprintln!("Usage: sidekar relay <list>");
            std::process::exit(1);
        }
    }
}

/// Handle `sidekar daemon [start|stop|restart|status]`.
pub async fn handle_daemon(args: &[String]) -> Result<()> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("");
    match sub {
        "start" => sidekar::daemon::start().await,
        "relaunch" => {
            let old_pid = args
                .get(1)
                .and_then(|s| s.parse::<i32>().ok())
                .unwrap_or_else(|| {
                    eprintln!("Usage: sidekar daemon relaunch <old_pid>");
                    std::process::exit(1);
                });
            sidekar::daemon::relaunch_after_exit(old_pid).await
        }
        "stop" => sidekar::daemon::stop(),
        "restart" => sidekar::daemon::restart(),
        "status" => sidekar::daemon::status(),
        "" => {
            if sidekar::daemon::is_running() {
                sidekar::daemon::status()
            } else {
                sidekar::daemon::ensure_running()
            }
        }
        _ => {
            eprintln!("Usage: sidekar daemon [start|stop|restart|status]");
            std::process::exit(1);
        }
    }
}
