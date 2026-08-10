//! Tự động nạp shell integration (OSC 133) vào zsh qua ZDOTDIR tạm,
//! không đụng tới cấu hình gốc của người dùng.

use std::fs;
use std::path::PathBuf;

use anyhow::Result;

/// Nội dung `.zshenv` được inject: khôi phục môi trường user rồi cài hook
/// preexec/precmd phát OSC báo lệnh + exit code + cwd cho termul.
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
    printf '\e]1337;TermulBuf=%d;%s\a' "$CURSOR" "$(print -rn -- "$BUFFER" | base64 | tr -d '\n')" >/dev/tty
  }

  # Cài hook sau khi .zshrc của user nạp xong (một lần) để không bị plugin ghi đè
  _termul_init() {
    add-zsh-hook -d precmd _termul_init
    add-zsh-hook preexec _termul_preexec
    add-zsh-hook precmd _termul_precmd
    add-zle-hook-widget line-pre-redraw _termul_report_buffer
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
        let dir = std::env::temp_dir().join(format!("termul-zdotdir-{}", std::process::id()));
        fs::create_dir_all(&dir)?;
        fs::write(dir.join(".zshenv"), ZSHENV)?;
        Ok(Self { dir })
    }

    /// Biến môi trường cần đặt khi spawn shell `shell` để bật integration.
    /// Trả rỗng nếu shell không phải zsh (chưa hỗ trợ shell khác).
    pub fn env_for(&self, shell: &str) -> Vec<(String, String)> {
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
            ("ZDOTDIR".to_string(), self.dir.to_string_lossy().to_string()),
            ("TERMUL_ORIG_ZDOTDIR".to_string(), orig),
        ]
    }
}

impl Drop for ShellIntegration {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}
