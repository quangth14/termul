//! Điều phối `AppEvent`: dữ liệu PTY, đóng pane, phím/chuột, và marker OSC 133.

use std::io::Write;
use std::time::Instant;

use crossterm::event::Event;

use crate::app::*;
use crate::input::{handle_key, handle_mouse, handle_paste, write_clipboard_to};
use crate::layout::PaneId;
use crate::mention::rebuild_mention;
use crate::osc::OscEvent;
use crate::session::{active_focus, do_close};
use crate::suggest::rebuild_suggest;

pub(crate) fn handle_event(app: &mut App, ev: AppEvent) {
    match ev {
        AppEvent::PtyData(pid, bytes) => {
            let mut osc_events = Vec::new();
            let effects = app.panes.get_mut(&pid).map(|pane| {
                let effects = pane.grid.process(&bytes);
                for response in &effects.pty_responses {
                    pane.pty.write(response);
                }
                if let Some(cwd) = effects.cwd.as_deref().and_then(normalize_reported_cwd) {
                    pane.cwd = cwd;
                }
                if let Some(title) = &effects.title {
                    pane.title = title.clone();
                }
                pane.osc.scan(&bytes, &mut osc_events);
                effects
            });
            if let Some(effects) = effects {
                if effects.bell_count > 0 {
                    let _ = std::io::stdout().write_all(&vec![b'\x07'; effects.bell_count]);
                    let _ = std::io::stdout().flush();
                }
                for clipboard in effects.clipboard_writes {
                    let _ = write_clipboard_to(&clipboard.data, clipboard.target);
                }
            }
            for ev in osc_events {
                handle_osc(app, pid, ev);
            }
        }
        AppEvent::PtyClosed(pid) => {
            // Shell thoát tự nhiên (vd gõ `exit`) → đóng pane đó.
            if app.panes.contains_key(&pid) {
                do_close(app, pid);
            }
        }
        AppEvent::Term(Event::Key(key)) => handle_key(app, key),
        AppEvent::Term(Event::Mouse(me)) => handle_mouse(app, me),
        AppEvent::Term(Event::Paste(text)) => handle_paste(app, text),
        AppEvent::Term(event @ (Event::FocusGained | Event::FocusLost)) => {
            let focused = matches!(event, Event::FocusGained);
            let pid = active_focus(app);
            if let Some(pane) = app.panes.get_mut(&pid)
                && let Some(response) = pane.grid.focus_report(focused)
            {
                pane.pty.write(response);
            }
        }
        AppEvent::Term(Event::Resize(_, _)) => {}
        AppEvent::MentionReady(result) => {
            if result.pane == active_focus(app)
                && result.generation == app.mention_generation
                && !result.matches.is_empty()
            {
                app.mention = Some(Mention {
                    token_start: result.token_start,
                    token_end: result.token_end,
                    matches: result.matches,
                    selected: 0,
                    offset: 0,
                });
                app.suggest = None;
            }
        }
    }
}

/// Xử lý marker OSC 133 bắt lệnh: bắt đầu → nhớ pending; kết thúc → ghi lịch sử.
fn normalize_reported_cwd(value: &str) -> Option<String> {
    let path = if let Some(rest) = value.strip_prefix("file://") {
        let slash = rest.find('/')?;
        &rest[slash..]
    } else {
        value
    };
    if !path.starts_with('/') {
        return None;
    }

    let bytes = path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let Ok(value) = u8::from_str_radix(&path[index + 1..index + 3], 16)
        {
            decoded.push(value);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

pub(crate) fn handle_osc(app: &mut App, pid: PaneId, ev: OscEvent) {
    match ev {
        OscEvent::CommandStart { cmd, cwd } => {
            if let Some(pane) = app.panes.get_mut(&pid) {
                pane.cwd = cwd.clone();
                pane.input = InputLine::default();
                pane.pending = Some(PendingCmd {
                    cmd,
                    cwd,
                    start: Instant::now(),
                });
            }
            app.suggest = None;
            app.mention = None;
            app.suggest_dismissed_for = None;
        }
        OscEvent::CommandEnd { exit } => {
            let done = app.panes.get_mut(&pid).and_then(|pane| pane.pending.take());
            if let Some(p) = done {
                let dur = p.start.elapsed().as_millis() as u64;
                app.history.record(&p.cmd, &p.cwd, exit, dur);
            }
            app.suggest = None; // lệnh đã chạy → ẩn popup
            app.mention = None;
        }
        OscEvent::BufferUpdate {
            cursor,
            buffer,
            cwd,
        } => {
            if let Some(pane) = app.panes.get_mut(&pid) {
                pane.input = InputLine { buffer, cursor };
                pane.cwd = cwd;
            }
            if pid == active_focus(app) {
                if !rebuild_mention(app) {
                    rebuild_suggest(app);
                } else {
                    app.suggest = None;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_reported_cwd;

    #[test]
    fn normalizes_file_uri_and_percent_encoded_cwd() {
        assert_eq!(
            normalize_reported_cwd("file://localhost/tmp/my%20dir").as_deref(),
            Some("/tmp/my dir")
        );
        assert_eq!(normalize_reported_cwd("relative/path"), None);
    }
}
