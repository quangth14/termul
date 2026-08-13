//! State của ứng dụng: `App` cùng toàn bộ struct/enum/hằng số mô tả trạng thái
//! (pane, tab, các overlay) và `AppEvent` cho event loop.

use std::collections::HashMap;
use std::io::Stdout;
use std::sync::mpsc::Sender;
use std::time::Instant;

use crossterm::event::Event;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;

use crate::config::Config;
use crate::history::{HistoryEntry, HistoryStore};
use crate::layout::{self, Layout, PaneId, SplitDir, SplitId};
use crate::osc::OscScanner;
use crate::pty::PtySession;
use crate::shell::ShellIntegration;
use crate::term::{GridPoint, GridSelection, TermGrid};

/// Sự kiện hợp nhất đẩy vào event loop chính.
pub(crate) enum AppEvent {
    /// Bytes output từ shell của một pane.
    PtyData(PaneId, Vec<u8>),
    /// Shell của một pane đã thoát (EOF).
    PtyClosed(PaneId),
    /// Sự kiện terminal (phím/chuột/resize) từ crossterm.
    Term(Event),
}

pub(crate) type Backend = CrosstermBackend<Stdout>;

/// Lệnh đang chạy trong một pane, chờ exit code để ghi lịch sử.
pub(crate) struct PendingCmd {
    pub(crate) cmd: String,
    pub(crate) cwd: String,
    pub(crate) start: Instant,
}

/// Dòng nhập hiện tại của pane (từ zle line-pre-redraw).
#[derive(Default, Clone)]
pub(crate) struct InputLine {
    pub(crate) buffer: String,
    pub(crate) cursor: usize, // vị trí con trỏ theo ký tự
}

/// Một pane: emulator + PTY + trạng thái bắt lệnh (OSC 133).
pub(crate) struct Pane {
    pub(crate) grid: TermGrid,
    pub(crate) pty: PtySession,
    pub(crate) osc: OscScanner,
    pub(crate) cwd: String,
    pub(crate) pending: Option<PendingCmd>,
    pub(crate) input: InputLine,
}

/// Fuzzy history palette (mở bằng Ctrl+B r).
pub(crate) struct Palette {
    pub(crate) query: String,
    pub(crate) results: Vec<HistoryEntry>,
    pub(crate) selected: usize,
    pub(crate) rect: Rect,
}

/// Popup autocomplete hiện khi gõ (gợi ý từ history hợp nhất, kiểu VSCode).
pub(crate) struct Suggest {
    pub(crate) matches: Vec<String>,
    pub(crate) selected: usize,
    pub(crate) offset: usize, // chỉ số dòng đầu tiên đang hiển thị (cuộn)
}

/// Một tab: cây layout + pane đang focus riêng.
pub(crate) struct Tab {
    pub(crate) name: String,
    pub(crate) layout: Layout,
    pub(crate) focus: PaneId,
}

/// Vùng bấm được trên statusbar.
#[derive(Clone, Copy)]
pub(crate) enum StatusKind {
    Switch(usize),
    NewTab,
}

/// Độ rộng tối thiểu của một tab (tên căn giữa, có đệm hai bên).
pub(crate) const TAB_MIN_W: u16 = 8;

/// Một đoạn hiển thị trên statusbar (đồng thời là vùng hit-test).
pub(crate) struct StatusSeg {
    pub(crate) x: u16,
    pub(crate) text: String,
    pub(crate) kind: StatusKind,
    pub(crate) active: bool,
}

/// Trạng thái đang kéo divider để resize.
#[derive(Clone, Copy)]
pub(crate) struct DragState {
    pub(crate) id: SplitId,
    pub(crate) dir: SplitDir,
    pub(crate) bounds: Rect,
}

/// Hành động trong context menu (click chuột phải) — mang sẵn đối tượng đích.
#[derive(Clone, Copy)]
pub(crate) enum MenuAction {
    SplitRight(PaneId),
    SplitDown(PaneId),
    ClosePane(PaneId),
    NewTab,
    RenameTab(usize),
    CloseTab(usize),
}

pub(crate) struct MenuEntry {
    pub(crate) label: &'static str,
    pub(crate) action: MenuAction,
}

/// Context menu đang mở (dùng chung cho pane và tab).
pub(crate) struct ContextMenu {
    pub(crate) rect: Rect,
    pub(crate) items: Vec<MenuEntry>,
    pub(crate) hovered: usize,
}

pub(crate) const RENAME_W: u16 = 40;
pub(crate) const RENAME_H: u16 = 3; // viền(2) + 1 hàng nhập

/// Chế độ đổi tên tab.
pub(crate) struct RenameState {
    pub(crate) tab_index: usize,
    pub(crate) buffer: String,
}

pub(crate) const CONFIRM_MSG: &str = "Close the last pane and quit?";
pub(crate) const CONFIRM_OPTS: [&str; 2] = ["Cancel (keep pane)", "Quit"];
pub(crate) const CONFIRM_W: u16 = 40;
pub(crate) const CONFIRM_H: u16 = 6; // viền(2) + thông điệp(1) + trống(1) + 2 lựa chọn

/// Popup xác nhận khi đóng pane cuối cùng (sẽ thoát app).
pub(crate) struct ConfirmDialog {
    pub(crate) rect: Rect,
    pub(crate) selected: usize, // 0 = Huỷ (mặc định), 1 = Thoát
}

pub(crate) const SUGGEST_MAX_VISIBLE: usize = 10; // số dòng hiển thị tối đa
pub(crate) const SUGGEST_FETCH: usize = 50; // số gợi ý nạp về tối đa

pub(crate) const PALETTE_W: u16 = 72;
pub(crate) const PALETTE_H: u16 = 18;

pub(crate) struct App {
    pub(crate) panes: HashMap<PaneId, Pane>,
    pub(crate) tabs: Vec<Tab>,
    pub(crate) active_tab: usize,
    pub(crate) shell: String,
    pub(crate) tx: Sender<AppEvent>,
    pub(crate) history: HistoryStore,
    pub(crate) integ: ShellIntegration,
    pub(crate) cfg: Config,
    pub(crate) next_pane: u64,
    pub(crate) next_split: u64,
    pub(crate) next_tab: u64,

    // Kết quả layout lần vẽ gần nhất — dùng hit-test chuột.
    pub(crate) screen: Rect,
    pub(crate) areas: HashMap<PaneId, Rect>,
    pub(crate) inner_areas: HashMap<PaneId, Rect>,
    pub(crate) dividers: Vec<layout::Divider>,
    pub(crate) status_y: u16,
    pub(crate) status_segs: Vec<StatusSeg>,

    pub(crate) dragging: Option<DragState>,
    pub(crate) selection: Option<PaneSelection>,
    pub(crate) menu: Option<ContextMenu>,
    pub(crate) confirm: Option<ConfirmDialog>,
    pub(crate) rename: Option<RenameState>,
    pub(crate) palette: Option<Palette>,
    pub(crate) suggest: Option<Suggest>,
    pub(crate) suggest_rect: Option<Rect>,
    /// Giá trị buffer mà popup đã bị đóng chủ động (accept/Esc) → không tự mở lại
    /// cho tới khi buffer đổi khác.
    pub(crate) suggest_dismissed_for: Option<String>,
    pub(crate) prefix_active: bool,
    pub(crate) should_quit: bool,
}

/// Vùng văn bản đang được chọn trong một pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PaneSelection {
    pub(crate) pane: PaneId,
    pub(crate) range: GridSelection,
}

impl PaneSelection {
    pub(crate) fn new(pane: PaneId, point: GridPoint) -> Self {
        Self {
            pane,
            range: GridSelection::new(point),
        }
    }
}
