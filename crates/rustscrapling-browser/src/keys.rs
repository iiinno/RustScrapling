//! US keyboard layout for CDP `Input.dispatchKeyEvent`.
//!
//! Accepts the key names used by Playwright's `keyboard.press` (e.g. `Enter`,
//! `ArrowLeft`, `a`, `KeyA`, `ControlOrMeta+A`, `Shift++`) so that MCP
//! `press_key` actions written for Scrapling v0.5 behave the same way.

use crate::BrowserError;

#[derive(Clone, Copy, Debug)]
pub(crate) struct KeyDef {
    pub key: &'static str,
    pub code: &'static str,
    pub key_code: u32,
    pub shift_key: Option<&'static str>,
    pub text: Option<&'static str>,
    pub location: u32,
}

const fn k(key: &'static str, code: &'static str, key_code: u32) -> KeyDef {
    KeyDef {
        key,
        code,
        key_code,
        shift_key: None,
        text: None,
        location: 0,
    }
}

const fn printable(
    key: &'static str,
    shift: &'static str,
    code: &'static str,
    key_code: u32,
) -> KeyDef {
    KeyDef {
        key,
        code,
        key_code,
        shift_key: Some(shift),
        text: Some(key),
        location: 0,
    }
}

const fn located(key: &'static str, code: &'static str, key_code: u32, location: u32) -> KeyDef {
    KeyDef {
        key,
        code,
        key_code,
        shift_key: None,
        text: None,
        location,
    }
}

const fn keypad(key: &'static str, code: &'static str, key_code: u32) -> KeyDef {
    KeyDef {
        key,
        code,
        key_code,
        shift_key: None,
        text: Some(key),
        location: 3,
    }
}

/// Earlier entries win when several definitions share a `key` name, so the
/// main keyboard is listed before the right-hand modifiers and keypad.
const LAYOUT: &[KeyDef] = &[
    KeyDef {
        text: Some("\r"),
        ..k("Enter", "Enter", 13)
    },
    k("Tab", "Tab", 9),
    k("Backspace", "Backspace", 8),
    k("Escape", "Escape", 27),
    KeyDef {
        text: Some(" "),
        ..k(" ", "Space", 32)
    },
    k("Delete", "Delete", 46),
    k("Insert", "Insert", 45),
    k("Home", "Home", 36),
    k("End", "End", 35),
    k("PageUp", "PageUp", 33),
    k("PageDown", "PageDown", 34),
    k("ArrowLeft", "ArrowLeft", 37),
    k("ArrowUp", "ArrowUp", 38),
    k("ArrowRight", "ArrowRight", 39),
    k("ArrowDown", "ArrowDown", 40),
    k("CapsLock", "CapsLock", 20),
    k("ContextMenu", "ContextMenu", 93),
    k("Pause", "Pause", 19),
    k("PrintScreen", "PrintScreen", 44),
    k("ScrollLock", "ScrollLock", 145),
    k("NumLock", "NumLock", 144),
    located("Shift", "ShiftLeft", 16, 1),
    located("Control", "ControlLeft", 17, 1),
    located("Alt", "AltLeft", 18, 1),
    located("Meta", "MetaLeft", 91, 1),
    located("Shift", "ShiftRight", 16, 2),
    located("Control", "ControlRight", 17, 2),
    located("Alt", "AltRight", 18, 2),
    located("Meta", "MetaRight", 92, 2),
    k("F1", "F1", 112),
    k("F2", "F2", 113),
    k("F3", "F3", 114),
    k("F4", "F4", 115),
    k("F5", "F5", 116),
    k("F6", "F6", 117),
    k("F7", "F7", 118),
    k("F8", "F8", 119),
    k("F9", "F9", 120),
    k("F10", "F10", 121),
    k("F11", "F11", 122),
    k("F12", "F12", 123),
    printable("0", ")", "Digit0", 48),
    printable("1", "!", "Digit1", 49),
    printable("2", "@", "Digit2", 50),
    printable("3", "#", "Digit3", 51),
    printable("4", "$", "Digit4", 52),
    printable("5", "%", "Digit5", 53),
    printable("6", "^", "Digit6", 54),
    printable("7", "&", "Digit7", 55),
    printable("8", "*", "Digit8", 56),
    printable("9", "(", "Digit9", 57),
    printable("a", "A", "KeyA", 65),
    printable("b", "B", "KeyB", 66),
    printable("c", "C", "KeyC", 67),
    printable("d", "D", "KeyD", 68),
    printable("e", "E", "KeyE", 69),
    printable("f", "F", "KeyF", 70),
    printable("g", "G", "KeyG", 71),
    printable("h", "H", "KeyH", 72),
    printable("i", "I", "KeyI", 73),
    printable("j", "J", "KeyJ", 74),
    printable("k", "K", "KeyK", 75),
    printable("l", "L", "KeyL", 76),
    printable("m", "M", "KeyM", 77),
    printable("n", "N", "KeyN", 78),
    printable("o", "O", "KeyO", 79),
    printable("p", "P", "KeyP", 80),
    printable("q", "Q", "KeyQ", 81),
    printable("r", "R", "KeyR", 82),
    printable("s", "S", "KeyS", 83),
    printable("t", "T", "KeyT", 84),
    printable("u", "U", "KeyU", 85),
    printable("v", "V", "KeyV", 86),
    printable("w", "W", "KeyW", 87),
    printable("x", "X", "KeyX", 88),
    printable("y", "Y", "KeyY", 89),
    printable("z", "Z", "KeyZ", 90),
    printable(";", ":", "Semicolon", 186),
    printable("=", "+", "Equal", 187),
    printable(",", "<", "Comma", 188),
    printable("-", "_", "Minus", 189),
    printable(".", ">", "Period", 190),
    printable("/", "?", "Slash", 191),
    printable("`", "~", "Backquote", 192),
    printable("[", "{", "BracketLeft", 219),
    printable("\\", "|", "Backslash", 220),
    printable("]", "}", "BracketRight", 221),
    printable("'", "\"", "Quote", 222),
    keypad("0", "Numpad0", 96),
    keypad("1", "Numpad1", 97),
    keypad("2", "Numpad2", 98),
    keypad("3", "Numpad3", 99),
    keypad("4", "Numpad4", 100),
    keypad("5", "Numpad5", 101),
    keypad("6", "Numpad6", 102),
    keypad("7", "Numpad7", 103),
    keypad("8", "Numpad8", 104),
    keypad("9", "Numpad9", 105),
    keypad("*", "NumpadMultiply", 106),
    keypad("+", "NumpadAdd", 107),
    keypad("-", "NumpadSubtract", 109),
    keypad(".", "NumpadDecimal", 110),
    keypad("/", "NumpadDivide", 111),
    KeyDef {
        text: Some("\r"),
        location: 3,
        ..k("Enter", "NumpadEnter", 13)
    },
];

/// A resolved key event description.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KeyDescription {
    pub key: String,
    pub code: &'static str,
    pub key_code: u32,
    pub text: String,
    pub location: u32,
}

pub(crate) const MOD_ALT: u32 = 1;
pub(crate) const MOD_CONTROL: u32 = 2;
pub(crate) const MOD_META: u32 = 4;
pub(crate) const MOD_SHIFT: u32 = 8;

pub(crate) fn modifier_bit(key: &str) -> Option<u32> {
    match key {
        "Alt" => Some(MOD_ALT),
        "Control" => Some(MOD_CONTROL),
        "Meta" => Some(MOD_META),
        "Shift" => Some(MOD_SHIFT),
        _ => None,
    }
}

fn find(name: &str) -> Option<(&'static KeyDef, bool)> {
    // A shifted printable character (e.g. "A" or "!") maps to its base key.
    if let Some(def) = LAYOUT.iter().find(|def| def.key == name) {
        return Some((def, false));
    }
    if let Some(def) = LAYOUT.iter().find(|def| def.code == name) {
        return Some((def, false));
    }
    LAYOUT
        .iter()
        .find(|def| def.shift_key == Some(name))
        .map(|def| (def, true))
}

pub(crate) fn has_key(name: &str) -> bool {
    find(name).is_some()
}

/// Describe a key given the modifiers currently held.
pub(crate) fn describe(name: &str, modifiers: u32) -> Result<KeyDescription, BrowserError> {
    let (def, shifted_name) = find(name).ok_or_else(|| unknown(name))?;
    let shift = modifiers & MOD_SHIFT != 0;
    let mut key = def.key.to_owned();
    let mut text = def.text.unwrap_or("").to_owned();
    if shifted_name || (shift && def.shift_key.is_some()) {
        let shifted = def.shift_key.expect("shift key exists");
        key = shifted.to_owned();
        text = shifted.to_owned();
    }
    if def.code == name && def.code != def.key && !shift && !shifted_name {
        // Addressing by code ("KeyA") yields the unshifted key, like a real keyboard.
        key = def.key.to_owned();
    }
    // Shortcuts with Control/Alt/Meta never insert text.
    if modifiers & !MOD_SHIFT != 0 {
        text.clear();
    }
    Ok(KeyDescription {
        key,
        code: def.code,
        key_code: def.key_code,
        text,
        location: def.location,
    })
}

fn unknown(name: &str) -> BrowserError {
    BrowserError::InvalidArgument(format!("Unknown key: \"{name}\""))
}

/// Split a `press` shortcut into modifier keys and the final key. Accepts
/// `ControlOrMeta`, mapped to Meta on macOS and Control elsewhere.
pub(crate) fn split_shortcut(shortcut: &str) -> Result<Vec<String>, BrowserError> {
    if shortcut.is_empty() {
        return Err(unknown(shortcut));
    }
    let mut tokens: Vec<String> = Vec::new();
    let (body, plus) = if shortcut == "+" {
        ("", true)
    } else if let Some(rest) = shortcut.strip_suffix("++") {
        (rest, true)
    } else {
        (shortcut, false)
    };
    if !body.is_empty() {
        tokens.extend(body.split('+').map(str::to_owned));
    }
    if plus {
        tokens.push("+".into());
    }
    if tokens.iter().any(String::is_empty) {
        return Err(unknown(shortcut));
    }
    for token in &mut tokens {
        if token == "ControlOrMeta" {
            *token = if cfg!(target_os = "macos") {
                "Meta".into()
            } else {
                "Control".into()
            };
        }
    }
    for token in &tokens {
        if !has_key(token) {
            return Err(unknown(token));
        }
    }
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcuts_and_shifted_keys_resolve_like_a_us_keyboard() {
        assert_eq!(
            split_shortcut("ControlOrMeta+A").unwrap(),
            vec![
                if cfg!(target_os = "macos") {
                    "Meta"
                } else {
                    "Control"
                },
                "A"
            ]
        );
        assert_eq!(split_shortcut("Shift++").unwrap(), vec!["Shift", "+"]);
        assert_eq!(split_shortcut("+").unwrap(), vec!["+"]);
        assert!(split_shortcut("Foo")
            .unwrap_err()
            .to_string()
            .contains("Unknown key: \"Foo\""));
        let a = describe("a", 0).unwrap();
        assert_eq!(
            (a.key.as_str(), a.code, a.text.as_str()),
            ("a", "KeyA", "a")
        );
        let upper = describe("a", MOD_SHIFT).unwrap();
        assert_eq!((upper.key.as_str(), upper.text.as_str()), ("A", "A"));
        let bang = describe("!", 0).unwrap();
        assert_eq!((bang.key.as_str(), bang.code), ("!", "Digit1"));
        let ctrl_a = describe("a", MOD_CONTROL).unwrap();
        assert_eq!(ctrl_a.text, "");
        let enter = describe("Enter", 0).unwrap();
        assert_eq!((enter.text.as_str(), enter.key_code), ("\r", 13));
        assert_eq!(describe("KeyB", 0).unwrap().key, "b");
        assert_eq!(describe("Control", 0).unwrap().location, 1);
        assert!(describe("日", 0).is_err());
    }
}
