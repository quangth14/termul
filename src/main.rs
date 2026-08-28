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
mod mention;
mod osc;
mod palette;
mod pty;
mod rename;
mod session;
mod shell;
mod suggest;
mod term;
mod terminal_theme;
mod ui;
mod xtgettcap;

use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture,
};
#[cfg(not(windows))]
use crossterm::event::{
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
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
use crate::terminal_theme::HostTerminalCapabilities;
use crate::ui::draw;

fn main() -> Result<()> {
    install_panic_hook();
    let mut terminal = setup_terminal()?;
    let result = run(&mut terminal);
    restore_terminal(&mut terminal)?;
    result
}

#[cfg(not(windows))]
fn push_keyboard_enhancement_flags() -> std::io::Result<()> {
    let flags = KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
        | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
        | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS;
    execute!(io::stdout(), PushKeyboardEnhancementFlags(flags))
}

#[cfg(windows)]
fn push_keyboard_enhancement_flags() -> std::io::Result<()> {
    Ok(())
}

#[cfg(not(windows))]
fn pop_keyboard_enhancement_flags() -> std::io::Result<()> {
    execute!(io::stdout(), PopKeyboardEnhancementFlags)
}

#[cfg(windows)]
fn pop_keyboard_enhancement_flags() -> std::io::Result<()> {
    Ok(())
}

/// Khôi phục terminal khi panic để không để lại raw mode / alternate screen.
fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = pop_keyboard_enhancement_flags();
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableBracketedPaste,
            DisableFocusChange,
            DisableMouseCapture
        );
        let _ = io::stdout().write_all(b"\x1b]112\x1b\\");
        original(info);
    }));
}

fn setup_terminal() -> Result<Terminal<Backend>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableFocusChange,
        EnableBracketedPaste
    )?;
    push_keyboard_enhancement_flags()?;
    let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    Ok(terminal)
}

fn restore_terminal(terminal: &mut Terminal<Backend>) -> Result<()> {
    disable_raw_mode()?;
    pop_keyboard_enhancement_flags()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableBracketedPaste,
        DisableFocusChange,
        DisableMouseCapture
    )?;
    terminal.backend_mut().write_all(b"\x1b]112\x1b\\")?;
    terminal.backend_mut().flush()?;
    terminal.show_cursor()?;
    Ok(())
}

const MIN_RENDER_INTERVAL: Duration = Duration::from_millis(16);

fn run(terminal: &mut Terminal<Backend>) -> Result<()> {
    let size = terminal.size()?;
    let area = Rect::new(0, 0, size.width, size.height);
    let (init_cols, init_rows) = grid_dims(inner_of(area));

    let (tx, rx): (Sender<AppEvent>, Receiver<AppEvent>) = mpsc::channel();
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());

    let integ = ShellIntegration::setup()?;
    let history = HistoryStore::open_default()?;
    let cfg = Config::load();
    let host_capabilities = HostTerminalCapabilities::query();
    let host_terminal_theme = host_capabilities.theme;

    let first_id = PaneId(0);
    let initial_cwd = std::env::current_dir()?.to_string_lossy().into_owned();
    let env = integ.env_for(&shell, first_id.0);
    let pty = PtySession::spawn(first_id, init_rows, init_cols, &shell, &env, tx.clone())?;
    spawn_input_thread(tx.clone());

    let mut app = App {
        panes: HashMap::from([(
            first_id,
            Pane {
                grid: TermGrid::with_host_capabilities(
                    init_rows,
                    init_cols,
                    cfg.scrollback_limit_bytes,
                    host_terminal_theme,
                    host_capabilities.cell_size,
                ),
                pty,
                osc: OscScanner::default(),
                cwd: initial_cwd,
                title: String::new(),
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
        host_terminal_theme,
        cell_pixel_size: host_capabilities.cell_size,
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
        mention: None,
        mention_rect: None,
        mention_generation: 0,
        suggest: None,
        suggest_rect: None,
        suggest_dismissed_for: None,
        prefix_active: false,
        should_quit: false,
    };

    draw(terminal, &mut app)?;

    let mut last_render_at = Instant::now();
    let mut needs_render = false;
    let mut force_render = false;
    loop {
        let synchronized_output = app
            .panes
            .get(&app.tabs[app.active_tab].focus)
            .is_some_and(|pane| pane.grid.synchronized_output());
        let wait = render_wait(
            needs_render,
            force_render,
            synchronized_output,
            last_render_at.elapsed(),
        );
        let event = match wait {
            Some(wait) => match rx.recv_timeout(wait) {
                Ok(event) => Some(event),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            },
            None => match rx.recv() {
                Ok(event) => Some(event),
                Err(_) => break,
            },
        };

        if let Some(event) = event {
            force_render |= !matches!(&event, AppEvent::PtyData(_, _));
            needs_render = true;
            handle_event(&mut app, event);
            while let Ok(event) = rx.try_recv() {
                force_render |= !matches!(&event, AppEvent::PtyData(_, _));
                handle_event(&mut app, event);
            }
        }
        if app.should_quit {
            break;
        }

        let synchronized_output = app
            .panes
            .get(&app.tabs[app.active_tab].focus)
            .is_some_and(|pane| pane.grid.synchronized_output());
        if render_wait(
            needs_render,
            force_render,
            synchronized_output,
            last_render_at.elapsed(),
        ) == Some(Duration::ZERO)
        {
            draw(terminal, &mut app)?;
            last_render_at = Instant::now();
            needs_render = false;
            force_render = false;
        }
    }

    Ok(())
}

fn render_wait(
    needs_render: bool,
    force_render: bool,
    synchronized_output: bool,
    elapsed: Duration,
) -> Option<Duration> {
    if !needs_render || (synchronized_output && !force_render) {
        None
    } else {
        Some(MIN_RENDER_INTERVAL.saturating_sub(elapsed))
    }
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
    use crate::config::{Config, DEFAULT_SCROLLBACK_LIMIT_BYTES};
    use crate::confirm::{confirm_option_at, confirm_rect};
    use crate::event::handle_osc;
    use crate::history::HistoryStore;
    use crate::input::{handle_key, handle_mouse};
    use crate::layout::{Layout, PaneId};
    use crate::menu::{menu_item_at, popup_rect};
    use crate::osc::{OscEvent, OscScanner};
    use crate::pty::PtySession;
    use crate::session::{build_status_segs, recompute};
    use crate::shell::ShellIntegration;
    use crate::suggest::{rebuild_suggest, suggest_accept};
    use crate::term::{GridPoint, TermGrid};
    use crate::terminal_theme::HostTerminalTheme;
    use crate::{MIN_RENDER_INTERVAL, render_wait};

    #[test]
    fn delete_removes_selection_from_current_zle_input() {
        let mut app = one_pane_app();
        let pid = PaneId(0);
        app.panes.get_mut(&pid).unwrap().grid.process(b"$ abcdef");
        set_input(&mut app, "abcdef");
        app.selection = Some(PaneSelection {
            pane: pid,
            range: crate::term::GridSelection {
                anchor: GridPoint { row: 0, col: 3 },
                end: GridPoint { row: 0, col: 4 },
            },
        });

        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE),
        );

        assert!(app.selection.is_none());
    }

    #[test]
    fn delete_removes_multiline_selection_from_current_zle_input() {
        let mut app = one_pane_app();
        let pid = PaneId(0);
        app.panes
            .get_mut(&pid)
            .unwrap()
            .grid
            .process(b"$ apksigner sign \\\r\n  --ks release.jks \\\r\n  --out app.apk");
        set_input(
            &mut app,
            "apksigner sign \\\n  --ks release.jks \\\n  --out app.apk",
        );
        app.selection = Some(PaneSelection {
            pane: pid,
            range: crate::term::GridSelection {
                anchor: GridPoint { row: 0, col: 2 },
                end: GridPoint { row: 2, col: 14 },
            },
        });

        handle_key(&mut app, KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));

        assert!(app.selection.is_none());
    }

    #[test]
    fn click_without_drag_moves_zle_cursor_inside_multiline_input() {
        let mut app = one_pane_app();
        let pid = PaneId(0);
        app.panes
            .get_mut(&pid)
            .unwrap()
            .grid
            .process(b"$ ab \\\r\n  cd");
        set_input(&mut app, "ab \\\n  cd");
        let screen = app.screen;
        recompute(&mut app, screen);
        let inner = app.inner_areas[&pid];
        let click = |kind| MouseEvent {
            kind,
            column: inner.x + 3,
            row: inner.y + 1,
            modifiers: KeyModifiers::NONE,
        };

        handle_mouse(&mut app, click(MouseEventKind::Down(MouseButton::Left)));
        assert!(app.selection.is_some());
        handle_mouse(&mut app, click(MouseEventKind::Up(MouseButton::Left)));

        assert!(
            app.selection.is_none(),
            "click không kéo phải đặt con trỏ, không để selection"
        );
    }

    #[test]
    fn render_scheduler_coalesces_frames_and_respects_sync_output() {
        assert_eq!(render_wait(false, false, false, Duration::ZERO), None);
        assert_eq!(
            render_wait(true, false, false, Duration::ZERO),
            Some(MIN_RENDER_INTERVAL)
        );
        assert_eq!(
            render_wait(true, false, false, MIN_RENDER_INTERVAL),
            Some(Duration::ZERO)
        );
        assert_eq!(render_wait(true, false, true, MIN_RENDER_INTERVAL), None);
        assert_eq!(
            render_wait(true, true, true, MIN_RENDER_INTERVAL),
            Some(Duration::ZERO)
        );
    }

    /// Dựng một App tối thiểu với đúng 1 pane để test layout.
    fn one_pane_app() -> App {
        let (tx, _rx) = mpsc::channel();
        let pid = PaneId(0);
        let pty = PtySession::spawn(pid, 24, 80, "/bin/sh", &[], tx.clone()).expect("spawn pty");
        App {
            panes: HashMap::from([(
                pid,
                Pane {
                    grid: TermGrid::new(24, 80, DEFAULT_SCROLLBACK_LIMIT_BYTES),
                    pty,
                    osc: OscScanner::default(),
                    cwd: String::new(),
                    title: String::new(),
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
            host_terminal_theme: HostTerminalTheme::default(),
            cell_pixel_size: Default::default(),
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
            mention: None,
            mention_rect: None,
            mention_generation: 0,
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

        // Shell báo buffer đã accept → popup vẫn không được tự mở lại.
        handle_osc(
            &mut app,
            PaneId(0),
            OscEvent::BufferUpdate {
                cursor: dismissed.chars().count(),
                buffer: dismissed.clone(),
                cwd: "/x".into(),
            },
        );
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
        let env = integ.env_for("/bin/zsh", 0);
        assert!(!env.is_empty());
        assert!(integ.env_for("/bin/bash", 0).is_empty());
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
            _tc_buf() { printf '\\e]1337;TermulBuf=%d;%s;%s\\a' \"$CURSOR\" \"$(print -rn -- \"$BUFFER\" | base64 | tr -d '\\n')\" \"$(print -rn -- \"$PWD\" | base64 | tr -d '\\n')\" >/dev/tty; }\n\
            _tc_edit() { local cursor buffer; { IFS= read -r cursor; IFS= read -r -d '' buffer; } <\"$TERMUL_EDIT_FILE\"; printf '\\e[?2026h' >/dev/tty; BUFFER=\"$buffer\"; CURSOR=\"$cursor\"; POSTDISPLAY=''; zle redisplay; printf '\\e[?2026l' >/dev/tty; }\n\
            _tc_init() { add-zsh-hook -d precmd _tc_init; add-zle-hook-widget line-pre-redraw _tc_buf; zle -N _tc_edit; bindkey -M emacs $'\\e[99~' _tc_edit; bindkey -M viins $'\\e[99~' _tc_edit; }\n\
            add-zsh-hook precmd _tc_init\n";
        std::fs::write(dir.join(".zshenv"), hooks).unwrap();

        let (tx, rx) = mpsc::channel();
        let env = vec![
            ("ZDOTDIR".to_string(), dir.to_string_lossy().to_string()),
            (
                "TERMUL_EDIT_FILE".to_string(),
                dir.join("edit").to_string_lossy().to_string(),
            ),
        ];
        let mut pty = PtySession::spawn(PaneId(0), 24, 80, "/bin/zsh", &env, tx).unwrap();
        std::thread::sleep(Duration::from_millis(600));
        pty.write(b"abcdefgh"); // gõ một dòng dài, KHÔNG Enter

        let mut scanner = OscScanner::default();
        let mut events = Vec::new();
        let mut raw_output = Vec::new();
        let mut saw_initial = false;
        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline && !saw_initial {
            if let Ok(AppEvent::PtyData(_, bytes)) = rx.recv_timeout(Duration::from_millis(200)) {
                raw_output.extend_from_slice(&bytes);
                scanner.scan(&bytes, &mut events);
                for ev in events.drain(..) {
                    if let OscEvent::BufferUpdate {
                        cursor,
                        buffer,
                        cwd,
                    } = ev
                        && buffer == "abcdefgh"
                    {
                        assert_eq!(cursor, 8);
                        assert!(!cwd.is_empty());
                        saw_initial = true;
                    }
                }
            }
        }
        assert!(saw_initial, "không nhận BufferUpdate từ zsh");

        // Widget termul cập nhật buffer và cursor nguyên tử trong một lần.
        std::fs::write(dir.join("edit"), "1\nxy").unwrap();
        let edit_output_start = raw_output.len();
        pty.write(b"\x1b[99~");
        let mut saw_edit = false;
        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline && !saw_edit {
            if let Ok(AppEvent::PtyData(_, bytes)) = rx.recv_timeout(Duration::from_millis(200)) {
                raw_output.extend_from_slice(&bytes);
                scanner.scan(&bytes, &mut events);
                for ev in events.drain(..) {
                    if let OscEvent::BufferUpdate { cursor, buffer, .. } = ev
                        && buffer == "xy"
                        && cursor == 1
                    {
                        saw_edit = true;
                    }
                }
            }
        }
        while let Ok(AppEvent::PtyData(_, bytes)) =
            rx.recv_timeout(Duration::from_millis(200))
        {
            raw_output.extend_from_slice(&bytes);
        }
        let edit_output = &raw_output[edit_output_start..];
        let sync_start = edit_output
            .windows(b"\x1b[?2026h".len())
            .position(|bytes| bytes == b"\x1b[?2026h")
            .expect("edit phải bật synchronized output");
        let sync_end = edit_output
            .windows(b"\x1b[?2026l".len())
            .position(|bytes| bytes == b"\x1b[?2026l")
            .expect("edit phải tắt synchronized output");
        assert!(sync_start < sync_end);

        let mut rendered = TermGrid::new(24, 80, DEFAULT_SCROLLBACK_LIMIT_BYTES);
        rendered.process(&raw_output);
        let contents = rendered.screen().contents();
        assert!(contents.contains("% xy"), "không vẽ buffer mới: {contents:?}");
        assert!(
            !contents.contains("xycdefgh"),
            "còn ghost text của buffer cũ: {contents:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            saw_edit,
            "zsh không áp dụng edit selection: {}",
            String::from_utf8_lossy(&raw_output)
        );
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

        let mut grid = TermGrid::new(24, 80, DEFAULT_SCROLLBACK_LIMIT_BYTES);
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
        let mut grid = TermGrid::new(24, 80, DEFAULT_SCROLLBACK_LIMIT_BYTES);
        assert_eq!(
            grid.encode_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Some(vec![b'\r'])
        );
        assert_eq!(
            grid.encode_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(vec![0x03])
        );
        assert_eq!(
            grid.encode_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)),
            Some(vec![b'a'])
        );
        assert_eq!(
            grid.encode_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)),
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
        let mut grid = TermGrid::new(inner.height, inner.width, DEFAULT_SCROLLBACK_LIMIT_BYTES);
        grid.process(b"\x1b[?1000h\x1b[?1006h");
        let me = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 12,
            row: 10,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(
            grid.encode_mouse(me, inner),
            Some(b"\x1b[<0;12;10M".to_vec())
        );

        // Crossterm không có pixel chính xác: yêu cầu 1016 phải hạ xuống SGR
        // theo ô thay vì chia sai tọa độ cell cho kích thước pixel.
        grid.process(b"\x1b[?1016h");
        assert_eq!(
            grid.encode_mouse(me, inner),
            Some(b"\x1b[<0;12;10M".to_vec())
        );
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
