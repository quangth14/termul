#!/usr/bin/env bash
# Cài đặt termul: build bản release rồi chép binary vào thư mục trong PATH.
# Tuỳ biến thư mục đích: PREFIX=/usr/local/bin ./install.sh
set -euo pipefail

cd "$(dirname "$0")"

BIN="termul"
PREFIX="${PREFIX:-$HOME/.local/bin}"

if ! command -v cargo >/dev/null 2>&1; then
  echo "Lỗi: không tìm thấy 'cargo'. Cài Rust trước: https://rustup.rs" >&2
  exit 1
fi

if ! command -v zig >/dev/null 2>&1 || [[ "$(zig version)" != 0.15.* ]]; then
  echo "Lỗi: libghostty-vt cần Zig 0.15.x (khuyến nghị 0.15.2)." >&2
  echo "macOS: brew install zig@0.15 && brew link --force --overwrite zig@0.15" >&2
  exit 1
fi

echo "==> Build bản release…"
cargo build --release

echo "==> Cài '$BIN' vào $PREFIX"
mkdir -p "$PREFIX"
install -m 755 "target/release/$BIN" "$PREFIX/$BIN"

echo "==> Xong: $PREFIX/$BIN"

case ":$PATH:" in
  *":$PREFIX:"*) ;;
  *)
    echo
    echo "Lưu ý: '$PREFIX' chưa có trong PATH. Thêm dòng sau vào ~/.zshrc:"
    echo "  export PATH=\"$PREFIX:\$PATH\""
    ;;
esac
