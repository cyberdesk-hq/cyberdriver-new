// SPDX-License-Identifier: AGPL-3.0-only
//
// Input endpoints for Cyberdesk's HTTP-over-WS tunnel.
//
// M6 intentionally routes through RustDesk's existing input service
// instead of using a separate automation library. On Windows this means
// input_service delegates to portable_service, preserving the service /
// secure-desktop behavior that lets RustDesk control the login screen.

use hbb_common::{
    anyhow::{bail, Context, Result},
    message_proto::{key_event, ControlKey, KeyEvent, KeyboardMode, MouseEvent},
};
use serde_derive::Deserialize;
use serde_json::json;
use std::{thread, time::Duration};

const CYBERDESK_TUNNEL_CONN_ID: i32 = 0;
const MAX_INPUT_BODY_BYTES: usize = 64 * 1024;
const MAX_KEY_GROUPS: usize = 64;
const KEY_TAP_DELAY: Duration = Duration::from_millis(20);
const MOUSE_CLICK_DELAY: Duration = Duration::from_millis(35);
const CLIPBOARD_RETRY_ATTEMPTS: usize = 8;
const CLIPBOARD_INITIAL_DELAY: Duration = Duration::from_millis(200);
const CLIPBOARD_RETRY_STEP: Duration = Duration::from_millis(100);

#[derive(Debug, Deserialize)]
struct MouseMoveRequest {
    x: i32,
    y: i32,
}

#[derive(Debug, Deserialize)]
struct MouseClickRequest {
    x: Option<i32>,
    y: Option<i32>,
    #[serde(default = "default_mouse_button")]
    button: String,
    down: Option<bool>,
    #[serde(default = "default_clicks")]
    clicks: u8,
}

#[derive(Debug, Deserialize)]
struct MouseScrollRequest {
    direction: String,
    amount: i32,
    x: Option<i32>,
    y: Option<i32>,
}

#[derive(Debug, Deserialize)]
struct MouseDragRequest {
    to_x: i32,
    to_y: i32,
    start_x: i32,
    start_y: i32,
    duration: Option<f64>,
    #[serde(default = "default_mouse_button")]
    button: String,
}

#[derive(Debug, Deserialize)]
struct KeyboardTypeRequest {
    text: String,
}

#[derive(Debug, Deserialize)]
struct KeyboardKeyRequest {
    text: String,
    down: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct CopyToClipboardRequest {
    text: String,
}

#[derive(Debug, Clone, Copy)]
enum KeyToken {
    Char(char),
    Control(ControlKey),
}

struct ParsedKeyGroup {
    modifiers: Vec<ControlKey>,
    key: KeyToken,
}

pub fn mouse_position() -> Result<Vec<u8>> {
    let (x, y) = crate::get_cursor_pos().context("failed to get cursor position")?;
    Ok(serde_json::to_vec(&json!({ "x": x, "y": y }))?)
}

pub fn mouse_move(body: &[u8]) -> Result<Vec<u8>> {
    let request: MouseMoveRequest = parse_json(body)?;
    send_mouse(crate::input::MOUSE_TYPE_MOVE, 0, request.x, request.y);
    empty_json()
}

pub fn mouse_click(body: &[u8]) -> Result<Vec<u8>> {
    let request: MouseClickRequest = parse_json(body)?;
    let button = mouse_button(&request.button)?;
    let clicks = validate_clicks(request.clicks)?;

    if let (Some(x), Some(y)) = (request.x, request.y) {
        send_mouse(crate::input::MOUSE_TYPE_MOVE, 0, x, y);
    }

    match request.down {
        Some(true) => send_mouse(crate::input::MOUSE_TYPE_DOWN, button, 0, 0),
        Some(false) => send_mouse(crate::input::MOUSE_TYPE_UP, button, 0, 0),
        None => {
            for _ in 0..clicks {
                send_mouse(crate::input::MOUSE_TYPE_DOWN, button, 0, 0);
                thread::sleep(MOUSE_CLICK_DELAY);
                send_mouse(crate::input::MOUSE_TYPE_UP, button, 0, 0);
                thread::sleep(MOUSE_CLICK_DELAY);
            }
        }
    }

    empty_json()
}

pub fn mouse_scroll(body: &[u8]) -> Result<Vec<u8>> {
    let request: MouseScrollRequest = parse_json(body)?;
    if request.amount < 0 {
        bail!("'amount' must be non-negative");
    }
    if request.amount == 0 {
        return empty_json();
    }

    if let (Some(x), Some(y)) = (request.x, request.y) {
        send_mouse(crate::input::MOUSE_TYPE_MOVE, 0, x, y);
    }

    let (x, y) = scroll_delta(&request.direction, request.amount)?;
    send_mouse(crate::input::MOUSE_TYPE_WHEEL, 0, x, y);
    empty_json()
}

pub fn mouse_drag(body: &[u8]) -> Result<Vec<u8>> {
    let request: MouseDragRequest = parse_json(body)?;
    let button = mouse_button(&request.button)?;

    send_mouse(
        crate::input::MOUSE_TYPE_MOVE,
        0,
        request.start_x,
        request.start_y,
    );
    send_mouse(crate::input::MOUSE_TYPE_DOWN, button, 0, 0);

    if let Some(duration) = request.duration {
        if duration > 0.0 {
            thread::sleep(Duration::from_secs_f64(duration.min(5.0)));
        }
    }

    send_mouse(crate::input::MOUSE_TYPE_MOVE, 0, request.to_x, request.to_y);
    thread::sleep(MOUSE_CLICK_DELAY);
    send_mouse(crate::input::MOUSE_TYPE_UP, button, 0, 0);
    empty_json()
}

pub fn keyboard_type(body: &[u8]) -> Result<Vec<u8>> {
    let mut request: KeyboardTypeRequest = parse_json(body)?;
    if request.text.is_empty() {
        bail!("'text' field must not be empty");
    }

    request.text = normalize_text_for_typing(&request.text);
    if request.text.is_empty() {
        bail!("'text' field must not be empty after normalization");
    }

    let mut event = KeyEvent::new();
    event.set_seq(request.text);
    event.mode = KeyboardMode::Translate.into();
    crate::input_service::handle_key(&event);
    empty_json()
}

pub fn keyboard_key(body: &[u8]) -> Result<Vec<u8>> {
    let request: KeyboardKeyRequest = parse_json(body)?;
    if request.text.trim().is_empty() {
        bail!("missing 'text' field");
    }

    let groups = parse_key_sequence(&request.text)?;
    if request.down.is_some() && groups.len() != 1 {
        bail!("'down' may only be used with a single key group");
    }

    match request.down {
        Some(down) => send_key_group(&groups[0], down),
        None => {
            for group in groups {
                send_key_group(&group, true);
                if is_one_shot_function_key(&group) {
                    thread::sleep(KEY_TAP_DELAY);
                    continue;
                }
                thread::sleep(KEY_TAP_DELAY);
                send_key_group(&group, false);
                thread::sleep(KEY_TAP_DELAY);
            }
        }
    }

    empty_json()
}

pub fn copy_to_clipboard(body: &[u8]) -> Result<Vec<u8>> {
    let request: CopyToClipboardRequest = parse_json(body)?;
    let key_name = request.text.trim();
    if key_name.is_empty() {
        bail!("missing 'text' field (key name)");
    }

    clear_text_clipboard();

    let copy_group = parse_key_group("ctrl+c")?;
    send_key_group(&copy_group, true);
    thread::sleep(KEY_TAP_DELAY);
    send_key_group(&copy_group, false);

    let clipboard_content = read_text_clipboard_with_retries();
    Ok(serde_json::to_vec(&json!({ key_name: clipboard_content }))?)
}

fn send_mouse(event_type: i32, button: i32, x: i32, y: i32) {
    let mut event = MouseEvent::new();
    event.mask = event_type | (button << 3);
    event.x = x;
    event.y = y;
    crate::input_service::handle_mouse(
        &event,
        CYBERDESK_TUNNEL_CONN_ID,
        "cyberdesk_tunnel".to_string(),
        0,
        true,
        false,
    );
}

fn send_key_group(group: &ParsedKeyGroup, down: bool) {
    let mut event = KeyEvent {
        down,
        mode: KeyboardMode::Legacy.into(),
        modifiers: group.modifiers.iter().copied().map(Into::into).collect(),
        ..Default::default()
    };

    match group.key {
        KeyToken::Char(ch) => event.set_chr(ch as u32),
        KeyToken::Control(key) => event.set_control_key(key),
    }

    crate::input_service::handle_key(&event);
}

fn is_one_shot_function_key(group: &ParsedKeyGroup) -> bool {
    matches!(
        group.key,
        KeyToken::Control(ControlKey::CtrlAltDel | ControlKey::LockScreen)
    )
}

fn parse_key_sequence(sequence: &str) -> Result<Vec<ParsedKeyGroup>> {
    let mut groups = Vec::new();
    for raw_group in sequence.split_whitespace() {
        if groups.len() >= MAX_KEY_GROUPS {
            bail!("key sequence exceeds {MAX_KEY_GROUPS} group limit");
        }
        groups.push(parse_key_group(raw_group)?);
    }
    if groups.is_empty() {
        bail!("missing key sequence");
    }
    Ok(groups)
}

fn parse_key_group(group: &str) -> Result<ParsedKeyGroup> {
    if let Some(special) = special_key_group(group) {
        return Ok(special);
    }

    let mut modifiers = Vec::new();
    let mut key = None;

    for raw_token in group.split('+') {
        let token = normalize_key_token(raw_token);
        if token.is_empty() {
            continue;
        }
        if let Some(modifier) = modifier_key(&token) {
            modifiers.push(modifier);
            continue;
        }
        if key.is_some() {
            bail!("key group '{group}' has more than one non-modifier key");
        }
        key = Some(named_key(&token).unwrap_or_else(|| {
            let mut chars = token.chars();
            match (chars.next(), chars.next()) {
                (Some(ch), None) => KeyToken::Char(ch),
                _ => KeyToken::Control(ControlKey::Unknown),
            }
        }));
    }

    let key = match key {
        Some(KeyToken::Control(ControlKey::Unknown)) | None => {
            bail!("unsupported key group '{group}'")
        }
        Some(key) => key,
    };

    Ok(ParsedKeyGroup { modifiers, key })
}

fn special_key_group(group: &str) -> Option<ParsedKeyGroup> {
    let tokens = group
        .split('+')
        .map(normalize_key_token)
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();

    let is_ctrl_alt_del = tokens.len() == 3
        && tokens
            .iter()
            .any(|token| matches!(modifier_key(token), Some(ControlKey::Control)))
        && tokens
            .iter()
            .any(|token| matches!(modifier_key(token), Some(ControlKey::Alt)))
        && tokens.iter().any(|token| {
            matches!(
                named_key(token),
                Some(KeyToken::Control(ControlKey::Delete))
            )
        });
    if is_ctrl_alt_del {
        return Some(ParsedKeyGroup {
            modifiers: Vec::new(),
            key: KeyToken::Control(ControlKey::CtrlAltDel),
        });
    }

    None
}

fn normalize_key_token(raw: &str) -> String {
    raw.trim()
        .to_ascii_lowercase()
        .replace('-', "_")
        .replace(' ', "_")
}

fn modifier_key(token: &str) -> Option<ControlKey> {
    match token {
        "ctrl" | "control" | "ctl" => Some(ControlKey::Control),
        "alt" | "option" | "opt" => Some(ControlKey::Alt),
        "shift" => Some(ControlKey::Shift),
        "win" | "windows" | "meta" | "super" | "cmd" | "command" => Some(ControlKey::Meta),
        _ => None,
    }
}

fn named_key(token: &str) -> Option<KeyToken> {
    let key = match token {
        "backspace" => ControlKey::Backspace,
        "tab" => ControlKey::Tab,
        "enter" | "return" => ControlKey::Return,
        "escape" | "esc" => ControlKey::Escape,
        "space" | "spacebar" => ControlKey::Space,
        "delete" | "del" => ControlKey::Delete,
        "insert" | "ins" => ControlKey::Insert,
        "home" => ControlKey::Home,
        "end" => ControlKey::End,
        "page_up" | "pageup" | "pgup" => ControlKey::PageUp,
        "page_down" | "pagedown" | "pgdn" => ControlKey::PageDown,
        "arrow_up" | "up_arrow" | "up" => ControlKey::UpArrow,
        "arrow_down" | "down_arrow" | "down" => ControlKey::DownArrow,
        "arrow_left" | "left_arrow" | "left" => ControlKey::LeftArrow,
        "arrow_right" | "right_arrow" | "right" => ControlKey::RightArrow,
        "caps_lock" | "capslock" => ControlKey::CapsLock,
        "num_lock" | "numlock" => ControlKey::NumLock,
        "ctrl_alt_del" | "ctrlaltdel" | "ctrl_alt_delete" | "ctrlaltdelete" => {
            ControlKey::CtrlAltDel
        }
        "lock_screen" | "lockscreen" => ControlKey::LockScreen,
        "f1" => ControlKey::F1,
        "f2" => ControlKey::F2,
        "f3" => ControlKey::F3,
        "f4" => ControlKey::F4,
        "f5" => ControlKey::F5,
        "f6" => ControlKey::F6,
        "f7" => ControlKey::F7,
        "f8" => ControlKey::F8,
        "f9" => ControlKey::F9,
        "f10" => ControlKey::F10,
        "f11" => ControlKey::F11,
        "f12" => ControlKey::F12,
        _ => return None,
    };
    Some(KeyToken::Control(key))
}

fn mouse_button(raw: &str) -> Result<i32> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "left" => Ok(crate::input::MOUSE_BUTTON_LEFT),
        "right" => Ok(crate::input::MOUSE_BUTTON_RIGHT),
        "middle" | "wheel" => Ok(crate::input::MOUSE_BUTTON_WHEEL),
        other => bail!("invalid button: expected 'left', 'right', or 'middle', got '{other}'"),
    }
}

fn validate_clicks(clicks: u8) -> Result<u8> {
    if (1..=3).contains(&clicks) {
        Ok(clicks)
    } else {
        bail!("clicks must be 1, 2, or 3")
    }
}

fn scroll_delta(direction: &str, amount: i32) -> Result<(i32, i32)> {
    match direction.trim().to_ascii_lowercase().as_str() {
        "up" => Ok((0, amount)),
        "down" => Ok((0, -amount)),
        "left" => Ok((amount, 0)),
        "right" => Ok((-amount, 0)),
        other => {
            bail!("invalid direction: expected 'up', 'down', 'left', or 'right', got '{other}'")
        }
    }
}

fn parse_json<T: for<'de> serde::Deserialize<'de>>(body: &[u8]) -> Result<T> {
    if body.is_empty() {
        bail!("missing JSON request body");
    }
    if body.len() > MAX_INPUT_BODY_BYTES {
        bail!(
            "input request body exceeds {} byte limit",
            MAX_INPUT_BODY_BYTES
        );
    }
    Ok(serde_json::from_slice(body).context("invalid JSON request body")?)
}

fn clear_text_clipboard() {
    if let Ok(mut clipboard) = arboard::Clipboard::new() {
        let _ = clipboard.set_text(String::new());
    }
}

fn read_text_clipboard_with_retries() -> String {
    for attempt in 0..CLIPBOARD_RETRY_ATTEMPTS {
        thread::sleep(CLIPBOARD_INITIAL_DELAY + (CLIPBOARD_RETRY_STEP * attempt as u32));
        if let Ok(mut clipboard) = arboard::Clipboard::new() {
            if let Ok(text) = clipboard.get_text() {
                if !text.is_empty() {
                    return text;
                }
            }
        }
    }

    String::new()
}

fn empty_json() -> Result<Vec<u8>> {
    Ok(b"{}".to_vec())
}

fn default_mouse_button() -> String {
    "left".to_string()
}

fn default_clicks() -> u8 {
    1
}

fn normalize_text_for_typing(text: &str) -> String {
    text.replace('\u{2018}', "'")
        .replace('\u{2019}', "'")
        .replace('\u{201C}', "\"")
        .replace('\u{201D}', "\"")
        .replace('\u{2013}', "-")
        .replace('\u{2014}', "-")
}
