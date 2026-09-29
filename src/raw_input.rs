//! Đọc duy nhất stream input của terminal host và tách OSC response khỏi input người dùng.

use std::io::Read;
#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::sync::mpsc::Sender;
use std::thread;

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

use crate::app::AppEvent;
use crate::terminal_theme::HostTerminalCapabilities;

const ESC: u8 = 0x1b;
const BRACKETED_PASTE_START: &[u8] = b"\x1b[200~";
const BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";

pub(crate) fn spawn_input_thread(tx: Sender<AppEvent>) {
    thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut reader = stdin.lock();
        let mut framer = InputFramer::default();
        let mut scratch = [0u8; 1024];

        loop {
            match reader.read(&mut scratch) {
                Ok(0) | Err(_) => break,
                Ok(count) => {
                    for event in framer.push(&scratch[..count]) {
                        if tx.send(event).is_err() {
                            return;
                        }
                    }
                    if framer.has_pending_escape() && stdin_read_ready(&reader, 25) == Some(false) {
                        for event in framer.flush_escape() {
                            if tx.send(event).is_err() {
                                return;
                            }
                        }
                    }
                }
            }
        }
    });
}

#[derive(Default)]
struct InputFramer {
    buffer: Vec<u8>,
    capabilities: HostTerminalCapabilities,
}

impl InputFramer {
    fn push(&mut self, bytes: &[u8]) -> Vec<AppEvent> {
        self.buffer.extend_from_slice(bytes);
        let mut events = Vec::new();
        while let Some((event, consumed)) = self.extract_one() {
            self.buffer.drain(..consumed);
            if let Some(event) = event {
                events.push(event);
            }
        }
        events
    }

    fn has_pending_escape(&self) -> bool {
        self.buffer == [ESC]
    }

    fn flush_escape(&mut self) -> Vec<AppEvent> {
        if self.has_pending_escape() {
            self.buffer.clear();
            vec![AppEvent::Term(Event::Key(key(KeyCode::Esc)))]
        } else {
            Vec::new()
        }
    }

    fn extract_one(&mut self) -> Option<(Option<AppEvent>, usize)> {
        let bytes = &self.buffer;
        let first = *bytes.first()?;
        if first != ESC {
            return plain_key(bytes, false)
                .map(|(key, consumed)| (Some(AppEvent::Term(Event::Key(key))), consumed));
        }
        if bytes.len() == 1 {
            return None;
        }
        match bytes[1] {
            b']' => self.osc_event(),
            b'[' => self.csi_event(),
            b'O' => self.ss3_event(),
            ESC => Some((Some(AppEvent::Term(Event::Key(key(KeyCode::Esc)))), 1)),
            _ => plain_key(&bytes[1..], true)
                .map(|(key, consumed)| (Some(AppEvent::Term(Event::Key(key))), consumed + 1)),
        }
    }

    fn osc_event(&mut self) -> Option<(Option<AppEvent>, usize)> {
        let end = osc_end(&self.buffer)?;
        let sequence = &self.buffer[..end];
        let changed = self.capabilities.apply_response(sequence);
        let event = if changed {
            Some(AppEvent::HostCapabilities(self.capabilities))
        } else {
            None
        };
        Some((event, end))
    }

    fn csi_event(&mut self) -> Option<(Option<AppEvent>, usize)> {
        if self.buffer.starts_with(BRACKETED_PASTE_START) {
            let end = find_subsequence(&self.buffer, BRACKETED_PASTE_END)?;
            let text = String::from_utf8_lossy(&self.buffer[BRACKETED_PASTE_START.len()..end]);
            return Some((
                Some(AppEvent::Term(Event::Paste(text.into_owned()))),
                end + BRACKETED_PASTE_END.len(),
            ));
        }
        let end = csi_end(&self.buffer)?;
        let sequence = std::str::from_utf8(&self.buffer[..end]).ok()?;
        if self.capabilities.apply_response(sequence.as_bytes()) {
            return Some((Some(AppEvent::HostCapabilities(self.capabilities)), end));
        }
        Some((parse_csi(sequence).map(AppEvent::Term), end))
    }

    fn ss3_event(&self) -> Option<(Option<AppEvent>, usize)> {
        let byte = *self.buffer.get(2)?;
        let code = match byte {
            b'A' => KeyCode::Up,
            b'B' => KeyCode::Down,
            b'C' => KeyCode::Right,
            b'D' => KeyCode::Left,
            b'H' => KeyCode::Home,
            b'F' => KeyCode::End,
            b'P' => KeyCode::F(1),
            b'Q' => KeyCode::F(2),
            b'R' => KeyCode::F(3),
            b'S' => KeyCode::F(4),
            _ => return Some((None, 3)),
        };
        Some((Some(AppEvent::Term(Event::Key(key(code)))), 3))
    }
}

fn osc_end(bytes: &[u8]) -> Option<usize> {
    let body = &bytes[2..];
    body.iter().enumerate().find_map(|(index, byte)| {
        if *byte == 0x07 {
            Some(index + 3)
        } else if *byte == ESC && body.get(index + 1) == Some(&b'\\') {
            Some(index + 4)
        } else {
            None
        }
    })
}

fn csi_end(bytes: &[u8]) -> Option<usize> {
    bytes[2..]
        .iter()
        .position(|byte| (0x40..=0x7e).contains(byte))
        .map(|index| index + 3)
}

fn find_subsequence(bytes: &[u8], needle: &[u8]) -> Option<usize> {
    bytes
        .windows(needle.len())
        .position(|window| window == needle)
}

fn parse_csi(sequence: &str) -> Option<Event> {
    if sequence == "\x1b[I" {
        return Some(Event::FocusGained);
    }
    if sequence == "\x1b[O" {
        return Some(Event::FocusLost);
    }
    if sequence == "\x1b[Z" {
        return Some(Event::Key(KeyEvent::new(
            KeyCode::BackTab,
            KeyModifiers::SHIFT,
        )));
    }
    if sequence.starts_with("\x1b[<") {
        return parse_sgr_mouse(sequence).map(Event::Mouse);
    }
    let body = sequence.strip_prefix("\x1b[")?;
    let final_byte = body.chars().last()?;
    let params = &body[..body.len() - final_byte.len_utf8()];
    let (code, modifiers, kind) = parse_params(params);
    let key_code = match final_byte {
        'A' => KeyCode::Up,
        'B' => KeyCode::Down,
        'C' => KeyCode::Right,
        'D' => KeyCode::Left,
        'H' => KeyCode::Home,
        'F' => KeyCode::End,
        '~' => match code? {
            1 | 7 => KeyCode::Home,
            2 => KeyCode::Insert,
            3 => KeyCode::Delete,
            4 | 8 => KeyCode::End,
            5 => KeyCode::PageUp,
            6 => KeyCode::PageDown,
            11..=24 => KeyCode::F((code? - 10) as u8),
            _ => return None,
        },
        'u' => kitty_key_code(code?)?,
        _ => return None,
    };
    Some(Event::Key(KeyEvent::new_with_kind(
        key_code, modifiers, kind,
    )))
}

fn parse_params(params: &str) -> (Option<u32>, KeyModifiers, KeyEventKind) {
    let mut parts = params.split(';');
    let code = parts.next().and_then(|value| value.parse().ok());
    let (modifiers, kind) = parts
        .next()
        .map(|value| {
            let (modifiers, event_type) = value.split_once(':').unwrap_or((value, "1"));
            (
                modifiers
                    .parse::<u8>()
                    .map(modifiers_from_param)
                    .unwrap_or(KeyModifiers::NONE),
                match event_type {
                    "2" => KeyEventKind::Repeat,
                    "3" => KeyEventKind::Release,
                    _ => KeyEventKind::Press,
                },
            )
        })
        .unwrap_or((KeyModifiers::NONE, KeyEventKind::Press));
    (code, modifiers, kind)
}

fn kitty_key_code(code: u32) -> Option<KeyCode> {
    Some(match code {
        9 => KeyCode::Tab,
        13 => KeyCode::Enter,
        27 => KeyCode::Esc,
        127 => KeyCode::Backspace,
        _ => KeyCode::Char(char::from_u32(code)?),
    })
}

fn modifiers_from_param(value: u8) -> KeyModifiers {
    let bits = value.saturating_sub(1);
    let mut modifiers = KeyModifiers::NONE;
    if bits & 1 != 0 {
        modifiers.insert(KeyModifiers::SHIFT);
    }
    if bits & 2 != 0 {
        modifiers.insert(KeyModifiers::ALT);
    }
    if bits & 4 != 0 {
        modifiers.insert(KeyModifiers::CONTROL);
    }
    modifiers
}

fn parse_sgr_mouse(sequence: &str) -> Option<MouseEvent> {
    let body = sequence.strip_prefix("\x1b[<")?;
    let final_byte = body.chars().last()?;
    let payload = &body[..body.len() - 1];
    let mut fields = payload.split(';');
    let code = fields.next()?.parse::<u8>().ok()?;
    let column = fields.next()?.parse::<u16>().ok()?.checked_sub(1)?;
    let row = fields.next()?.parse::<u16>().ok()?.checked_sub(1)?;
    let modifiers = mouse_modifiers(code);
    let kind = if code & 64 != 0 {
        if code & 1 == 0 {
            MouseEventKind::ScrollUp
        } else {
            MouseEventKind::ScrollDown
        }
    } else {
        let button = match code & 3 {
            0 => MouseButton::Left,
            1 => MouseButton::Middle,
            2 => MouseButton::Right,
            _ => MouseButton::Left,
        };
        if final_byte == 'm' {
            MouseEventKind::Up(button)
        } else if code & 32 != 0 {
            MouseEventKind::Drag(button)
        } else {
            MouseEventKind::Down(button)
        }
    };
    Some(MouseEvent {
        kind,
        column,
        row,
        modifiers,
    })
}

fn mouse_modifiers(code: u8) -> KeyModifiers {
    let mut modifiers = KeyModifiers::NONE;
    if code & 4 != 0 {
        modifiers.insert(KeyModifiers::SHIFT);
    }
    if code & 8 != 0 {
        modifiers.insert(KeyModifiers::ALT);
    }
    if code & 16 != 0 {
        modifiers.insert(KeyModifiers::CONTROL);
    }
    modifiers
}

fn plain_key(bytes: &[u8], alt: bool) -> Option<(KeyEvent, usize)> {
    let first = *bytes.first()?;
    let mut modifiers = if alt {
        KeyModifiers::ALT
    } else {
        KeyModifiers::NONE
    };
    let code = match first {
        b'\r' => KeyCode::Enter,
        b'\t' => KeyCode::Tab,
        0x7f => KeyCode::Backspace,
        0 => {
            modifiers.insert(KeyModifiers::CONTROL);
            KeyCode::Char('`')
        }
        1..=26 => {
            modifiers.insert(KeyModifiers::CONTROL);
            KeyCode::Char(char::from(b'a' + first - 1))
        }
        28..=31 => {
            modifiers.insert(KeyModifiers::CONTROL);
            KeyCode::Char(char::from(b'\\' + first - 28))
        }
        _ => {
            let text = std::str::from_utf8(bytes).ok()?;
            let character = text.chars().next()?;
            return Some((
                KeyEvent::new(KeyCode::Char(character), modifiers),
                character.len_utf8(),
            ));
        }
    };
    Some((KeyEvent::new(code, modifiers), 1))
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new_with_kind(code, KeyModifiers::NONE, KeyEventKind::Press)
}

#[cfg(unix)]
fn stdin_read_ready<R: AsRawFd>(reader: &R, timeout_ms: i32) -> Option<bool> {
    let mut fd = libc::pollfd {
        fd: reader.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: fd trỏ tới bộ nhớ hợp lệ trong suốt lời gọi poll.
    let result = unsafe { libc::poll(&mut fd, 1, timeout_ms) };
    (result >= 0).then_some(result > 0)
}

#[cfg(not(unix))]
fn stdin_read_ready<R>(_reader: &R, _timeout_ms: i32) -> Option<bool> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separates_host_response_from_keyboard_input() {
        let mut framer = InputFramer::default();
        let events = framer.push(b"\x1b]10;rgb:ffff/0000/0000\x1b\\a");

        assert!(matches!(
            events.first(),
            Some(AppEvent::HostCapabilities(capabilities))
                if capabilities.theme.foreground.is_some()
        ));
        assert!(matches!(
            events.get(1),
            Some(AppEvent::Term(Event::Key(event)))
                if event.code == KeyCode::Char('a')
        ));
    }

    #[test]
    fn buffers_split_osc_response_without_emitting_input() {
        let mut framer = InputFramer::default();
        assert!(framer.push(b"\x1b]11;rgb:0000").is_empty());

        let events = framer.push(b"/1111/2222\x1b\\");
        assert!(matches!(
            events.as_slice(),
            [AppEvent::HostCapabilities(capabilities)]
                if capabilities.theme.background.is_some()
        ));
    }

    #[test]
    fn parses_kitty_enter_and_modifier_event_kind() {
        let mut framer = InputFramer::default();
        let events = framer.push(b"\x1b[13;5:2u");

        assert!(matches!(
            events.as_slice(),
            [AppEvent::Term(Event::Key(event))]
                if event.code == KeyCode::Enter
                    && event.modifiers == KeyModifiers::CONTROL
                    && event.kind == KeyEventKind::Repeat
        ));
    }

    #[test]
    fn flushes_a_standalone_escape() {
        let mut framer = InputFramer::default();
        assert!(framer.push(b"\x1b").is_empty());
        assert!(matches!(
            framer.flush_escape().as_slice(),
            [AppEvent::Term(Event::Key(event))] if event.code == KeyCode::Esc
        ));
    }
}
