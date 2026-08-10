//! Fuzzy history palette (Ctrl+B r): mở, tìm lại, chấp nhận, xử lý phím/chuột,
//! và render.

use crossterm::event::{KeyCode, KeyEvent, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Clear};

use crate::app::*;
use crate::session::active_focus;
use crate::ui::truncate_pad;

pub(crate) fn palette_rect(s: Rect) -> Rect {
    let w = PALETTE_W.min(s.width.max(1));
    let h = PALETTE_H.min(s.height.max(1));
    let x = s.x + s.width.saturating_sub(w) / 2;
    let y = s.y + s.height.saturating_sub(h) / 2;
    Rect::new(x, y, w, h)
}

/// cwd của pane đang focus (để ưu tiên lệnh cùng thư mục).
pub(crate) fn focused_cwd(app: &App) -> String {
    app.panes
        .get(&active_focus(app))
        .map(|p| p.cwd.clone())
        .unwrap_or_default()
}

/// Số hàng kết quả hiển thị được trong popup.
pub(crate) fn palette_capacity(rect: Rect) -> usize {
    Block::bordered().inner(rect).height.saturating_sub(1) as usize
}

pub(crate) fn open_palette(app: &mut App) {
    let rect = palette_rect(app.screen);
    let cwd = focused_cwd(app);
    let results = app.history.search("", &cwd, palette_capacity(rect));
    app.palette = Some(Palette {
        query: String::new(),
        results,
        selected: 0,
        rect,
    });
}

pub(crate) fn palette_research(app: &mut App) {
    let cwd = focused_cwd(app);
    if let Some(pal) = &app.palette {
        let cap = palette_capacity(pal.rect);
        let results = app.history.search(&pal.query, &cwd, cap);
        if let Some(pal) = &mut app.palette {
            pal.results = results;
            pal.selected = 0;
        }
    }
}

/// Chèn lệnh đang chọn vào pane focus (không tự Enter, để người dùng sửa/chạy).
pub(crate) fn palette_accept(app: &mut App) {
    if let Some(pal) = app.palette.take()
        && let Some(entry) = pal.results.get(pal.selected)
    {
        let bytes = entry.cmdline.clone().into_bytes();
        let focus = active_focus(app);
        if let Some(pane) = app.panes.get_mut(&focus) {
            pane.pty.write(&bytes);
        }
    }
}

/// Chỉ số kết quả tại (col,row) trong popup.
pub(crate) fn palette_row_at(pal: &Palette, col: u16, row: u16) -> Option<usize> {
    let inner = Block::bordered().inner(pal.rect);
    let base = inner.y + 1; // hàng 0 = ô nhập
    if col < inner.x || col >= inner.x + inner.width || row < base {
        return None;
    }
    let idx = (row - base) as usize;
    (idx < pal.results.len()).then_some(idx)
}

pub(crate) fn handle_palette_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => app.palette = None,
        KeyCode::Enter => palette_accept(app),
        KeyCode::Up => {
            if let Some(pal) = &mut app.palette {
                pal.selected = pal.selected.saturating_sub(1);
            }
        }
        KeyCode::Down => {
            if let Some(pal) = &mut app.palette {
                pal.selected = (pal.selected + 1).min(pal.results.len().saturating_sub(1));
            }
        }
        KeyCode::Backspace => {
            if let Some(pal) = &mut app.palette {
                pal.query.pop();
            }
            palette_research(app);
        }
        KeyCode::Char(c) => {
            if let Some(pal) = &mut app.palette {
                pal.query.push(c);
            }
            palette_research(app);
        }
        _ => {}
    }
}

pub(crate) fn handle_palette_mouse(app: &mut App, me: MouseEvent) {
    let (col, row) = (me.column, me.row);
    match me.kind {
        MouseEventKind::Moved | MouseEventKind::Drag(_) => {
            if let Some(pal) = &mut app.palette
                && let Some(i) = palette_row_at(pal, col, row)
            {
                pal.selected = i;
            }
        }
        MouseEventKind::Down(_) => {
            let hit = app
                .palette
                .as_ref()
                .and_then(|pal| palette_row_at(pal, col, row));
            match hit {
                Some(i) => {
                    if let Some(pal) = &mut app.palette {
                        pal.selected = i;
                    }
                    palette_accept(app);
                }
                None => app.palette = None, // click ngoài → đóng
            }
        }
        _ => {}
    }
}

pub(crate) fn render_palette(frame: &mut Frame, pal: &Palette, accent: Color) {
    let mauve = accent;
    frame.render_widget(Clear, pal.rect);
    let block = Block::bordered()
        .border_style(Style::default().fg(mauve))
        .title("History — Enter: insert · Esc: close")
        .style(Style::default().bg(Color::Rgb(24, 24, 37)).fg(Color::Gray));
    let inner = block.inner(pal.rect);
    frame.render_widget(&block, pal.rect);

    let maxw = inner.width as usize;
    // Ô nhập truy vấn.
    let q = format!("> {}", pal.query);
    frame.buffer_mut().set_string(
        inner.x,
        inner.y,
        truncate_pad(&q, maxw),
        Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
    );

    // Danh sách kết quả.
    for (i, entry) in pal.results.iter().enumerate() {
        let y = inner.y + 1 + i as u16;
        if y >= inner.y + inner.height {
            break;
        }
        let selected = i == pal.selected;
        let style = if selected {
            Style::default().bg(mauve).fg(Color::Black)
        } else if entry.cwd_match {
            Style::default().fg(Color::Rgb(166, 227, 161))
        } else {
            Style::default().fg(Color::Gray)
        };
        let marker = if entry.cwd_match { "● " } else { "  " };
        let line = format!("{}{}", marker, entry.cmdline);
        frame
            .buffer_mut()
            .set_string(inner.x, y, truncate_pad(&line, maxw), style);
    }
}
