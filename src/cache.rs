use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;

use crate::error::VexError;

const PR_NUMBER_TTL: i64 = 300; // 5 minutes
const PR_DATA_TTL: i64 = 120; // 2 minutes

pub struct PrCache {
    conn: Connection,
}

impl PrCache {
    pub fn open() -> Result<Self, VexError> {
        let path = Self::default_path()?;
        Self::open_path(&path)
    }

    fn default_path() -> Result<PathBuf, VexError> {
        let home = crate::config::vex_home()?;
        std::fs::create_dir_all(&home).map_err(|e| VexError::io(&home, e))?;
        Ok(home.join("cache.db"))
    }

    fn open_path(path: &PathBuf) -> Result<Self, VexError> {
        match Self::try_open(path) {
            Ok(cache) => Ok(cache),
            Err(_) => {
                // Corrupt DB — delete and retry once
                let _ = std::fs::remove_file(path);
                Self::try_open(path)
            }
        }
    }

    fn try_open(path: &PathBuf) -> Result<Self, VexError> {
        let conn = Connection::open(path).map_err(|e| VexError::Cache(format!("open: {e}")))?;

        conn.execute_batch("PRAGMA journal_mode=WAL;")
            .map_err(|e| VexError::Cache(format!("WAL: {e}")))?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS pr_numbers (
                repo_path TEXT NOT NULL,
                branch TEXT NOT NULL,
                pr_number INTEGER NOT NULL,
                fetched_at INTEGER NOT NULL,
                PRIMARY KEY (repo_path, branch)
            );
            CREATE TABLE IF NOT EXISTS pr_structured (
                repo_path TEXT NOT NULL,
                pr_number INTEGER NOT NULL,
                data_json TEXT NOT NULL,
                fetched_at INTEGER NOT NULL,
                PRIMARY KEY (repo_path, pr_number)
            );",
        )
        .map_err(|e| VexError::Cache(format!("schema: {e}")))?;

        let cache = PrCache { conn };
        cache.evict_expired();
        Ok(cache)
    }

    fn now() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64
    }

    fn evict_expired(&self) {
        let now = Self::now();
        let _ = self.conn.execute(
            "DELETE FROM pr_numbers WHERE fetched_at < ?1",
            [now - PR_NUMBER_TTL],
        );
        let _ = self.conn.execute(
            "DELETE FROM pr_structured WHERE fetched_at < ?1",
            [now - PR_DATA_TTL],
        );
    }

    /// Get cached PR numbers for a repo. Returns None on miss or error.
    #[allow(dead_code)]
    pub fn get_pr_numbers(&self, repo_path: &str) -> Option<Vec<(String, u64)>> {
        let cutoff = Self::now() - PR_NUMBER_TTL;
        let mut stmt = self
            .conn
            .prepare(
                "SELECT branch, pr_number FROM pr_numbers WHERE repo_path = ?1 AND fetched_at >= ?2",
            )
            .ok()?;
        let rows = stmt
            .query_map(rusqlite::params![repo_path, cutoff], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
            })
            .ok()?;
        let results: Vec<(String, u64)> = rows.filter_map(|r| r.ok()).collect();
        if results.is_empty() {
            None
        } else {
            Some(results)
        }
    }

    /// Replace all cached PR numbers for a repo.
    pub fn set_pr_numbers(&self, repo_path: &str, entries: &[(String, u64)]) {
        let now = Self::now();
        let _ = self
            .conn
            .execute("DELETE FROM pr_numbers WHERE repo_path = ?1", [repo_path]);
        for (branch, pr_number) in entries {
            let _ = self.conn.execute(
                "INSERT OR REPLACE INTO pr_numbers (repo_path, branch, pr_number, fetched_at) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![repo_path, branch, pr_number, now],
            );
        }
    }

    /// Get cached structured PR data. Returns None on miss or error.
    pub fn get_pr_structured(&self, repo_path: &str, pr_number: u64) -> Option<String> {
        let cutoff = Self::now() - PR_DATA_TTL;
        self.conn
            .query_row(
                "SELECT data_json FROM pr_structured WHERE repo_path = ?1 AND pr_number = ?2 AND fetched_at >= ?3",
                rusqlite::params![repo_path, pr_number, cutoff],
                |row| row.get(0),
            )
            .ok()
    }

    /// Store structured PR data.
    pub fn set_pr_structured(&self, repo_path: &str, pr_number: u64, data: &str) {
        let now = Self::now();
        let _ = self.conn.execute(
            "INSERT OR REPLACE INTO pr_structured (repo_path, pr_number, data_json, fetched_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![repo_path, pr_number, data, now],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_cache(dir: &std::path::Path) -> PrCache {
        let path = dir.join("test_cache.db");
        PrCache::open_path(&path).unwrap()
    }

    #[test]
    fn pr_numbers_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = test_cache(tmp.path());

        assert!(cache.get_pr_numbers("/repo").is_none());

        cache.set_pr_numbers("/repo", &[("main".into(), 1), ("feat".into(), 2)]);

        let result = cache.get_pr_numbers("/repo").unwrap();
        assert_eq!(result.len(), 2);
        assert!(result.contains(&("main".into(), 1)));
        assert!(result.contains(&("feat".into(), 2)));
    }

    #[test]
    fn pr_numbers_replacement() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = test_cache(tmp.path());

        cache.set_pr_numbers("/repo", &[("main".into(), 1)]);
        cache.set_pr_numbers("/repo", &[("feat".into(), 2)]);

        let result = cache.get_pr_numbers("/repo").unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], ("feat".into(), 2));
    }

    #[test]
    fn pr_structured_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = test_cache(tmp.path());

        assert!(cache.get_pr_structured("/repo", 42).is_none());

        cache.set_pr_structured("/repo", 42, r#"{"title":"test"}"#);

        let result = cache.get_pr_structured("/repo", 42).unwrap();
        assert_eq!(result, r#"{"title":"test"}"#);
    }

    #[test]
    fn corrupt_db_recovery() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("corrupt.db");
        std::fs::write(&path, "this is not a valid sqlite database").unwrap();

        // Should recover by deleting and recreating
        let cache = PrCache::open_path(&path).unwrap();
        cache.set_pr_numbers("/repo", &[("main".into(), 1)]);
        assert!(cache.get_pr_numbers("/repo").is_some());
    }
}
