//! Bọc vt100::Parser và render lưới ô của nó vào buffer của ratatui.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Widget;

/// Trạng thái emulator terminal cho một pane (Phase 0: chỉ một pane).
pub struct TermGrid {
    parser: vt100::Parser,
    query_tail: Vec<u8>,
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
        Self {
            parser: vt100::Parser::new(rows, cols, 1000),
            query_tail: Vec::new(),
        }
    }

    pub fn process(&mut self, bytes: &[u8]) -> TerminalQueries {
        // vt100 hiểu OSC để cập nhật state nhưng không tự trả lời truy vấn màu.
        // Giữ một tail ngắn để vẫn nhận ra sequence bị chia giữa hai lần đọc PTY.
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
        self.parser.process(bytes);
        queries
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        if rows > 0 && cols > 0 {
            self.parser.screen_mut().set_size(rows, cols);
        }
    }

    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    /// Vị trí scrollback hiện tại (0 = đang ở đáy, xem nội dung mới nhất).
    pub fn scrollback(&self) -> usize {
        self.parser.screen().scrollback()
    }

    /// Đặt vị trí scrollback (số dòng tính từ đáy); vt100 tự kẹp về giới hạn.
    pub fn set_scrollback(&mut self, rows: usize) {
        self.parser.screen_mut().set_scrollback(rows);
    }

    /// App trong pane có đang dùng alternate screen (vim/less/…) hay không.
    pub fn in_alt_screen(&self) -> bool {
        self.parser.screen().alternate_screen()
    }
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|part| part == needle)
}

/// Mã hoá câu trả lời OSC 10/11 theo dạng RGB 16-bit mà xterm quy ước.
pub fn palette_reply(slot: u8, (r, g, b): (u8, u8, u8)) -> Vec<u8> {
    format!("\x1b]{slot};rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}\x1b\\")
        .into_bytes()
}

/// Phản hồi DSR vị trí cursor, dùng toạ độ 1-based theo chuẩn terminal.
pub fn cursor_position_reply((row, col): (u16, u16)) -> Vec<u8> {
    format!("\x1b[{};{}R", row.saturating_add(1), col.saturating_add(1)).into_bytes()
}

/// Chuyển vt100::Color → ratatui::Color.
fn convert_color(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

/// Widget vẽ nội dung một `vt100::Screen` vào một `Rect`.
pub struct TermView<'a> {
    pub screen: &'a vt100::Screen,
}

impl Widget for TermView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let (rows, cols) = self.screen.size();
        let max_rows = rows.min(area.height);
        let max_cols = cols.min(area.width);

        for row in 0..max_rows {
            for col in 0..max_cols {
                let Some(cell) = self.screen.cell(row, col) else {
                    continue;
                };
                // Ô nối tiếp của ký tự rộng (CJK): bỏ qua, đã vẽ ở ô trước.
                if cell.is_wide_continuation() {
                    continue;
                }

                let x = area.x + col;
                let y = area.y + row;
                let is_wide = cell.is_wide();

                if let Some(buf_cell) = buf.cell_mut((x, y)) {
                    let contents = cell.contents();
                    if contents.is_empty() {
                        buf_cell.set_symbol(" ");
                    } else {
                        buf_cell.set_symbol(contents);
                    }

                    let mut fg = convert_color(cell.fgcolor());
                    let mut bg = convert_color(cell.bgcolor());
                    if cell.inverse() {
                        std::mem::swap(&mut fg, &mut bg);
                    }
                    let mut style = Style::default().fg(fg).bg(bg);
                    if cell.bold() {
                        style = style.add_modifier(Modifier::BOLD);
                    }
                    if cell.dim() {
                        style = style.add_modifier(Modifier::DIM);
                    }
                    if cell.italic() {
                        style = style.add_modifier(Modifier::ITALIC);
                    }
                    if cell.underline() {
                        style = style.add_modifier(Modifier::UNDERLINED);
                    }
                    buf_cell.set_style(style);
                }

                // Ký tự rộng chiếm 2 ô: đánh dấu ô kế bên là skip để không lệch.
                // (borrow của buf_cell ở trên đã kết thúc nên mượn lại buf an toàn.)
                if is_wide
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
        }
        .render(area, &mut buf);

        let cell = &buf[(0, 0)];
        assert_eq!(cell.fg, Color::Rgb(12, 34, 56));
        assert!(cell.modifier.contains(Modifier::DIM));
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
}
