//! Cursor state + pure logic for `read --delta` — incremental scrollback
//! reads that return only the lines that appeared since the previous delta
//! read, so an agent polling a pane never re-ingests old output.
//!
//! The stream coordinate: tmux's `#{history_size}` plus a visible-row offset
//! gives every pane line a stable absolute index (row 0 = top of the visible
//! pane, negatives reach into history — the same coordinates `capture-pane
//! -S` uses). A cursor stores `total` (absolute line count delivered so far)
//! and `anchor` (the text of the last delivered line). The next delta read
//! captures from the anchor's row and verifies the anchor before emitting:
//!
//! - exact match  → the anchor line is skipped, everything below is new;
//! - prefix match → the anchor line *grew* (a prompt the agent typed a
//!   command onto: `$ ` → `$ ls`), so it is re-emitted along with the rest;
//! - mismatch     → the cursor is lost (pane cleared, TUI redraw, history
//!   trimmed past the anchor, session recreated) → fall back to a full
//!   seed read and start over.
//!
//! In-place rewrites of already-delivered lines (progress bars, TUI redraws)
//! are never re-delivered — delta reads are for line-oriented output; TUIs
//! stay on plain `read` + `key`.
//!
//! State lives in `read_cursors.json` in helm's state dir (same home as
//! `activity.jsonl` and the mosh probe cache), keyed per target. Best-effort
//! like the mosh cache: a missing/corrupt file just means reseeding.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::runcmd::strip_trailing_blank;
use crate::tmux;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    /// Absolute pane lines delivered so far (`history_size` + visible rows
    /// consumed at the last read). Always ≥ 1 when stored — an empty pane
    /// stores no cursor.
    pub total: u64,
    /// Text of the last delivered (non-blank) line, verified on the next read.
    pub anchor: String,
}

/// Cursor key for a `helm shell` target (per host alias + tmux session).
pub fn key_shell(alias: &str, session: &str) -> String {
    format!("shell:{alias}:{session}")
}

/// Cursor key for a `helm pane` target (per local pane id, e.g. `%3`). Pane
/// ids recycle after a kill; a stale cursor then fails the anchor check and
/// reseeds, so recycling is harmless.
pub fn key_pane(pane_id: &str) -> String {
    format!("pane:{pane_id}")
}

// ---- pure logic -------------------------------------------------------------

/// Outcome of verifying a delta capture against the stored cursor.
#[derive(Debug, PartialEq, Eq)]
pub enum Delta {
    /// Anchor verified. `emit` holds the new lines (possibly none) and
    /// `cursor` the advanced position.
    Advanced { emit: Vec<String>, cursor: Cursor },
    /// Anchor not found where expected — reseed with a full read.
    Lost,
}

/// Verify `captured_body` (a capture starting at the anchor's computed row)
/// against `prev` and slice out the new lines. Pure — the whole delta
/// decision is unit-tested without tmux.
pub fn advance(prev: &Cursor, captured_body: &str) -> Delta {
    let body = strip_trailing_blank(captured_body);
    if body.is_empty() {
        return Delta::Lost;
    }
    let lines: Vec<&str> = body.lines().collect();
    let first = lines[0];
    let emit_from = if first == prev.anchor {
        1 // anchor unchanged → everything below it is new
    } else if first.starts_with(prev.anchor.as_str()) {
        0 // anchor line grew (prompt + typed command) → re-emit it
    } else {
        return Delta::Lost;
    };
    let cursor = Cursor {
        // Line 0 sits at absolute index `prev.total - 1`; the capture ends at
        // the last non-blank visible line, so counting lines re-derives the
        // new total without a second history_size query.
        total: prev.total.saturating_sub(1) + lines.len() as u64,
        anchor: lines.last().map(|s| s.to_string()).unwrap_or_default(),
    };
    let emit = lines[emit_from..].iter().map(|s| s.to_string()).collect();
    Delta::Advanced { emit, cursor }
}

/// Derive a fresh cursor from a full seed capture (`-S -<lines>`) taken when
/// `history_size` was `hist`. The capture's first line sits at absolute index
/// `max(hist - lines, 0)`; adding the stripped line count gives the total.
/// Returns the stripped body plus the cursor (`None` for an empty pane —
/// nothing to anchor on yet).
pub fn seed(hist: u64, lines: u32, captured_body: &str) -> (String, Option<Cursor>) {
    let body = strip_trailing_blank(captured_body);
    let count = body.lines().count() as u64;
    if count == 0 {
        return (body, None);
    }
    let cursor = Cursor {
        total: hist.saturating_sub(u64::from(lines)) + count,
        anchor: body.lines().last().unwrap_or_default().to_string(),
    };
    (body, Some(cursor))
}

/// Cap an emit to its last `cap` lines. Returns `(skipped, kept)` — the
/// caller reports `skipped` so the agent knows lines were dropped (the
/// cursor has already advanced past them).
pub fn apply_cap(emit: &[String], cap: usize) -> (usize, &[String]) {
    if emit.len() > cap {
        (emit.len() - cap, &emit[emit.len() - cap..])
    } else {
        (0, emit)
    }
}

// ---- the shared delta-read driver -------------------------------------------

/// Result of one `read --delta` call, shaped for printing by both the shell
/// and pane CLIs.
pub enum DeltaRead {
    /// Cursor verified: `emit` = new lines only (may be empty), `skipped` =
    /// lines dropped by the `-n` cap.
    New { emit: String, skipped: usize },
    /// No usable cursor (first delta read, or the anchor was lost) — a full
    /// seed read was taken instead and the cursor re-established.
    Seeded { body: String, lost: bool },
}

impl DeltaRead {
    /// What goes on stdout (empty string = print nothing).
    pub fn stdout(&self) -> &str {
        match self {
            DeltaRead::New { emit, .. } => emit,
            DeltaRead::Seeded { body, .. } => body,
        }
    }

    /// Diagnostic note for stderr, if any (`lines` is the effective cap).
    pub fn note(&self, lines: u32) -> Option<String> {
        match self {
            DeltaRead::New { emit, skipped } => {
                if emit.is_empty() {
                    Some("no new output since last --delta read".to_string())
                } else if *skipped > 0 {
                    Some(format!(
                        "showing last {lines} new lines ({skipped} earlier new lines \
                         skipped; the cursor has advanced past them)"
                    ))
                } else {
                    None
                }
            }
            DeltaRead::Seeded { lost: true, .. } => Some(
                "delta cursor lost (pane cleared, redrawn, or history trimmed) — \
                 full read; the next --delta resumes from here"
                    .to_string(),
            ),
            DeltaRead::Seeded { lost: false, .. } => Some(
                "first --delta read — full read; subsequent --delta calls return \
                 only new lines"
                    .to_string(),
            ),
        }
    }
}

/// One `read --delta` against `tmux_target` (a session name for `helm shell`,
/// a pane id for `helm pane`) on `alias`'s tmux server. Loads the cursor,
/// captures, advances or reseeds, stores the new cursor.
pub fn delta_read(alias: &str, tmux_target: &str, key: &str, lines: u32) -> Result<DeltaRead> {
    let mut lost = false;
    if let Some(prev) = load(key) {
        let cap = tmux::capture_delta(alias, tmux_target, Some(prev.total), lines)?;
        match advance(&prev, &cap.body) {
            Delta::Advanced { emit, cursor } => {
                store(key, Some(&cursor));
                let (skipped, kept) = apply_cap(&emit, lines as usize);
                return Ok(DeltaRead::New {
                    emit: kept.join("\n"),
                    skipped,
                });
            }
            Delta::Lost => lost = true,
        }
    }
    // First delta read for this target, or the cursor was lost — take a full
    // seed capture and (re)establish the cursor.
    let cap = tmux::capture_delta(alias, tmux_target, None, lines)?;
    let (body, cursor) = seed(cap.hist, lines, &cap.body);
    store(key, cursor.as_ref());
    Ok(DeltaRead::Seeded { body, lost })
}

// ---- state file ---------------------------------------------------------------

type Map = HashMap<String, Cursor>;

fn state_path() -> Option<PathBuf> {
    crate::activity::state_dir().map(|d| d.join("read_cursors.json"))
}

fn load_map(path: &Path) -> Map {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_map(path: &Path, map: &Map) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(s) = serde_json::to_string(map) {
        let _ = std::fs::write(path, s);
    }
}

/// Load the cursor for `key`, if one is stored.
pub fn load(key: &str) -> Option<Cursor> {
    let p = state_path()?;
    load_map(&p).get(key).cloned()
}

/// Store (or with `None`, drop) the cursor for `key`. Best-effort — a failed
/// write only means the next delta read reseeds.
pub fn store(key: &str, cursor: Option<&Cursor>) {
    let Some(p) = state_path() else { return };
    let mut map = load_map(&p);
    match cursor {
        Some(c) => map.insert(key.to_string(), c.clone()),
        None => map.remove(key),
    };
    save_map(&p, &map);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cur(total: u64, anchor: &str) -> Cursor {
        Cursor {
            total,
            anchor: anchor.to_string(),
        }
    }

    #[test]
    fn advance_exact_anchor_emits_only_new_lines() {
        let prev = cur(10, "$ ls");
        match advance(&prev, "$ ls\nfile1\nfile2\n$ \n\n\n") {
            Delta::Advanced { emit, cursor } => {
                assert_eq!(emit, vec!["file1", "file2", "$ "]);
                assert_eq!(cursor, cur(13, "$ ")); // 9 + 4 captured lines
            }
            Delta::Lost => panic!("expected Advanced"),
        }
    }

    #[test]
    fn advance_prefix_anchor_reemits_grown_line() {
        // The prompt line the agent typed onto: `$ ` grew to `$ uptime`.
        let prev = cur(5, "$ ");
        match advance(&prev, "$ uptime\n 10:00 up 3 days\n$ \n") {
            Delta::Advanced { emit, cursor } => {
                assert_eq!(emit, vec!["$ uptime", " 10:00 up 3 days", "$ "]);
                assert_eq!(cursor, cur(7, "$ "));
            }
            Delta::Lost => panic!("expected Advanced"),
        }
    }

    #[test]
    fn advance_no_new_output_keeps_cursor_stable() {
        let prev = cur(10, "$ ls");
        match advance(&prev, "$ ls\n\n\n") {
            Delta::Advanced { emit, cursor } => {
                assert!(emit.is_empty());
                assert_eq!(cursor, prev);
            }
            Delta::Lost => panic!("expected Advanced"),
        }
    }

    #[test]
    fn advance_mismatch_and_empty_are_lost() {
        let prev = cur(10, "$ ls");
        assert_eq!(advance(&prev, "something else\nmore\n"), Delta::Lost);
        assert_eq!(advance(&prev, "\n\n"), Delta::Lost);
        // A shrunk/redrawn line is not a prefix match.
        assert_eq!(advance(&prev, "$ l\n"), Delta::Lost);
    }

    #[test]
    fn seed_math_with_deep_and_shallow_history() {
        // History deeper than the capture window: first line abs = 500 - 200.
        let (body, c) = seed(500, 200, "a\nb\nc\n\n\n");
        assert_eq!(body, "a\nb\nc");
        assert_eq!(c, Some(cur(303, "c")));
        // History shallower than the window: first line abs = 0.
        let (_, c) = seed(2, 200, "a\nb\nc\n");
        assert_eq!(c, Some(cur(3, "c")));
    }

    #[test]
    fn seed_empty_pane_stores_no_cursor() {
        let (body, c) = seed(0, 200, "\n\n\n");
        assert_eq!(body, "");
        assert_eq!(c, None);
    }

    #[test]
    fn apply_cap_keeps_last_lines() {
        let v: Vec<String> = ["a", "b", "c", "d"].iter().map(|s| s.to_string()).collect();
        let (skipped, kept) = apply_cap(&v, 2);
        assert_eq!(skipped, 2);
        assert_eq!(kept, &v[2..]);
        let (skipped, kept) = apply_cap(&v, 10);
        assert_eq!(skipped, 0);
        assert_eq!(kept, &v[..]);
    }

    #[test]
    fn map_round_trips_and_removes() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("read_cursors.json");
        let mut m = Map::new();
        m.insert("shell:web:helm".to_string(), cur(42, "$ "));
        save_map(&p, &m);
        let loaded = load_map(&p);
        assert_eq!(loaded.get("shell:web:helm"), Some(&cur(42, "$ ")));
        // Corrupt file degrades to empty (reseed), never errors.
        std::fs::write(&p, "not json").unwrap();
        assert!(load_map(&p).is_empty());
    }

    #[test]
    fn delta_read_notes() {
        let r = DeltaRead::New {
            emit: String::new(),
            skipped: 0,
        };
        assert!(r.note(200).unwrap().contains("no new output"));
        let r = DeltaRead::New {
            emit: "x".to_string(),
            skipped: 3,
        };
        assert!(r.note(200).unwrap().contains("3 earlier new lines"));
        let r = DeltaRead::Seeded {
            body: "x".to_string(),
            lost: true,
        };
        assert!(r.note(200).unwrap().contains("cursor lost"));
    }
}
