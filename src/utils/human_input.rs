use super::*;

pub async fn human_click(cdp: &mut CdpClient, x: f64, y: f64) -> Result<()> {
    let start_x = x + (rand::random::<f64>() - 0.5) * 200.0 + 50.0;
    let start_y = y + (rand::random::<f64>() - 0.5) * 200.0 + 50.0;
    cdp.send(
        "Input.dispatchMouseEvent",
        json!({ "type": "mouseMoved", "x": start_x, "y": start_y }),
    )
    .await?;
    human_mouse_move(cdp, start_x, start_y, x, y).await?;
    sleep(Duration::from_millis(
        (50.0 + rand::random::<f64>() * 150.0) as u64,
    ))
    .await;
    cdp.send(
        "Input.dispatchMouseEvent",
        json!({ "type": "mousePressed", "x": x, "y": y, "button": "left", "clickCount": 1 }),
    )
    .await?;
    sleep(Duration::from_millis(
        (30.0 + rand::random::<f64>() * 90.0) as u64,
    ))
    .await;
    let release_x = x + (rand::random::<f64>() - 0.5) * 2.0;
    let release_y = y + (rand::random::<f64>() - 0.5) * 2.0;
    cdp.send(
        "Input.dispatchMouseEvent",
        json!({ "type": "mouseReleased", "x": release_x, "y": release_y, "button": "left", "clickCount": 1 }),
    )
    .await?;
    Ok(())
}

pub async fn human_mouse_move(
    cdp: &mut CdpClient,
    from_x: f64,
    from_y: f64,
    to_x: f64,
    to_y: f64,
) -> Result<()> {
    let distance = ((to_x - from_x).powi(2) + (to_y - from_y).powi(2)).sqrt();
    let duration = 100.0 + (distance / 2000.0) * 200.0 + rand::random::<f64>() * 100.0;
    let steps = (duration / 20.0).round().clamp(5.0, 30.0) as usize;

    let cp1_x = from_x + (to_x - from_x) * 0.25 + (rand::random::<f64>() - 0.5) * 50.0;
    let cp1_y = from_y + (to_y - from_y) * 0.25 + (rand::random::<f64>() - 0.5) * 50.0;
    let cp2_x = from_x + (to_x - from_x) * 0.75 + (rand::random::<f64>() - 0.5) * 50.0;
    let cp2_y = from_y + (to_y - from_y) * 0.75 + (rand::random::<f64>() - 0.5) * 50.0;

    for i in 0..=steps {
        let t = i as f64 / steps as f64;
        let u = 1.0 - t;
        let x = u.powi(3) * from_x
            + 3.0 * u.powi(2) * t * cp1_x
            + 3.0 * u * t.powi(2) * cp2_x
            + t.powi(3) * to_x
            + (rand::random::<f64>() - 0.5) * 2.0;
        let y = u.powi(3) * from_y
            + 3.0 * u.powi(2) * t * cp1_y
            + 3.0 * u * t.powi(2) * cp2_y
            + t.powi(3) * to_y
            + (rand::random::<f64>() - 0.5) * 2.0;
        cdp.send(
            "Input.dispatchMouseEvent",
            json!({ "type": "mouseMoved", "x": x, "y": y }),
        )
        .await?;
        sleep(Duration::from_millis(
            (16.0 + rand::random::<f64>() * 8.0) as u64,
        ))
        .await;
    }
    Ok(())
}

pub async fn human_type_text(cdp: &mut CdpClient, text: &str, fast: bool) -> Result<()> {
    let base_delay = if fast { 40.0 } else { 80.0 };
    let chars = text.chars().collect::<Vec<_>>();
    for (i, ch) in chars.iter().enumerate() {
        let c = ch.to_string();
        cdp.send(
            "Input.dispatchKeyEvent",
            json!({ "type": "keyDown", "text": c, "unmodifiedText": ch.to_string() }),
        )
        .await?;
        cdp.send(
            "Input.dispatchKeyEvent",
            json!({ "type": "keyUp", "text": ch.to_string(), "unmodifiedText": ch.to_string() }),
        )
        .await?;
        let mut delay = base_delay + rand::random::<f64>() * (base_delay / 2.0);
        if rand::random::<f64>() < 0.05 {
            delay += rand::random::<f64>() * 500.0;
        }
        if i > 0 && chars[i - 1] == *ch {
            delay /= 2.0;
        }
        if rand::random::<f64>() < 0.03 && i < chars.len() - 1 {
            let wrong_char = ((b'a' + (rand::random::<u8>() % 26)) as char).to_string();
            cdp.send(
                "Input.dispatchKeyEvent",
                json!({ "type": "keyDown", "text": wrong_char, "unmodifiedText": wrong_char }),
            )
            .await?;
            cdp.send(
                "Input.dispatchKeyEvent",
                json!({ "type": "keyUp", "text": wrong_char, "unmodifiedText": wrong_char }),
            )
            .await?;
            sleep(Duration::from_millis(
                (50.0 + rand::random::<f64>() * 100.0) as u64,
            ))
            .await;
            if let Err(e) = cdp
                .send(
                    "Input.dispatchKeyEvent",
                    json!({
                        "type": "keyDown",
                        "key": "Backspace",
                        "code": "Backspace",
                        "keyCode": 8,
                        "windowsVirtualKeyCode": 8
                    }),
                )
                .await
            {
                crate::broker::try_log_event(
                    "warn",
                    "input",
                    "failed to send Backspace",
                    Some(&format!("{e:#}")),
                );
            }
            if let Err(e) = cdp
                .send(
                    "Input.dispatchKeyEvent",
                    json!({
                        "type": "keyUp",
                        "key": "Backspace",
                        "code": "Backspace",
                        "keyCode": 8,
                        "windowsVirtualKeyCode": 8
                    }),
                )
                .await
            {
                crate::broker::try_log_event(
                    "warn",
                    "input",
                    "failed to send Backspace keyUp",
                    Some(&format!("{e:#}")),
                );
            }
            sleep(Duration::from_millis(
                (30.0 + rand::random::<f64>() * 70.0) as u64,
            ))
            .await;
        }
        sleep(Duration::from_millis(delay as u64)).await;
    }
    Ok(())
}
