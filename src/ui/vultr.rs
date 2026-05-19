use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
};

use crate::app::{App, VultrToast, VultrToastKind};
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

    let toast_h: u16 = if app.vultr_toast.is_some() { 1 } else { 0 };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(toast_h),
        ])
        .split(inner);

    draw_header(f, chunks[0]);
    draw_body(f, chunks[1], app, cache);
    if let Some(toast) = app.vultr_toast.as_ref() {
        draw_toast(f, chunks[2], toast);
    }
}

fn draw_toast(f: &mut Frame, area: Rect, toast: &VultrToast) {
    let (label, bg) = match toast.kind {
        VultrToastKind::Firing => (" FIRING ", Color::Yellow),
        VultrToastKind::Success => (" OK ", Color::Green),
        VultrToastKind::Error => (" ERR ", Color::Red),
    };
    let line = Line::from(vec![
        Span::styled(
            label.to_string(),
            Style::default()
                .fg(Color::Black)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            toast.message.clone(),
            Style::default().fg(bg).add_modifier(Modifier::BOLD),
        ),
    ]);
    f.render_widget(Paragraph::new(line), area);
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
        .enumerate()
        .map(|(idx, inst)| {
            instance_row(inst, cache.cost_for(&inst.plan), idx == app.vultr_selected)
        })
        .collect();
    let total = all_items.len();
    let viewport = area.height as usize;
    // Keep the selected row visible as the user moves through the list.
    app.vultr_scroll
        .ensure_visible(app.vultr_selected, total, viewport);
    let start = app.vultr_scroll.render_start(total, viewport);
    let end = (start + viewport).min(total);
    let visible: Vec<ListItem> = all_items.into_iter().skip(start).take(end - start).collect();
    f.render_widget(List::new(visible), area);

    // Overlay the confirm modal on top of the body when a request is pending.
    if let Some(confirm) = app.vultr_confirm.as_ref() {
        draw_confirm_modal(f, area, confirm);
    }
}

fn draw_confirm_modal(f: &mut Frame, area: Rect, confirm: &crate::app::VultrConfirm) {
    // Centered 60% × 30% box.
    let w = (area.width * 6 / 10).max(50);
    // 7 rows for non-billable actions, +1 for the snapshot cost warning.
    let h = if confirm.action == crate::vultr::ActionKind::Snapshot { 8 } else { 7 };
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.y + area.height.saturating_sub(h) / 2;
    let modal_area = Rect { x, y, width: w, height: h };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" confirm vultr action ")
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(modal_area);
    // Wipe behind the modal so the table doesn't bleed through.
    f.render_widget(ratatui::widgets::Clear, modal_area);
    f.render_widget(block, modal_area);

    let mut lines = vec![
        ratatui::text::Line::from(vec![
            Span::raw("action: "),
            Span::styled(
                confirm.action.label(),
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            ),
        ]),
        ratatui::text::Line::from(vec![
            Span::raw("target: "),
            Span::styled(
                confirm.label.clone(),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  ({})", confirm.instance_id),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
    ];
    if confirm.action == crate::vultr::ActionKind::Snapshot {
        lines.push(ratatui::text::Line::from(vec![
            Span::styled(
                "$ ",
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "BILLABLE",
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" — vultr charges ~$0.05/GB/mo until deleted"),
        ]));
    }
    lines.push(ratatui::text::Line::from(""));
    lines.push(ratatui::text::Line::from(vec![
        Span::styled(
            "[y]",
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" confirm   "),
        Span::styled(
            "[n/esc]",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" cancel"),
    ]));
    f.render_widget(Paragraph::new(lines), inner);
}

fn instance_row<'a>(inst: &Instance, cost: Option<f32>, selected: bool) -> ListItem<'a> {
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

    let base = if selected {
        Style::default().bg(Color::DarkGray)
    } else {
        Style::default()
    };

    ListItem::new(Line::from(vec![
        Span::styled(label_cell, base),
        Span::styled("  ".to_string(), base),
        Span::styled(region_cell, base),
        Span::styled("  ".to_string(), base),
        Span::styled(plan_cell, base),
        Span::styled("  ".to_string(), base),
        Span::styled(cost_cell, base),
        Span::styled("  ".to_string(), base),
        Span::styled(
            status_cell,
            base.fg(status_color).add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ".to_string(), base),
        Span::styled(
            power_cell,
            base.fg(power_color).add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ".to_string(), base),
        Span::styled(ip_cell, base),
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
