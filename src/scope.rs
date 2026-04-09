use crate::*;

pub const PROJECT_SCOPE: &str = "project";
pub const GLOBAL_SCOPE: &str = "global";

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ScopeView {
    Project,
    Global,
    All,
}

impl ScopeView {
    pub fn parse(value: Option<&str>) -> Result<Self> {
        match value.unwrap_or(PROJECT_SCOPE) {
            PROJECT_SCOPE => Ok(Self::Project),
            GLOBAL_SCOPE => Ok(Self::Global),
            "all" => Ok(Self::All),
            other => bail!("Invalid scope: {other}. Valid: project, global, all"),
        }
    }
}

pub fn parse_stored_scope(value: &str) -> Result<&'static str> {
    match value {
        PROJECT_SCOPE => Ok(PROJECT_SCOPE),
        GLOBAL_SCOPE => Ok(GLOBAL_SCOPE),
        other => bail!("Invalid scope: {other}. Valid: project, global"),
    }
}

pub fn resolve_project_name(cwd: Option<&str>) -> String {
    let path = project_root_path(cwd);
    path.file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| "default".to_string())
}

pub fn resolve_project_root(cwd: Option<&str>) -> String {
    project_root_path(cwd).to_string_lossy().to_string()
}

fn project_root_path(cwd: Option<&str>) -> PathBuf {
    let path = cwd
        .map(PathBuf::from)
        .or_else(|| env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    if let Ok(output) = Command::new("git")
        .args([
            "-C",
            &path.to_string_lossy(),
            "rev-parse",
            "--show-toplevel",
        ])
        .output()
    {
        if output.status.success() {
            let top = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !top.is_empty() {
                let top_path = PathBuf::from(top);
                return fs::canonicalize(&top_path).unwrap_or(top_path);
            }
        }
    }
    fs::canonicalize(&path).unwrap_or(path)
}

#[cfg(test)]
mod tests;
