# Hướng dẫn sử dụng termul

## 1. Chạy chương trình

```bash
cargo run            # chạy từ mã nguồn
# hoặc
cargo build --release && ./target/release/termul
```

Khi khởi động, termul mở **1 tab, 1 pane** chạy shell mặc định của bạn (`$SHELL`). Bạn dùng pane này như một terminal bình thường.

> **Lưu ý:** termul **không lưu phiên**. Thoát rồi mở lại luôn bắt đầu mới (1 tab / 1 pane). Chỉ có **lịch sử lệnh** là được giữ lại (để gợi ý).

***

## 2. Thiết lập tích hợp shell (zsh)

Bạn **không cần cấu hình gì thủ công**. Khi pane dùng **zsh**, termul tự động:

1. Tạo một `ZDOTDIR` tạm và nạp integration, rồi **khôi phục lại** `ZDOTDIR` gốc để `.zshrc`/`.zprofile` của bạn vẫn nạp bình thường.
2. Cài hook `preexec`/`precmd` để ghi lệnh vào lịch sử (kèm thư mục, exit code, thời lượng).
3. Báo **dòng đang gõ** theo từng phím để hiện popup gợi ý.
4. Cho phép `zsh-autosuggestions` tiếp tục hoạt động; khi accept popup, termul xóa `POSTDISPLAY` cũ để không còn ghost suffix. `Ctrl+R` của zsh vẫn dùng bình thường.

Cấu hình gốc của bạn (plugin, theme, alias…) không bị thay đổi.

**Shell không phải zsh:** vẫn chạy được như terminal thường, nhưng chưa có command memory và popup gợi ý.

**Vị trí lưu lịch sử:**

* macOS: `~/Library/Application Support/termul/history.db`

* Linux: `~/.local/share/termul/history.db`

***

## 3. Dùng chuột (mouse-first)

| Thao tác                                        | Kết quả                                            |
| ----------------------------------------------- | -------------------------------------------------- |
| **Click trái** vào một pane                     | Focus pane đó                                      |
| **Kéo** đường phân chia (divider) giữa hai pane | Thay đổi tỉ lệ (resize)                            |
| **Chuột phải** trong pane                       | Mở menu: **Split Right / Split Down / Close Pane** |
| **Click trái** vào một tab (tabbar trên cùng)   | Chuyển sang tab đó                                 |
| **Chuột phải** vào một tab                      | Mở menu: **New tab / Rename / Close**              |
| **Cuộn chuột** khi popup gợi ý đang hiện        | Di chuyển mục chọn trong popup                     |
| **Click** vào một mục trong popup gợi ý         | Chấp nhận gợi ý đó                                 |

Menu chuột phải điều hướng bằng `↑`/`↓` và `Enter`, đóng bằng `Esc` (hoặc click ra ngoài).

***

## 4. Phím tắt

termul dùng **prefix** kiểu tmux: nhấn `Ctrl+Backtick` trước, rồi nhấn phím lệnh.

### Toàn cục

| Phím     | Chức năng                                     |
| -------- | --------------------------------------------- |
| `Ctrl+Backtick` | Vào **prefix mode** (chờ phím lệnh tiếp theo) |
| `Ctrl+Q` | Thoát termul ngay lập tức                     |

### Pane (sau `Ctrl+Backtick`)

| Phím            | Chức năng                         |
| --------------- | --------------------------------- |
| `d`             | Split **trái/phải** (chia dọc)    |
| `s`             | Split **trên/dưới** (chia ngang)  |
| `x`             | Đóng pane đang focus              |
| `←` `→` `↑` `↓` | Chuyển focus sang pane theo hướng |

> Đóng pane **cuối cùng** của toàn app sẽ hiện hộp **xác nhận** thay vì thoát thẳng.

### Tab (sau `Ctrl+Backtick`)

| Phím | Chức năng            |
| ---- | -------------------- |
| `c`  | Tạo tab mới          |
| `n`  | Tab kế tiếp          |
| `p`  | Tab trước            |
| `,`  | Đổi tên tab hiện tại |
| `w`  | Đóng tab hiện tại    |

### Khác (sau `Ctrl+Backtick`)

| Phím | Chức năng                                         |
| ---- | ------------------------------------------------- |
| `r`  | Mở **history palette** (tìm kiếm mờ lịch sử lệnh) |
| `q`  | Thoát termul                                      |

***

## 5. Popup gợi ý autocomplete

Khi bạn gõ trong pane (zsh), termul tra lịch sử và hiện **popup** ngay dưới con trỏ với các lệnh đã từng chạy có **chứa** phần bạn đang gõ (không phân biệt hoa/thường), xếp theo frecency (hay dùng + gần đây + cùng thư mục được ưu tiên).

| Phím               | Chức năng                                                         |
| ------------------ | ----------------------------------------------------------------- |
| `↓` / `↑`          | Di chuyển mục chọn (có scroll khi vượt số dòng hiển thị)          |
| `Enter` hoặc `Tab` | **Chấp nhận** gợi ý đang chọn — điền cả dòng lệnh vào (chưa chạy) |
| `Esc`              | Đóng popup (không tự mở lại cho đến khi dòng gõ thay đổi)         |

Đặc điểm:

* **Tối đa 10 dòng** hiển thị cùng lúc, nạp về tối đa 50 gợi ý; có chỉ báo scroll `▲`/`▼`.

* `Enter` là **accept** (điền lệnh), không phải chạy. Nhấn `Enter` **lần nữa** để chạy lệnh vừa điền — popup sẽ không tự bật lại cho đúng dòng đó, tránh kẹt.

* Vì khớp kiểu *contains*, khi chấp nhận, termul **thay cả dòng** bằng lệnh được chọn (xoá dòng hiện tại rồi điền lại).

***

## 6. Mention file và thư mục

Trong pane dùng zsh, gõ `@` để mở popup tìm file và thư mục bên dưới **thư mục hiện tại (`cwd`)**. Phần nằm sau `@` được fuzzy match trên toàn bộ đường dẫn, vì vậy `@srcmn` có thể tìm thấy `src/main.rs`. Mention vẫn hoạt động khi con trỏ ở giữa dòng lệnh.

| Phím      | Chức năng                                      |
| --------- | ---------------------------------------------- |
| `↓` / `↑` | Di chuyển mục chọn                             |
| `Enter`   | Chèn đường dẫn đang chọn vào dòng lệnh         |
| `Esc`     | Đóng popup mention                             |
| Click     | Chọn và chèn file hoặc thư mục được click      |
| Cuộn      | Di chuyển danh sách khi popup có nhiều kết quả |

Khi chấp nhận, termul bỏ `@query` và chèn đường dẫn tương đối có tiền tố `./`, ví dụ `@src/ma` thành `./src/main.rs`. Ký tự đặc biệt được tự escape cho shell, ví dụ `./My\ Folder/file.txt`. Phần còn lại của dòng lệnh và vị trí con trỏ được giữ nguyên.

Các đường dẫn bị `.gitignore` và toàn bộ thư mục `.git/` không xuất hiện. Dotfile như `.env` hoặc `.github/` vẫn được hiển thị nếu không bị ignore.

Có thể bổ sung pattern exclude trong `config.toml`; pattern dùng cú pháp `.gitignore` và được tính tương đối từ `cwd` của pane:

```toml
[mention]
exclude = [
  "target/",
  "*.log",
  "secrets/**",
]
```

Pattern cấu hình được áp dụng bổ sung bên cạnh `.gitignore`. Pattern không hợp lệ được bỏ qua riêng lẻ và không làm hỏng các pattern còn lại.

***

## 7. History palette (`Ctrl+Backtick r`)

Bảng tìm kiếm mờ toàn bộ lịch sử lệnh:

| Phím        | Chức năng                                |
| ----------- | ---------------------------------------- |
| (gõ chữ)    | Lọc mờ (fuzzy) theo nội dung nhập        |
| `↑` / `↓`   | Chọn dòng                                |
| `Enter`     | Chấp nhận lệnh đang chọn (điền vào pane) |
| `Backspace` | Xoá ký tự lọc                            |
| `Esc`       | Đóng palette                             |

Kết quả xếp hạng theo frecency; lệnh cùng thư mục hiện tại được ưu tiên.

***

## 8. Hộp xác nhận & đổi tên

* **Xác nhận đóng pane cuối:** `y`/`Y` để thoát, `n`/`N` hoặc `Esc` để huỷ; hoặc dùng `←`/`→`/`↑`/`↓` chọn nút rồi `Enter`.

* **Đổi tên tab:** gõ tên mới, `Enter` để lưu, `Esc` để huỷ, `Backspace` để xoá.

***

## 9. Thứ tự ưu tiên xử lý input

Khi nhiều lớp giao diện cùng mở, input được xử lý theo thứ tự:

```
palette  >  rename  >  confirm  >  menu  >  popup mention  >  popup gợi ý  >  pane
```

Nghĩa là nếu palette đang mở thì phím đi vào palette; nếu không có modal nào, phím đi thẳng vào shell của pane đang focus.
