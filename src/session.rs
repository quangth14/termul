//! Logic lõi: vòng đời pane/tab, tính layout, và điều hướng focus.

use std::collections::HashMap;

use ratatui::layout::Rect;
use ratatui::widgets::Block;

use crate::app::*;
use crate::confirm::open_confirm;
use crate::input::center;
use crate::layout::{self, Layout, PaneId, SplitDir, SplitId};
use crate::osc::OscScanner;
use crate::pty::PtySession;
use crate::term::TermGrid;

// ── Layout / kích thước ─────────────────────────────────────────────

/// Vùng nội dung (trong viền 1 ô) của một pane rect.
pub(crate) fn inner_of(rect: Rect) -> Rect {
    Block::bordered().inner(rect)
}

/// (cols, rows) cấp cho emulator/PTY, sàn 2×2 tránh vt100 tràn số ở size nhỏ.
pub(crate) fn grid_dims(inner: Rect) -> (u16, u16) {
    (inner.width.max(2), inner.height.max(2))
}

/// Tính lại layout, resize toàn bộ PTY/emulator, và lưu vùng để hit-test.
pub(crate) fn recompute(app: &mut App, area: Rect) {
    // Tabbar ở hàng trên cùng; vùng nội dung nằm dưới nó.
    let status_y = area.y;
    let content = Rect::new(
        area.x,
        area.y + 1,
        area.width,
        area.height.saturating_sub(1),
    );

    let mut areas = HashMap::new();
    let mut dividers = Vec::new();
    app.tabs[app.active_tab]
        .layout
        .compute(content, &mut areas, &mut dividers);

    // Một pane duy nhất (không split) → không viền, pane chiếm trọn vùng.
    let single = areas.len() == 1;
    let mut inner_areas = HashMap::new();
    for (pid, rect) in &areas {
        let inner = if single { *rect } else { inner_of(*rect) };
        inner_areas.insert(*pid, inner);
        let (cols, rows) = grid_dims(inner);
        if let Some(pane) = app.panes.get_mut(pid) {
            pane.pty.resize(rows, cols);
            pane.grid.resize(rows, cols);
        }
    }

    app.screen = area;
    app.areas = areas;
    app.inner_areas = inner_areas;
    app.dividers = dividers;
    app.status_y = status_y;
    app.status_segs =
        build_status_segs(&app.tabs, app.active_tab, area.width, app.cfg.tab_min_width);
}

// Nhãn tab: tên căn giữa trong độ rộng tối thiểu, có đệm hai bên.
pub(crate) fn tab_label(name: &str, min_w: u16) -> String {
    let name_w = name.chars().count();
    let total = (min_w as usize).max(name_w + 2);
    let pad = total - name_w;
    format!(" {}{}", name, " ".repeat(pad))
}

/// Dựng các đoạn statusbar: [   tab   ] … [ + ]; đồng thời là vùng hit-test.
pub(crate) fn build_status_segs(
    tabs: &[Tab],
    active_tab: usize,
    width: u16,
    min_w: u16,
) -> Vec<StatusSeg> {
    let mut segs = Vec::new();
    let mut x: u16 = 0;
    for (i, tab) in tabs.iter().enumerate() {
        let active = i == active_tab;
        let label = tab_label(&tab.name, min_w);
        let lw = label.chars().count() as u16;
        if x + lw + 1 > width {
            break; // hết chỗ
        }
        segs.push(StatusSeg {
            x,
            text: label,
            kind: StatusKind::Switch(i),
            active,
        });
        x += lw;
        x += 1; // khoảng cách giữa các tab
    }
    segs
}

// ── Vòng đời pane ───────────────────────────────────────────────────

pub(crate) fn spawn_pane(app: &mut App) -> Option<PaneId> {
    let id = PaneId(app.next_pane);
    // Kích thước khởi tạo tuỳ ý — recompute() sẽ resize lại ngay.
    let env = app.integ.env_for(&app.shell);
    let pty = PtySession::spawn(id, 24, 80, &app.shell, &env, app.tx.clone()).ok()?;
    app.next_pane += 1;
    app.panes.insert(
        id,
        Pane {
            grid: TermGrid::new(24, 80),
            pty,
            osc: OscScanner::default(),
            cwd: String::new(),
            pending: None,
            input: InputLine::default(),
        },
    );
    Some(id)
}

/// Pane đang focus của tab đang active.
pub(crate) fn active_focus(app: &App) -> PaneId {
    app.tabs[app.active_tab].focus
}

pub(crate) fn set_active_focus(app: &mut App, pid: PaneId) {
    if app.tabs[app.active_tab].focus != pid {
        app.suggest = None; // đổi focus → ẩn popup gợi ý
    }
    app.tabs[app.active_tab].focus = pid;
}

/// Chỉ số tab chứa pane này.
pub(crate) fn tab_index_of(app: &App, pid: PaneId) -> Option<usize> {
    app.tabs.iter().position(|t| t.layout.contains(pid))
}

pub(crate) fn do_split(app: &mut App, target: PaneId, dir: SplitDir) {
    let Some(ti) = tab_index_of(app, target) else {
        return;
    };
    let Some(new_pane) = spawn_pane(app) else {
        return;
    };
    let sid = SplitId(app.next_split);
    app.next_split += 1;
    app.tabs[ti].layout.split_leaf(target, sid, dir, new_pane);
    app.tabs[ti].focus = new_pane;
}

/// Điểm vào cho thao tác đóng pane do người dùng chủ động (menu / nút ✕ / phím).
/// Nếu đây là pane cuối cùng của toàn app thì hỏi xác nhận thay vì thoát.
pub(crate) fn request_close(app: &mut App, pid: PaneId) {
    if app.panes.len() <= 1 {
        open_confirm(app);
    } else {
        do_close(app, pid);
    }
}

/// Gỡ pane khỏi tab của nó; tab rỗng → gỡ tab; hết tab → thoát.
pub(crate) fn do_close(app: &mut App, pid: PaneId) {
    let Some(ti) = tab_index_of(app, pid) else {
        app.panes.remove(&pid);
        return;
    };
    let root = std::mem::replace(&mut app.tabs[ti].layout, Layout::Leaf(PaneId(u64::MAX)));
    app.panes.remove(&pid);
    match layout::remove_pane(root, pid) {
        Some(new_root) => {
            if app.tabs[ti].focus == pid {
                app.tabs[ti].focus = new_root.first_leaf();
            }
            app.tabs[ti].layout = new_root;
        }
        None => {
            // Tab rỗng → gỡ tab.
            app.tabs.remove(ti);
            if app.tabs.is_empty() {
                app.should_quit = true;
            } else if app.active_tab >= app.tabs.len() {
                app.active_tab = app.tabs.len() - 1;
            }
        }
    }
}

// ── Vòng đời tab ────────────────────────────────────────────────────

pub(crate) fn new_tab(app: &mut App) {
    let Some(pid) = spawn_pane(app) else {
        return;
    };
    let name = app.next_tab.to_string();
    app.next_tab += 1;
    app.tabs.push(Tab {
        name,
        layout: Layout::Leaf(pid),
        focus: pid,
    });
    app.active_tab = app.tabs.len() - 1;
}

pub(crate) fn switch_tab(app: &mut App, index: usize) {
    if index < app.tabs.len() {
        app.active_tab = index;
        app.suggest = None;
    }
}

pub(crate) fn next_tab(app: &mut App) {
    if !app.tabs.is_empty() {
        app.active_tab = (app.active_tab + 1) % app.tabs.len();
        app.suggest = None;
    }
}

pub(crate) fn prev_tab(app: &mut App) {
    if !app.tabs.is_empty() {
        app.active_tab = (app.active_tab + app.tabs.len() - 1) % app.tabs.len();
        app.suggest = None;
    }
}

/// Điểm vào cho đóng cả tab do người dùng chủ động.
/// Đóng tab cuối cùng = thoát app → hỏi xác nhận.
pub(crate) fn request_close_tab(app: &mut App, index: usize) {
    if app.tabs.len() <= 1 {
        open_confirm(app);
    } else {
        close_tab(app, index);
    }
}

pub(crate) fn close_tab(app: &mut App, index: usize) {
    if index >= app.tabs.len() {
        return;
    }
    let mut leaves = Vec::new();
    app.tabs[index].layout.leaves(&mut leaves);
    for pid in leaves {
        app.panes.remove(&pid); // drop PtySession → đóng PTY
    }
    app.tabs.remove(index);
    if app.tabs.is_empty() {
        app.should_quit = true;
    } else if app.active_tab >= app.tabs.len() {
        app.active_tab = app.tabs.len() - 1;
    }
}

// ── Điều hướng focus theo hướng ─────────────────────────────────────

#[derive(Clone, Copy)]
pub(crate) enum Dir {
    Left,
    Right,
    Up,
    Down,
}

/// Chuyển focus sang pane lân cận gần nhất theo hướng.
pub(crate) fn focus_dir(app: &mut App, dir: Dir) {
    let focus = active_focus(app);
    let Some(cur) = app.areas.get(&focus).copied() else {
        return;
    };
    let (cx, cy) = center(cur);
    let mut best: Option<(PaneId, i32)> = None;
    for (pid, rect) in &app.areas {
        if *pid == focus {
            continue;
        }
        let (px, py) = center(*rect);
        let in_dir = match dir {
            Dir::Left => px < cx,
            Dir::Right => px > cx,
            Dir::Up => py < cy,
            Dir::Down => py > cy,
        };
        if !in_dir {
            continue;
        }
        let dist = (px - cx).pow(2) + (py - cy).pow(2);
        if best.is_none_or(|(_, bd)| dist < bd) {
            best = Some((*pid, dist));
        }
    }
    if let Some((pid, _)) = best {
        set_active_focus(app, pid);
    }
}
