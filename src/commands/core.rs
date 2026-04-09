use super::*;

mod browser;
mod page;

pub(crate) use browser::*;
pub(crate) use page::*;

pub(super) async fn cmd_setup(_ctx: &mut AppContext) -> Result<()> {
    crate::skill::install_skill();
    Ok(())
}

pub(super) async fn cmd_uninstall(_ctx: &mut AppContext) -> Result<()> {
    crate::skill::remove_skill();

    let data_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".sidekar");
    if data_dir.exists() {
        eprintln!("Removing data directory: {}", data_dir.display());
        let _ = std::fs::remove_dir_all(&data_dir);
    }

    let config_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".config")
        .join("sidekar");
    if config_dir.exists() {
        eprintln!("Removing config directory: {}", config_dir.display());
        let _ = std::fs::remove_dir_all(&config_dir);
    }

    if let Ok(entries) = std::fs::read_dir("/tmp") {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("sidekar-") && name.ends_with(".sock") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    let agents_skill_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".agents")
        .join("skills")
        .join("sidekar");
    if agents_skill_dir.exists() {
        eprintln!(
            "Removing old skill directory: {}",
            agents_skill_dir.display()
        );
        let _ = std::fs::remove_dir_all(&agents_skill_dir);
    }

    if let Ok(exe_path) = std::env::current_exe() {
        eprintln!("Removing binary: {}", exe_path.display());
        let _ = std::fs::remove_file(&exe_path);
    }

    eprintln!("sidekar uninstalled.");
    Ok(())
}
