//! Bọc vt100::Parser và render lưới ô của nó vào buffer của ratatui.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Widget;

/// Trạng thái emulator terminal cho một pane (Phase 0: chỉ một pane).
pub struct TermGrid {
    parser: vt100::Parser,
}

impl TermGrid {
    pub fn new(rows: u16, cols: u16) -> Self {
        Self {
            parser: vt100::Parser::new(rows, cols, 1000),
        }
    }

    pub fn process(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        if rows > 0 && cols > 0 {
            self.parser.screen_mut().set_size(rows, cols);
        }
    }

    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }
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
