use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
};

use crate::app::App;
use crate::vultr::{Instance, VultrCache};

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let block = Block::default().borders(Borders::ALL).title("vultr");
    let inner = block.inner(area);
    f.render_widget(block, area);

    if let Some(err) = app.vultr_error.as_ref() {
        let p = Paragraph::new(Line::from(Span::styled(
            format!("error: {err}"),
            Style::default().fg(Color::Red),
        )));
        f.render_widget(p, inner);
        return;
    }

    let Some(cache) = app.vultr_cache.as_ref() else {
        let msg = if !app.vultr_fetch_attempted {
            "(set $VULTR_API_KEY to enable)"
        } else {
            "(fetching…)"
        };
        let p = Paragraph::new(Line::from(Span::styled(
            msg,
            Style::default().fg(Color::DarkGray),
        )));
        f.render_widget(p, inner);
        return;
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(inner);

    draw_header(f, chunks[0]);
    draw_body(f, chunks[1], app, cache);
}

fn draw_header(f: &mut Frame, area: Rect) {
    let line = Line::from(vec![
        header_cell(&format!("{:<14}", "LABEL")),
        Span::raw("  "),
        header_cell(&format!("{:<6}", "REGION")),
        Span::raw("  "),
        header_cell(&format!("{:<14}", "PLAN")),
        Span::raw("  "),
        header_cell(&format!("{:>6}", "$/mo")),
        Span::raw("  "),
        header_cell(&format!("{:<10}", "STATUS")),
        Span::raw("  "),
        header_cell(&format!("{:<8}", "POWER")),
        Span::raw("  "),
        header_cell(&format!("{:<15}", "IP")),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn header_cell(s: &str) -> Span<'static> {
    Span::styled(
        s.to_string(),
        Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD),
    )
}

fn draw_body(f: &mut Frame, area: Rect, app: &App, cache: &VultrCache) {
    let all_items: Vec<ListItem> = cache
        .instances
        .iter()
        .map(|inst| instance_row(inst, cache.cost_for(&inst.plan)))
        .collect();
    let total = all_items.len();
    let viewport = area.height as usize;
    let start = app.vultr_scroll.render_start(total, viewport);
    let end = (start + viewport).min(total);
    let visible: Vec<ListItem> = all_items.into_iter().skip(start).take(end - start).collect();
    f.render_widget(List::new(visible), area);
}

fn instance_row<'a>(inst: &Instance, cost: Option<f32>) -> ListItem<'a> {
    let label_cell = format!("{:<14}", truncate(&inst.label, 14));
    let region_cell = format!("{:<6}", truncate(&inst.region, 6));
    let plan_cell = format!("{:<14}", truncate(&inst.plan, 14));
    let cost_cell = match cost {
        Some(c) => format!("{:>6}", format!("${c:.2}")),
        None => format!("{:>6}", "?"),
    };
    let status_cell = format!("{:<10}", truncate(&inst.status, 10));
    let power_cell = format!("{:<8}", truncate(&inst.power_status, 8));
    let ip_cell = format!("{:<15}", truncate(&inst.main_ip, 15));

    let status_color = if inst.status == "active" {
        Color::Green
    } else {
        Color::Red
    };
    let power_color = if inst.power_status == "running" {
        Color::Green
    } else {
        Color::Red
    };

    ListItem::new(Line::from(vec![
        Span::raw(label_cell),
        Span::raw("  "),
        Span::raw(region_cell),
        Span::raw("  "),
        Span::raw(plan_cell),
        Span::raw("  "),
        Span::raw(cost_cell),
        Span::raw("  "),
        Span::styled(
            status_cell,
            Style::default().fg(status_color).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            power_cell,
            Style::default().fg(power_color).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::raw(ip_cell),
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
