//! Golden-file snapshot tests for the TUI.
//!
//! Each test renders the full chrome (header / body / footer) into a
//! `TestBackend` of a fixed size, dumps the cell grid as plain text with
//! trailing whitespace trimmed per line, and compares against a checked-in
//! fixture in `src/ui/snapshots/*.txt`.
//!
//! To regenerate after an intentional change:
//!     HELM_UPDATE_SNAPSHOTS=1 cargo test ui::snapshots
//! and review the resulting diff with `git diff src/ui/snapshots/`.
//!
//! Style attributes (color, bold) are deliberately dropped — fixtures
//! diff on layout + content only. A future iteration can capture style
//! using `Cell::fg/bg` if regressions in color rendering ever surface.

#![cfg(test)]

use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

use crate::app::{App, Mode};
use crate::config::Config;

fn render<F: FnOnce(&mut App)>(width: u16, height: u16, configure: F) -> String {
    let mut app = App::new(Config::default());
    configure(&mut app);
    let backend = TestBackend::new(width, height);
    let mut term = Terminal::new(backend).expect("create test terminal");
    term.draw(|f| super::draw(f, &app)).expect("draw frame");
    buffer_to_string(term.backend().buffer())
}

fn buffer_to_string(buf: &Buffer) -> String {
    let area = buf.area;
    let mut out = String::new();
    for y in 0..area.height {
        let mut row = String::new();
        for x in 0..area.width {
            if let Some(cell) = buf.cell((x, y)) {
                row.push_str(cell.symbol());
            }
        }
        // Trim trailing spaces so fixtures stay diff-friendly when blocks
        // re-flow into trailing whitespace.
        let trimmed = row.trim_end();
        out.push_str(trimmed);
        out.push('\n');
    }
    out
}

fn assert_snapshot(name: &str, actual: &str) {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/ui/snapshots")
        .join(format!("{name}.txt"));

    let update = std::env::var("HELM_UPDATE_SNAPSHOTS").is_ok();
    if update || !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create snapshots dir");
        }
        std::fs::write(&path, actual).expect("write snapshot");
        if !update {
            // First-run bootstrap: warn the dev that a new fixture was
            // written so they can review it before committing.
            eprintln!(
                "snapshot[{name}]: created new fixture at {}",
                path.display()
            );
        }
        return;
    }

    let expected = std::fs::read_to_string(&path).expect("read snapshot fixture");
    if expected != actual {
        panic!(
            "snapshot drift in `{name}`. Re-run with HELM_UPDATE_SNAPSHOTS=1 to accept.\n\
             --- expected ({} bytes) ---\n{expected}\
             --- actual ({} bytes) ---\n{actual}\
             --- end ---",
            expected.len(),
            actual.len(),
        );
    }
}

#[test]
fn browse_empty_fleet() {
    let s = render(80, 16, |_| {});
    assert_snapshot("browse_empty_fleet", &s);
}

#[test]
fn help_overlay_from_browse() {
    let s = render(80, 20, |app| {
        app.open_help();
    });
    assert_snapshot("help_overlay_from_browse", &s);
}

#[test]
fn money_pane_no_cache() {
    let s = render(80, 16, |app| {
        app.mode = Mode::Money;
    });
    assert_snapshot("money_pane_no_cache", &s);
}

#[test]
fn vultr_pane_no_key() {
    let s = render(80, 16, |app| {
        app.mode = Mode::Vultr;
    });
    assert_snapshot("vultr_pane_no_key", &s);
}
