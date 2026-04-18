use crate::*;

#[cfg(target_os = "macos")]
fn new_enigo() -> Result<enigo::Enigo> {
    use enigo::{Enigo, Settings};
    Enigo::new(&Settings::default()).map_err(|e| anyhow!("failed to initialize enigo: {e}"))
}

#[cfg(target_os = "macos")]
pub fn click_at(x: f64, y: f64) -> Result<()> {
    use enigo::{Button, Coordinate, Direction, Enigo, Mouse, Settings};
    let mut enigo =
        Enigo::new(&Settings::default()).map_err(|e| anyhow!("failed to initialize enigo: {e}"))?;
    enigo
        .move_mouse(x.round() as i32, y.round() as i32, Coordinate::Abs)
        .map_err(|e| anyhow!("failed to move mouse: {e}"))?;
    enigo
        .button(Button::Left, Direction::Click)
        .map_err(|e| anyhow!("failed to click: {e}"))?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn key_from_token(token: &str) -> Result<enigo::Key> {
    use enigo::Key;

    let t = token.trim().to_lowercase();
    let key = match t.as_str() {
        "cmd" | "command" | "meta" | "super" => Key::Meta,
        "ctrl" | "control" => Key::Control,
        "alt" | "option" | "opt" => Key::Alt,
        "shift" => Key::Shift,
        "enter" | "return" => Key::Return,
        "tab" => Key::Tab,
        "esc" | "escape" => Key::Escape,
        "space" => Key::Space,
        "backspace" => Key::Backspace,
        "delete" | "del" => Key::Delete,
        "home" => Key::Home,
        "end" => Key::End,
        "left" | "leftarrow" => Key::LeftArrow,
        "right" | "rightarrow" => Key::RightArrow,
        "up" | "uparrow" => Key::UpArrow,
        "down" | "downarrow" => Key::DownArrow,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "capslock" => Key::CapsLock,
        "help" => Key::Help,
        "f1" => Key::F1,
        "f2" => Key::F2,
        "f3" => Key::F3,
        "f4" => Key::F4,
        "f5" => Key::F5,
        "f6" => Key::F6,
        "f7" => Key::F7,
        "f8" => Key::F8,
        "f9" => Key::F9,
        "f10" => Key::F10,
        "f11" => Key::F11,
        "f12" => Key::F12,
        "f13" => Key::F13,
        "f14" => Key::F14,
        "f15" => Key::F15,
        "f16" => Key::F16,
        "f17" => Key::F17,
        "f18" => Key::F18,
        "f19" => Key::F19,
        "f20" => Key::F20,
        _ => {
            let mut chars = t.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Key::Unicode(c),
                _ => bail!("unsupported key token: {token}"),
            }
        }
    };
    Ok(key)
}

#[cfg(target_os = "macos")]
pub fn press_chord(spec: &str) -> Result<()> {
    use enigo::{Direction, Keyboard};

    let parts: Vec<&str> = spec
        .split('+')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if parts.is_empty() {
        bail!("empty key chord");
    }

    let mut enigo = new_enigo()?;
    let keys: Vec<enigo::Key> = parts
        .iter()
        .map(|part| key_from_token(part))
        .collect::<Result<Vec<_>>>()?;

    if keys.len() == 1 {
        enigo
            .key(keys[0], Direction::Click)
            .map_err(|e| anyhow!("failed to press key: {e}"))?;
        return Ok(());
    }

    for key in &keys[..keys.len() - 1] {
        enigo
            .key(*key, Direction::Press)
            .map_err(|e| anyhow!("failed to press modifier: {e}"))?;
    }
    enigo
        .key(*keys.last().unwrap(), Direction::Click)
        .map_err(|e| anyhow!("failed to press key chord: {e}"))?;
    for key in keys[..keys.len() - 1].iter().rev() {
        enigo
            .key(*key, Direction::Release)
            .map_err(|e| anyhow!("failed to release modifier: {e}"))?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn type_text(text: &str) -> Result<()> {
    use enigo::Keyboard;
    let mut enigo = new_enigo()?;
    enigo
        .text(text)
        .map_err(|e| anyhow!("failed to type text: {e}"))?;
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn set_clipboard_text(text: &str) -> Result<()> {
    let mut child = Command::new("pbcopy")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to launch pbcopy")?;
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        stdin
            .write_all(text.as_bytes())
            .context("failed writing clipboard contents")?;
    }
    let output = child
        .wait_with_output()
        .context("failed waiting for pbcopy")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("pbcopy failed: {}", stderr.trim());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn paste_text(text: &str) -> Result<()> {
    set_clipboard_text(text)?;
    press_chord("cmd+v")
}

#[cfg(target_os = "macos")]
pub fn read_clipboard_text() -> Result<String> {
    let output = Command::new("pbpaste")
        .output()
        .context("failed to run pbpaste")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("pbpaste failed: {}", stderr.trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
