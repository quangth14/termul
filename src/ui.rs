//! Render: hàm `draw` vẽ toàn bộ khung hình (pane, tabbar, các overlay).

use std::io::Write;

use anyhow::Result;
use crossterm::cursor::SetCursorStyle;
use crossterm::execute;
use crossterm::terminal::SetTitle;
use ratatui::Terminal;
use ratatui::layout::{Alignment, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph};

use crate::app::*;
use crate::menu::popup_rect;
use crate::mention::compute_mention_rect;
use crate::palette::render_palette;
use crate::session::{active_focus, recompute};
use crate::suggest::{compute_suggest_rect, suggest_visible};
use crate::term::{CursorShape, TermView};

pub(crate) fn draw(terminal: &mut Terminal<Backend>, app: &mut App) -> Result<()> {
    let size = terminal.size()?;
    let area = Rect::new(0, 0, size.width, size.height);
    recompute(app, area);
    let focus = active_focus(app);
    app.mention_rect = compute_mention_rect(app, focus);
    app.suggest_rect = compute_suggest_rect(app, focus);

    let accent = app.cfg.accent;
    let bg = app.cfg.bg;
    let green = app.cfg.green;
    let focus_border = app.cfg.blue;
    let red = app.cfg.red;
    let yellow = app.cfg.yellow;
    let max_visible = app.cfg.suggest_max_visible;
    let cursor_color = if app.rename.is_none()
        && app.menu.is_none()
        && app.confirm.is_none()
        && app.palette.is_none()
    {
        app.panes
            .get(&focus)
            .and_then(|pane| pane.grid.screen().cursor_color())
            .filter(|color| Some(*color) != app.host_terminal_theme.cursor)
    } else {
        None
    };
    let terminal_title = app
        .panes
        .get(&focus)
        .map(|pane| pane.title.as_str())
        .filter(|title| !title.is_empty())
        .unwrap_or("termul");
    let cursor_style = if app.rename.is_none()
        && app.menu.is_none()
        && app.confirm.is_none()
        && app.palette.is_none()
    {
        app.panes.get(&focus).map_or(SetCursorStyle::DefaultUserShape, |pane| {
            match (pane.grid.screen().cursor_shape(), pane.grid.screen().cursor_blinking()) {
                (CursorShape::Block, true) => SetCursorStyle::BlinkingBlock,
                (CursorShape::Block, false) => SetCursorStyle::SteadyBlock,
                (CursorShape::Underline, true) => SetCursorStyle::BlinkingUnderScore,
                (CursorShape::Underline, false) => SetCursorStyle::SteadyUnderScore,
                (CursorShape::Bar, true) => SetCursorStyle::BlinkingBar,
                (CursorShape::Bar, false) => SetCursorStyle::SteadyBar,
            }
        })
    } else {
        SetCursorStyle::DefaultUserShape
    };

    // Một pane duy nhất → không viền, pane chiếm trọn vùng.
    let single = app.areas.len() == 1;

    terminal.draw(|frame| {
        for (pid, rect) in &app.areas {
            let inner = if single {
                *rect
            } else {
                let focused = *pid == focus;
                let border_style = if focused {
                    Style::default().fg(focus_border)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                let block = Block::bordered().border_style(border_style);
                let inner = block.inner(*rect);
                frame.render_widget(&block, *rect);
                inner
            };
            if let Some(pane) = app.panes.get(pid) {
                frame.render_widget(
                    TermView {
                        screen: pane.grid.screen(),
                        selection: app
                            .selection
                            .filter(|selection| selection.pane == *pid)
                            .map(|selection| selection.range),
                        default_bg: bg,
                    },
                    inner,
                );
            }
        }

        // Tabbar (hàng trên cùng): các tab + nút thêm. Tab active nền tím (mauve).
        let sy = app.status_y;
        let bar_bg = Style::default()
            .bg(Color::Rgb(24, 24, 37))
            .fg(Color::Rgb(110, 110, 130));

        let active_bg = Style::default()
            .bg(bg)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD);

        frame
            .buffer_mut()
            .set_string(0, sy, " ".repeat(area.width as usize), bar_bg);
        for seg in &app.status_segs {
            let style = if seg.active { active_bg } else { bar_bg };

            if seg.active {
                frame.buffer_mut().set_string(
                    seg.x,
                    app.status_y - 1,
                    underline_each_char(" ").repeat(seg.text.len()),
                    Style::default().bg(bg).fg(app.cfg.accent),
                );
            }

            frame.buffer_mut().set_string(seg.x, sy, &seg.text, style);
        }

        // Popup mention file/thư mục (ưu tiên hơn gợi ý history).
        if app.menu.is_none()
            && app.confirm.is_none()
            && app.rename.is_none()
            && app.palette.is_none()
            && let (Some(mention), Some(rect)) = (&app.mention, app.mention_rect)
        {
            frame.render_widget(Clear, rect);
            let block = Block::bordered()
                .title(" Files ")
                .border_style(Style::default().fg(green))
                .style(Style::default().bg(bg));
            let inner = block.inner(rect);
            frame.render_widget(&block, rect);
            let visible = mention.matches.len().min(max_visible);
            for row in 0..visible {
                let idx = mention.offset + row;
                let Some(path) = mention.matches.get(idx) else { break };
                let style = if idx == mention.selected {
                    Style::default().bg(green).fg(Color::Black)
                } else {
                    Style::default().bg(bg).fg(Color::Gray)
                };
                frame.buffer_mut().set_string(
                    inner.x,
                    inner.y + row as u16,
                    truncate_pad(path, inner.width as usize),
                    style,
                );
            }
        }

        // Popup gợi ý autocomplete (chỉ khi không có modal khác).
        if app.menu.is_none()
            && app.confirm.is_none()
            && app.rename.is_none()
            && app.palette.is_none()
            && app.mention.is_none()
            && let (Some(sug), Some(rect)) = (&app.suggest, app.suggest_rect)
        {
            let mauve = accent;
            frame.render_widget(Clear, rect);
            let block = Block::bordered()
                .border_style(Style::default().fg(mauve))
                .style(Style::default().bg(bg));
            let inner = block.inner(rect);
            frame.render_widget(&block, rect);

            let visible = suggest_visible(sug.matches.len(), max_visible);
            for row in 0..visible {
                let idx = sug.offset + row;
                let Some(m) = sug.matches.get(idx) else { break };
                let style = if idx == sug.selected {
                    Style::default().bg(mauve).fg(Color::Black)
                } else {
                    Style::default().bg(bg).fg(Color::Gray)
                };
                frame.buffer_mut().set_string(
                    inner.x,
                    inner.y + row as u16,
                    truncate_pad(m, inner.width as usize),
                    style,
                );
            }
            // Chỉ báo cuộn ▲/▼ trên viền phải nếu còn nội dung ẩn.
            let right = rect.x + rect.width - 1;
            if sug.offset > 0
                && let Some(c) = frame.buffer_mut().cell_mut((right, rect.y))
            {
                c.set_symbol("▲");
                c.set_style(Style::default().fg(yellow));
            }
            if sug.offset + visible < sug.matches.len()
                && let Some(c) = frame
                    .buffer_mut()
                    .cell_mut((right, rect.y + rect.height - 1))
            {
                c.set_symbol("▼");
                c.set_style(Style::default().fg(yellow));
            }
        }

        // Overlay context menu (nếu đang mở).
        if let Some(menu) = &app.menu {
            frame.render_widget(Clear, menu.rect);
            let items: Vec<ListItem> = menu.items.iter().map(|e| ListItem::new(e.label)).collect();
            let list = List::new(items)
                .block(Block::bordered().border_style(Style::default().fg(accent)))
                .style(Style::default().bg(bg).fg(Color::Gray))
                .highlight_style(
                    Style::default()
                        .bg(accent)
                        .fg(Color::Black)
                        .add_modifier(Modifier::BOLD),
                );
            let mut state = ListState::default();
            state.select(Some(menu.hovered));
            frame.render_stateful_widget(list, menu.rect, &mut state);
        }

        // Overlay popup xác nhận (nếu đang mở) — topmost.
        if let Some(dialog) = &app.confirm {
            frame.render_widget(Clear, dialog.rect);
            let block = Block::bordered().border_style(Style::default().fg(red));
            let inner = block.inner(dialog.rect);
            frame.render_widget(&block, dialog.rect);

            let msg = Paragraph::new(CONFIRM_MSG)
                .alignment(Alignment::Center)
                .style(Style::default().fg(Color::White).bg(bg));
            frame.render_widget(msg, Rect::new(inner.x, inner.y, inner.width, 1));

            let items: Vec<ListItem> = CONFIRM_OPTS.iter().map(|s| ListItem::new(*s)).collect();
            let list = List::new(items)
                .style(Style::default().bg(bg).fg(Color::Gray))
                .highlight_style(
                    Style::default()
                        .bg(red)
                        .fg(Color::Black)
                        .add_modifier(Modifier::BOLD),
                );
            let mut state = ListState::default();
            state.select(Some(dialog.selected));
            frame.render_stateful_widget(
                list,
                Rect::new(inner.x, inner.y + 2, inner.width, 2),
                &mut state,
            );
        }

        // Overlay đổi tên tab — có ô nhập + con trỏ.
        if let Some(state) = &app.rename {
            let rect = popup_rect(app.screen, 2, 1, RENAME_W, RENAME_H);
            frame.render_widget(Clear, rect);
            let block = Block::bordered()
                .border_style(Style::default().fg(accent))
                .title("Rename tab (Enter: OK · Esc: Cancel)")
                .style(Style::default().bg(bg).fg(Color::White));
            let inner = block.inner(rect);
            frame.render_widget(&block, rect);
            frame.buffer_mut().set_string(
                inner.x,
                inner.y,
                &state.buffer,
                Style::default().fg(Color::White),
            );
            let cx = inner.x + state.buffer.chars().count() as u16;
            if cx < inner.x + inner.width {
                frame.set_cursor_position(Position { x: cx, y: inner.y });
            }
        }

        // Overlay history palette — topmost, có ô nhập + con trỏ.
        if let Some(pal) = &app.palette {
            render_palette(frame, pal, accent, bg, green);
            let inner = Block::bordered().inner(pal.rect);
            let cx = inner.x + 2 + pal.query.chars().count() as u16;
            if cx < inner.x + inner.width {
                frame.set_cursor_position(Position { x: cx, y: inner.y });
            }
        } else if app.rename.is_none()
            && app.menu.is_none()
            && app.confirm.is_none()
            && let (Some(inner), Some(pane)) = (app.inner_areas.get(&focus), app.panes.get(&focus))
        {
            // Con trỏ của pane đang focus (ẩn khi có modal).
            let screen = pane.grid.screen();
            if !screen.hide_cursor() {
                let (crow, ccol) = screen.cursor_position();
                let x = inner.x + ccol;
                let y = inner.y + crow;
                if x < inner.x + inner.width && y < inner.y + inner.height {
                    frame.set_cursor_position(Position { x, y });
                }
            }
        }
    })?;
    execute!(terminal.backend_mut(), cursor_style, SetTitle(terminal_title))?;
    if let Some(color) = cursor_color {
        write!(
            terminal.backend_mut(),
            "\x1b]12;rgb:{:02x}/{:02x}/{:02x}\x1b\\",
            color.r, color.g, color.b
        )?;
    } else {
        terminal.backend_mut().write_all(b"\x1b]112\x1b\\")?;
    }
    terminal.backend_mut().flush()?;
    Ok(())
}

/// Cắt/đệm chuỗi đúng `width` cột (theo số ký tự) để tô nền đều.
pub(crate) fn truncate_pad(s: &str, width: usize) -> String {
    let mut out: String = s.chars().take(width).collect();
    let len = out.chars().count();
    if len < width {
        out.extend(std::iter::repeat_n(' ', width - len));
    }
    out
}

fn underline_each_char(input: &str) -> String {
    input.chars().flat_map(|ch| [ch, '\u{0332}']).collect()
}
