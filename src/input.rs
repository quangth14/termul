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
use crate::term::{ClipboardTarget, GridPoint, GridSelection, TermScreen};

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
        || pane.input.buffer.contains(['\r', '\t'])
    {
        return None;
    }

    let screen = pane.grid.screen();
    let cols = screen.size().1;
    let (char_range, cell_range) = input_selection_char_range(
        &pane.input.buffer,
        pane.input.cursor,
        screen.cursor_position(),
        cols,
        selection,
        |line, row_end| locate_first_input_line(screen, line, row_end),
    )?;
    let start_byte = char_byte_offset(&pane.input.buffer, char_range.start)?;
    let end_byte = char_byte_offset(&pane.input.buffer, char_range.end)?;
    let removed = pane.input.buffer.get(start_byte..end_byte)?.to_string();
    if removed.is_empty() || cell_range.start < 0 {
        return None;
    }
    // Đối chiếu text đang hiển thị trên các ô sẽ bị xóa với buffer: vị trí các
    // dòng là suy luận (prompt, RPROMPT, dòng đầu) nên lệch thì thà không xóa.
    let shown = screen.selected_text(GridSelection {
        anchor: cell_point(cell_range.start, cols),
        end: cell_point(cell_range.end - 1, cols),
    });
    if strip_whitespace(&shown) != strip_whitespace(&removed) {
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

fn cell_point(cell: i64, cols: u16) -> GridPoint {
    GridPoint {
        row: (cell / i64::from(cols)) as u16,
        col: (cell % i64::from(cols)) as u16,
    }
}

fn strip_whitespace(value: &str) -> String {
    value.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Biên grapheme của một dòng: (số ký tự đã đi qua, số ô đã chiếm), bắt đầu (0, 0).
fn grapheme_boundaries(chars: &[char]) -> Option<Vec<(usize, i64)>> {
    let mut boundaries = vec![(0, 0_i64)];
    let mut index = 0;
    let mut cells = 0_i64;
    while index < chars.len() {
        let (consumed, width) = unicode::grapheme_width(&chars[index..]);
        if consumed == 0 {
            return None;
        }
        index += consumed;
        cells += i64::from(width);
        boundaries.push((index, cells));
    }
    Some(boundaries)
}

/// Bố cục buffer ZLE trên màn hình: biên grapheme (chỉ số ký tự, ô linear
/// `row * cols + col`) tăng dần của các dòng đã xác định được vị trí. Buffer có
/// thể nhiều dòng (`\n`): dòng chứa con trỏ neo theo vị trí con trỏ; các dòng
/// sau bắt đầu ở cột 0 của hàng kế (zsh xuống hàng "eager": dòng vừa đầy hàng
/// thì `\n` tạo thêm một hàng trống); các dòng trước suy ngược từ số hàng chiếm;
/// riêng dòng đầu nằm sau prompt nên nhờ `locate_first(text, ô đầu hàng kế sau
/// dòng đầu)` tìm trên màn hình — không tìm được thì bỏ dòng đầu khỏi bố cục.
fn input_layout(
    buffer: &str,
    current_cursor: usize,
    cursor: (u16, u16),
    cols: u16,
    locate_first: impl FnOnce(&str, i64) -> Option<i64>,
) -> Option<Vec<(usize, i64)>> {
    if cols == 0 || buffer.contains(['\r', '\t']) {
        return None;
    }
    let cols = i64::from(cols);
    // Mỗi dòng logic: (text, chỉ số ký tự bắt đầu, biên grapheme tính từ đầu dòng).
    let mut lines = Vec::new();
    let mut char_start = 0;
    for line in buffer.split('\n') {
        let chars: Vec<char> = line.chars().collect();
        lines.push((line, char_start, grapheme_boundaries(&chars)?));
        char_start += chars.len() + 1;
    }
    let width = |line: usize| width_cells(&lines[line].2);
    let cursor_line = lines
        .iter()
        .position(|(_, start, boundaries)| current_cursor <= start + width_chars(boundaries))?;
    let cursor_offset = lines[cursor_line].2.iter().find_map(|(index, offset)| {
        (lines[cursor_line].1 + index == current_cursor).then_some(*offset)
    })?;

    let mut starts = vec![None; lines.len()];
    starts[cursor_line] = Some(i64::from(cursor.0) * cols + i64::from(cursor.1) - cursor_offset);
    for line in cursor_line + 1..lines.len() {
        let end = starts[line - 1]? + width(line - 1);
        starts[line] = Some((end / cols + 1) * cols);
    }
    for line in (1..cursor_line).rev() {
        let next_row = starts[line + 1]? / cols;
        starts[line] = Some((next_row - 1 - width(line) / cols) * cols);
    }
    if cursor_line > 0 {
        starts[0] = locate_first(lines[0].0, starts[1]?);
    }

    Some(
        lines
            .iter()
            .zip(&starts)
            .filter_map(|((_, char_start, boundaries), start)| {
                start.map(|start| {
                    boundaries
                        .iter()
                        .map(move |(index, offset)| (char_start + index, start + offset))
                })
            })
            .flatten()
            .collect(),
    )
}

/// Đổi selection cell-inclusive thành khoảng ký tự ZLE cùng khoảng ô linear
/// tương ứng theo bố cục `input_layout`. Biên được ghim theo grapheme.
fn input_selection_char_range(
    buffer: &str,
    current_cursor: usize,
    cursor: (u16, u16),
    cols: u16,
    selection: GridSelection,
    locate_first: impl FnOnce(&str, i64) -> Option<i64>,
) -> Option<(Range<usize>, Range<i64>)> {
    let boundaries = input_layout(buffer, current_cursor, cursor, cols, locate_first)?;
    let cols = i64::from(cols);
    let buffer_start = boundaries.first()?.1;
    let buffer_end = boundaries.last()?.1;
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
    let (start_index, start_cell) = *boundaries
        .iter()
        .rev()
        .find(|(_, cell)| *cell <= selected_start)?;
    let (end_index, end_cell) = *boundaries.iter().find(|(_, cell)| *cell >= selected_end)?;
    (start_index < end_index).then_some((start_index..end_index, start_cell..end_cell))
}

fn width_chars(boundaries: &[(usize, i64)]) -> usize {
    boundaries.last().map_or(0, |last| last.0)
}

/// Tìm ô linear bắt đầu dòng đầu của buffer trên màn hình. Dòng đầu kết thúc
/// trong hàng ngay trước `row_end` (ô đầu hàng kế); thử từng vị trí khả dĩ, khớp
/// text từng ô và ô ngay sau phải trống (để không bắt nhầm text trong prompt).
fn locate_first_input_line(screen: &TermScreen, line: &str, row_end: i64) -> Option<i64> {
    let cols = i64::from(screen.size().1);
    let chars: Vec<char> = line.chars().collect();
    let boundaries = grapheme_boundaries(&chars)?;
    let graphemes: Vec<(String, i64)> = boundaries
        .windows(2)
        .map(|pair| {
            (
                chars[pair[0].0..pair[1].0].iter().collect(),
                pair[1].1 - pair[0].1,
            )
        })
        .collect();
    let width = width_cells(&boundaries);
    if width == 0 {
        return None;
    }
    let cell_text = |cell: i64| {
        (cell >= 0)
            .then(|| screen.cell_text((cell / cols) as u16, (cell % cols) as u16))
            .flatten()
    };
    (row_end - cols - width..row_end - width).find(|&start| {
        let mut cell = start;
        let matched = graphemes.iter().all(|(text, width)| {
            let matched = cell_text(cell) == Some(text.as_str());
            cell += width;
            matched
        });
        matched && cell_text(cell).is_none_or(|text| text.trim().is_empty())
    })
}

fn width_cells(boundaries: &[(usize, i64)]) -> i64 {
    boundaries.last().map_or(0, |last| last.1)
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
                let point = pane_point(app, selection.pane, col, row, true);
                let range = point.map(|end| crate::term::GridSelection {
                    end,
                    ..selection.range
                });

                if let Some(range) = range
                    && range.anchor == range.end
                    && !me.modifiers.contains(KeyModifiers::SHIFT)
                    && move_input_cursor_to(app, selection.pane, range.end)
                {
                    app.selection = None;
                    return;
                }

                if let Some(range) = range {
                    app.selection = Some(PaneSelection { range, ..selection });
                    if let Some(pane) = app.panes.get(&selection.pane)
                        && pane.grid.screen().selected_text(range).trim().is_empty()
                    {
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

/// Di chuyển con trỏ ZLE tới ô được click bằng một lần cập nhật nguyên tử.
/// Chỉ hoạt động khi shell đang chờ input và viewport ở đáy.
#[allow(dead_code)]
fn move_input_cursor_to(app: &mut App, pid: PaneId, click: GridPoint) -> bool {
    let Some(pane) = app.panes.get(&pid) else {
        return false;
    };
    if pane.pending.is_some() || pane.grid.scrollback() != 0 || pane.grid.screen().hide_cursor() {
        return false;
    }

    let screen = pane.grid.screen();
    let Some(target) = click_cursor_index(
        &pane.input.buffer,
        pane.input.cursor,
        screen.cursor_position(),
        click,
        screen.size().1,
        |line, row_end| locate_first_input_line(screen, line, row_end),
    ) else {
        return false;
    };
    let current = pane.input.cursor.min(pane.input.buffer.chars().count());
    if target == current {
        return true;
    }
    let buffer = pane.input.buffer.clone();

    let Ok(Some(bytes)) = app.integ.prepare_zle_edit(pid.0, &buffer, target) else {
        return false;
    };
    let Some(pane) = app.panes.get_mut(&pid) else {
        return false;
    };
    pane.pty.write(&bytes);
    true
}

/// Ánh xạ ô click vào biên grapheme gần nhất trên cùng hàng theo bố cục
/// `input_layout`. Biên ở cột 0 hàng kế cũng tính cho hàng này khi chỉ là wrap
/// (không phải đầu dòng sau `\n`), để click cuối hàng đầy đặt con trỏ sau ký tự cuối.
fn click_cursor_index(
    buffer: &str,
    current_index: usize,
    cursor: (u16, u16),
    click: GridPoint,
    cols: u16,
    locate_first: impl FnOnce(&str, i64) -> Option<i64>,
) -> Option<usize> {
    if buffer.is_empty() {
        return None;
    }
    let boundaries = input_layout(buffer, current_index, cursor, cols, locate_first)?;
    let chars: Vec<char> = buffer.chars().collect();
    let cols = i64::from(cols);
    let click_row = i64::from(click.row);
    // So sánh theo nửa ô để click trên ký tự wide chọn đúng nửa trái/phải.
    let click_center = (click_row * cols + i64::from(click.col)) * 2 + 1;
    boundaries
        .iter()
        .filter(|(index, cell)| {
            cell.div_euclid(cols) == click_row
                || (*cell == (click_row + 1) * cols && *index > 0 && chars[index - 1] != '\n')
        })
        .min_by_key(|(_, cell)| (cell * 2 - click_center).abs())
        .map(|(index, _)| *index)
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

    fn single_line(
        buffer: &str,
        cursor: usize,
        cursor_pos: (u16, u16),
        cols: u16,
        selection: GridSelection,
    ) -> Option<Range<usize>> {
        input_selection_char_range(buffer, cursor, cursor_pos, cols, selection, |_, _| None)
            .map(|(chars, _)| chars)
    }

    fn sel(anchor: (u16, u16), end: (u16, u16)) -> GridSelection {
        GridSelection {
            anchor: GridPoint {
                row: anchor.0,
                col: anchor.1,
            },
            end: GridPoint {
                row: end.0,
                col: end.1,
            },
        }
    }

    #[test]
    fn maps_selection_inside_zle_buffer_to_character_range() {
        assert_eq!(
            single_line("abcdef", 6, (0, 8), 10, sel((0, 3), (0, 4))),
            Some(1..3)
        );
        assert_eq!(
            single_line("abcdef", 6, (0, 8), 10, sel((0, 4), (0, 3))),
            Some(1..3)
        );
    }

    #[test]
    fn maps_wrapped_and_wide_input_but_rejects_prompt_cells() {
        assert_eq!(
            single_line("abcdefgh", 8, (2, 3), 5, sel((1, 3), (2, 1))),
            Some(3..7)
        );
        assert_eq!(
            single_line("a界b", 3, (0, 6), 10, sel((0, 4), (0, 4))),
            Some(1..2)
        );
        assert_eq!(
            single_line("abcdef", 6, (0, 8), 10, sel((0, 6), (0, 9))),
            Some(4..6)
        );
        assert_eq!(
            single_line("abcdef", 6, (0, 8), 10, sel((0, 1), (0, 3))),
            None
        );
    }

    #[test]
    fn maps_multiline_buffer_using_cursor_anchor_and_first_line_locator() {
        // "$ ab \" / "  cd \" / "  ef" — con trỏ cuối buffer, cols = 10.
        let buffer = "ab \\\n  cd \\\n  ef";
        let locate = |line: &str, row_end: i64| {
            assert_eq!((line, row_end), ("ab \\", 10));
            Some(2)
        };
        assert_eq!(
            input_selection_char_range(buffer, 16, (2, 4), 10, sel((0, 2), (2, 3)), locate),
            Some((0..16, 2..24))
        );
        // Kéo hết hàng giữa (kể cả ô trống) → xoá cả dòng lẫn `\n` của nó.
        assert_eq!(
            input_selection_char_range(buffer, 16, (2, 4), 10, sel((1, 0), (1, 9)), locate),
            Some((5..12, 10..20))
        );
        // Bắt đầu từ prompt → không thuộc buffer.
        assert_eq!(
            input_selection_char_range(buffer, 16, (2, 4), 10, sel((0, 0), (2, 3)), locate),
            None
        );
        // Không tìm được dòng đầu: chỉ các dòng sau còn chọn được.
        assert_eq!(
            input_selection_char_range(buffer, 16, (2, 4), 10, sel((0, 2), (2, 3)), |_, _| None),
            None
        );
        assert_eq!(
            input_selection_char_range(buffer, 16, (2, 4), 10, sel((1, 2), (2, 3)), |_, _| None),
            Some((7..16, 12..24))
        );
        // Con trỏ ở dòng đầu: các dòng sau suy xuôi, không cần locator.
        assert_eq!(
            input_selection_char_range(buffer, 4, (0, 6), 10, sel((0, 2), (2, 3)), |_, _| {
                panic!("không cần locator")
            }),
            Some((0..16, 2..24))
        );
    }

    #[test]
    fn multiline_layout_follows_zsh_eager_wrap() {
        // Dòng đầu vừa đầy hàng (2 + 8 = 10 ô) → `\n` tạo một hàng trống, "x" ở hàng 2.
        assert_eq!(
            input_selection_char_range(
                "abcdefgh\nx",
                10,
                (2, 1),
                10,
                sel((0, 2), (2, 0)),
                |_, _| Some(2)
            ),
            Some((0..10, 2..21))
        );
        // Suy ngược: dòng giữa dài đúng 10 ô chiếm hàng 1, hàng 2 trống, "c" ở hàng 3.
        let buffer = "a\nbbbbbbbbbb\nc";
        let locate = |_: &str, row_end: i64| {
            assert_eq!(row_end, 10);
            Some(2)
        };
        assert_eq!(
            input_selection_char_range(buffer, 14, (3, 1), 10, sel((1, 0), (1, 9)), locate),
            Some((2..12, 10..20))
        );
        // Kéo qua cả hàng trống → xoá luôn `\n` của dòng đó.
        assert_eq!(
            input_selection_char_range(buffer, 14, (3, 1), 10, sel((1, 0), (2, 0)), locate),
            Some((2..13, 10..30))
        );
    }

    #[test]
    fn click_maps_to_nearest_boundary_on_same_row_across_lines() {
        // "$ ab \" / "  cd \" / "  ef" — con trỏ cuối buffer, cols = 10.
        let buffer = "ab \\\n  cd \\\n  ef";
        let locate = |_: &str, _: i64| Some(2);
        let click = |row, col| GridPoint { row, col };
        assert_eq!(
            click_cursor_index(buffer, 16, (2, 4), click(1, 3), 10, locate),
            Some(8)
        );
        // Click vào vùng trống sau dòng → cuối dòng đó (trước `\n`), không nhảy sang dòng sau.
        assert_eq!(
            click_cursor_index(buffer, 16, (2, 4), click(1, 9), 10, locate),
            Some(11)
        );
        assert_eq!(
            click_cursor_index(buffer, 16, (2, 4), click(0, 0), 10, locate),
            Some(0)
        );
        assert_eq!(
            click_cursor_index(buffer, 16, (2, 4), click(3, 0), 10, locate),
            None
        );
        // Không tìm được dòng đầu → click dòng đầu bị từ chối, dòng sau vẫn được.
        assert_eq!(
            click_cursor_index(buffer, 16, (2, 4), click(0, 3), 10, |_, _| None),
            None
        );
        assert_eq!(
            click_cursor_index(buffer, 16, (2, 4), click(2, 2), 10, |_, _| None),
            Some(14)
        );
        // Dòng wrap: click cuối hàng đầy → sau ký tự cuối của hàng (ở cột 0 hàng kế).
        assert_eq!(
            click_cursor_index("abcdefgh", 8, (2, 3), click(1, 4), 5, |_, _| None),
            Some(4)
        );
        assert_eq!(
            click_cursor_index("a界b", 3, (0, 6), click(0, 4), 10, |_, _| None),
            Some(2)
        );
    }

    #[test]
    fn locates_first_input_line_on_screen_after_prompt() {
        let mut grid = crate::term::TermGrid::new(4, 12, 0);
        grid.process(b"$ ab \\\r\n  cd");
        assert_eq!(locate_first_input_line(grid.screen(), "ab \\", 12), Some(2));
        assert_eq!(locate_first_input_line(grid.screen(), "zz", 12), None);
        assert_eq!(locate_first_input_line(grid.screen(), "", 12), None);

        // Prompt chứa cùng text: bỏ qua vì ô kế tiếp không trống; RPROMPT bên phải bị bỏ qua.
        let mut grid = crate::term::TermGrid::new(4, 12, 0);
        grid.process(b"ab$ ab    ab\r\ncd");
        assert_eq!(locate_first_input_line(grid.screen(), "ab", 12), Some(4));
    }
}
