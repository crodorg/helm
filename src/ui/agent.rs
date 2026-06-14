//! Agent activity audit pane.
//!
//! Reads `activity.jsonl` (the append-only audit log written by every
//! agent-facing helm CLI invocation) and renders the tail as a vertical
//! list of rows the operator can scan at a glance. Every `helm exec`,
//! `helm shell open/send/read/list/close` lands here — there is no
//! agent-driven action helm performs that is invisible from this pane.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::activity::{self, ActivityKind, ActivityRecord};
use crate::app::App;

const TAIL_LIMIT: usize = 200;

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let records = activity::tail(TAIL_LIMIT);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(
            " agent activity  ({} recent · {}) ",
            records.len(),
            activity::log_path()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "no log path".to_string())
        ))
        .border_style(Style::default().fg(Color::Magenta));

    let mut lines: Vec<Line> = Vec::new();

    if records.is_empty() {
        lines.push(Line::from(Span::styled(
            "no agent activity logged yet — try `helm shell send local:scratch \"echo hi\"`",
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "every `helm exec` and `helm shell {open,send,read,list,close}` writes a",
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(Span::styled(
            "row here, regardless of which agent (Claude / Cursor / Aider / scripts) invoked it.",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        // Newest at the top — the operator's eye lands there first.
        for record in records.iter().rev() {
            for line in render_record(record) {
                lines.push(line);
            }
        }
    }

    let inner_h = area.height.saturating_sub(2) as usize;
    let total = lines.len();
    let start = app.agent_tail_scroll.render_start(total, inner_h);

    let p = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((start as u16, 0));
    f.render_widget(p, area);
}

fn render_record(r: &ActivityRecord) -> Vec<Line<'static>> {
    let time = format_hms(r.ts_unix);
    let kind = r.kind.label();
    let kind_color = kind_color(r.kind);
    let target = if r.session.is_empty() {
        r.alias.clone()
    } else {
        format!("{}:{}", r.alias, r.session)
    };
    let exit_span = match r.exit {
        Some(0) => Span::styled("ok ", Style::default().fg(Color::Green)),
        Some(c) => Span::styled(
            format!("exit {c} "),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        None => Span::styled("…   ", Style::default().fg(Color::Yellow)),
    };

    let mut header_spans: Vec<Span<'static>> = vec![
        Span::styled(format!("{time} "), Style::default().fg(Color::DarkGray)),
        exit_span,
        Span::styled(
            format!("{kind:<5} "),
            Style::default().fg(kind_color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{target:<22} "),
            Style::default().fg(target_color(&r.alias)),
        ),
    ];
    if r.has_privilege_escalation {
        header_spans.push(Span::styled(
            "[DOAS] ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Red)
                .add_modifier(Modifier::BOLD),
        ));
    }
    header_spans.push(Span::raw(truncate(&r.cmd, 200)));

    let mut out = vec![Line::from(header_spans)];
    if !r.output_preview.is_empty() {
        out.push(Line::from(vec![
            Span::raw("                                            "),
            Span::styled(
                format!("↳ {}", truncate(&r.output_preview, 200)),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    }
    out
}

fn format_hms(ts_unix: u64) -> String {
    // No chrono dep — keep it stdlib-only. Render UTC HH:MM:SS; the user
    // can correlate against their local clock if needed.
    let secs_today = (ts_unix % 86_400) as u32;
    let h = secs_today / 3600;
    let m = (secs_today % 3600) / 60;
    let s = secs_today % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{head}…")
    }
}

fn kind_color(k: ActivityKind) -> Color {
    match k {
        ActivityKind::Exec => Color::Cyan,
        ActivityKind::ShellSend => Color::Yellow,
        ActivityKind::ShellOpen => Color::Green,
        ActivityKind::ShellRead => Color::Blue,
        ActivityKind::ShellList => Color::Gray,
        ActivityKind::ShellClose => Color::Red,
    }
}

fn target_color(alias: &str) -> Color {
    if alias == crate::tmux::LOCAL_ALIAS {
        Color::Cyan
    } else {
        Color::Magenta
    }
}
