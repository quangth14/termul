//! Logic popup autocomplete: dựng lại theo dòng nhập, cuộn, chấp nhận,
//! và tính vị trí hiển thị.

use ratatui::layout::Rect;

use crate::app::*;
use crate::layout::PaneId;
use crate::session::active_focus;

/// Số dòng đang thực sự hiển thị của popup.
pub(crate) fn suggest_visible(n: usize, max_visible: usize) -> usize {
    n.min(max_visible)
}

/// Điều chỉnh offset để dòng đang chọn luôn nằm trong khung hiển thị.
pub(crate) fn suggest_scroll_to_selected(sug: &mut Suggest, max_visible: usize) {
    let visible = suggest_visible(sug.matches.len(), max_visible);
    if sug.selected < sug.offset {
        sug.offset = sug.selected;
    } else if sug.selected >= sug.offset + visible {
        sug.offset = sug.selected + 1 - visible;
    }
}

/// Dựng lại popup gợi ý cho pane đang focus theo dòng nhập hiện tại.
/// Chỉ hiện khi con trỏ ở CUỐI dòng và có lệnh bắt đầu bằng nội dung đang nhập.
pub(crate) fn rebuild_suggest(app: &mut App) {
    let focus = active_focus(app);
    let Some(pane) = app.panes.get(&focus) else {
        app.suggest = None;
        return;
    };
    let buffer = pane.input.buffer.clone();
    let at_end = pane.input.cursor == buffer.chars().count();
    if buffer.trim().is_empty() || !at_end {
        app.suggest = None;
        return;
    }
    // Đã đóng chủ động cho đúng buffer này → không tự mở lại.
    if app.suggest_dismissed_for.as_deref() == Some(buffer.as_str()) {
        app.suggest = None;
        return;
    }
    let cwd = pane.cwd.clone();
    let matches = app.history.suggest(&buffer, &cwd, app.cfg.suggest_fetch);
    app.suggest = if matches.is_empty() {
        None
    } else {
        Some(Suggest {
            selected: 0,
            offset: 0,
            matches,
        })
    };
}

/// Chấp nhận gợi ý đang chọn bằng một lần cập nhật ZLE nguyên tử.
pub(crate) fn suggest_accept(app: &mut App) {
    let focus = active_focus(app);
    let Some(sug) = app.suggest.take() else { return };
    let Some(full) = sug.matches.get(sug.selected).cloned() else {
        return;
    };
    // Sau khi thay dòng, buffer sẽ bằng `full`; đánh dấu để popup không tự mở lại
    // (Enter kế tiếp sẽ chạy lệnh thay vì accept lần nữa).
    app.suggest_dismissed_for = Some(full.clone());
    let atomic_edit = app
        .integ
        .prepare_zle_edit(focus.0, &full, full.chars().count())
        .ok()
        .flatten();
    if let Some(pane) = app.panes.get_mut(&focus) {
        if let Some(bytes) = atomic_edit {
            pane.pty.write(&bytes);
        } else {
            let mut bytes = vec![0x01, 0x0b]; // Fallback cho history nhiều dòng/tab.
            bytes.extend_from_slice(full.as_bytes());
            pane.pty.write(&bytes);
        }
    }
}

/// Rectangle của popup gợi ý: neo ngay dưới con trỏ pane focus, kẹp trong màn hình.
pub(crate) fn compute_suggest_rect(app: &App, focus: PaneId) -> Option<Rect> {
    let sug = app.suggest.as_ref()?;
    let inner = app.inner_areas.get(&focus)?;
    let pane = app.panes.get(&focus)?;
    let (crow, ccol) = pane.grid.screen().cursor_position();
    let cx = inner.x + ccol;
    let cy = inner.y + crow;

    let longest = sug.matches.iter().map(|m| m.chars().count()).max().unwrap_or(8) as u16;
    // +2 viền + 2 đệm hai bên cho phần nội dung.
    let w = (longest + 4).clamp(8, app.screen.width.max(8));
    let rows = suggest_visible(sug.matches.len(), app.cfg.suggest_max_visible) as u16;
    let h = rows + 2; // + viền trên/dưới

    let mut y = cy + 1;
    if y + h > app.screen.y + app.screen.height {
        y = cy.saturating_sub(h); // không đủ chỗ dưới → hiện phía trên con trỏ
    }
    let mut x = cx;
    if x + w > app.screen.x + app.screen.width {
        x = (app.screen.x + app.screen.width).saturating_sub(w);
    }
    Some(Rect::new(x, y, w, h))
}
