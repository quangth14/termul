//! Context menu chuột phải (dùng chung cho pane và tab): mở, hit-test,
//! và xử lý phím/chuột.

use crossterm::event::{KeyCode, KeyEvent, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::widgets::Block;

use crate::app::*;
use crate::input::within;
use crate::layout::{PaneId, SplitDir};
use crate::rename::start_rename;
use crate::session::{do_split, new_tab, request_close, request_close_tab};

/// Vị trí popup kích thước w×h tại (col,row), kẹp gọn trong màn hình `s`.
pub(crate) fn popup_rect(s: Rect, col: u16, row: u16, w: u16, h: u16) -> Rect {
    let w = w.min(s.width.max(1));
    let h = h.min(s.height.max(1));
    let mut x = col;
    let mut y = row;
    if x + w > s.x + s.width {
        x = (s.x + s.width).saturating_sub(w);
    }
    if y + h > s.y + s.height {
        y = (s.y + s.height).saturating_sub(h);
    }
    Rect::new(x, y, w, h)
}

pub(crate) fn open_menu_with(app: &mut App, items: Vec<MenuEntry>, col: u16, row: u16) {
    let label_w = items
        .iter()
        .map(|e| e.label.chars().count())
        .max()
        .unwrap_or(4) as u16;
    let w = label_w + 4; // 2 viền + 2 đệm
    let h = items.len() as u16 + 2; // + 2 viền
    let rect = popup_rect(app.screen, col, row, w, h);
    app.menu = Some(ContextMenu {
        rect,
        items,
        hovered: 0,
    });
}

/// Menu chuột phải cho một pane.
pub(crate) fn open_pane_menu(app: &mut App, pid: PaneId, col: u16, row: u16) {
    let items = vec![
        MenuEntry {
            label: "Split Right",
            action: MenuAction::SplitRight(pid),
        },
        MenuEntry {
            label: "Split Down",
            action: MenuAction::SplitDown(pid),
        },
        MenuEntry {
            label: "Close Pane",
            action: MenuAction::ClosePane(pid),
        },
    ];
    open_menu_with(app, items, col, row);
}

/// Menu chuột phải cho một tab (giống herdr: New tab / Rename / Close).
pub(crate) fn open_tab_menu(app: &mut App, tab_index: usize, col: u16, row: u16) {
    let items = vec![
        MenuEntry {
            label: "New tab",
            action: MenuAction::NewTab,
        },
        MenuEntry {
            label: "Rename",
            action: MenuAction::RenameTab(tab_index),
        },
        MenuEntry {
            label: "Close",
            action: MenuAction::CloseTab(tab_index),
        },
    ];
    open_menu_with(app, items, col, row);
}

/// Chỉ số mục menu tại (col,row), nếu con trỏ nằm trên một mục.
pub(crate) fn menu_item_at(menu: &ContextMenu, col: u16, row: u16) -> Option<usize> {
    let inner = Block::bordered().inner(menu.rect);
    if !within(inner, col, row) {
        return None;
    }
    let idx = (row - inner.y) as usize;
    (idx < menu.items.len()).then_some(idx)
}

pub(crate) fn exec_menu(app: &mut App, action: MenuAction) {
    match action {
        MenuAction::SplitRight(p) => do_split(app, p, SplitDir::LeftRight),
        MenuAction::SplitDown(p) => do_split(app, p, SplitDir::TopBottom),
        MenuAction::ClosePane(p) => request_close(app, p),
        MenuAction::NewTab => new_tab(app),
        MenuAction::RenameTab(i) => start_rename(app, i),
        MenuAction::CloseTab(i) => request_close_tab(app, i),
    }
}

pub(crate) fn handle_menu_mouse(app: &mut App, me: MouseEvent) {
    let (col, row) = (me.column, me.row);
    match me.kind {
        MouseEventKind::Moved | MouseEventKind::Drag(_) => {
            if let Some(menu) = &mut app.menu
                && let Some(i) = menu_item_at(menu, col, row)
            {
                menu.hovered = i;
            }
        }
        MouseEventKind::Down(_) => {
            let menu = app.menu.take().expect("menu mở");
            if let Some(i) = menu_item_at(&menu, col, row) {
                exec_menu(app, menu.items[i].action);
            }
            // Click ngoài mục → menu đã bị take() nên tự đóng.
        }
        _ => {}
    }
}

pub(crate) fn handle_menu_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => app.menu = None,
        KeyCode::Up => {
            if let Some(menu) = &mut app.menu {
                menu.hovered = menu.hovered.saturating_sub(1);
            }
        }
        KeyCode::Down => {
            if let Some(menu) = &mut app.menu {
                menu.hovered = (menu.hovered + 1).min(menu.items.len().saturating_sub(1));
            }
        }
        KeyCode::Enter => {
            if let Some(menu) = app.menu.take() {
                exec_menu(app, menu.items[menu.hovered].action);
            }
        }
        _ => {}
    }
}
