//! Spawn shell trong PTY và đọc output qua một thread nền.

use std::io::{Read, Write};
use std::sync::mpsc::Sender;
use std::thread;

use anyhow::Result;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

use crate::app::AppEvent;
use crate::layout::PaneId;
use crate::terminal_theme::CellPixelSize;

/// Một phiên PTY: giữ master để ghi/resize, child process, và kích thước hiện tại.
pub struct PtySession {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    _child: Box<dyn Child + Send + Sync>,
    size: (u16, u16, u16, u16), // (rows, cols, pixel_width, pixel_height)
}

impl PtySession {
    /// Mở PTY, spawn shell, và khởi động thread đọc output đẩy vào `tx` (gắn `pane_id`).
    pub fn spawn(
        pane_id: PaneId,
        rows: u16,
        cols: u16,
        shell: &str,
        extra_env: &[(String, String)],
        tx: Sender<AppEvent>,
    ) -> Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let mut cmd = CommandBuilder::new(shell);
        cmd.env("TERM", "xterm-256color");
        // TermGrid/vt100 và CrosstermBackend đều giữ được màu RGB 24-bit. Khai báo
        // rõ capability này để TUI khách không tự hạ bảng màu xuống ANSI-256.
        cmd.env("COLORTERM", "truecolor");
        // Không để app khách nhận nhầm mình đang chạy trực tiếp trong VS Code,
        // Kitty... từ TERM_PROGRAM được kế thừa của terminal cha.
        cmd.env("TERM_PROGRAM", "termul");
        cmd.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        if let Ok(cwd) = std::env::current_dir() {
            cmd.cwd(cwd);
        }
        let child = pair.slave.spawn_command(cmd)?;
        // Đóng slave ở tiến trình cha để nhận EOF khi shell thoát.
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;

        thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => {
                        let _ = tx.send(AppEvent::PtyClosed(pane_id));
                        break;
                    }
                    Ok(n) => {
                        if tx
                            .send(AppEvent::PtyData(pane_id, buf[..n].to_vec()))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(_) => {
                        let _ = tx.send(AppEvent::PtyClosed(pane_id));
                        break;
                    }
                }
            }
        });

        Ok(Self {
            master: pair.master,
            writer,
            _child: child,
            size: (rows, cols, 0, 0),
        })
    }

    /// Ghi bytes vào shell (input người dùng).
    pub fn write(&mut self, data: &[u8]) {
        let _ = self.writer.write_all(data);
        let _ = self.writer.flush();
    }

    /// Resize PTY khi vùng hiển thị đổi (gửi SIGWINCH tới shell).
    pub fn resize(&mut self, rows: u16, cols: u16, cell_size: CellPixelSize) {
        let pixel_width = u32::from(cols)
            .saturating_mul(cell_size.width)
            .min(u32::from(u16::MAX)) as u16;
        let pixel_height = u32::from(rows)
            .saturating_mul(cell_size.height)
            .min(u32::from(u16::MAX)) as u16;
        if (rows, cols, pixel_width, pixel_height) != self.size && rows > 0 && cols > 0 {
            let _ = self.master.resize(PtySize {
                rows,
                cols,
                pixel_width,
                pixel_height,
            });
            self.size = (rows, cols, pixel_width, pixel_height);
        }
    }
}
