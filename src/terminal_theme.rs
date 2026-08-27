//! Đồng bộ foreground, background và bảng màu ANSI từ terminal host.

use std::fmt::Write as _;

use libghostty_vt::style::RgbColor;

/// Màu mặc định mà terminal bên ngoài đang sử dụng.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HostTerminalTheme {
    pub(crate) foreground: Option<RgbColor>,
    pub(crate) background: Option<RgbColor>,
    pub(crate) cursor: Option<RgbColor>,
    pub(crate) palette: [Option<RgbColor>; 256],
}

impl Default for HostTerminalTheme {
    fn default() -> Self {
        Self {
            foreground: None,
            background: None,
            cursor: None,
            palette: [None; 256],
        }
    }
}

impl HostTerminalTheme {
    fn update_from_sequence(&mut self, sequence: &[u8]) {
        let Ok(sequence) = std::str::from_utf8(sequence) else {
            return;
        };
        let Some(body) = sequence.strip_prefix("\x1b]") else {
            return;
        };
        let body = body
            .strip_suffix("\x1b\\")
            .or_else(|| body.strip_suffix('\u{7}'))
            .unwrap_or(body);
        let mut fields = body.split(';');
        match fields.next() {
            Some("10") => self.foreground = fields.next().and_then(parse_rgb_color),
            Some("11") => self.background = fields.next().and_then(parse_rgb_color),
            Some("12") => self.cursor = fields.next().and_then(parse_rgb_color),
            Some("4") => {
                while let (Some(index), Some(color)) = (fields.next(), fields.next()) {
                    if let (Ok(index), Some(color)) = (index.parse::<u8>(), parse_rgb_color(color))
                    {
                        self.palette[usize::from(index)] = Some(color);
                    }
                }
            }
            _ => {}
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CellPixelSize {
    pub(crate) width: u32,
    pub(crate) height: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct HostTerminalCapabilities {
    pub(crate) theme: HostTerminalTheme,
    pub(crate) cell_size: CellPixelSize,
}

impl HostTerminalCapabilities {
    /// Hỏi màu và kích thước cell của terminal host.
    pub(crate) fn query() -> Self {
        query_host_terminal_capabilities().unwrap_or_default()
    }
}

fn host_terminal_query_sequence() -> String {
    let mut sequence = String::from("\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b]12;?\x1b\\\x1b[16t");
    for index in 0..=u8::MAX {
        let _ = write!(sequence, "\x1b]4;{index};?\x1b\\");
    }
    sequence
}

fn parse_rgb_color(value: &str) -> Option<RgbColor> {
    if let Some(rgb) = value.strip_prefix("rgb:") {
        let mut parts = rgb.split('/');
        return Some(RgbColor {
            r: parse_hex_component(parts.next()?)?,
            g: parse_hex_component(parts.next()?)?,
            b: parse_hex_component(parts.next()?)?,
        })
        .filter(|_| parts.next().is_none());
    }

    let hex = value.strip_prefix('#')?;
    let digits = hex.len() / 3;
    if !matches!(digits, 1..=4) || hex.len() != digits * 3 {
        return None;
    }
    Some(RgbColor {
        r: parse_hex_component(&hex[..digits])?,
        g: parse_hex_component(&hex[digits..digits * 2])?,
        b: parse_hex_component(&hex[digits * 2..])?,
    })
}

fn parse_hex_component(component: &str) -> Option<u8> {
    if component.is_empty()
        || component.len() > 4
        || !component.chars().all(|ch| ch.is_ascii_hexdigit())
    {
        return None;
    }
    let value = u32::from_str_radix(component, 16).ok()?;
    let max = (1u32 << (component.len() * 4)) - 1;
    Some(((value * 255 + max / 2) / max) as u8)
}

#[cfg(unix)]
fn query_host_terminal_capabilities() -> std::io::Result<HostTerminalCapabilities> {
    use std::fs::OpenOptions;
    use std::io::{IsTerminal, Read, Write};
    use std::os::fd::AsRawFd;
    use std::time::{Duration, Instant};

    if !std::io::stdout().is_terminal() {
        return Ok(HostTerminalCapabilities::default());
    }

    let mut tty = OpenOptions::new().read(true).open("/dev/tty")?;
    let mut stdout = std::io::stdout();
    stdout.write_all(host_terminal_query_sequence().as_bytes())?;
    stdout.flush()?;

    let started = Instant::now();
    let mut last_data = None;
    let mut bytes = Vec::new();
    loop {
        let elapsed = started.elapsed();
        if elapsed >= Duration::from_millis(400)
            || last_data.is_some_and(|last: Instant| last.elapsed() >= Duration::from_millis(40))
        {
            break;
        }
        let timeout = if last_data.is_some() { 40 } else { 120 };
        let mut poll_fd = libc::pollfd {
            fd: tty.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll_fd trỏ tới một phần tử hợp lệ trong suốt lời gọi poll.
        let ready = unsafe { libc::poll(&mut poll_fd, 1, timeout) };
        if ready <= 0 {
            if last_data.is_none() {
                break;
            }
            continue;
        }
        let mut chunk = [0u8; 8192];
        let count = tty.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..count]);
        last_data = Some(Instant::now());
    }

    Ok(parse_host_terminal_capabilities(&bytes))
}

#[cfg(not(unix))]
fn query_host_terminal_capabilities() -> std::io::Result<HostTerminalCapabilities> {
    Ok(HostTerminalCapabilities::default())
}

fn parse_host_terminal_capabilities(bytes: &[u8]) -> HostTerminalCapabilities {
    HostTerminalCapabilities {
        theme: parse_host_terminal_theme(bytes),
        cell_size: parse_cell_pixel_size(bytes).unwrap_or_default(),
    }
}

fn parse_cell_pixel_size(bytes: &[u8]) -> Option<CellPixelSize> {
    let text = std::str::from_utf8(bytes).ok()?;
    for part in text.split("\x1b[").skip(1) {
        let Some(report) = part.strip_prefix("6;") else {
            continue;
        };
        let Some((report, _)) = report.split_once('t') else {
            continue;
        };
        let Some((height, width)) = report.split_once(';') else {
            continue;
        };
        if let (Ok(height), Ok(width)) = (height.parse(), width.parse()) {
            return Some(CellPixelSize { width, height });
        }
    }
    None
}

fn parse_host_terminal_theme(bytes: &[u8]) -> HostTerminalTheme {
    let mut theme = HostTerminalTheme::default();
    let mut offset = 0;
    while offset + 2 <= bytes.len() {
        let Some(start) = bytes[offset..].windows(2).position(|part| part == b"\x1b]") else {
            break;
        };
        let start = offset + start;
        let body = &bytes[start + 2..];
        let Some((end, terminator_len)) = body.iter().enumerate().find_map(|(index, byte)| {
            if *byte == 0x07 {
                Some((index, 1))
            } else if *byte == 0x1b && body.get(index + 1) == Some(&b'\\') {
                Some((index, 2))
            } else {
                None
            }
        }) else {
            break;
        };
        let sequence_end = start + 2 + end + terminator_len;
        theme.update_from_sequence(&bytes[start..sequence_end]);
        offset = sequence_end;
    }
    theme
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_default_colors_and_palette_with_both_terminators() {
        let theme = parse_host_terminal_theme(
            b"noise\x1b]10;rgb:cccc/dddd/eeee\x1b\\\x1b]11;#123456\x07\x1b]4;7;rgb:ffff/8000/0\x1b\\",
        );
        assert_eq!(
            theme.foreground,
            Some(RgbColor {
                r: 0xcc,
                g: 0xdd,
                b: 0xee
            })
        );
        assert_eq!(
            theme.background,
            Some(RgbColor {
                r: 0x12,
                g: 0x34,
                b: 0x56
            })
        );
        assert_eq!(
            theme.palette[7],
            Some(RgbColor {
                r: 255,
                g: 128,
                b: 0
            })
        );
    }

    #[test]
    fn ignores_incomplete_and_invalid_responses() {
        let theme = parse_host_terminal_theme(b"\x1b]10;rgb:ff/ff\x1b\\\x1b]11;rgb:00/11/22");
        assert_eq!(theme, HostTerminalTheme::default());
    }

    #[test]
    fn parses_cell_pixel_size_among_other_responses() {
        let capabilities =
            parse_host_terminal_capabilities(b"\x1b]10;rgb:ffff/ffff/ffff\x1b\\\x1b[6;18;9t");
        assert_eq!(
            capabilities.cell_size,
            CellPixelSize {
                width: 9,
                height: 18
            }
        );
    }

    #[test]
    fn builds_queries_for_all_palette_entries() {
        let query = host_terminal_query_sequence();
        assert!(query.starts_with("\x1b]10;?"));
        assert!(query.contains("\x1b[16t"));
        assert!(query.ends_with("\x1b]4;255;?\x1b\\"));
        assert_eq!(query.matches("\x1b]4;").count(), 256);
    }
}
