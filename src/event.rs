//! Điều phối `AppEvent`: dữ liệu PTY, đóng pane, phím/chuột, và marker OSC 133.

use std::time::Instant;

use crossterm::event::Event;
use ratatui::style::Color;

use crate::app::*;
use crate::input::{handle_key, handle_mouse, handle_paste};
use crate::layout::PaneId;
use crate::mention::rebuild_mention;
use crate::osc::OscEvent;
use crate::session::{active_focus, do_close};
use crate::suggest::rebuild_suggest;
use crate::term::{cursor_position_reply, palette_reply};

pub(crate) fn handle_event(app: &mut App, ev: AppEvent) {
    match ev {
        AppEvent::PtyData(pid, bytes) => {
            let mut osc_events = Vec::new();
            if let Some(pane) = app.panes.get_mut(&pid) {
                let queries = pane.grid.process(&bytes);
                // Codex/Claude dùng OSC 10+11 để suy ra theme và pha màu nền
                // composer. Không trả lời sẽ khiến chúng rơi về style không nền.
                if queries.foreground {
                    pane.pty.write(&palette_reply(10, (248, 248, 242)));
                }
                if queries.background {
                    let bg = match app.cfg.bg {
                        Color::Rgb(r, g, b) => (r, g, b),
                        _ => (40, 42, 54),
                    };
                    pane.pty.write(&palette_reply(11, bg));
                }
                if queries.cursor_position {
                    pane.pty
                        .write(&cursor_position_reply(pane.grid.screen().cursor_position()));
                }
                if queries.keyboard_flags {
                    pane.pty.write(&pane.grid.keyboard_flags_reply());
                }
                // xterm-compatible primary DA, dùng làm sentinel kết thúc
                // capability negotiation của Claude Code/Pi.
                if queries.device_attributes {
                    pane.pty.write(b"\x1b[?1;2c");
                }
                pane.osc.scan(&bytes, &mut osc_events);
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
        AppEvent::Term(Event::Resize(_, _)) => {}
        AppEvent::Term(_) => {}
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
pub(crate) fn handle_osc(app: &mut App, pid: PaneId, ev: OscEvent) {
    match ev {
        OscEvent::CommandStart { cmd, cwd } => {
            if let Some(pane) = app.panes.get_mut(&pid) {
                pane.cwd = cwd.clone();
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
            let done = app.panes.get_mut(&pid).and_then(|p| p.pending.take());
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
