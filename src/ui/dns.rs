use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
};

use crate::app::{App, DnsState};
use crate::inventory::dns::{DnsCheck, DnsVerdict};

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let Some(state) = app.dns_pane.as_ref() else {
        let p = Paragraph::new(Span::styled(
            "no dns state — should not happen",
            Style::default().fg(Color::Red),
        ))
        .block(Block::default().borders(Borders::ALL).title("dns"));
        f.render_widget(p, area);
        return;
    };

    let title = format!(
        "dns › {} businesses ({} pending)",
        state.rows.len(),
        state.pending_count()
    );
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(inner);

    draw_header(f, chunks[0]);
    draw_body(f, chunks[1], state);
}

fn draw_header(f: &mut Frame, area: Rect) {
    let line = Line::from(vec![
        header_cell(&format!("{:<16}", "BUSINESS")),
        Span::raw("  "),
        header_cell(&format!("{:<28}", "DOMAIN")),
        Span::raw("  "),
        header_cell(&format!("{:<8}", "VERDICT")),
        Span::raw("  "),
        header_cell("A / AAAA / MX / CAA"),
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

fn draw_body(f: &mut Frame, area: Rect, s: &DnsState) {
    let mut all_items: Vec<ListItem> = Vec::new();
    for (idx, name) in s.business_names.iter().enumerate() {
        let row = s.rows.get(idx).and_then(|r| r.as_ref());
        all_items.extend(rows_for(name, row));
    }
    let total = all_items.len();
    let viewport = area.height as usize;
    let start = s.scroll.render_start(total, viewport);
    let end = (start + viewport).min(total);
    let visible: Vec<ListItem> = all_items
        .into_iter()
        .skip(start)
        .take(end - start)
        .collect();
    f.render_widget(List::new(visible), area);
}

/// One business produces 1 summary line + up to 3 detail lines (one per
/// non-empty record set excluding A, which goes on the summary line for
/// at-a-glance verdict context).
fn rows_for<'a>(name: &str, row: Option<&DnsCheck>) -> Vec<ListItem<'a>> {
    let biz_cell = format!("{:<16}", truncate(name, 16));
    let Some(c) = row else {
        return vec![ListItem::new(Line::from(vec![
            Span::raw(biz_cell),
            Span::raw("  "),
            Span::styled(
                format!("{:<28}", "(pending)"),
                Style::default().fg(Color::DarkGray),
            ),
        ]))];
    };

    let domain_cell = format!("{:<28}", truncate(&c.domain, 28));
    let (verdict_text, verdict_color) = match c.verdict {
        DnsVerdict::Match => ("MATCH", Color::Green),
        DnsVerdict::Mismatch => ("MISMATCH", Color::Red),
        DnsVerdict::Unknown => ("?", Color::DarkGray),
        DnsVerdict::Error => ("ERROR", Color::Red),
    };
    let a_summary = if c.a.is_empty() {
        "(no A records)".to_string()
    } else {
        c.a.join(", ")
    };

    let summary = ListItem::new(Line::from(vec![
        Span::raw(biz_cell),
        Span::raw("  "),
        Span::raw(domain_cell),
        Span::raw("  "),
        Span::styled(
            format!("{:<8}", verdict_text),
            Style::default()
                .fg(verdict_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            format!("A {}", truncate(&a_summary, 120)),
            Style::default().fg(Color::Cyan),
        ),
    ]));

    let mut out = vec![summary];

    if let Some(err) = c.error.as_ref() {
        out.push(detail_line("err", err, Color::Red));
    }
    if let Some(ip) = c.expected_ip.as_ref() {
        out.push(detail_line(
            "expected",
            &format!("{ip} (from host hostname)"),
            Color::DarkGray,
        ));
    }
    if !c.aaaa.is_empty() {
        out.push(detail_line("AAAA", &c.aaaa.join(", "), Color::Cyan));
    }
    if !c.mx.is_empty() {
        out.push(detail_line("MX", &c.mx.join(", "), Color::Cyan));
    }
    if !c.caa.is_empty() {
        out.push(detail_line("CAA", &c.caa.join(", "), Color::Yellow));
    }
    out.push(ListItem::new(Line::from("")));
    out
}

fn detail_line<'a>(label: &str, body: &str, accent: Color) -> ListItem<'a> {
    ListItem::new(Line::from(vec![
        Span::raw("                  "),
        Span::styled(
            format!("{:<10}", label),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::raw(truncate(body, 160)),
    ]))
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
