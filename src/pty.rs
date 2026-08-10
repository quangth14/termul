//! Spawn shell trong PTY và đọc output qua một thread nền.

use std::io::{Read, Write};
use std::sync::mpsc::Sender;
use std::thread;

use anyhow::Result;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

use crate::layout::PaneId;
use crate::app::AppEvent;

/// Một phiên PTY: giữ master để ghi/resize, child process, và kích thước hiện tại.
pub struct PtySession {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    _child: Box<dyn Child + Send + Sync>,
    size: (u16, u16), // (rows, cols)
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
            size: (rows, cols),
        })
    }

    /// Ghi bytes vào shell (input người dùng).
    pub fn write(&mut self, data: &[u8]) {
        let _ = self.writer.write_all(data);
        let _ = self.writer.flush();
    }

    /// Resize PTY khi vùng hiển thị đổi (gửi SIGWINCH tới shell).
    pub fn resize(&mut self, rows: u16, cols: u16) {
        if (rows, cols) != self.size && rows > 0 && cols > 0 {
            let _ = self.master.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
            self.size = (rows, cols);
        }
    }
}
