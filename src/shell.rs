//! Tự động nạp shell integration (OSC 133) vào zsh qua ZDOTDIR tạm,
//! không đụng tới cấu hình gốc của người dùng.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;

/// Nội dung `.zshenv` được inject: khôi phục môi trường user rồi cài hook
/// preexec/precmd phát OSC báo lệnh + exit code + cwd cho termul.
static NEXT_INTEGRATION_ID: AtomicU64 = AtomicU64::new(0);

const ZSHENV: &str = r#"# termul shell integration (tự sinh) — an toàn với plugin của user
() {
  # Khôi phục ZDOTDIR gốc để .zshrc/.zprofile của user nạp bình thường
  if [[ -n "$TERMUL_ORIG_ZDOTDIR" ]]; then
    export ZDOTDIR="$TERMUL_ORIG_ZDOTDIR"
  else
    unset ZDOTDIR
  fi
  unset TERMUL_ORIG_ZDOTDIR
  local zdd="${ZDOTDIR:-$HOME}"
  [[ -f "$zdd/.zshenv" ]] && source "$zdd/.zshenv"
}

if [[ -o interactive ]]; then
  autoload -Uz add-zsh-hook add-zle-hook-widget

  _termul_preexec() {
    local b64cmd b64cwd
    b64cmd="$(print -rn -- "$1" | base64 | tr -d '\n')"
    b64cwd="$(print -rn -- "$PWD" | base64 | tr -d '\n')"
    printf '\e]1337;TermulCmd=%s;%s\a' "$b64cmd" "$b64cwd"
  }
  _termul_precmd() {
    printf '\e]1337;TermulEnd=%d\a' "$?"
  }
  # Báo dòng nhập hiện tại mỗi lần zle vẽ lại (để termul hiện popup gợi ý).
  # Phải ghi thẳng /dev/tty: trong widget zle, stdout không được flush ra terminal.
  _termul_report_buffer() {
    local b64buf b64cwd
    b64buf="$(print -rn -- "$BUFFER" | base64 | tr -d '\n')"
    b64cwd="$(print -rn -- "$PWD" | base64 | tr -d '\n')"
    printf '\e]1337;TermulBuf=%d;%s;%s\a' "$CURSOR" "$b64buf" "$b64cwd" >/dev/tty
  }

  # Đọc edit đã được termul ghi riêng cho pane rồi cập nhật ZLE nguyên tử.
  # Dòng đầu file là CURSOR, phần còn lại (tới EOF) là BUFFER — có thể nhiều dòng.
  _termul_apply_edit() {
    local cursor buffer
    { IFS= read -r cursor; IFS= read -r -d '' buffer; } <"$TERMUL_EDIT_FILE"
    [[ "$cursor" == <-> ]] || return
    printf '\e[?2026h' >/dev/tty
    BUFFER="$buffer"
    CURSOR="$cursor"
    POSTDISPLAY=''
    zle redisplay
    printf '\e[?2026l' >/dev/tty
  }

  # Enter khi dòng kết thúc bằng `\` (số lẻ): chèn xuống dòng vào BUFFER thay vì
  # accept-line, để lệnh nhiều dòng gõ tay vẫn là một buffer (sửa/cut được như paste)
  # thay vì bị zsh đẩy các dòng trước vào PREBUFFER read-only.
  _termul_accept_line() {
    local trailing=${BUFFER##*[^\\]}
    if (( ${#trailing} % 2 )); then
      BUFFER+=$'\n'
      CURSOR=${#BUFFER}
      return
    fi
    zle _termul_orig_accept_line -- "$@"
  }

  # Cài hook sau khi .zshrc của user nạp xong (một lần) để không bị plugin ghi đè
  _termul_init() {
    add-zsh-hook -d precmd _termul_init
    add-zsh-hook preexec _termul_preexec
    add-zsh-hook precmd _termul_precmd
    add-zle-hook-widget line-pre-redraw _termul_report_buffer
    zle -A accept-line _termul_orig_accept_line
    zle -N accept-line _termul_accept_line
    zle -N _termul_apply_edit
    bindkey -M emacs $'\e[99~' _termul_apply_edit
    bindkey -M viins $'\e[99~' _termul_apply_edit
  }
  add-zsh-hook precmd _termul_init
fi
"#;

/// Thư mục ZDOTDIR tạm chứa integration.
pub struct ShellIntegration {
    dir: PathBuf,
}

impl ShellIntegration {
    /// Tạo thư mục tạm và ghi `.zshenv`.
    pub fn setup() -> Result<Self> {
        let id = NEXT_INTEGRATION_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("termul-zdotdir-{}-{id}", std::process::id()));
        fs::create_dir_all(&dir)?;
        fs::write(dir.join(".zshenv"), ZSHENV)?;
        Ok(Self { dir })
    }

    /// Biến môi trường cần đặt khi spawn shell `shell` để bật integration.
    /// Trả rỗng nếu shell không phải zsh (chưa hỗ trợ shell khác).
    pub fn env_for(&self, shell: &str, pane_id: u64) -> Vec<(String, String)> {
        let is_zsh = std::path::Path::new(shell)
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n == "zsh")
            .unwrap_or(false);
        if !is_zsh {
            return Vec::new();
        }
        let orig = std::env::var("ZDOTDIR").unwrap_or_default();
        vec![
            (
                "ZDOTDIR".to_string(),
                self.dir.to_string_lossy().to_string(),
            ),
            ("TERMUL_ORIG_ZDOTDIR".to_string(), orig),
            (
                "TERMUL_EDIT_FILE".to_string(),
                self.edit_file(pane_id).to_string_lossy().to_string(),
            ),
        ]
    }

    /// Chuẩn bị edit cho widget của đúng pane rồi trả phím kích hoạt widget.
    pub(crate) fn prepare_zle_edit(
        &self,
        pane_id: u64,
        buffer: &str,
        cursor: usize,
    ) -> Result<Option<Vec<u8>>> {
        if buffer.contains('\0') || cursor > buffer.chars().count() {
            return Ok(None);
        }
        fs::write(self.edit_file(pane_id), format!("{cursor}\n{buffer}"))?;
        Ok(Some(b"\x1b[99~".to_vec()))
    }

    fn edit_file(&self, pane_id: u64) -> PathBuf {
        self.dir.join(format!("edit-{pane_id}"))
    }
}

impl Drop for ShellIntegration {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[cfg(test)]
mod tests {
    use super::ShellIntegration;

    #[test]
    fn clears_zsh_autosuggestion_postdisplay_during_atomic_edit() {
        let integration = ShellIntegration::setup().unwrap();
        let script = std::fs::read_to_string(integration.dir.join(".zshenv")).unwrap();
        assert!(!script.contains("_ZSH_AUTOSUGGEST_DISABLED"));
        assert!(script.contains("POSTDISPLAY=''"));
    }

    #[test]
    fn wraps_accept_line_to_keep_backslash_continuation_in_buffer() {
        let integration = ShellIntegration::setup().unwrap();
        let script = std::fs::read_to_string(integration.dir.join(".zshenv")).unwrap();
        assert!(script.contains("zle -A accept-line _termul_orig_accept_line"));
        assert!(script.contains("zle -N accept-line _termul_accept_line"));
    }

    #[test]
    fn prepares_atomic_zle_edit_including_multiline_payload() {
        let integration = ShellIntegration::setup().unwrap();
        assert_eq!(
            integration.prepare_zle_edit(7, "echo hé", 4).unwrap(),
            Some(b"\x1b[99~".to_vec())
        );
        assert_eq!(
            std::fs::read(integration.edit_file(7)).unwrap(),
            b"4\necho h\xc3\xa9"
        );
        assert_eq!(
            integration
                .prepare_zle_edit(7, "echo \\\n\tnext\n", 4)
                .unwrap(),
            Some(b"\x1b[99~".to_vec())
        );
        assert_eq!(
            std::fs::read(integration.edit_file(7)).unwrap(),
            b"4\necho \\\n\tnext\n"
        );
        assert_eq!(integration.prepare_zle_edit(7, "echo", 5).unwrap(), None);
        assert_eq!(integration.prepare_zle_edit(7, "a\0b", 0).unwrap(), None);
    }
}
