//! termul — Phase 3: command memory (OSC 133 → SQLite), popup autocomplete
//! live khi gõ (kiểu VSCode) + fuzzy history palette,
//! trên nền tabs + tiling multi-pane, mouse-first.

mod app;
mod config;
mod confirm;
mod event;
mod history;
mod input;
mod layout;
mod menu;
mod osc;
mod palette;
mod pty;
mod rename;
mod session;
mod shell;
mod suggest;
mod term;
mod ui;

use std::collections::HashMap;
use std::io::{self};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use anyhow::Result;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;

use crate::app::{App, AppEvent, Backend, InputLine, Pane, Tab};
use crate::config::Config;
use crate::event::handle_event;
use crate::history::HistoryStore;
use crate::layout::{Layout, PaneId};
use crate::osc::OscScanner;
use crate::pty::PtySession;
use crate::session::{grid_dims, inner_of};
use crate::shell::ShellIntegration;
use crate::term::TermGrid;
use crate::ui::draw;

fn main() -> Result<()> {
    install_panic_hook();
    let mut terminal = setup_terminal()?;
    let result = run(&mut terminal);
    restore_terminal(&mut terminal)?;
    result
}

/// Khôi phục terminal khi panic để không để lại raw mode / alternate screen.
fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
        original(info);
    }));
}

fn setup_terminal() -> Result<Terminal<Backend>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    Ok(terminal)
}

fn restore_terminal(terminal: &mut Terminal<Backend>) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}

fn run(terminal: &mut Terminal<Backend>) -> Result<()> {
    let size = terminal.size()?;
    let area = Rect::new(0, 0, size.width, size.height);
    let (init_cols, init_rows) = grid_dims(inner_of(area));

    let (tx, rx): (Sender<AppEvent>, Receiver<AppEvent>) = mpsc::channel();
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());

    let integ = ShellIntegration::setup()?;
    let history = HistoryStore::open_default()?;
    let cfg = Config::load();

    let first_id = PaneId(0);
    let env = integ.env_for(&shell);
    let pty = PtySession::spawn(first_id, init_rows, init_cols, &shell, &env, tx.clone())?;
    spawn_input_thread(tx.clone());

    let mut app = App {
        panes: HashMap::from([(
            first_id,
            Pane {
                grid: TermGrid::new(init_rows, init_cols),
                pty,
                osc: OscScanner::default(),
                cwd: String::new(),
                pending: None,
                input: InputLine::default(),
            },
        )]),
        tabs: vec![Tab {
            name: "1".to_string(),
            layout: Layout::Leaf(first_id),
            focus: first_id,
        }],
        active_tab: 0,
        shell,
        tx,
        history,
        integ,
        cfg,
        next_pane: 1,
        next_split: 0,
        next_tab: 2,
        screen: area,
        areas: HashMap::new(),
        inner_areas: HashMap::new(),
        dividers: Vec::new(),
        status_y: 0,
        status_segs: Vec::new(),
        dragging: None,
        selection: None,
        menu: None,
        confirm: None,
        rename: None,
        palette: None,
        suggest: None,
        suggest_rect: None,
        suggest_dismissed_for: None,
        prefix_active: false,
        should_quit: false,
    };

    draw(terminal, &mut app)?;

    while let Ok(ev) = rx.recv() {
        handle_event(&mut app, ev);
        while let Ok(ev) = rx.try_recv() {
            handle_event(&mut app, ev);
        }
        if app.should_quit {
            break;
        }
        draw(terminal, &mut app)?;
    }

    Ok(())
}

fn spawn_input_thread(tx: Sender<AppEvent>) {
    thread::spawn(move || {
        while let Ok(ev) = crossterm::event::read() {
            if tx.send(AppEvent::Term(ev)).is_err() {
                break;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use ratatui::layout::Rect;
    use ratatui::widgets::Block;

    use crate::app::*;
    use crate::config::Config;
    use crate::confirm::{confirm_option_at, confirm_rect};
    use crate::history::HistoryStore;
    use crate::input::{encode_key, handle_mouse};
    use crate::layout::{Layout, PaneId};
    use crate::menu::{menu_item_at, popup_rect};
    use crate::osc::{OscEvent, OscScanner};
    use crate::pty::PtySession;
    use crate::session::{build_status_segs, recompute};
    use crate::shell::ShellIntegration;
    use crate::suggest::{rebuild_suggest, suggest_accept};
    use crate::term::{GridPoint, TermGrid};

    /// Dựng một App tối thiểu với đúng 1 pane để test layout.
    fn one_pane_app() -> App {
        let (tx, _rx) = mpsc::channel();
        let pid = PaneId(0);
        let pty = PtySession::spawn(pid, 24, 80, "/bin/sh", &[], tx.clone()).expect("spawn pty");
        App {
            panes: HashMap::from([(
                pid,
                Pane {
                    grid: TermGrid::new(24, 80),
                    pty,
                    osc: OscScanner::default(),
                    cwd: String::new(),
                    pending: None,
                    input: InputLine::default(),
                },
            )]),
            tabs: vec![Tab {
                name: "1".into(),
                layout: Layout::Leaf(pid),
                focus: pid,
            }],
            active_tab: 0,
            shell: "/bin/sh".into(),
            tx,
            history: HistoryStore::open(":memory:".into()).unwrap(),
            integ: ShellIntegration::setup().unwrap(),
            cfg: Config::default(),
            next_pane: 1,
            next_split: 0,
            next_tab: 2,
            screen: Rect::new(0, 0, 80, 24),
            areas: HashMap::new(),
            inner_areas: HashMap::new(),
            dividers: Vec::new(),
            status_y: 0,
            status_segs: Vec::new(),
            dragging: None,
            selection: None,
            menu: None,
            confirm: None,
            rename: None,
            palette: None,
            suggest: None,
            suggest_rect: None,
            suggest_dismissed_for: None,
            prefix_active: false,
            should_quit: false,
        }
    }

    fn set_input(app: &mut App, s: &str) {
        let p = app.panes.get_mut(&PaneId(0)).unwrap();
        p.cwd = "/x".into();
        p.input = InputLine {
            cursor: s.chars().count(),
            buffer: s.to_string(),
        };
    }

    /// Accept đặt `suggest_dismissed_for`, và popup KHÔNG tự mở lại cho lệnh vừa
    /// accept (để Enter kế tiếp chạy lệnh); nhưng mở lại khi buffer đổi khác.
    #[test]
    fn suggest_accept_then_no_reopen() {
        let mut app = one_pane_app();
        app.history.record("echo hello", "/x", 0, 1);
        app.history.record("echo hello world", "/x", 0, 1);

        set_input(&mut app, "echo he");
        rebuild_suggest(&mut app);
        assert!(app.suggest.is_some(), "popup phải mở khi gõ khớp");

        suggest_accept(&mut app);
        assert!(app.suggest.is_none(), "accept phải đóng popup");
        let dismissed = app.suggest_dismissed_for.clone().expect("đã đặt dismissed");

        // Shell echo lại lệnh đầy đủ → buffer == dismissed → KHÔNG mở lại.
        set_input(&mut app, &dismissed);
        rebuild_suggest(&mut app);
        assert!(app.suggest.is_none(), "không được tự mở lại cho lệnh vừa accept");

        // Gõ khác đi → dismissed hết hiệu lực → popup mở lại.
        set_input(&mut app, "echo he");
        rebuild_suggest(&mut app);
        assert!(app.suggest.is_some(), "buffer đổi khác thì popup mở lại");
    }

    /// Pane đơn phải nằm ngay dưới tabbar (2 hàng) và **chạm đáy** — không padding.
    #[test]
    fn single_pane_fills_below_tabbar() {
        let mut app = one_pane_app();
        recompute(&mut app, Rect::new(0, 0, 80, 24));
        let rect = app.areas[&PaneId(0)];
        assert_eq!(rect.y, 2, "pane phải bắt đầu ngay dưới tabbar 2 hàng");
        assert_eq!(rect.y + rect.height, 24, "pane phải chạm đáy màn hình");
        assert_eq!(app.status_y, 1, "hàng tên tab nằm ở row 1 (dưới hàng viền top)");
    }

    #[test]
    fn shell_integration_env_only_for_zsh() {
        let integ = ShellIntegration::setup().unwrap();
        // zsh → có ZDOTDIR; bash → rỗng (chưa hỗ trợ)
        let env = integ.env_for("/bin/zsh");
        assert!(!env.is_empty());
        assert!(integ.env_for("/bin/bash").is_empty());
        // .zshenv được ghi ra thư mục ZDOTDIR
        let zdot = &env.iter().find(|(k, _)| k == "ZDOTDIR").unwrap().1;
        assert!(std::path::Path::new(zdot).join(".zshenv").exists());
    }

    /// End-to-end: zsh thật + hook OSC 133 → OscScanner bắt được lệnh + exit code.
    /// Dùng ZDOTDIR tối giản (chỉ hook) để deterministic, không nạp config user.
    #[test]
    fn zsh_integration_captures_command() {
        if !std::path::Path::new("/bin/zsh").exists() {
            eprintln!("bỏ qua: không có /bin/zsh");
            return;
        }
        let dir = std::env::temp_dir().join(format!("termul-test-zdot-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let hooks = "autoload -Uz add-zsh-hook\n\
            _tc_pre() { printf '\\e]1337;TermulCmd=%s;%s\\a' \"$(print -rn -- \"$1\" | base64 | tr -d '\\n')\" \"$(print -rn -- \"$PWD\" | base64 | tr -d '\\n')\"; }\n\
            _tc_post() { printf '\\e]1337;TermulEnd=%d\\a' \"$?\"; }\n\
            add-zsh-hook preexec _tc_pre\n\
            add-zsh-hook precmd _tc_post\n";
        std::fs::write(dir.join(".zshenv"), hooks).unwrap();

        let (tx, rx) = mpsc::channel();
        let env = vec![("ZDOTDIR".to_string(), dir.to_string_lossy().to_string())];
        let mut pty = PtySession::spawn(PaneId(0), 24, 80, "/bin/zsh", &env, tx).unwrap();
        std::thread::sleep(Duration::from_millis(600));
        pty.write(b"echo tc_probe\n");

        let mut scanner = OscScanner::default();
        let mut events = Vec::new();
        let (mut got_start, mut got_end) = (false, false);
        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline && !(got_start && got_end) {
            if let Ok(AppEvent::PtyData(_, bytes)) = rx.recv_timeout(Duration::from_millis(200)) {
                scanner.scan(&bytes, &mut events);
                for ev in events.drain(..) {
                    match ev {
                        OscEvent::CommandStart { cmd, .. } if cmd == "echo tc_probe" => {
                            got_start = true;
                        }
                        OscEvent::CommandEnd { exit } if got_start => {
                            assert_eq!(exit, 0);
                            got_end = true;
                        }
                        _ => {}
                    }
                }
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(got_start, "không nhận CommandStart từ zsh");
        assert!(got_end, "không nhận CommandEnd từ zsh");
    }

    /// End-to-end: hook zle `line-pre-redraw` báo `$BUFFER` mỗi keystroke →
    /// OscScanner nhận `BufferUpdate` (cơ sở cho popup autocomplete).
    #[test]
    fn zsh_reports_input_buffer_live() {
        if !std::path::Path::new("/bin/zsh").exists() {
            eprintln!("bỏ qua: không có /bin/zsh");
            return;
        }
        let dir =
            std::env::temp_dir().join(format!("termul-test-buf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Cài hook zle TRỄ (trong precmd, sau khi zle sẵn sàng) và ghi /dev/tty.
        let hooks = "autoload -Uz add-zsh-hook add-zle-hook-widget\n\
            _tc_buf() { printf '\\e]1337;TermulBuf=%d;%s\\a' \"$CURSOR\" \"$(print -rn -- \"$BUFFER\" | base64 | tr -d '\\n')\" >/dev/tty; }\n\
            _tc_init() { add-zsh-hook -d precmd _tc_init; add-zle-hook-widget line-pre-redraw _tc_buf; }\n\
            add-zsh-hook precmd _tc_init\n";
        std::fs::write(dir.join(".zshenv"), hooks).unwrap();

        let (tx, rx) = mpsc::channel();
        let env = vec![("ZDOTDIR".to_string(), dir.to_string_lossy().to_string())];
        let mut pty = PtySession::spawn(PaneId(0), 24, 80, "/bin/zsh", &env, tx).unwrap();
        std::thread::sleep(Duration::from_millis(600));
        pty.write(b"ls"); // gõ 2 ký tự, KHÔNG Enter

        let mut scanner = OscScanner::default();
        let mut events = Vec::new();
        let mut saw_ls = false;
        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline && !saw_ls {
            if let Ok(AppEvent::PtyData(_, bytes)) = rx.recv_timeout(Duration::from_millis(200)) {
                scanner.scan(&bytes, &mut events);
                for ev in events.drain(..) {
                    if let OscEvent::BufferUpdate { cursor, buffer } = ev
                        && buffer == "ls"
                    {
                        assert_eq!(cursor, 2);
                        saw_ls = true;
                    }
                }
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(saw_ls, "không nhận BufferUpdate 'ls' từ zsh");
    }

    /// Kiểm chứng pipeline lõi mà không cần tty:
    /// spawn shell trong PTY → thread đọc đẩy bytes có gắn PaneId → vt100 parse.
    /// Dùng `$((6*7))` để chứng minh shell *thực thi thật* (output "RESULT=42").
    #[test]
    fn pty_output_parses_through_emulator() {
        let (tx, rx) = mpsc::channel();
        let pid = PaneId(7);
        let mut pty = PtySession::spawn(pid, 24, 80, "/bin/sh", &[], tx).expect("spawn pty");

        std::thread::sleep(Duration::from_millis(300));
        pty.write(b"echo RESULT=$((6*7))\n");

        let mut grid = TermGrid::new(24, 80);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if grid.screen().contents().contains("RESULT=42") {
                return;
            }
            if Instant::now() > deadline {
                panic!(
                    "timeout: không thấy 'RESULT=42'.\n--- screen ---\n{}",
                    grid.screen().contents()
                );
            }
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(AppEvent::PtyData(p, bytes)) => {
                    assert_eq!(p, pid);
                    grid.process(&bytes);
                }
                Ok(_) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    panic!("PTY đóng sớm.\n{}", grid.screen().contents())
                }
            }
        }
    }

    #[test]
    fn encode_key_basic() {
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Some(vec![b'\r'])
        );
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(vec![0x03])
        );
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)),
            Some(vec![b'a'])
        );
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)),
            Some(b"\x1b[D".to_vec())
        );
    }

    #[test]
    fn popup_rect_clamps_within_screen() {
        let s = Rect::new(0, 0, 80, 24);
        // Click gần góc phải-dưới → popup phải dịch vào trong.
        let r = popup_rect(s, 78, 23, 15, 5);
        assert!(r.x + r.width <= 80, "tràn phải: {r:?}");
        assert!(r.y + r.height <= 24, "tràn dưới: {r:?}");
        assert_eq!((r.width, r.height), (15, 5));
        // Click bình thường → popup mở ngay tại con trỏ.
        let r = popup_rect(s, 10, 5, 15, 5);
        assert_eq!((r.x, r.y), (10, 5));
    }

    fn dummy_menu(rect: Rect) -> ContextMenu {
        ContextMenu {
            rect,
            items: vec![
                MenuEntry {
                    label: "New tab",
                    action: MenuAction::NewTab,
                },
                MenuEntry {
                    label: "Rename",
                    action: MenuAction::RenameTab(0),
                },
                MenuEntry {
                    label: "Close",
                    action: MenuAction::CloseTab(0),
                },
            ],
            hovered: 0,
        }
    }

    #[test]
    fn menu_item_hit_test() {
        let menu = dummy_menu(Rect::new(5, 5, 15, 5));
        let inner = Block::bordered().inner(menu.rect); // x=6,y=6
        // Ba dòng mục ↔ ba action.
        assert_eq!(menu_item_at(&menu, inner.x, inner.y), Some(0));
        assert_eq!(menu_item_at(&menu, inner.x + 1, inner.y + 1), Some(1));
        assert_eq!(menu_item_at(&menu, inner.x, inner.y + 2), Some(2));
        // Trên viền → không trúng mục.
        assert_eq!(menu_item_at(&menu, menu.rect.x, menu.rect.y), None);
        // Ngoài popup → None.
        assert_eq!(menu_item_at(&menu, 0, 0), None);
    }

    #[test]
    fn status_segs_layout() {
        let tabs = vec![
            Tab {
                name: "1".into(),
                layout: Layout::Leaf(PaneId(0)),
                focus: PaneId(0),
            },
            Tab {
                name: "2".into(),
                layout: Layout::Leaf(PaneId(1)),
                focus: PaneId(1),
            },
        ];
        let segs = build_status_segs(&tabs, 1, 80, TAB_MIN_W);
        // 2 tab → 2 đoạn Switch và nút thêm tab ở cuối.
        assert_eq!(segs.len(), 3);
        // đoạn đầu là Switch(0) tại x=0, không active, rộng >= TAB_MIN_W
        assert!(matches!(segs[0].kind, StatusKind::Switch(0)));
        assert_eq!(segs[0].x, 0);
        assert!(!segs[0].active);
        assert!(segs[0].text.chars().count() as u16 >= TAB_MIN_W);
        // tab index 1 đang active
        assert!(matches!(segs[1].kind, StatusKind::Switch(1)));
        assert!(segs[1].active);
        assert!(matches!(segs[2].kind, StatusKind::NewTab));
        assert_eq!(segs[2].text, " + ");
        // x tăng dần
        assert!(segs[1].x > segs[0].x, "x phải tăng dần");
        assert!(segs[2].x > segs[1].x, "nút + phải nằm sau tab cuối");
    }

    #[test]
    fn status_segs_hide_new_tab_button_when_out_of_space() {
        let tabs = vec![Tab {
            name: "1".into(),
            layout: Layout::Leaf(PaneId(0)),
            focus: PaneId(0),
        }];
        let tab_width = crate::session::tab_label("1", TAB_MIN_W)
            .chars()
            .count() as u16;
        let segs = build_status_segs(&tabs, 0, tab_width + 1, TAB_MIN_W);
        assert_eq!(segs.len(), 1);
        assert!(matches!(segs[0].kind, StatusKind::Switch(0)));
    }

    #[test]
    fn clicking_new_tab_button_creates_and_activates_tab() {
        let mut app = one_pane_app();
        recompute(&mut app, Rect::new(0, 0, 80, 24));
        let add = app
            .status_segs
            .iter()
            .find(|seg| matches!(seg.kind, StatusKind::NewTab))
            .expect("phải có nút thêm tab");
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: add.x + 1,
            row: app.status_y,
            modifiers: KeyModifiers::NONE,
        };

        handle_mouse(&mut app, click);

        assert_eq!(app.tabs.len(), 2);
        assert_eq!(app.active_tab, 1);
    }

    #[test]
    fn confirm_dialog_centered_and_hit_test() {
        let s = Rect::new(0, 0, 80, 24);
        let r = confirm_rect(s);
        assert_eq!((r.width, r.height), (CONFIRM_W, CONFIRM_H));
        // canh giữa
        assert_eq!(r.x, (80 - CONFIRM_W) / 2);
        assert_eq!(r.y, (24 - CONFIRM_H) / 2);

        let dialog = ConfirmDialog {
            rect: r,
            selected: 0,
        };
        let inner = Block::bordered().inner(r);
        let base = inner.y + 2; // 2 hàng lựa chọn bắt đầu ở đây
        assert_eq!(confirm_option_at(&dialog, inner.x, base), Some(0));
        assert_eq!(confirm_option_at(&dialog, inner.x, base + 1), Some(1));
        // hàng thông điệp (inner.y) không phải lựa chọn
        assert_eq!(confirm_option_at(&dialog, inner.x, inner.y), None);
        // ngoài popup
        assert_eq!(confirm_option_at(&dialog, 0, 0), None);
    }

    #[test]
    fn ghostty_encodes_mouse_sgr_offsets_into_pane() {
        let inner = Rect::new(1, 1, 80, 24);
        let mut grid = TermGrid::new(inner.height, inner.width);
        grid.process(b"\x1b[?1000h\x1b[?1006h");
        let me = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 1,
            row: 1,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(grid.encode_mouse(me, inner), Some(b"\x1b[<0;1;1M".to_vec()));
        let me = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(grid.encode_mouse(me, inner), None);
    }

    #[test]
    fn mouse_selects_normally_and_shift_overrides_mouse_tracking() {
        let mut app = one_pane_app();
        let pid = PaneId(0);
        recompute(&mut app, Rect::new(0, 0, 80, 24));
        let down = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 2,
            row: 2,
            modifiers: KeyModifiers::NONE,
        };
        handle_mouse(&mut app, down);
        assert_eq!(
            app.selection.unwrap().range.anchor,
            GridPoint { row: 0, col: 2 }
        );
        handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Drag(MouseButton::Left),
                column: 5,
                row: 3,
                ..down
            },
        );
        assert_eq!(
            app.selection.unwrap().range.end,
            GridPoint { row: 1, col: 5 }
        );

        app.panes
            .get_mut(&pid)
            .unwrap()
            .grid
            .process(b"\x1b[?1000h\x1b[?1006h");
        handle_mouse(&mut app, down);
        assert!(app.selection.is_none());

        handle_mouse(
            &mut app,
            MouseEvent {
                modifiers: KeyModifiers::SHIFT,
                ..down
            },
        );
        assert!(app.selection.is_some());
    }

    #[test]
    fn wheel_scrolls_normal_pane_scrollback() {
        let mut app = one_pane_app();
        let pid = PaneId(0);
        recompute(&mut app, Rect::new(0, 0, 80, 24));
        // Nhiều newline hơn chiều cao để Ghostty có scrollback thực sự.
        app.panes
            .get_mut(&pid)
            .unwrap()
            .grid
            .process("line\n".repeat(40).as_bytes());
        assert_eq!(app.panes[&pid].grid.scrollback(), 0);

        let wheel_up = MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 0,
            row: 2,
            modifiers: KeyModifiers::NONE,
        };
        handle_mouse(&mut app, wheel_up);
        assert_eq!(app.panes[&pid].grid.scrollback(), 3);

        // Frame kế tiếp recompute cùng kích thước không được reset scrollback.
        recompute(&mut app, Rect::new(0, 0, 80, 24));
        assert_eq!(app.panes[&pid].grid.scrollback(), 3);

        let wheel_down = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            ..wheel_up
        };
        handle_mouse(&mut app, wheel_down);
        assert_eq!(app.panes[&pid].grid.scrollback(), 0);
    }
}
