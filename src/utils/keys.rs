use super::*;

pub fn parse_key_combo(combo: &str) -> (KeyModifiers, String) {
    let mut modifiers = KeyModifiers {
        ctrl: false,
        alt: false,
        shift: false,
        meta: false,
    };
    let mut key = String::new();

    for part in combo.split('+') {
        match part.to_lowercase().as_str() {
            "ctrl" | "control" => modifiers.ctrl = true,
            "alt" | "option" => modifiers.alt = true,
            "shift" => modifiers.shift = true,
            "meta" | "cmd" | "command" => modifiers.meta = true,
            _ => key = part.to_string(),
        }
    }

    (modifiers, key)
}

pub fn key_mapping(input: &str) -> KeyMapping {
    let key = input.to_string();
    match input.to_lowercase().as_str() {
        "enter" => KeyMapping {
            key: "Enter".to_string(),
            code: "Enter".to_string(),
            key_code: 13,
        },
        "tab" => KeyMapping {
            key: "Tab".to_string(),
            code: "Tab".to_string(),
            key_code: 9,
        },
        "escape" => KeyMapping {
            key: "Escape".to_string(),
            code: "Escape".to_string(),
            key_code: 27,
        },
        "backspace" => KeyMapping {
            key: "Backspace".to_string(),
            code: "Backspace".to_string(),
            key_code: 8,
        },
        "delete" => KeyMapping {
            key: "Delete".to_string(),
            code: "Delete".to_string(),
            key_code: 46,
        },
        "arrowup" => KeyMapping {
            key: "ArrowUp".to_string(),
            code: "ArrowUp".to_string(),
            key_code: 38,
        },
        "arrowdown" => KeyMapping {
            key: "ArrowDown".to_string(),
            code: "ArrowDown".to_string(),
            key_code: 40,
        },
        "arrowleft" => KeyMapping {
            key: "ArrowLeft".to_string(),
            code: "ArrowLeft".to_string(),
            key_code: 37,
        },
        "arrowright" => KeyMapping {
            key: "ArrowRight".to_string(),
            code: "ArrowRight".to_string(),
            key_code: 39,
        },
        "space" => KeyMapping {
            key: " ".to_string(),
            code: "Space".to_string(),
            key_code: 32,
        },
        "home" => KeyMapping {
            key: "Home".to_string(),
            code: "Home".to_string(),
            key_code: 36,
        },
        "end" => KeyMapping {
            key: "End".to_string(),
            code: "End".to_string(),
            key_code: 35,
        },
        "pageup" => KeyMapping {
            key: "PageUp".to_string(),
            code: "PageUp".to_string(),
            key_code: 33,
        },
        "pagedown" => KeyMapping {
            key: "PageDown".to_string(),
            code: "PageDown".to_string(),
            key_code: 34,
        },
        _ => {
            if input.chars().count() == 1 {
                let upper = input.to_uppercase();
                let c = upper.chars().next().unwrap_or('A');
                KeyMapping {
                    key,
                    code: format!("Key{}", upper),
                    key_code: c as i64,
                }
            } else {
                KeyMapping {
                    key,
                    code: input.to_string(),
                    key_code: 0,
                }
            }
        }
    }
}
