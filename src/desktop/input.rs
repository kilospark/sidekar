use crate::*;

#[cfg(target_os = "macos")]
pub fn click_at(x: f64, y: f64) -> Result<()> {
    use enigo::{Enigo, Settings, Mouse, Coordinate, Button, Direction};
    let mut enigo = Enigo::new(&Settings::default())
        .map_err(|e| anyhow!("failed to initialize enigo: {e}"))?;
    enigo.move_mouse(x.round() as i32, y.round() as i32, Coordinate::Abs)
        .map_err(|e| anyhow!("failed to move mouse: {e}"))?;
    enigo.button(Button::Left, Direction::Click)
        .map_err(|e| anyhow!("failed to click: {e}"))?;
    Ok(())
}
