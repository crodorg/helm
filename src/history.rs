//! SQLite-backed history of agent and operator command runs.
//!
//! Both `helm exec` traffic (agent) and TUI Runner submissions (operator)
//! land in the same `runs` table tagged by `source`. Lines are stored in
//! a child table keyed on `(run_id, seq)` so the original order is
//! recoverable without depending on rowid.
//!
//! Writes happen in a single transaction at run completion: one `runs`
//! insert plus one bulk insert of every transcript line. This trades the
//! "live streaming to disk" property for a simpler model and avoids
//! interleaving DB I/O with UI render ticks. If helm crashes mid-run, the
//! in-flight run is lost — the in-memory `agent_history` ring captures
//! everything for the current session.
//!
//! Schema is created idempotently in `open()` (single migration; future
//! columns add via ALTER TABLE in a versioned `schema_version` row).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{Connection, params};

const SCHEMA_VERSION: i64 = 1;

#[derive(Debug, Clone, PartialEq)]
pub struct RunRecord {
    pub id: i64,
    pub source: RunSource,
    pub alias: String,
    pub cmd: String,
    pub started_at_unix: i64,
    pub exit: Option<i32>,
    pub duration_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunSource {
    Agent,
    Operator,
}

impl RunSource {
    fn as_str(self) -> &'static str {
        match self {
            RunSource::Agent => "agent",
            RunSource::Operator => "operator",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "agent" => Some(Self::Agent),
            "operator" => Some(Self::Operator),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineKind {
    Out,
    Err,
    System,
}

impl LineKind {
    fn as_str(&self) -> &'static str {
        match self {
            LineKind::Out => "out",
            LineKind::Err => "err",
            LineKind::System => "system",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineRecord {
    pub kind: LineKind,
    pub line: String,
}

pub struct HistoryStore {
    conn: Connection,
}

impl HistoryStore {
    /// Open or create a SQLite database at `path`. Parent directory is
    /// created if missing. Schema migration is idempotent.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create history dir {}", parent.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("open history db {}", path.display()))?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    /// Open at the XDG default location: `$XDG_DATA_HOME/helm/state.db`.
    pub fn open_default() -> Result<Self> {
        let path =
            default_path().context("XDG data dir unavailable — set $XDG_DATA_HOME or $HOME")?;
        Self::open(&path)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS schema_meta (
                key TEXT PRIMARY KEY,
                value INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS runs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                source TEXT NOT NULL CHECK (source IN ('agent','operator')),
                alias TEXT NOT NULL,
                cmd TEXT NOT NULL,
                started_at INTEGER NOT NULL,
                exit INTEGER,
                duration_ms INTEGER
            );
            CREATE INDEX IF NOT EXISTS runs_started_idx ON runs(started_at DESC);
            CREATE INDEX IF NOT EXISTS runs_source_idx ON runs(source, started_at DESC);
            CREATE TABLE IF NOT EXISTS run_lines (
                run_id INTEGER NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
                seq INTEGER NOT NULL,
                kind TEXT NOT NULL CHECK (kind IN ('out','err','system')),
                line TEXT NOT NULL,
                PRIMARY KEY (run_id, seq)
            );
            "#,
        )?;
        self.conn.execute(
            "INSERT OR REPLACE INTO schema_meta(key, value) VALUES ('schema_version', ?1)",
            params![SCHEMA_VERSION],
        )?;
        // Foreign keys are off by default in SQLite; turn them on so
        // run_lines get cleaned up when their parent run is pruned.
        self.conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        Ok(())
    }

    /// Insert one completed run + every transcript line in a single
    /// transaction. Returns the new row id.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_run(
        &mut self,
        source: RunSource,
        alias: &str,
        cmd: &str,
        started_at_unix: i64,
        exit: Option<i32>,
        duration_ms: Option<i64>,
        lines: &[LineRecord],
    ) -> Result<i64> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO runs(source, alias, cmd, started_at, exit, duration_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                source.as_str(),
                alias,
                cmd,
                started_at_unix,
                exit,
                duration_ms
            ],
        )?;
        let run_id = tx.last_insert_rowid();
        {
            let mut stmt = tx.prepare(
                "INSERT INTO run_lines(run_id, seq, kind, line) VALUES (?1, ?2, ?3, ?4)",
            )?;
            for (seq, line) in lines.iter().enumerate() {
                stmt.execute(params![run_id, seq as i64, line.kind.as_str(), line.line])?;
            }
        }
        tx.commit()?;
        Ok(run_id)
    }

    /// Most-recent `limit` runs of either source. Newest first.
    pub fn recent_runs(&self, source: Option<RunSource>, limit: usize) -> Result<Vec<RunRecord>> {
        let (sql, params_vec): (&str, Vec<rusqlite::types::Value>) = match source {
            Some(s) => (
                "SELECT id, source, alias, cmd, started_at, exit, duration_ms
                 FROM runs WHERE source = ?1 ORDER BY started_at DESC, id DESC LIMIT ?2",
                vec![s.as_str().to_owned().into(), (limit as i64).into()],
            ),
            None => (
                "SELECT id, source, alias, cmd, started_at, exit, duration_ms
                 FROM runs ORDER BY started_at DESC, id DESC LIMIT ?1",
                vec![(limit as i64).into()],
            ),
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params_vec), |row| {
            let source_str: String = row.get(1)?;
            Ok(RunRecord {
                id: row.get(0)?,
                source: RunSource::parse(&source_str).unwrap_or(RunSource::Agent),
                alias: row.get(2)?,
                cmd: row.get(3)?,
                started_at_unix: row.get(4)?,
                exit: row.get(5)?,
                duration_ms: row.get(6)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Keep the newest `max_runs` rows; delete the rest (and their lines
    /// via ON DELETE CASCADE).
    pub fn prune_to(&self, max_runs: usize) -> Result<usize> {
        let n = self.conn.execute(
            "DELETE FROM runs WHERE id IN (
                 SELECT id FROM runs ORDER BY started_at DESC, id DESC LIMIT -1 OFFSET ?1
             )",
            params![max_runs as i64],
        )?;
        Ok(n)
    }
}

/// XDG-default path for the history DB.
pub fn default_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "helm").map(|d| d.data_dir().join("state.db"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fresh() -> (TempDir, HistoryStore) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("state.db");
        let store = HistoryStore::open(&path).unwrap();
        (dir, store)
    }

    fn sample_lines() -> Vec<LineRecord> {
        vec![
            LineRecord {
                kind: LineKind::System,
                line: "$ ssh vps1 'uptime'".into(),
            },
            LineRecord {
                kind: LineKind::Out,
                line: " 12:34:56 up 5 days".into(),
            },
            LineRecord {
                kind: LineKind::System,
                line: "exit 0".into(),
            },
        ]
    }

    #[test]
    fn open_creates_db_and_parent_dir() {
        let dir = TempDir::new().unwrap();
        let nested = dir.path().join("a/b/c/state.db");
        let _ = HistoryStore::open(&nested).unwrap();
        assert!(nested.exists(), "db file created");
    }

    #[test]
    fn insert_then_recent_returns_round_trip() {
        let (_d, mut store) = fresh();
        let id = store
            .insert_run(
                RunSource::Agent,
                "vps1",
                "uptime",
                1_779_062_400,
                Some(0),
                Some(123),
                &sample_lines(),
            )
            .unwrap();
        assert!(id > 0);
        let recent = store.recent_runs(None, 10).unwrap();
        assert_eq!(recent.len(), 1);
        let r = &recent[0];
        assert_eq!(r.source, RunSource::Agent);
        assert_eq!(r.alias, "vps1");
        assert_eq!(r.cmd, "uptime");
        assert_eq!(r.started_at_unix, 1_779_062_400);
        assert_eq!(r.exit, Some(0));
        assert_eq!(r.duration_ms, Some(123));
    }

    #[test]
    fn recent_filters_by_source() {
        let (_d, mut store) = fresh();
        for (src, alias, cmd, t) in [
            (RunSource::Agent, "vps1", "a", 100),
            (RunSource::Operator, "vps1", "b", 200),
            (RunSource::Agent, "vps2", "c", 300),
            (RunSource::Operator, "vps2", "d", 400),
        ] {
            store
                .insert_run(src, alias, cmd, t, Some(0), None, &[])
                .unwrap();
        }
        let agents = store.recent_runs(Some(RunSource::Agent), 10).unwrap();
        assert_eq!(agents.len(), 2);
        assert_eq!(agents[0].cmd, "c"); // newest first
        assert_eq!(agents[1].cmd, "a");
        let operators = store.recent_runs(Some(RunSource::Operator), 10).unwrap();
        assert_eq!(operators.len(), 2);
        assert_eq!(operators[0].cmd, "d");
    }

    #[test]
    fn recent_orders_newest_first() {
        let (_d, mut store) = fresh();
        for t in [500, 100, 300, 200, 400] {
            store
                .insert_run(
                    RunSource::Agent,
                    "h",
                    &format!("c{t}"),
                    t,
                    Some(0),
                    None,
                    &[],
                )
                .unwrap();
        }
        let recent = store.recent_runs(None, 10).unwrap();
        let times: Vec<i64> = recent.iter().map(|r| r.started_at_unix).collect();
        assert_eq!(times, vec![500, 400, 300, 200, 100]);
    }

    #[test]
    fn recent_respects_limit() {
        let (_d, mut store) = fresh();
        for t in 0..10 {
            store
                .insert_run(RunSource::Agent, "h", "c", t, Some(0), None, &[])
                .unwrap();
        }
        assert_eq!(store.recent_runs(None, 3).unwrap().len(), 3);
        assert_eq!(store.recent_runs(None, 100).unwrap().len(), 10);
    }

    #[test]
    fn prune_to_drops_oldest() {
        let (_d, mut store) = fresh();
        for t in 0..10 {
            store
                .insert_run(
                    RunSource::Agent,
                    "h",
                    "c",
                    t,
                    Some(0),
                    None,
                    &sample_lines(),
                )
                .unwrap();
        }
        let removed = store.prune_to(3).unwrap();
        assert_eq!(removed, 7);
        let remaining = store.recent_runs(None, 100).unwrap();
        assert_eq!(remaining.len(), 3);
        // Cascade deleted the line rows too.
        let total_lines: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM run_lines", [], |r| r.get(0))
            .unwrap();
        assert_eq!(total_lines, 3 * 3);
    }

    #[test]
    fn migrate_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("state.db");
        let _a = HistoryStore::open(&path).unwrap();
        let _b = HistoryStore::open(&path).unwrap();
        let _c = HistoryStore::open(&path).unwrap();
        // No panic.
    }

    #[test]
    fn null_exit_round_trips() {
        let (_d, mut store) = fresh();
        store
            .insert_run(RunSource::Agent, "h", "c", 100, None, None, &[])
            .unwrap();
        let recent = store.recent_runs(None, 1).unwrap();
        assert_eq!(recent[0].exit, None);
        assert_eq!(recent[0].duration_ms, None);
    }

    #[test]
    fn empty_lines_vec_is_valid() {
        let (_d, mut store) = fresh();
        store
            .insert_run(RunSource::Agent, "h", "c", 100, Some(0), None, &[])
            .unwrap();
        // No transcript lines were written for this run.
        let n: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM run_lines", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }
}
