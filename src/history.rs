//! Lưu lịch sử lệnh (SQLite) và tìm kiếm mờ có xếp hạng frecency cho palette.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};
use rusqlite::Connection;

pub struct HistoryStore {
    conn: Connection,
}

/// Một ứng viên trong palette (đã gộp theo cmdline).
pub struct HistoryEntry {
    pub cmdline: String,
    pub cwd_match: bool,
}

impl HistoryStore {
    /// Mở DB tại `~/.local/share/termul/history.db` (hoặc tương đương theo OS).
    pub fn open_default() -> Result<Self> {
        let path = default_db_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Self::open(path)
    }

    pub fn open(path: PathBuf) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS commands (
                id INTEGER PRIMARY KEY,
                cmdline TEXT NOT NULL,
                cwd TEXT NOT NULL,
                exit_code INTEGER,
                ts INTEGER NOT NULL,
                duration_ms INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_cmd_ts ON commands(ts);
            CREATE INDEX IF NOT EXISTS idx_cmd_cwd ON commands(cwd);",
        )?;
        Ok(Self { conn })
    }

    /// Ghi một lệnh đã chạy xong.
    pub fn record(&self, cmdline: &str, cwd: &str, exit: i32, duration_ms: u64) {
        let cmd = cmdline.trim();
        if cmd.is_empty() {
            return;
        }
        let ts = now_secs();
        let _ = self.conn.execute(
            "INSERT INTO commands (cmdline, cwd, exit_code, ts, duration_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![cmd, cwd, exit, ts as i64, duration_ms as i64],
        );
    }

    /// Gợi ý cho popup autocomplete: các lệnh **chứa** `query` (không phân biệt
    /// hoa/thường, khác `query` chính nó), xếp hạng frecency + ưu tiên cùng `cwd`.
    pub fn suggest(&self, query: &str, cwd: &str, limit: usize) -> Vec<String> {
        if query.is_empty() {
            return Vec::new();
        }
        let needle = query.to_lowercase();
        let now = now_secs() as i64;
        let mut scored: Vec<(String, f64)> = Vec::new();
        for (cmdline, last_ts, cnt, cwd_match) in self.aggregate(cwd) {
            if cmdline != query && cmdline.to_lowercase().contains(&needle) {
                scored.push((cmdline, frecency(now, last_ts, cnt, cwd_match)));
            }
        }
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);
        scored.into_iter().map(|(c, _)| c).collect()
    }

    /// Gộp theo cmdline: (cmdline, last_ts, count, cwd_match).
    fn aggregate(&self, cwd: &str) -> Vec<(String, i64, i64, bool)> {
        let mut rows = Vec::new();
        if let Ok(mut stmt) = self.conn.prepare(
            "SELECT cmdline, MAX(ts) AS last_ts, COUNT(*) AS cnt,
                    MAX(CASE WHEN cwd = ?1 THEN 1 ELSE 0 END) AS cwd_match
             FROM commands GROUP BY cmdline ORDER BY last_ts DESC LIMIT 5000",
        ) && let Ok(mapped) = stmt.query_map(rusqlite::params![cwd], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)? != 0,
            ))
        }) {
            rows.extend(mapped.flatten());
        }
        rows
    }

    /// Tìm kiếm cho palette: gộp theo cmdline, lọc mờ theo `query`,
    /// xếp hạng frecency (tần suất × độ mới), ưu tiên lệnh cùng `cwd`.
    pub fn search(&self, query: &str, cwd: &str, limit: usize) -> Vec<HistoryEntry> {
        let now = now_secs() as i64;
        let rows = self.aggregate(cwd);

        let mut scored: Vec<(HistoryEntry, f64, u32)> = if query.trim().is_empty() {
            rows.into_iter()
                .map(|(cmdline, last_ts, cnt, cwd_match)| {
                    let f = frecency(now, last_ts, cnt, cwd_match);
                    (HistoryEntry { cmdline, cwd_match }, f, 0)
                })
                .collect()
        } else {
            let mut matcher = Matcher::new(Config::DEFAULT);
            let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
            let mut buf = Vec::new();
            rows.into_iter()
                .filter_map(|(cmdline, last_ts, cnt, cwd_match)| {
                    let haystack = nucleo_matcher::Utf32Str::new(&cmdline, &mut buf);
                    let score = pattern.score(haystack, &mut matcher)?;
                    let f = frecency(now, last_ts, cnt, cwd_match);
                    Some((HistoryEntry { cmdline, cwd_match }, f, score))
                })
                .collect()
        };

        // Query rỗng: xếp theo frecency. Có query: xếp theo điểm fuzzy rồi frecency.
        scored.sort_by(|a, b| {
            b.2.cmp(&a.2)
                .then(b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal))
        });
        scored.truncate(limit);
        scored.into_iter().map(|(e, _, _)| e).collect()
    }

    pub fn remove(&self, cmdline: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM commands WHERE cmdline = ?1",
            rusqlite::params![cmdline],
        )?;
        Ok(())
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Điểm frecency: tần suất (log) × độ mới, nhân đôi nếu cùng cwd.
fn frecency(now: i64, last_ts: i64, cnt: i64, cwd_match: bool) -> f64 {
    let age_days = ((now - last_ts).max(0) as f64) / 86_400.0;
    let recency = 1.0 / (1.0 + age_days);
    let base = ((cnt + 1) as f64).ln() * recency;
    if cwd_match { base * 2.0 } else { base }
}

fn default_db_path() -> Result<PathBuf> {
    let base = dirs::data_dir().ok_or_else(|| anyhow::anyhow!("không tìm được data dir"))?;
    Ok(base.join("termul").join("history.db"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_and_search() {
        let store = HistoryStore::open(":memory:".into()).unwrap();
        store.record("git status", "/repo", 0, 12);
        store.record("git commit", "/repo", 0, 30);
        store.record("git status", "/repo", 0, 8); // lặp → tần suất cao hơn
        store.record("cargo build", "/other", 0, 900);

        // Query rỗng, cwd=/repo → lệnh git đứng trên (cùng cwd + tần suất)
        let res = store.search("", "/repo", 10);
        assert!(!res.is_empty());
        assert_eq!(res[0].cmdline, "git status");
        assert!(res[0].cwd_match);

        // Fuzzy "gcom" → khớp "git commit"
        let res = store.search("gcom", "/repo", 10);
        assert_eq!(res[0].cmdline, "git commit");

        // Fuzzy không khớp gì
        let res = store.search("zzzzz", "/repo", 10);
        assert!(res.is_empty());
    }

    #[test]
    fn suggest_contains() {
        let store = HistoryStore::open(":memory:".into()).unwrap();
        store.record("git status", "/repo", 0, 5);
        store.record("git stash", "/repo", 0, 5);
        store.record("cargo build", "/repo", 0, 5);

        // "status" nằm GIỮA lệnh → contains khớp (startsWith sẽ không)
        let s = store.suggest("status", "/repo", 10);
        assert!(s.contains(&"git status".to_string()));

        // Không phân biệt hoa/thường
        let s = store.suggest("GIT", "/repo", 10);
        assert!(s.contains(&"git status".to_string()));
        assert!(s.contains(&"git stash".to_string()));
        assert!(!s.contains(&"cargo build".to_string()));

        // Trùng hẳn một lệnh → không gợi ý chính nó
        let s = store.suggest("git status", "/repo", 10);
        assert!(!s.contains(&"git status".to_string()));

        // Query rỗng → không gợi ý
        assert!(store.suggest("", "/repo", 10).is_empty());
    }
}
