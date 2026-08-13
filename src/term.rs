//! Backend terminal dựa trên `libghostty-vt`, cùng hướng triển khai với Herdr.

use libghostty_vt::render::{CellIterator, RenderState, RowIterator};
use libghostty_vt::screen::{CellWide, Screen};
use libghostty_vt::style::{RgbColor, Underline};
use libghostty_vt::terminal::ScrollViewport;
use libghostty_vt::{Terminal, TerminalOptions};
use libghostty_vt::{key, mouse};
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Widget;

const MAX_SCROLLBACK_LINES: usize = 100_000;

/// Toạ độ một ô trong viewport terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct GridPoint {
    pub row: u16,
    pub col: u16,
}

/// Vùng chọn tuyến tính, tính cả hai đầu mút.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridSelection {
    pub anchor: GridPoint,
    pub end: GridPoint,
}

impl GridSelection {
    pub fn new(point: GridPoint) -> Self {
        Self {
            anchor: point,
            end: point,
        }
    }

    fn contains(self, point: GridPoint) -> bool {
        let (start, end) = if self.anchor <= self.end {
            (self.anchor, self.end)
        } else {
            (self.end, self.anchor)
        };
        point >= start && point <= end
    }
}

/// Trạng thái emulator terminal cho một pane.
pub struct TermGrid {
    terminal: Terminal<'static, 'static>,
    render_state: RenderState<'static>,
    row_iterator: RowIterator<'static>,
    cell_iterator: CellIterator<'static>,
    mouse_encoder: mouse::Encoder<'static>,
    mouse_event: mouse::Event<'static>,
    screen: TermScreen,
    query_tail: Vec<u8>,
}

#[derive(Clone, Default)]
struct TermCell {
    contents: String,
    fg: Option<RgbColor>,
    bg: Option<RgbColor>,
    bold: bool,
    faint: bool,
    italic: bool,
    underline: bool,
    inverse: bool,
    wide: CellWideKind,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum CellWideKind {
    #[default]
    Narrow,
    Wide,
    Spacer,
}

/// Snapshot nhẹ của viewport Ghostty để UI chỉ cần mượn bất biến khi render.
pub struct TermScreen {
    rows: u16,
    cols: u16,
    cells: Vec<TermCell>,
    cursor: (u16, u16),
    hide_cursor: bool,
    #[cfg_attr(not(test), allow(dead_code))]
    contents: String,
}

/// Các truy vấn capability mà app khách gửi lúc khởi động.
#[derive(Default)]
pub struct TerminalQueries {
    pub foreground: bool,
    pub background: bool,
    pub cursor_position: bool,
    pub device_attributes: bool,
    pub keyboard_flags: bool,
}

impl TermGrid {
    pub fn new(rows: u16, cols: u16) -> Self {
        let rows = rows.max(1);
        let cols = cols.max(1);
        let terminal = Terminal::new(TerminalOptions {
            cols,
            rows,
            max_scrollback: MAX_SCROLLBACK_LINES,
        })
        .expect("không thể tạo libghostty-vt terminal");
        let mut grid = Self {
            terminal,
            render_state: RenderState::new().expect("không thể tạo Ghostty render state"),
            row_iterator: RowIterator::new().expect("không thể tạo Ghostty row iterator"),
            cell_iterator: CellIterator::new().expect("không thể tạo Ghostty cell iterator"),
            mouse_encoder: mouse::Encoder::new().expect("không thể tạo Ghostty mouse encoder"),
            mouse_event: mouse::Event::new().expect("không thể tạo Ghostty mouse event"),
            screen: TermScreen::empty(rows, cols),
            query_tail: Vec::new(),
        };
        grid.refresh();
        grid
    }

    pub fn process(&mut self, bytes: &[u8]) -> TerminalQueries {
        // Giữ compatibility replies hiện có. Ghostty vẫn xử lý toàn bộ VT state;
        // những query cần ghi ngược PTY được event loop trả lời sau hàm này.
        let mut scan = std::mem::take(&mut self.query_tail);
        scan.extend_from_slice(bytes);
        let queries = TerminalQueries {
            foreground: contains_bytes(&scan, b"\x1b]10;?"),
            background: contains_bytes(&scan, b"\x1b]11;?"),
            cursor_position: contains_bytes(&scan, b"\x1b[6n"),
            device_attributes: contains_bytes(&scan, b"\x1b[c"),
            keyboard_flags: contains_bytes(&scan, b"\x1b[?u"),
        };
        self.query_tail = scan[scan.len().saturating_sub(5)..].to_vec();
        self.terminal.vt_write(bytes);
        self.refresh();
        queries
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        if rows == 0 || cols == 0 || self.screen.size() == (rows, cols) {
            return;
        }
        self.terminal
            .resize(cols, rows, 1, 1)
            .expect("libghostty-vt resize thất bại");
        self.refresh();
    }

    pub fn screen(&self) -> &TermScreen {
        &self.screen
    }

    /// Số dòng viewport đang cách đáy scrollback.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn scrollback(&self) -> usize {
        self.terminal
            .scrollbar()
            .map(|bar| (bar.total - bar.len - bar.offset) as usize)
            .unwrap_or(0)
    }

    /// Đặt vị trí viewport theo số dòng tính từ đáy.
    pub fn set_scrollback(&mut self, rows: usize) {
        let Ok(bar) = self.terminal.scrollbar() else {
            return;
        };
        let max_offset = (bar.total - bar.len) as usize;
        self.terminal
            .scroll_viewport(ScrollViewport::Row(max_offset.saturating_sub(rows)));
        self.refresh();
    }

    /// Cuộn viewport; delta âm là lên lịch sử, dương là về nội dung mới.
    pub fn scroll_lines(&mut self, delta: isize) {
        self.terminal.scroll_viewport(ScrollViewport::Delta(delta));
        self.refresh();
    }

    pub fn in_alt_screen(&self) -> bool {
        self.terminal
            .active_screen()
            .is_ok_and(|screen| screen == Screen::Alternate)
    }

    pub fn has_mouse_tracking(&self) -> bool {
        self.terminal.is_mouse_tracking().unwrap_or(false)
    }

    /// Mã hoá mouse event theo đúng tracking mode/format mà app đã bật.
    /// Ghostty tự chọn X10, UTF-8, SGR, urxvt hoặc SGR-pixels từ terminal state.
    pub fn encode_mouse(&mut self, event: MouseEvent, inner: Rect) -> Option<Vec<u8>> {
        if event.column < inner.x
            || event.row < inner.y
            || event.column >= inner.x + inner.width
            || event.row >= inner.y + inner.height
        {
            return None;
        }

        let (action, button) = match event.kind {
            MouseEventKind::Down(button) => (mouse::Action::Press, convert_mouse_button(button)),
            MouseEventKind::Up(button) => (mouse::Action::Release, convert_mouse_button(button)),
            MouseEventKind::Drag(button) => (mouse::Action::Motion, convert_mouse_button(button)),
            MouseEventKind::Moved => (mouse::Action::Motion, None),
            MouseEventKind::ScrollUp => (mouse::Action::Press, Some(mouse::Button::Four)),
            MouseEventKind::ScrollDown => (mouse::Action::Press, Some(mouse::Button::Five)),
            MouseEventKind::ScrollLeft => (mouse::Action::Press, Some(mouse::Button::Six)),
            MouseEventKind::ScrollRight => (mouse::Action::Press, Some(mouse::Button::Seven)),
        };
        let mut mods = key::Mods::empty();
        if event.modifiers.contains(KeyModifiers::SHIFT) {
            mods.insert(key::Mods::SHIFT);
        }
        if event.modifiers.contains(KeyModifiers::ALT) {
            mods.insert(key::Mods::ALT);
        }
        if event.modifiers.contains(KeyModifiers::CONTROL) {
            mods.insert(key::Mods::CTRL);
        }

        self.mouse_event
            .set_action(action)
            .set_button(button)
            .set_mods(mods)
            .set_position(mouse::Position {
                x: f32::from(event.column - inner.x),
                y: f32::from(event.row - inner.y),
            });
        self.mouse_encoder
            .set_options_from_terminal(&self.terminal)
            .set_size(mouse::EncoderSize {
                screen_width: u32::from(inner.width),
                screen_height: u32::from(inner.height),
                cell_width: 1,
                cell_height: 1,
                padding_top: 0,
                padding_bottom: 0,
                padding_right: 0,
                padding_left: 0,
            })
            .set_any_button_pressed(matches!(event.kind, MouseEventKind::Drag(_)));
        let mut bytes = Vec::with_capacity(32);
        self.mouse_encoder
            .encode_to_vec(&self.mouse_event, &mut bytes)
            .ok()?;
        (!bytes.is_empty()).then_some(bytes)
    }

    fn refresh(&mut self) {
        let mut cells = Vec::new();
        let (rows, cols, cursor, hide_cursor, contents) = {
            let snapshot = self
                .render_state
                .update(&self.terminal)
                .expect("không thể cập nhật Ghostty render state");
            let rows = snapshot.rows().unwrap_or(0);
            let cols = snapshot.cols().unwrap_or(0);
            cells.reserve(rows as usize * cols as usize);

            let mut row_iter = self
                .row_iterator
                .update(&snapshot)
                .expect("không thể đọc hàng Ghostty");
            let mut contents = String::new();
            let mut row_index = 0_u16;
            while let Some(row) = row_iter.next() {
                let mut cell_iter = self
                    .cell_iterator
                    .update(row)
                    .expect("không thể đọc cell Ghostty");
                while let Some(cell) = cell_iter.next() {
                    let style = cell.style().unwrap_or_default();
                    let text: String = cell.graphemes().unwrap_or_default().into_iter().collect();
                    if !matches!(
                        cell.raw_cell().and_then(|c| c.wide()),
                        Ok(CellWide::SpacerTail)
                    ) {
                        contents.push_str(&text);
                    }
                    let wide = match cell.raw_cell().and_then(|c| c.wide()) {
                        Ok(CellWide::Wide) => CellWideKind::Wide,
                        Ok(CellWide::SpacerTail | CellWide::SpacerHead) => CellWideKind::Spacer,
                        _ => CellWideKind::Narrow,
                    };
                    cells.push(TermCell {
                        contents: text,
                        fg: cell.fg_color().unwrap_or(None),
                        bg: cell.bg_color().unwrap_or(None),
                        bold: style.bold,
                        faint: style.faint,
                        italic: style.italic,
                        underline: style.underline != Underline::None,
                        inverse: style.inverse,
                        wide,
                    });
                }
                cells.resize_with((row_index as usize + 1) * cols as usize, TermCell::default);
                if row_index + 1 < rows {
                    contents.push('\n');
                }
                row_index += 1;
            }
            cells.resize_with(rows as usize * cols as usize, TermCell::default);
            let cursor = snapshot
                .cursor_viewport()
                .ok()
                .flatten()
                .map(|cursor| (cursor.y, cursor.x))
                .unwrap_or((0, 0));
            let hide_cursor = !snapshot.cursor_visible().unwrap_or(false)
                || snapshot.cursor_viewport().ok().flatten().is_none();
            (rows, cols, cursor, hide_cursor, contents)
        };

        self.screen = TermScreen {
            rows,
            cols,
            cells,
            cursor,
            hide_cursor,
            contents,
        };
    }
}

fn convert_mouse_button(button: MouseButton) -> Option<mouse::Button> {
    Some(match button {
        MouseButton::Left => mouse::Button::Left,
        MouseButton::Right => mouse::Button::Right,
        MouseButton::Middle => mouse::Button::Middle,
    })
}

impl TermScreen {
    fn empty(rows: u16, cols: u16) -> Self {
        Self {
            rows,
            cols,
            cells: vec![TermCell::default(); rows as usize * cols as usize],
            cursor: (0, 0),
            hide_cursor: false,
            contents: String::new(),
        }
    }

    pub fn size(&self) -> (u16, u16) {
        (self.rows, self.cols)
    }

    pub fn cursor_position(&self) -> (u16, u16) {
        self.cursor
    }

    pub fn hide_cursor(&self) -> bool {
        self.hide_cursor
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn contents(&self) -> &str {
        &self.contents
    }

    fn cell(&self, row: u16, col: u16) -> Option<&TermCell> {
        (row < self.rows && col < self.cols)
            .then(|| &self.cells[row as usize * self.cols as usize + col as usize])
    }

    /// Lấy nội dung text của vùng chọn trong viewport hiện tại.
    pub fn selected_text(&self, selection: GridSelection) -> String {
        let (start, end) = if selection.anchor <= selection.end {
            (selection.anchor, selection.end)
        } else {
            (selection.end, selection.anchor)
        };
        let mut lines = Vec::new();
        for row in start.row..=end.row.min(self.rows.saturating_sub(1)) {
            let first_col = if row == start.row { start.col } else { 0 };
            let last_col = if row == end.row {
                end.col
            } else {
                self.cols.saturating_sub(1)
            };
            let mut line = String::new();
            for col in first_col..=last_col.min(self.cols.saturating_sub(1)) {
                let Some(cell) = self.cell(row, col) else {
                    continue;
                };
                if cell.wide == CellWideKind::Spacer {
                    continue;
                }
                if cell.contents.is_empty() {
                    line.push(' ');
                } else {
                    line.push_str(&cell.contents);
                }
            }
            lines.push(line.trim_end().to_string());
        }
        lines.join("\n")
    }
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|part| part == needle)
}

/// Mã hoá câu trả lời OSC 10/11 theo dạng RGB 16-bit mà xterm quy ước.
pub fn palette_reply(slot: u8, (r, g, b): (u8, u8, u8)) -> Vec<u8> {
    format!("\x1b]{slot};rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}\x1b\\").into_bytes()
}

/// Phản hồi DSR vị trí cursor, dùng toạ độ 1-based theo chuẩn terminal.
pub fn cursor_position_reply((row, col): (u16, u16)) -> Vec<u8> {
    format!("\x1b[{};{}R", row.saturating_add(1), col.saturating_add(1)).into_bytes()
}

fn convert_color(color: Option<RgbColor>) -> Color {
    color.map_or(Color::Reset, |c| Color::Rgb(c.r, c.g, c.b))
}

/// Widget vẽ snapshot viewport của Ghostty vào một `Rect`.
pub struct TermView<'a> {
    pub screen: &'a TermScreen,
    pub selection: Option<GridSelection>,
    pub default_bg: Color,
}

fn blend_selection_bg(bg: Color, default_bg: Color) -> Color {
    let Color::Rgb(r, g, b) = bg else {
        return blend_selection_bg(default_bg, Color::Rgb(40, 42, 54));
    };
    let blend = |channel: u8| ((u16::from(channel) * 7 + 128 * 3) / 10) as u8;
    Color::Rgb(blend(r), blend(g), blend(b))
}

impl Widget for TermView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let (rows, cols) = self.screen.size();
        for row in 0..rows.min(area.height) {
            for col in 0..cols.min(area.width) {
                let Some(cell) = self.screen.cell(row, col) else {
                    continue;
                };
                if cell.wide == CellWideKind::Spacer {
                    continue;
                }
                let x = area.x + col;
                let y = area.y + row;
                if let Some(buf_cell) = buf.cell_mut((x, y)) {
                    buf_cell.set_symbol(if cell.contents.is_empty() {
                        " "
                    } else {
                        &cell.contents
                    });
                    let mut fg = convert_color(cell.fg);
                    let mut bg = convert_color(cell.bg);
                    let selected = self
                        .selection
                        .is_some_and(|selection| selection.contains(GridPoint { row, col }));
                    let reverse_defaults = cell.inverse && fg == bg && !selected;
                    if cell.inverse && !reverse_defaults {
                        std::mem::swap(&mut fg, &mut bg);
                    }
                    if selected {
                        bg = blend_selection_bg(bg, self.default_bg);
                    }
                    let mut style = Style::default().fg(fg).bg(bg);
                    if reverse_defaults {
                        style = style.add_modifier(Modifier::REVERSED);
                    }
                    if cell.bold {
                        style = style.add_modifier(Modifier::BOLD);
                    }
                    if cell.faint {
                        style = style.add_modifier(Modifier::DIM);
                    }
                    if cell.italic {
                        style = style.add_modifier(Modifier::ITALIC);
                    }
                    if cell.underline {
                        style = style.add_modifier(Modifier::UNDERLINED);
                    }
                    buf_cell.set_style(style);
                }
                if cell.wide == CellWideKind::Wide
                    && x + 1 < area.x + area.width
                    && let Some(next) = buf.cell_mut((x + 1, y))
                {
                    next.set_symbol("");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_truecolor_and_dim_attributes() {
        let area = Rect::new(0, 0, 8, 1);
        let mut grid = TermGrid::new(1, 8);
        grid.process(b"\x1b[2;38;2;12;34;56mX");
        let mut buf = Buffer::empty(area);
        TermView {
            screen: grid.screen(),
            selection: None,
            default_bg: Color::Rgb(40, 42, 54),
        }
        .render(area, &mut buf);
        let cell = &buf[(0, 0)];
        assert_eq!(cell.fg, Color::Rgb(12, 34, 56));
        assert!(cell.modifier.contains(Modifier::DIM));
    }

    #[test]
    fn selection_contains_points_in_both_drag_directions() {
        let start = GridPoint { row: 1, col: 2 };
        let end = GridPoint { row: 2, col: 1 };
        let forward = GridSelection { anchor: start, end };
        let backward = GridSelection {
            anchor: end,
            end: start,
        };

        for selection in [forward, backward] {
            assert!(selection.contains(start));
            assert!(selection.contains(GridPoint { row: 1, col: 7 }));
            assert!(selection.contains(end));
            assert!(!selection.contains(GridPoint { row: 1, col: 1 }));
            assert!(!selection.contains(GridPoint { row: 2, col: 2 }));
        }
    }

    #[test]
    fn selection_blends_gray_over_rendered_background() {
        let area = Rect::new(0, 0, 2, 1);
        let mut grid = TermGrid::new(1, 2);
        grid.process(b"\x1b[38;2;12;34;56;48;2;60;70;80mXY");
        let mut buf = Buffer::empty(area);
        TermView {
            screen: grid.screen(),
            selection: Some(GridSelection {
                anchor: GridPoint { row: 0, col: 0 },
                end: GridPoint { row: 0, col: 0 },
            }),
            default_bg: Color::Rgb(40, 42, 54),
        }
        .render(area, &mut buf);

        assert_eq!(buf[(0, 0)].fg, Color::Rgb(12, 34, 56));
        assert_eq!(buf[(0, 0)].bg, Color::Rgb(80, 87, 94));
        assert_eq!(buf[(1, 0)].fg, Color::Rgb(12, 34, 56));
        assert_eq!(buf[(1, 0)].bg, Color::Rgb(60, 70, 80));
    }

    #[test]
    fn selection_blends_gray_over_default_terminal_background() {
        let area = Rect::new(0, 0, 1, 1);
        let mut grid = TermGrid::new(1, 1);
        grid.process(b"X");
        let mut buf = Buffer::empty(area);
        TermView {
            screen: grid.screen(),
            selection: Some(GridSelection::new(GridPoint { row: 0, col: 0 })),
            default_bg: Color::Rgb(40, 42, 54),
        }
        .render(area, &mut buf);

        assert_eq!(buf[(0, 0)].bg, Color::Rgb(66, 67, 76));
    }

    #[test]
    fn selected_text_handles_reverse_drag_rows_and_wide_cells() {
        let mut grid = TermGrid::new(2, 8);
        grid.process("ab中 d\r\nef".as_bytes());
        let selection = GridSelection {
            anchor: GridPoint { row: 1, col: 1 },
            end: GridPoint { row: 0, col: 1 },
        };

        assert_eq!(grid.screen().selected_text(selection), "b中 d\nef");
    }

    #[test]
    fn detects_split_palette_queries_and_formats_reply() {
        let mut grid = TermGrid::new(1, 8);
        let first = grid.process(b"\x1b]1");
        assert!(!first.background);
        let second = grid.process(b"1;?\x1b\\");
        assert!(second.background);
        assert_eq!(
            palette_reply(11, (40, 42, 54)),
            b"\x1b]11;rgb:2828/2a2a/3636\x1b\\"
        );
    }

    #[test]
    fn detects_batched_startup_capability_queries() {
        let mut grid = TermGrid::new(24, 80);
        let queries = grid.process(b"\x1b[6n\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b[?u\x1b[c");
        assert!(queries.cursor_position);
        assert!(queries.foreground);
        assert!(queries.background);
        assert!(queries.keyboard_flags);
        assert!(queries.device_attributes);
        assert_eq!(cursor_position_reply((4, 9)), b"\x1b[5;10R");
    }

    #[test]
    fn ghostty_viewport_scrolls_and_returns_to_bottom() {
        let mut grid = TermGrid::new(3, 20);
        grid.process(b"one\r\ntwo\r\nthree\r\nfour\r\nfive");
        grid.scroll_lines(-2);
        assert!(grid.scrollback() > 0);
        assert!(grid.screen().contents().contains("two"));
        grid.set_scrollback(0);
        assert_eq!(grid.scrollback(), 0);
        assert!(grid.screen().contents().contains("five"));
    }
}
