use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
};

use crate::app::{App, HistoryState};
use crate::history::{RunRecord, RunSource};

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let Some(state) = app.history_pane.as_ref() else {
        let p = Paragraph::new(Span::styled(
            "no history state — should not happen",
            Style::default().fg(Color::Red),
        ))
        .block(Block::default().borders(Borders::ALL).title("history"));
        f.render_widget(p, area);
        return;
    };

    let title = format!("history › {} runs", state.entries.len());
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if let Some(err) = state.error.as_ref() {
        let p = Paragraph::new(Line::from(Span::styled(
            format!("error: {err}"),
            Style::default().fg(Color::Red),
        )));
        f.render_widget(p, inner);
        return;
    }

    if state.entries.is_empty() {
        let p = Paragraph::new(Line::from(Span::styled(
            "(no runs yet — every `helm exec` and Runner submission is logged here)",
            Style::default().fg(Color::DarkGray),
        )));
        f.render_widget(p, inner);
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(inner);

    draw_header(f, chunks[0]);
    draw_body(f, chunks[1], state);
}

fn draw_header(f: &mut Frame, area: Rect) {
    let line = Line::from(vec![
        header_cell(&format!("{:<19}", "WHEN")),
        Span::raw("  "),
        header_cell(&format!("{:<5}", "SRC")),
        Span::raw("  "),
        header_cell(&format!("{:<14}", "ALIAS")),
        Span::raw("  "),
        header_cell(&format!("{:>4}", "EXIT")),
        Span::raw("  "),
        header_cell(&format!("{:>6}", "DUR")),
        Span::raw("  "),
        header_cell("CMD"),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn header_cell(s: &str) -> Span<'static> {
    Span::styled(
        s.to_string(),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    )
}

fn draw_body(f: &mut Frame, area: Rect, s: &HistoryState) {
    let viewport = area.height as usize;
    let total = s.entries.len();
    // Keep the selected row visible by nudging the scroll offset to wrap it.
    s.scroll.ensure_visible(s.selected, total, viewport);
    let start = s.scroll.render_start(total, viewport);
    let end = (start + viewport).min(total);

    let visible: Vec<ListItem> = s
        .entries
        .iter()
        .enumerate()
        .skip(start)
        .take(end - start)
        .map(|(idx, rec)| row_to_item(rec, idx == s.selected))
        .collect();
    f.render_widget(List::new(visible), area);
}

fn row_to_item<'a>(r: &RunRecord, selected: bool) -> ListItem<'a> {
    let when = format_when(r.started_at_unix);
    let src = match r.source {
        RunSource::Agent => "agent",
        RunSource::Operator => "op",
    };
    let exit_cell = match r.exit {
        Some(0) => format!("{:>4}", "0"),
        Some(code) => format!("{:>4}", code),
        None => format!("{:>4}", "?"),
    };
    let exit_color = match r.exit {
        Some(0) => Color::Green,
        Some(_) => Color::Red,
        None => Color::DarkGray,
    };
    let dur_cell = match r.duration_ms {
        Some(ms) => format!("{:>6}", format_duration(ms)),
        None => format!("{:>6}", "?"),
    };

    let base = if selected {
        Style::default().bg(Color::DarkGray)
    } else {
        Style::default()
    };

    let spans = vec![
        Span::styled(format!("{:<19}", when), base),
        Span::styled("  ".to_string(), base),
        Span::styled(format!("{:<5}", src), base),
        Span::styled("  ".to_string(), base),
        Span::styled(
            format!("{:<14}", truncate(&r.alias, 14)),
            base.add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ".to_string(), base),
        Span::styled(exit_cell, base.fg(exit_color).add_modifier(Modifier::BOLD)),
        Span::styled("  ".to_string(), base),
        Span::styled(dur_cell, base),
        Span::styled("  ".to_string(), base),
        Span::styled(truncate(&r.cmd, 200), base),
    ];
    ListItem::new(Line::from(spans))
}

fn format_when(unix: i64) -> String {
    // Local clock via SystemTime — same trick the rest of helm uses.
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs() as i64;
    let delta = now - unix;
    if delta < 60 {
        return format!("{delta}s ago");
    }
    if delta < 3600 {
        return format!("{}m ago", delta / 60);
    }
    if delta < 86_400 {
        return format!("{}h {}m ago", delta / 3600, (delta % 3600) / 60);
    }
    let days = delta / 86_400;
    if days < 30 {
        return format!("{days}d ago");
    }
    // Older than a month: fall back to a stable YYYY-MM-DD using a minimal
    // date conversion (we already pull `directories` etc.; avoid chrono).
    format_date(unix)
}

fn format_date(unix: i64) -> String {
    // Civil-from-days algorithm (Howard Hinnant).
    let days = unix.div_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}", y, m, d)
}

fn format_duration(ms: i64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{}m{}s", ms / 60_000, (ms % 60_000) / 1000)
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_buckets() {
        assert_eq!(format_duration(0), "0ms");
        assert_eq!(format_duration(999), "999ms");
        assert_eq!(format_duration(1500), "1.5s");
        assert_eq!(format_duration(60_000), "1m0s");
        assert_eq!(format_duration(125_000), "2m5s");
    }

    #[test]
    fn date_format_known_epoch() {
        // 2026-05-24 00:00:00 UTC = 1_779_580_800 (per `date -u -d @1779580800`).
        assert_eq!(format_date(1_779_580_800), "2026-05-24");
        // 1970-01-01
        assert_eq!(format_date(0), "1970-01-01");
        // 2000-01-01
        assert_eq!(format_date(946_684_800), "2000-01-01");
    }

    #[test]
    fn truncate_handles_unicode() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("helloworld", 5), "hell…");
    }
}
