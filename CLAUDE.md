This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

**termul** là một terminal multiplexer viết bằng Rust (edition 2024), chạy trong terminal, lấy cảm hứng từ herdr/tmux. Điểm khác biệt chính: ghi nhớ lệnh đã chạy vào SQLite rồi gợi ý autocomplete kiểu popup VSCode ngay khi gõ. Toàn bộ comment/doc trong repo viết bằng tiếng Việt — giữ nguyên quy ước này khi thêm code.

## Lệnh thường dùng

```bash
cargo run                 # chạy từ mã nguồn (mở 1 tab / 1 pane với $SHELL)
cargo build --release     # binary ở target/release/termul
cargo test                # unit test (cạnh module, tập trung ở history.rs) + integration test
cargo test suggest_contains   # chạy một test cụ thể theo tên
./install.sh              # build release + cài vào ~/.local/bin (đổi đích: PREFIX=... ./install.sh)
```

Không có linter/CI riêng; dùng `cargo clippy` và `cargo fmt` theo mặc định Rust. Lưu ý `main.rs` chứa **integration test end-to-end chạy zsh thật qua PTY** (~L174-577) — chậm và cần zsh; unit test nhỏ đặt cạnh từng module.

## Kiến trúc tổng thể

Ứng dụng chạy theo mô hình **một event loop trên main thread + các thread phụ đẩy sự kiện qua `mpsc`**. Hiểu được app cần đọc chuỗi: `main.rs` → `event.rs` → `input.rs`/`osc.rs`.

- **Event loop** (`main.rs`, `fn main` ~L50, vòng lặp `while let Ok(ev) = rx.recv()` ~L150): main thread blocking-`recv` một `AppEvent`, drain thêm bằng `try_recv`, gọi `handle_event`, rồi vẽ lại bằng `ui::draw`. Có một input thread (`spawn_input_thread`, đọc crossterm event) và một reader thread **mỗi PTY** (spawn trong `pty.rs`) — tất cả gửi `AppEvent` về cùng channel. Phần đầu `main.rs` còn có logic warm-up shell integration với `recv_timeout` (chờ PTY ổn định trước khi vào loop).

- **`AppEvent`** (`app.rs`): 3 nhánh chính — `PtyData(PaneId, bytes)`, `PtyClosed(PaneId)`, `Term(crossterm::Event)`. `event.rs::handle_event` phân phối: PtyData → nạp vào `grid` (vt100) + quét OSC; PtyClosed → `do_close`; Key/Mouse → `input.rs`.

- **`App` struct** (`app.rs`): state trung tâm và là "source of truth" cho toàn bộ state. Quan trọng: `panes: HashMap<PaneId, Pane>`; `tabs: Vec<Tab>` + `active_tab` — **mỗi `Tab` giữ `layout` (cây tiling) và `focus: PaneId` riêng**; cache layout lần vẽ gần nhất để hit-test chuột (`areas`, `dividers`, `status_segs`); và các lớp modal độc lập — `palette`, `rename`, `confirm`, `menu`, `suggest` (popup gợi ý) — cùng `prefix_active`, `cfg: Config`, `history: HistoryStore`. Mỗi `Pane` giữ `grid: TermGrid`, `osc: OscScanner`, `cwd`, `pending: Option<PendingCmd>`, và `input: InputLine` (buffer/cursor dòng đang gõ, do shell integration cập nhật).

### Định tuyến input (quan trọng)

`input.rs::handle_key` và `handle_mouse` định tuyến theo **thứ tự ưu tiên modal cố định** — nếu lớp trên đang mở thì phím/chuột vào đó và `return`:

```
palette  >  rename  >  confirm  >  menu  >  popup gợi ý (suggest)  >  pane
```

Phím tắt dùng **prefix kiểu tmux**: nhấn prefix (mặc định `Ctrl+B`) đặt `prefix_active = true`; phím kế tiếp đi vào `handle_prefix`. Khi không có modal và không ở prefix mode, phím được `encode_key` và ghi thẳng vào PTY của pane focus. Tất cả keybind đọc từ `app.cfg.keys` (không hardcode) qua `key_matches`.

### Shell integration → command memory → gợi ý

Đây là luồng đặc trưng nhất, chỉ hỗ trợ đầy đủ với **zsh**:

1. `shell.rs::ShellIntegration` tạo một **`ZDOTDIR` tạm** nạp script integration rồi khôi phục `ZDOTDIR` gốc, nên `.zshrc` của người dùng không bị ảnh hưởng. Cũng đặt `_ZSH_AUTOSUGGEST_DISABLED=1` để tránh chồng gợi ý với zsh-autosuggestions.
2. Script phát các chuỗi **OSC** (kiểu OSC 133) khi: lệnh bắt đầu, lệnh kết thúc (kèm exit code), và **mỗi lần buffer dòng lệnh thay đổi**.
3. `osc.rs::OscScanner` là máy trạng thái quét byte PTY, sinh `OscEvent::{CommandStart, CommandEnd, BufferUpdate}`.
4. `event.rs::handle_osc` xử lý: `CommandStart` → lưu `pending` + xóa popup; `CommandEnd` → `history.record(cmd, cwd, exit, dur)`; `BufferUpdate` → cập nhật `pane.input` và (nếu là pane focus) gọi `suggest::rebuild_suggest`.

### Lịch sử & xếp hạng (`history.rs`)

SQLite tại `<data_dir>/termul/history.db` (dùng `dirs`). Bảng `commands(cmdline, cwd, exit_code, ts, duration_ms)`.

- **`suggest(query, cwd, limit)`** — cho popup autocomplete: khớp kiểu **contains** (không phân biệt hoa/thường, loại chính `query`), xếp theo `frecency`.
- **`search(query, cwd, limit)`** — cho history palette: query rỗng → xếp theo frecency; có query → fuzzy match bằng `nucleo-matcher` rồi frecency phụ.
- **`frecency(now, last_ts, cnt, cwd_match)`** (~L154): tần suất (log) × độ mới, **nhân đôi nếu cùng cwd**. Đây là logic xếp hạng chung cho cả hai đường.

Unit test của repo tập trung ở cuối `history.rs` (ví dụ `suggest_contains`) — dùng chúng làm mẫu khi đổi logic xếp hạng/khớp.

Vì match kiểu contains phải thay cả dòng, khi **accept** một gợi ý, `suggest.rs` ghi `Ctrl+A`+`Ctrl+K` (về đầu dòng + xóa tới cuối) rồi ghi lệnh đầy đủ vào PTY. `suggest_dismissed_for` chặn popup tự bật lại cho lệnh vừa accept để `Enter` kế tiếp chạy được lệnh.

### Layout tiling (`layout.rs`)

`Layout` là enum cây nhị phân: `Leaf(PaneId)` hoặc `Split { id, dir: SplitDir (LeftRight/TopBottom), first, second, ratio }`. `compute` quy đổi cây → danh sách `Rect` cho từng pane + danh sách `Divider` (để hit-test resize bằng chuột). `split_leaf` thay một leaf bằng Split mới; `set_ratio` cập nhật tỉ lệ khi kéo divider.

### Các module còn lại

- `session.rs` — thao tác mức phiên: `spawn_pane`, `do_split`/`do_close`, quản lý tab (`new_tab`/`switch_tab`/`close_tab`), `focus_dir`, `recompute` (tính lại rect + resize PTY theo grid), dựng status bar.
- `ui.rs` — `draw`: vẽ tabbar, các pane, và render các modal theo đúng thứ tự z.
- `term.rs` — bọc `vt100::Parser` thành grid + widget ratatui.
- `pty.rs` — spawn `$SHELL` trong PTY (`portable-pty`) + thread đọc output.
- `config.rs` — load `config.toml` từ config dir (`~/.config/termul/` hoặc macOS App Support); mọi field optional, giá trị sai bị bỏ qua và dùng default. Xem `config.example.toml` cho toàn bộ trường (`[appearance]`, `[behavior]`, `[keys]`).
- `palette.rs`, `menu.rs`, `confirm.rs`, `rename.rs`, `suggest.rs` — mỗi modal một file: state + xử lý phím/chuột + render riêng.

## Quy ước

- **Không persistence layout**: thoát rồi mở lại luôn là phiên mới (1 tab/1 pane). Chỉ lịch sử lệnh được lưu lâu dài.
- **Mouse-first**: click focus, kéo divider resize, chuột phải mở menu; phím tắt là bổ trợ.
- Dùng **if-let chains** của edition 2024 (`if let Some(x) = a && let Some(y) = b`) — đã có sẵn trong `palette.rs`, `input.rs`.
- Khi thêm keybind mới: thêm field vào `config.rs` (mục `[keys]`) và định tuyến trong `input.rs::handle_prefix`, **đừng hardcode phím**.
- Thêm một modal mới: tạo file riêng theo mẫu các modal hiện có, thêm field `Option<...>` vào `App`, và chèn nhánh ưu tiên vào `handle_key`/`handle_mouse` đúng vị trí trong chuỗi ưu tiên.
