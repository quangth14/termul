//! Xử lý input: phím (kèm prefix mode) và chuột (focus/resize/menu/forward),
//! cùng mã hoá phím/chuột thành bytes cho PTY.

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use ratatui::widgets::Block;
use std::io::Write;
use std::process::{Command, Stdio};

use crate::app::*;
use crate::config::key_matches;
use crate::confirm::*;
use crate::layout::{PaneId, SplitDir};
use crate::menu::*;
use crate::palette::*;
use crate::rename::*;
use crate::session::*;
use crate::suggest::*;
use crate::term::GridPoint;

pub(crate) fn handle_key(app: &mut App, key: KeyEvent) {
    if key.kind == KeyEventKind::Release {
        return;
    }
    // Modal ưu tiên: palette > rename > confirm > menu.
    if app.palette.is_some() {
        handle_palette_key(app, key);
        return;
    }
    if app.rename.is_some() {
        handle_rename_key(app, key);
        return;
    }
    if app.confirm.is_some() {
        handle_confirm_key(app, key);
        return;
    }
    if app.menu.is_some() {
        handle_menu_key(app, key);
        return;
    }
    if is_selection_copy_key(key)
        && let Some(selection) = app.selection
        && let Some(pane) = app.panes.get(&selection.pane)
    {
        let text = pane.grid.screen().selected_text(selection.range);
        if !text.trim().is_empty() && write_clipboard(text.as_bytes()) {
            app.selection = None;
            return;
        }
    }
    if app.prefix_active {
        app.prefix_active = false;
        handle_prefix(app, key);
        return;
    }
    // Prefix key = vào prefix mode; quit key = thoát nhanh.
    if key_matches(key.code, key.modifiers, app.cfg.keys.prefix) {
        app.prefix_active = true;
        return;
    }
    if key_matches(key.code, key.modifiers, app.cfg.keys.quit) {
        app.should_quit = true;
        return;
    }
    // Popup gợi ý đang mở: bắt phím điều hướng (không forward xuống shell).
    if app.suggest.is_some() {
        let mv = app.cfg.suggest_max_visible;
        match key.code {
            KeyCode::Down => {
                if let Some(s) = &mut app.suggest {
                    s.selected = (s.selected + 1).min(s.matches.len().saturating_sub(1));
                    suggest_scroll_to_selected(s, mv);
                }
                return;
            }
            KeyCode::Up => {
                if let Some(s) = &mut app.suggest {
                    s.selected = s.selected.saturating_sub(1);
                    suggest_scroll_to_selected(s, mv);
                }
                return;
            }
            // Enter: chấp nhận gợi ý đang chọn (không submit dòng).
            KeyCode::Enter => {
                suggest_accept(app);
                return;
            }
            KeyCode::Esc => {
                // Đóng popup và không cho tự mở lại cho buffer hiện tại.
                let buf = app
                    .panes
                    .get(&active_focus(app))
                    .map(|p| p.input.buffer.clone());
                app.suggest_dismissed_for = buf;
                app.suggest = None;
                return;
            }
            _ => {}
        }
    }
    let focus = active_focus(app);
    if let Some(bytes) = encode_key(key)
        && let Some(pane) = app.panes.get_mut(&focus)
    {
        // Gõ phím → nhảy về đáy để thấy input/output mới nhất.
        pane.grid.set_scrollback(0);
        pane.pty.write(&bytes);
    }
}

fn is_selection_copy_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('c' | 'C'))
        && matches!(key.modifiers, KeyModifiers::CONTROL)
}

fn write_clipboard(bytes: &[u8]) -> bool {
    #[cfg(target_os = "macos")]
    let commands: &[(&str, &[&str])] = &[("pbcopy", &[])];
    #[cfg(target_os = "linux")]
    let commands: &[(&str, &[&str])] = &[
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard"]),
        ("xsel", &["--clipboard", "--input"]),
    ];
    #[cfg(target_os = "windows")]
    let commands: &[(&str, &[&str])] = &[("clip.exe", &[])];
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let commands: &[(&str, &[&str])] = &[];

    commands.iter().any(|(program, args)| {
        let Ok(mut child) = Command::new(program)
            .args(*args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            return false;
        };
        let wrote = child
            .stdin
            .take()
            .is_some_and(|mut stdin| stdin.write_all(bytes).is_ok());
        wrote && child.wait().is_ok_and(|status| status.success())
    })
}

pub(crate) fn handle_prefix(app: &mut App, key: KeyEvent) {
    let focus = active_focus(app);
    let k = app.cfg.keys; // Copy → tránh mượn app.cfg khi gọi hàm mutate
    match key.code {
        // Điều hướng focus (phím mũi tên, cố định)
        KeyCode::Left => focus_dir(app, Dir::Left),
        KeyCode::Right => focus_dir(app, Dir::Right),
        KeyCode::Up => focus_dir(app, Dir::Up),
        KeyCode::Down => focus_dir(app, Dir::Down),
        // Các phím lệnh 1 ký tự (cấu hình được)
        KeyCode::Char(c) => {
            if c == k.split_right {
                do_split(app, focus, SplitDir::LeftRight);
            } else if c == k.split_down {
                do_split(app, focus, SplitDir::TopBottom);
            } else if c == k.close_pane {
                request_close(app, focus);
            } else if c == k.tab_new {
                new_tab(app);
            } else if c == k.tab_next {
                next_tab(app);
            } else if c == k.tab_prev {
                prev_tab(app);
            } else if c == k.tab_rename {
                start_rename(app, app.active_tab);
            } else if c == k.tab_close {
                request_close_tab(app, app.active_tab);
            } else if c == k.palette {
                open_palette(app);
            } else if c == k.quit_action {
                app.should_quit = true;
            }
        }
        _ => {}
    }
}

pub(crate) fn center(r: Rect) -> (i32, i32) {
    (
        r.x as i32 + r.width as i32 / 2,
        r.y as i32 + r.height as i32 / 2,
    )
}

pub(crate) fn handle_mouse(app: &mut App, me: MouseEvent) {
    let (col, row) = (me.column, me.row);
    // Modal ưu tiên: palette > rename > confirm > menu > pane.
    if app.palette.is_some() {
        handle_palette_mouse(app, me);
        return;
    }
    if app.rename.is_some() {
        return; // rename chỉ nhận bàn phím
    }
    if app.confirm.is_some() {
        handle_confirm_mouse(app, me);
        return;
    }
    if app.menu.is_some() {
        handle_menu_mouse(app, me);
        return;
    }
    // Popup gợi ý: hover chọn, click chấp nhận, cuộn wheel (toạ độ theo vùng trong viền).
    if let Some(rect) = app.suggest_rect {
        let inner = Block::bordered().inner(rect);
        let mv = app.cfg.suggest_max_visible;
        if within(inner, col, row) {
            match me.kind {
                MouseEventKind::ScrollDown => {
                    if let Some(s) = &mut app.suggest {
                        s.selected = (s.selected + 1).min(s.matches.len().saturating_sub(1));
                        suggest_scroll_to_selected(s, mv);
                    }
                    return;
                }
                MouseEventKind::ScrollUp => {
                    if let Some(s) = &mut app.suggest {
                        s.selected = s.selected.saturating_sub(1);
                        suggest_scroll_to_selected(s, mv);
                    }
                    return;
                }
                MouseEventKind::Moved | MouseEventKind::Drag(_) => {
                    if let Some(s) = &mut app.suggest {
                        let idx = s.offset + (row - inner.y) as usize;
                        if idx < s.matches.len() {
                            s.selected = idx;
                        }
                    }
                    return;
                }
                MouseEventKind::Down(_) => {
                    if let Some(s) = &mut app.suggest {
                        let idx = s.offset + (row - inner.y) as usize;
                        if idx < s.matches.len() {
                            s.selected = idx;
                        }
                    }
                    suggest_accept(app);
                    return;
                }
                _ => {}
            }
        }
    }
    match me.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some(kind) = status_hit(app, col, row) {
                match kind {
                    StatusKind::Switch(i) => switch_tab(app, i),
                    StatusKind::NewTab => new_tab(app),
                }
                return;
            }
            if let Some(drag) = divider_at(app, col, row) {
                app.dragging = Some(drag);
                return;
            }
            if let Some(pid) = pane_at(app, col, row) {
                set_active_focus(app, pid);
                let has_mouse = app
                    .panes
                    .get(&pid)
                    .is_some_and(|pane| pane.grid.has_mouse_tracking());
                if (!has_mouse || me.modifiers.contains(KeyModifiers::SHIFT))
                    && let Some(point) = pane_point(app, pid, col, row, false)
                {
                    app.selection = Some(PaneSelection::new(pid, point));
                } else {
                    app.selection = None;
                    forward_mouse(app, pid, me);
                }
            }
        }
        MouseEventKind::Down(MouseButton::Right) => {
            // Chuột phải trên tab → menu tab; trên pane → menu pane.
            if let Some(StatusKind::Switch(i)) = status_hit(app, col, row) {
                open_tab_menu(app, i, col, row);
            } else if let Some(pid) = pane_at(app, col, row) {
                open_pane_menu(app, pid, col, row);
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if let Some(drag) = app.dragging {
                apply_drag(app, drag, col, row);
            } else if let Some(selection) = app.selection
                && let Some(point) = pane_point(app, selection.pane, col, row, true)
            {
                app.selection = Some(PaneSelection {
                    range: crate::term::GridSelection {
                        end: point,
                        ..selection.range
                    },
                    ..selection
                });
            } else {
                forward_mouse(app, active_focus(app), me);
            }
        }
        MouseEventKind::Up(_) => {
            if app.dragging.is_some() {
                app.dragging = None;
            } else if let Some(selection) = app.selection {
                if let Some(point) = pane_point(app, selection.pane, col, row, true) {
                    app.selection = Some(PaneSelection {
                        range: crate::term::GridSelection {
                            end: point,
                            ..selection.range
                        },
                        ..selection
                    });
                }

                if let Some(pane) = app.panes.get(&selection.pane) {
                    let text = pane.grid.screen().selected_text(selection.range);
                    if text.trim().is_empty() {
                        app.selection = None;
                        return;
                    }
                }
            } else {
                forward_mouse(app, active_focus(app), me);
            }
        }
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            if let Some(pid) = pane_at(app, col, row) {
                scroll_pane(app, pid, matches!(me.kind, MouseEventKind::ScrollUp), me);
            }
        }
        _ => {
            if let Some(pid) = pane_at(app, col, row) {
                forward_mouse(app, pid, me);
            }
        }
    }
}

/// Lăn chuột trên pane:
/// - app có mouse-reporting: forward đúng mouse event;
/// - alternate screen nhưng không có mouse-reporting (less, pager...): đổi wheel
///   thành ba phím mũi tên;
/// - màn hình thường: cuộn viewport nội bộ của libghostty-vt.
pub(crate) fn scroll_pane(app: &mut App, pid: PaneId, up: bool, me: MouseEvent) {
    let (has_mouse, in_alt_screen) = app
        .panes
        .get(&pid)
        .map(|p| (p.grid.has_mouse_tracking(), p.grid.in_alt_screen()))
        .unwrap_or((false, false));

    if has_mouse {
        forward_mouse(app, pid, me);
        return;
    }

    if in_alt_screen {
        let key = if up { b"\x1b[A" } else { b"\x1b[B" };
        if let Some(pane) = app.panes.get_mut(&pid) {
            for _ in 0..3 {
                pane.pty.write(key);
            }
        }
        return;
    }

    if let Some(pane) = app.panes.get_mut(&pid) {
        pane.grid.scroll_lines(if up { -3 } else { 3 });
    }
}

/// Vùng statusbar tại (col,row), nếu có.
pub(crate) fn status_hit(app: &App, col: u16, row: u16) -> Option<StatusKind> {
    if row != app.status_y {
        return None;
    }
    app.status_segs
        .iter()
        .find(|seg| col >= seg.x && col < seg.x + seg.text.chars().count() as u16)
        .map(|seg| seg.kind)
}

pub(crate) fn divider_at(app: &App, col: u16, row: u16) -> Option<DragState> {
    app.dividers
        .iter()
        .find(|d| within(d.line, col, row))
        .map(|d| DragState {
            id: d.id,
            dir: d.dir,
            bounds: d.bounds,
        })
}

pub(crate) fn pane_at(app: &App, col: u16, row: u16) -> Option<PaneId> {
    app.areas
        .iter()
        .find(|(_, r)| within(**r, col, row))
        .map(|(pid, _)| *pid)
}

/// Đổi toạ độ màn hình sang ô trong viewport của pane.
/// Khi đang kéo, toạ độ ngoài pane được ghim vào mép gần nhất.
fn pane_point(app: &App, pid: PaneId, col: u16, row: u16, clamp: bool) -> Option<GridPoint> {
    let inner = app.inner_areas.get(&pid)?;
    if inner.width == 0 || inner.height == 0 {
        return None;
    }
    if !clamp && !within(*inner, col, row) {
        return None;
    }
    let col = col.clamp(inner.x, inner.x + inner.width - 1) - inner.x;
    let row = row.clamp(inner.y, inner.y + inner.height - 1) - inner.y;
    Some(GridPoint { row, col })
}

pub(crate) fn within(r: Rect, col: u16, row: u16) -> bool {
    col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height
}

pub(crate) fn apply_drag(app: &mut App, drag: DragState, col: u16, row: u16) {
    let ratio = match drag.dir {
        SplitDir::LeftRight => {
            col.saturating_sub(drag.bounds.x) as f32 / drag.bounds.width.max(1) as f32
        }
        SplitDir::TopBottom => {
            row.saturating_sub(drag.bounds.y) as f32 / drag.bounds.height.max(1) as f32
        }
    };
    let ti = app.active_tab;
    app.tabs[ti].layout.set_ratio(drag.id, ratio);
}

/// Forward chuột vào app trong pane, chỉ khi app đó bật mouse-reporting.
/// Giữ `Shift` = ép về mức multiplexer (không forward) để terminal ngoài chọn text.
pub(crate) fn forward_mouse(app: &mut App, pid: PaneId, me: MouseEvent) {
    if me.modifiers.contains(KeyModifiers::SHIFT) {
        return;
    }
    let Some(inner) = app.inner_areas.get(&pid).copied() else {
        return;
    };
    let has_mouse = app
        .panes
        .get(&pid)
        .map(|p| p.grid.has_mouse_tracking())
        .unwrap_or(false);
    if !has_mouse {
        return;
    }
    if let Some(pane) = app.panes.get_mut(&pid)
        && let Some(bytes) = pane.grid.encode_mouse(me, inner)
    {
        pane.pty.write(&bytes);
    }
}

// ── Mã hoá phím & chuột ─────────────────────────────────────────────

/// Mã hoá phím thành bytes ANSI để ghi vào PTY.
pub(crate) fn encode_key(key: KeyEvent) -> Option<Vec<u8>> {
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let mut out: Vec<u8> = Vec::new();

    match key.code {
        KeyCode::Char(c) => {
            if ctrl {
                let b = match c.to_ascii_lowercase() {
                    ' ' | '@' => 0,
                    c @ 'a'..='z' => (c as u8) - b'a' + 1,
                    '[' => 27,
                    '\\' => 28,
                    ']' => 29,
                    '^' => 30,
                    '_' => 31,
                    _ => return None,
                };
                if alt {
                    out.push(0x1b);
                }
                out.push(b);
            } else {
                if alt {
                    out.push(0x1b);
                }
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
        }
        KeyCode::Enter => {
            if alt {
                out.push(0x1b);
            }
            out.push(b'\r');
        }
        KeyCode::Backspace => {
            if alt {
                out.push(0x1b);
            }
            out.push(0x7f);
        }
        KeyCode::Tab => {
            if alt {
                out.push(0x1b);
            }
            out.push(b'\t');
        }
        KeyCode::BackTab => out.extend_from_slice(b"\x1b[Z"),
        KeyCode::Esc => out.push(0x1b),
        KeyCode::Left => out.extend_from_slice(b"\x1b[D"),
        KeyCode::Right => out.extend_from_slice(b"\x1b[C"),
        KeyCode::Up => out.extend_from_slice(b"\x1b[A"),
        KeyCode::Down => out.extend_from_slice(b"\x1b[B"),
        KeyCode::Home => out.extend_from_slice(b"\x1b[H"),
        KeyCode::End => out.extend_from_slice(b"\x1b[F"),
        KeyCode::PageUp => out.extend_from_slice(b"\x1b[5~"),
        KeyCode::PageDown => out.extend_from_slice(b"\x1b[6~"),
        KeyCode::Delete => out.extend_from_slice(b"\x1b[3~"),
        KeyCode::Insert => out.extend_from_slice(b"\x1b[2~"),
        KeyCode::F(n) => match n {
            1 => out.extend_from_slice(b"\x1bOP"),
            2 => out.extend_from_slice(b"\x1bOQ"),
            3 => out.extend_from_slice(b"\x1bOR"),
            4 => out.extend_from_slice(b"\x1bOS"),
            5 => out.extend_from_slice(b"\x1b[15~"),
            6 => out.extend_from_slice(b"\x1b[17~"),
            7 => out.extend_from_slice(b"\x1b[18~"),
            8 => out.extend_from_slice(b"\x1b[19~"),
            9 => out.extend_from_slice(b"\x1b[20~"),
            10 => out.extend_from_slice(b"\x1b[21~"),
            11 => out.extend_from_slice(b"\x1b[23~"),
            12 => out.extend_from_slice(b"\x1b[24~"),
            _ => return None,
        },
        _ => return None,
    }

    if out.is_empty() { None } else { Some(out) }
}

#[cfg(test)]
mod clipboard_tests {
    use super::*;

    fn key(modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(KeyCode::Char('c'), modifiers)
    }

    #[test]
    fn selection_copy_key_requires_exact_control_or_super() {
        assert!(is_selection_copy_key(key(KeyModifiers::SUPER)));
        assert!(is_selection_copy_key(key(KeyModifiers::CONTROL)));
        assert!(!is_selection_copy_key(key(KeyModifiers::NONE)));
        assert!(!is_selection_copy_key(key(
            KeyModifiers::CONTROL | KeyModifiers::SHIFT
        )));
    }
}
