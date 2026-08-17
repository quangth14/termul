//! Popup mention file/thư mục: nhận diện `@query`, quét theo gitignore và chèn đường dẫn.

use std::path::Path;
use std::thread;

use ignore::WalkBuilder;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use ratatui::layout::Rect;

use crate::app::{App, AppEvent, Mention, MentionResult};
use crate::layout::PaneId;
use crate::session::active_focus;

const MAX_MATCHES: usize = 100;

/// Phạm vi token mention theo chỉ số ký tự, không gồm ký tự `@` trong query.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MentionToken {
    start: usize,
    end: usize,
    query: String,
}

fn is_boundary(c: char) -> bool {
    c.is_whitespace() || matches!(c, '|' | '&' | ';' | '<' | '>' | '(' | ')' | '{' | '}')
}

fn mention_token(buffer: &str, cursor: usize) -> Option<MentionToken> {
    let chars: Vec<char> = buffer.chars().collect();
    let cursor = cursor.min(chars.len());
    let segment_start = chars[..cursor]
        .iter()
        .rposition(|c| is_boundary(*c))
        .map_or(0, |i| i + 1);
    let at = chars[segment_start..cursor]
        .iter()
        .rposition(|c| *c == '@')?
        + segment_start;
    if at > segment_start {
        return None;
    }
    let end = chars[cursor..]
        .iter()
        .position(|c| is_boundary(*c))
        .map_or(chars.len(), |i| cursor + i);
    Some(MentionToken {
        start: at,
        end,
        query: chars[at + 1..cursor].iter().collect(),
    })
}

/// Khởi chạy quét nền cho mention tại con trỏ; kết quả cũ sẽ bị event loop bỏ qua.
pub(crate) fn rebuild_mention(app: &mut App) -> bool {
    let focus = active_focus(app);
    let Some(pane) = app.panes.get(&focus) else {
        app.mention = None;
        return false;
    };
    let Some(token) = mention_token(&pane.input.buffer, pane.input.cursor) else {
        app.mention = None;
        return false;
    };
    if pane.cwd.is_empty() {
        app.mention = None;
        return true;
    }

    let cwd = pane.cwd.clone();
    let excludes = app.cfg.mention_exclude.clone();
    let tx = app.tx.clone();
    app.mention_generation = app.mention_generation.wrapping_add(1);
    let generation = app.mention_generation;
    app.mention = None;
    thread::spawn(move || {
        let matches = find_matches(Path::new(&cwd), &token.query, MAX_MATCHES, &excludes);
        let _ = tx.send(AppEvent::MentionReady(MentionResult {
            pane: focus,
            generation,
            token_start: token.start,
            token_end: token.end,
            matches,
        }));
    });
    true
}

fn build_excludes(root: &Path, patterns: &[String]) -> Option<Gitignore> {
    let mut builder = GitignoreBuilder::new(root);
    for pattern in patterns {
        let _ = builder.add_line(None, pattern);
    }
    builder.build().ok()
}

fn find_matches(root: &Path, query: &str, limit: usize, excludes: &[String]) -> Vec<String> {
    let mut paths = Vec::new();
    let excludes = build_excludes(root, excludes);
    let walker = WalkBuilder::new(root)
        .hidden(false)
        .require_git(false)
        .filter_entry(|entry| entry.file_name() != ".git")
        .build();
    for entry in walker.filter_map(Result::ok).skip(1) {
        let Ok(relative) = entry.path().strip_prefix(root) else {
            continue;
        };
        let is_dir = entry.file_type().is_some_and(|kind| kind.is_dir());
        if excludes.as_ref().is_some_and(|rules| {
            rules
                .matched_path_or_any_parents(entry.path(), is_dir)
                .is_ignore()
        }) {
            continue;
        }
        let mut path = relative.to_string_lossy().into_owned();
        if is_dir {
            path.push('/');
        }
        paths.push(path);
    }

    if query.is_empty() {
        paths.sort();
        paths.truncate(limit);
        return paths;
    }

    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut buf = Vec::new();
    let mut scored: Vec<_> = paths
        .into_iter()
        .filter_map(|path| {
            let score = pattern.score(Utf32Str::new(&path, &mut buf), &mut matcher)?;
            Some((score, path))
        })
        .collect();
    scored.sort_unstable_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored
        .into_iter()
        .take(limit)
        .map(|(_, path)| path)
        .collect()
}

pub(crate) fn mention_scroll_to_selected(mention: &mut Mention, max_visible: usize) {
    let visible = mention.matches.len().min(max_visible);
    if mention.selected < mention.offset {
        mention.offset = mention.selected;
    } else if mention.selected >= mention.offset + visible {
        mention.offset = mention.selected + 1 - visible;
    }
}

fn shell_escape(path: &str) -> String {
    let mut escaped = String::with_capacity(path.len());
    for c in path.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '.') {
            escaped.push(c);
        } else {
            escaped.push('\\');
            escaped.push(c);
        }
    }
    escaped
}

fn accepted_path(path: &str) -> String {
    format!("./{path}")
}

/// Thay đúng token mention, giữ nguyên phần trước/sau và vị trí con trỏ hợp lý.
pub(crate) fn mention_accept(app: &mut App) {
    let focus = active_focus(app);
    let Some(mention) = app.mention.take() else {
        return;
    };
    let Some(path) = mention.matches.get(mention.selected) else {
        return;
    };
    let Some(pane) = app.panes.get_mut(&focus) else {
        return;
    };
    let chars: Vec<char> = pane.input.buffer.chars().collect();
    if mention.token_start > mention.token_end || mention.token_end > chars.len() {
        return;
    }
    let escaped = shell_escape(&accepted_path(path));
    let mut replacement: String = chars[..mention.token_start].iter().collect();
    replacement.push_str(&escaped);
    replacement.extend(chars[mention.token_end..].iter());
    let cursor = mention.token_start + escaped.chars().count();
    let move_left = replacement.chars().count().saturating_sub(cursor);

    let mut bytes = vec![0x01, 0x0b];
    bytes.extend_from_slice(replacement.as_bytes());
    for _ in 0..move_left {
        bytes.extend_from_slice(b"\x1b[D");
    }
    pane.pty.write(&bytes);
}

pub(crate) fn compute_mention_rect(app: &App, focus: PaneId) -> Option<Rect> {
    let mention = app.mention.as_ref()?;
    let inner = app.inner_areas.get(&focus)?;
    let pane = app.panes.get(&focus)?;
    let (crow, ccol) = pane.grid.screen().cursor_position();
    let cx = inner.x + ccol;
    let cy = inner.y + crow;
    let longest = mention
        .matches
        .iter()
        .map(|path| path.chars().count())
        .max()
        .unwrap_or(8) as u16;
    let width = (longest + 4).clamp(8, app.screen.width.max(8));
    let height = mention.matches.len().min(app.cfg.suggest_max_visible) as u16 + 2;
    let mut y = cy + 1;
    if y + height > app.screen.y + app.screen.height {
        y = cy.saturating_sub(height);
    }
    let mut x = cx;
    if x + width > app.screen.x + app.screen.width {
        x = (app.screen.x + app.screen.width).saturating_sub(width);
    }
    Some(Rect::new(x, y, width, height))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn tim_token_tai_con_tro_giua_dong() {
        assert_eq!(
            mention_token("cat @src/main.rs --color", 11),
            Some(MentionToken {
                start: 4,
                end: 16,
                query: "src/ma".into(),
            })
        );
    }

    #[test]
    fn escape_ky_tu_dac_biet_cua_shell() {
        assert_eq!(shell_escape("My Folder/a$b.txt"), "My\\ Folder/a\\$b.txt");
    }

    #[test]
    fn them_tien_to_thu_muc_hien_tai_khi_accept() {
        assert_eq!(accepted_path("src/main.rs"), "./src/main.rs");
        assert_eq!(accepted_path(".env"), "./.env");
    }

    #[test]
    fn quet_fuzzy_ton_trong_gitignore_nhung_giu_dotfile() {
        let root = std::env::temp_dir().join(format!("termul-mention-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "").unwrap();
        fs::write(root.join("ignored.txt"), "").unwrap();
        fs::write(root.join(".visible"), "").unwrap();
        fs::write(root.join(".gitignore"), "ignored.txt\n").unwrap();
        fs::create_dir_all(root.join(".git/objects")).unwrap();
        fs::write(root.join(".git/config"), "").unwrap();

        let excludes = vec![
            "[".to_string(), // pattern lỗi phải không làm hỏng các pattern còn lại
            "src/".to_string(),
            "*.tmp".to_string(),
        ];
        fs::write(root.join("debug.tmp"), "").unwrap();
        let all = find_matches(&root, "", 20, &excludes);
        assert!(all.contains(&".visible".to_string()));
        assert!(!all.contains(&"ignored.txt".to_string()));
        assert!(!all.iter().any(|path| path.starts_with(".git/")));
        assert!(!all.iter().any(|path| path.starts_with("src/")));
        assert!(!all.contains(&"debug.tmp".to_string()));

        let no_excludes = Vec::new();
        assert_eq!(
            find_matches(&root, "srcmn", 20, &no_excludes),
            vec!["src/main.rs"]
        );
        fs::remove_dir_all(root).unwrap();
    }
}
