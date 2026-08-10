//! Quét luồng byte PTY để bắt OSC do shell integration phát ra:
//! - `ESC ] 1337 ; TermulCmd=<b64 cmd> ; <b64 cwd> BEL`  → bắt đầu chạy lệnh
//! - `ESC ] 1337 ; TermulEnd=<exit> BEL`                  → lệnh kết thúc

use base64::Engine;

#[derive(Debug, PartialEq)]
pub enum OscEvent {
    /// Lệnh bắt đầu chạy: (cmdline, cwd).
    CommandStart { cmd: String, cwd: String },
    /// Lệnh kết thúc: exit code.
    CommandEnd { exit: i32 },
    /// Dòng nhập hiện tại thay đổi: vị trí con trỏ (theo ký tự) + nội dung.
    BufferUpdate { cursor: usize, buffer: String },
}

#[derive(PartialEq)]
enum State {
    Normal,
    Esc,
    Osc,
}

/// Máy trạng thái quét OSC, giữ được qua nhiều chunk byte.
pub struct OscScanner {
    state: State,
    buf: Vec<u8>,
}

impl Default for OscScanner {
    fn default() -> Self {
        Self {
            state: State::Normal,
            buf: Vec::new(),
        }
    }
}

const MAX_OSC: usize = 256 * 1024;

impl OscScanner {
    /// Quét `bytes`, đẩy các event tìm được vào `out`.
    pub fn scan(&mut self, bytes: &[u8], out: &mut Vec<OscEvent>) {
        for &b in bytes {
            match self.state {
                State::Normal => {
                    if b == 0x1b {
                        self.state = State::Esc;
                    }
                }
                State::Esc => {
                    if b == b']' {
                        self.state = State::Osc;
                        self.buf.clear();
                    } else {
                        self.state = State::Normal;
                    }
                }
                State::Osc => {
                    // Kết thúc OSC bằng BEL; ST (ESC \) coi ESC như huỷ chuỗi.
                    if b == 0x07 {
                        if let Some(ev) = parse_payload(&self.buf) {
                            out.push(ev);
                        }
                        self.buf.clear();
                        self.state = State::Normal;
                    } else if b == 0x1b {
                        self.buf.clear();
                        self.state = State::Esc;
                    } else if self.buf.len() < MAX_OSC {
                        self.buf.push(b);
                    }
                }
            }
        }
    }
}

fn b64(s: &str) -> Option<String> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(s).ok()?;
    String::from_utf8(bytes).ok()
}

fn parse_payload(buf: &[u8]) -> Option<OscEvent> {
    let s = std::str::from_utf8(buf).ok()?;
    if let Some(rest) = s.strip_prefix("1337;TermulCmd=") {
        let (b64cmd, b64cwd) = rest.split_once(';')?;
        Some(OscEvent::CommandStart {
            cmd: b64(b64cmd)?,
            cwd: b64(b64cwd)?,
        })
    } else if let Some(rest) = s.strip_prefix("1337;TermulEnd=") {
        Some(OscEvent::CommandEnd {
            exit: rest.parse().ok()?,
        })
    } else if let Some(rest) = s.strip_prefix("1337;TermulBuf=") {
        let (cursor, b64buf) = rest.split_once(';')?;
        Some(OscEvent::BufferUpdate {
            cursor: cursor.parse().ok()?,
            buffer: b64(b64buf)?,
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(s: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(s)
    }

    #[test]
    fn parses_command_start_and_end() {
        let mut sc = OscScanner::default();
        let mut out = Vec::new();
        let start = format!("\x1b]1337;TermulCmd={};{}\x07", enc("ls -la"), enc("/tmp/x"));
        sc.scan(start.as_bytes(), &mut out);
        sc.scan(b"\x1b]1337;TermulEnd=0\x07", &mut out);
        assert_eq!(
            out,
            vec![
                OscEvent::CommandStart {
                    cmd: "ls -la".into(),
                    cwd: "/tmp/x".into()
                },
                OscEvent::CommandEnd { exit: 0 },
            ]
        );
    }

    #[test]
    fn survives_split_across_chunks() {
        let mut sc = OscScanner::default();
        let mut out = Vec::new();
        let full = format!("\x1b]1337;TermulCmd={};{}\x07", enc("echo hi"), enc("/h"));
        let (a, b) = full.as_bytes().split_at(10);
        sc.scan(a, &mut out);
        assert!(out.is_empty());
        sc.scan(b, &mut out);
        assert_eq!(
            out,
            vec![OscEvent::CommandStart {
                cmd: "echo hi".into(),
                cwd: "/h".into()
            }]
        );
    }

    #[test]
    fn ignores_unrelated_osc() {
        let mut sc = OscScanner::default();
        let mut out = Vec::new();
        sc.scan(b"\x1b]0;window title\x07some text", &mut out);
        assert!(out.is_empty());
    }
}
