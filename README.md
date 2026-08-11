# termul

**termul** là một *terminal multiplexer* viết bằng Rust, chạy trong terminal — lấy cảm hứng từ herdr. Nó cho phép chia nhiều **pane**, quản lý nhiều **tab**, và điểm khác biệt chính: **ghi nhớ các lệnh đã chạy** rồi **gợi ý autocomplete** ngay khi bạn gõ (popup kiểu VSCode).

Triết lý thiết kế:

- **Mouse-first** — thao tác chính bằng chuột (click focus, kéo resize, chuột phải mở menu), phím tắt là bổ trợ.
- **Không persistence** — thoát rồi mở lại là một phiên hoàn toàn mới, không lưu trạng thái layout/tab/pane. *Ngoại lệ:* lịch sử lệnh được lưu lâu dài trong SQLite để phục vụ gợi ý.

---

## Tính năng

| Nhóm | Mô tả |
|------|-------|
| **Pane** | Chia dọc/ngang theo cây nhị phân (tiling), click chọn, kéo divider để resize, chuột phải để split/close |
| **Tab** | Tabbar ở trên cùng (tab active nền tím mauve), tạo/đổi tên/đóng tab, chuột phải mở menu |
| **Command memory** | Mỗi lệnh chạy xong được ghi vào SQLite kèm `cwd`, exit code, thời lượng — qua shell integration OSC |
| **Autocomplete popup** | Gõ tới đâu gợi ý tới đó; khớp kiểu *contains*, xếp hạng theo **frecency** (tần suất × độ mới × ưu tiên cùng thư mục) |
| **History palette** | Bảng tìm kiếm mờ (fuzzy) toàn bộ lịch sử, mở bằng `Ctrl+B r` |

---

## Yêu cầu

- **Rust** (edition 2024) + Cargo. Khuyến nghị bản mới (đã thử với 1.94.x).
- **Shell**: tích hợp đầy đủ (command memory + popup gợi ý) hiện chỉ hỗ trợ **zsh**. Shell khác vẫn chạy được như terminal thường nhưng không có ghi nhớ/gợi ý.
- macOS hoặc Linux.

## Cài đặt & chạy

```bash
# Build bản tối ưu
cargo build --release

# Hoặc chạy trực tiếp
cargo run
```

Binary sau khi build nằm ở `target/release/termul`.

## Nó hoạt động thế nào (tóm tắt)

- Mỗi pane là một **PTY** spawn shell của bạn (`$SHELL`), giả lập VT bằng `vt100`, render bằng `ratatui` + `crossterm`.
- Với zsh, termul nạp **shell integration** tự động qua một `ZDOTDIR` tạm (không đụng cấu hình gốc của bạn). Integration phát các chuỗi OSC báo: lệnh bắt đầu chạy, exit code, và **nội dung dòng đang gõ** — nhờ đó popup gợi ý hiện được ngay lúc gõ.
- Trong pane termul, gợi ý inline của **zsh-autosuggestions** được tắt để tránh chồng gợi ý (đặt `_ZSH_AUTOSUGGEST_DISABLED=1`).
- Lịch sử lưu ở `<data_dir>/termul/history.db`:
  - macOS: `~/Library/Application Support/termul/history.db`
  - Linux: `~/.local/share/termul/history.db`

Xem **[USAGE.md](USAGE.md)** để biết chi tiết cách dùng và toàn bộ phím tắt.

## Cấu trúc mã nguồn

| File | Vai trò |
|------|---------|
| `src/main.rs` | Entry point: setup terminal, vòng lặp sự kiện (mpsc), spawn input/PTY thread, integration test |
| `src/app.rs` | `App` — state trung tâm (panes, tabs, layout, modal, config), `AppEvent`, `Pane`, `Tab` |
| `src/event.rs` | Điều phối `AppEvent`: dữ liệu PTY, marker OSC, phím/chuột |
| `src/input.rs` | Định tuyến phím/chuột, prefix mode kiểu tmux, encode key/mouse ra PTY |
| `src/session.rs` | Vòng đời pane/tab (split/close/switch), `recompute` layout, điều hướng focus |
| `src/ui.rs` | `draw`: vẽ tabbar, panes và các lớp modal theo thứ tự z |
| `src/layout.rs` | Cây tiling nhị phân (split/close/resize/focus) |
| `src/pty.rs` | Spawn shell trong PTY + thread đọc output |
| `src/term.rs` | Bọc `vt100` thành grid + widget render cho ratatui |
| `src/osc.rs` | Máy trạng thái quét chuỗi OSC do shell integration phát |
| `src/shell.rs` | Nạp shell integration cho zsh qua ZDOTDIR tạm |
| `src/history.rs` | SQLite: ghi lệnh, gợi ý (contains + frecency), tìm kiếm mờ cho palette |
| `src/config.rs` | Nạp `config.toml` (appearance/behavior/keys); field thiếu/sai dùng default |
| `src/palette.rs` `suggest.rs` `menu.rs` `confirm.rs` `rename.rs` | Các lớp overlay/modal: mỗi file tự quản state + xử lý phím/chuột + render |

## Trạng thái

Dự án đang phát triển. Đã hoàn thành: đa pane tiling, tab + tabbar, command memory, palette lịch sử (có xóa entry bằng `Del`), popup autocomplete có scroll.
