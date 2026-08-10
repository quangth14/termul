//! Overlay xác nhận khi đóng pane/tab cuối cùng (sẽ thoát app).

use crossterm::event::{KeyCode, KeyEvent, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::widgets::Block;

use crate::app::*;

/// Rectangle canh giữa màn hình cho popup xác nhận.
pub(crate) fn confirm_rect(s: Rect) -> Rect {
    let w = CONFIRM_W.min(s.width.max(1));
    let h = CONFIRM_H.min(s.height.max(1));
    let x = s.x + s.width.saturating_sub(w) / 2;
    let y = s.y + s.height.saturating_sub(h) / 2;
    Rect::new(x, y, w, h)
}

pub(crate) fn open_confirm(app: &mut App) {
    app.confirm = Some(ConfirmDialog {
        rect: confirm_rect(app.screen),
        selected: 0,
    });
}

/// Chỉ số lựa chọn tại (col,row) trong popup xác nhận.
pub(crate) fn confirm_option_at(dialog: &ConfirmDialog, col: u16, row: u16) -> Option<usize> {
    let inner = Block::bordered().inner(dialog.rect);
    let base = inner.y + 2; // hàng 0 = thông điệp, hàng 1 = trống
    if col < inner.x || col >= inner.x + inner.width || row < base {
        return None;
    }
    let idx = (row - base) as usize;
    (idx < CONFIRM_OPTS.len()).then_some(idx)
}

/// Thực thi lựa chọn: 1 = thoát, 0 = huỷ.
pub(crate) fn exec_confirm(app: &mut App, selected: usize) {
    if selected == 1 {
        app.should_quit = true;
    }
}

pub(crate) fn handle_confirm_mouse(app: &mut App, me: MouseEvent) {
    let (col, row) = (me.column, me.row);
    match me.kind {
        MouseEventKind::Moved | MouseEventKind::Drag(_) => {
            if let Some(dialog) = &mut app.confirm
                && let Some(i) = confirm_option_at(dialog, col, row)
            {
                dialog.selected = i;
            }
        }
        MouseEventKind::Down(_) => {
            let dialog = app.confirm.take().expect("confirm mở");
            if let Some(i) = confirm_option_at(&dialog, col, row) {
                exec_confirm(app, i);
            }
            // Click ngoài → coi như huỷ (dialog đã bị take()).
        }
        _ => {}
    }
}

pub(crate) fn handle_confirm_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => app.confirm = None,
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            app.confirm = None;
            app.should_quit = true;
        }
        KeyCode::Left | KeyCode::Up => {
            if let Some(dialog) = &mut app.confirm {
                dialog.selected = dialog.selected.saturating_sub(1);
            }
        }
        KeyCode::Right | KeyCode::Down => {
            if let Some(dialog) = &mut app.confirm {
                dialog.selected = (dialog.selected + 1).min(CONFIRM_OPTS.len() - 1);
            }
        }
        KeyCode::Enter => {
            if let Some(dialog) = app.confirm.take() {
                exec_confirm(app, dialog.selected);
            }
        }
        _ => {}
    }
}
