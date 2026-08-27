//! Xử lý input: phím (kèm prefix mode) và chuột (focus/resize/menu/forward),
//! cùng mã hoá phím/chuột thành bytes cho PTY.

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use ratatui::widgets::Block;
use std::io::Write;
use std::ops::Range;
use std::process::{Command, Stdio};

use libghostty_vt::unicode;

use crate::app::*;
use crate::config::key_matches;
use crate::confirm::*;
use crate::layout::{PaneId, SplitDir};
use crate::mention::*;
use crate::menu::*;
use crate::palette::*;
use crate::rename::*;
use crate::session::*;
use crate::suggest::*;
use crate::term::{ClipboardTarget, GridPoint, GridSelection};

pub(crate) fn handle_key(app: &mut App, key: KeyEvent) {
    if key.kind == KeyEventKind::Release {
        // Legacy không sinh bytes; Kitty REPORT_EVENTS cần release để app trong
        // pane giữ đúng trạng thái phím nhưng release không được kích hoạt modal.
        let focus = active_focus(app);
        if let Some(pane) = app.panes.get_mut(&focus)
            && let Some(bytes) = pane.grid.encode_key(key)
        {
            pane.pty.write(&bytes);
        }
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
    if is_selection_cut_key(key) && edit_input_selection(app, true) {
        return;
    }
    if is_selection_delete_key(key) && edit_input_selection(app, false) {
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
    // if key_matches(key.code, key.modifiers, app.cfg.keys.quit) {
    //     app.should_quit = true;
    //     return;
    // }
    // Popup mention ưu tiên hơn gợi ý history.
    if app.mention.is_some() {
        let mv = app.cfg.suggest_max_visible;
        match key.code {
            KeyCode::Down => {
                if let Some(mention) = &mut app.mention {
                    mention.selected =
                        (mention.selected + 1).min(mention.matches.len().saturating_sub(1));
                    mention_scroll_to_selected(mention, mv);
                }
                return;
            }
            KeyCode::Up => {
                if let Some(mention) = &mut app.mention {
                    mention.selected = mention.selected.saturating_sub(1);
                    mention_scroll_to_selected(mention, mv);
                }
                return;
            }
            KeyCode::Enter => {
                mention_accept(app);
                return;
            }
            KeyCode::Esc => {
                app.mention = None;
                return;
            }
            _ => {}
        }
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
    if let Some(pane) = app.panes.get_mut(&focus)
        && let Some(bytes) = pane.grid.encode_key(key)
    {
        // Gõ phím → nhảy về đáy để thấy input/output mới nhất.
        pane.grid.set_scrollback(0);
        pane.pty.write(&bytes);
    }
}

/// Xử lý một lần paste nguyên khối. Chỉ bọc marker khi ứng dụng trong pane
/// đã bật DEC mode 2004; nếu không, gửi nội dung thô như terminal thông thường.
pub(crate) fn handle_paste(app: &mut App, text: String) {
    if let Some(pal) = &mut app.palette {
        pal.query.push_str(&text);
        palette_research(app);
        return;
    }
    if let Some(rename) = &mut app.rename {
        rename.buffer.push_str(&text);
        return;
    }
    if app.confirm.is_some() || app.menu.is_some() {
        return;
    }

    let focus = active_focus(app);
    if let Some(pane) = app.panes.get_mut(&focus) {
        pane.grid.set_scrollback(0);
        let bytes = encode_paste(&text, pane.grid.has_bracketed_paste());
        pane.pty.write(&bytes);
    }
}

fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    if !bracketed {
        return text.as_bytes().to_vec();
    }

    let mut bytes = Vec::with_capacity(text.len() + 12);
    bytes.extend_from_slice(b"\x1b[200~");
    bytes.extend_from_slice(text.as_bytes());
    bytes.extend_from_slice(b"\x1b[201~");
    bytes
}

fn is_selection_copy_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('c' | 'C'))
        && matches!(key.modifiers, KeyModifiers::CONTROL | KeyModifiers::SUPER)
}

fn is_selection_cut_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('x' | 'X'))
        && matches!(key.modifiers, KeyModifiers::CONTROL | KeyModifiers::SUPER)
}

fn is_selection_delete_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Backspace | KeyCode::Delete) && key.modifiers == KeyModifiers::NONE
}

struct InputSelectionEdit {
    replacement: String,
    removed: String,
    cursor: usize,
}

/// Xóa selection khỏi buffer ZLE hiện tại. Selection ở output/prompt hoặc khi
/// command đang chạy không được coi là editable và phím sẽ tiếp tục xuống PTY.
fn edit_input_selection(app: &mut App, cut: bool) -> bool {
    let Some(selection) = app.selection else {
        return false;
    };
    let focus = active_focus(app);
    if selection.pane != focus {
        return false;
    }
    let edit = app
        .panes
        .get(&focus)
        .and_then(|pane| input_selection_edit(pane, selection.range));
    let Some(edit) = edit else {
        return false;
    };
    if cut && !write_clipboard(edit.removed.as_bytes()) {
        return false;
    }

    let Ok(Some(bytes)) = app
        .integ
        .prepare_zle_edit(focus.0, &edit.replacement, edit.cursor)
    else {
        return false;
    };
    let Some(pane) = app.panes.get_mut(&focus) else {
        return false;
    };
    pane.pty.write(&bytes);
    app.selection = None;
    app.suggest = None;
    app.mention = None;
    true
}

fn input_selection_edit(pane: &Pane, selection: GridSelection) -> Option<InputSelectionEdit> {
    if pane.pending.is_some()
        || pane.grid.scrollback() != 0
        || pane.grid.screen().hide_cursor()
        || pane.input.buffer.is_empty()
        || pane.input.buffer.contains(['\n', '\r', '\t'])
    {
        return None;
    }

    let char_range = input_selection_char_range(
        &pane.input.buffer,
        pane.input.cursor,
        pane.grid.screen().cursor_position(),
        pane.grid.screen().size().1,
        selection,
    )?;
    let start_byte = char_byte_offset(&pane.input.buffer, char_range.start)?;
    let end_byte = char_byte_offset(&pane.input.buffer, char_range.end)?;
    let removed = pane.input.buffer.get(start_byte..end_byte)?.to_string();
    if removed.is_empty() {
        return None;
    }
    let replacement = format!(
        "{}{}",
        pane.input.buffer.get(..start_byte)?,
        pane.input.buffer.get(end_byte..)?
    );
    Some(InputSelectionEdit {
        replacement,
        removed,
        cursor: char_range.start,
    })
}

/// Đổi selection cell-inclusive thành khoảng ký tự ZLE khi điểm bắt đầu nằm
/// trong buffer đang hiển thị. Biên được ghim theo grapheme.
fn input_selection_char_range(
    buffer: &str,
    current_cursor: usize,
    cursor: (u16, u16),
    cols: u16,
    selection: GridSelection,
) -> Option<Range<usize>> {
    if cols == 0 || buffer.contains(['\n', '\r', '\t']) {
        return None;
    }
    let chars: Vec<char> = buffer.chars().collect();
    let mut boundaries = vec![(0_usize, 0_i64)];
    let mut char_index = 0;
    let mut cells = 0_i64;
    while char_index < chars.len() {
        let (consumed, width) = unicode::grapheme_width(&chars[char_index..]);
        if consumed == 0 {
            return None;
        }
        char_index += consumed;
        cells += i64::from(width);
        boundaries.push((char_index, cells));
    }
    let cursor_offset = boundaries
        .iter()
        .find_map(|(index, offset)| (*index == current_cursor).then_some(*offset))?;
    let cols = i64::from(cols);
    let buffer_start = i64::from(cursor.0) * cols + i64::from(cursor.1) - cursor_offset;
    let buffer_end = buffer_start + cells;
    let (start, end) = if selection.anchor <= selection.end {
        (selection.anchor, selection.end)
    } else {
        (selection.end, selection.anchor)
    };
    let selected_start = i64::from(start.row) * cols + i64::from(start.col);
    let selected_end = i64::from(end.row) * cols + i64::from(end.col) + 1;
    // Điểm bắt đầu phải nằm trong input. Cho phép kéo quá ký tự cuối sang
    // vùng trống cùng phía và ghim endpoint về cuối buffer như terminal thường.
    if selected_start < buffer_start || selected_start >= buffer_end {
        return None;
    }
    let selected_end = selected_end.min(buffer_end);
    if selected_start >= selected_end {
        return None;
    }
    let relative_start = selected_start - buffer_start;
    let relative_end = selected_end - buffer_start;
    let start_index = boundaries
        .iter()
        .rev()
        .find_map(|(index, offset)| (*offset <= relative_start).then_some(*index))?;
    let end_index = boundaries
        .iter()
        .find_map(|(index, offset)| (*offset >= relative_end).then_some(*index))?;
    (start_index < end_index).then_some(start_index..end_index)
}

fn char_byte_offset(value: &str, char_index: usize) -> Option<usize> {
    if char_index == value.chars().count() {
        Some(value.len())
    } else {
        value
            .char_indices()
            .nth(char_index)
            .map(|(offset, _)| offset)
    }
}

pub(crate) fn write_clipboard(bytes: &[u8]) -> bool {
    write_clipboard_to(bytes, ClipboardTarget::Standard)
}

pub(crate) fn write_clipboard_to(bytes: &[u8], target: ClipboardTarget) -> bool {
    #[cfg(not(target_os = "linux"))]
    let _ = target;
    #[cfg(target_os = "macos")]
    let commands: &[(&str, &[&str])] = &[("pbcopy", &[])];
    #[cfg(target_os = "linux")]
    let commands: &[(&str, &[&str])] = match target {
        ClipboardTarget::Standard => &[
            ("wl-copy", &[]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
        ],
        ClipboardTarget::Primary => &[
            ("wl-copy", &["--primary"]),
            ("xclip", &["-selection", "primary"]),
            ("xsel", &["--primary", "--input"]),
        ],
    };
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
    // Popup mention: hover chọn, click chấp nhận, cuộn wheel.
    if let Some(rect) = app.mention_rect {
        let inner = Block::bordered().inner(rect);
        let mv = app.cfg.suggest_max_visible;
        if within(inner, col, row) {
            match me.kind {
                MouseEventKind::ScrollDown => {
                    if let Some(mention) = &mut app.mention {
                        mention.selected =
                            (mention.selected + 1).min(mention.matches.len().saturating_sub(1));
                        mention_scroll_to_selected(mention, mv);
                    }
                    return;
                }
                MouseEventKind::ScrollUp => {
                    if let Some(mention) = &mut app.mention {
                        mention.selected = mention.selected.saturating_sub(1);
                        mention_scroll_to_selected(mention, mv);
                    }
                    return;
                }
                MouseEventKind::Moved | MouseEventKind::Drag(_) | MouseEventKind::Down(_) => {
                    if let Some(mention) = &mut app.mention {
                        let idx = mention.offset + (row - inner.y) as usize;
                        if idx < mention.matches.len() {
                            mention.selected = idx;
                        }
                    }
                    if matches!(me.kind, MouseEventKind::Down(_)) {
                        mention_accept(app);
                    }
                    return;
                }
                _ => {}
            }
        }
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

#[cfg(test)]
mod clipboard_tests {
    use super::*;

    fn key(modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(KeyCode::Char('c'), modifiers)
    }

    #[test]
    fn paste_payload_follows_inner_terminal_mode() {
        assert_eq!(encode_paste("một\nhai", false), "một\nhai".as_bytes());
        assert_eq!(
            encode_paste("một\nhai", true),
            b"\x1b[200~m\xe1\xbb\x99t\nhai\x1b[201~"
        );
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

    #[test]
    fn selection_cut_and_delete_keys_are_exact() {
        assert!(is_selection_cut_key(KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::CONTROL
        )));
        assert!(is_selection_cut_key(KeyEvent::new(
            KeyCode::Char('X'),
            KeyModifiers::SUPER
        )));
        assert!(is_selection_delete_key(KeyEvent::new(
            KeyCode::Backspace,
            KeyModifiers::NONE
        )));
        assert!(is_selection_delete_key(KeyEvent::new(
            KeyCode::Delete,
            KeyModifiers::NONE
        )));
        assert!(!is_selection_delete_key(KeyEvent::new(
            KeyCode::Delete,
            KeyModifiers::SHIFT
        )));
    }

    #[test]
    fn maps_selection_inside_zle_buffer_to_character_range() {
        assert_eq!(
            input_selection_char_range(
                "abcdef",
                6,
                (0, 8),
                10,
                GridSelection {
                    anchor: GridPoint { row: 0, col: 3 },
                    end: GridPoint { row: 0, col: 4 },
                },
            ),
            Some(1..3)
        );
        assert_eq!(
            input_selection_char_range(
                "abcdef",
                6,
                (0, 8),
                10,
                GridSelection {
                    anchor: GridPoint { row: 0, col: 4 },
                    end: GridPoint { row: 0, col: 3 },
                },
            ),
            Some(1..3)
        );
    }

    #[test]
    fn maps_wrapped_and_wide_input_but_rejects_prompt_cells() {
        assert_eq!(
            input_selection_char_range(
                "abcdefgh",
                8,
                (2, 3),
                5,
                GridSelection {
                    anchor: GridPoint { row: 1, col: 3 },
                    end: GridPoint { row: 2, col: 1 },
                },
            ),
            Some(3..7)
        );
        assert_eq!(
            input_selection_char_range(
                "a界b",
                3,
                (0, 6),
                10,
                GridSelection {
                    anchor: GridPoint { row: 0, col: 4 },
                    end: GridPoint { row: 0, col: 4 },
                },
            ),
            Some(1..2)
        );
        assert_eq!(
            input_selection_char_range(
                "abcdef",
                6,
                (0, 8),
                10,
                GridSelection {
                    anchor: GridPoint { row: 0, col: 6 },
                    end: GridPoint { row: 0, col: 9 },
                },
            ),
            Some(4..6)
        );
        assert_eq!(
            input_selection_char_range(
                "abcdef",
                6,
                (0, 8),
                10,
                GridSelection {
                    anchor: GridPoint { row: 0, col: 1 },
                    end: GridPoint { row: 0, col: 3 },
                },
            ),
            None
        );
    }
}
