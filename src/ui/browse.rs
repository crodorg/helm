use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::app::App;

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(area);

    draw_hosts(f, cols[0], app);
    draw_detail(f, cols[1], app);
}

fn draw_hosts(f: &mut Frame, area: Rect, app: &App) {
    let items: Vec<ListItem> = app
        .hosts()
        .iter()
        .map(|h| {
            let badge = Span::styled(
                format!(" {} ", h.provider.label()),
                Style::default().fg(Color::Black).bg(Color::Blue),
            );
            ListItem::new(Line::from(vec![
                badge,
                Span::raw(" "),
                Span::styled(h.name.clone(), Style::default().add_modifier(Modifier::BOLD)),
                Span::raw("  "),
                Span::styled(
                    h.display_hostname().to_string(),
                    Style::default().fg(Color::DarkGray),
                ),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("hosts"))
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    let mut state = ListState::default();
    if !app.hosts().is_empty() {
        state.select(Some(app.selected));
    }
    f.render_stateful_widget(list, area, &mut state);
}

fn draw_detail(f: &mut Frame, area: Rect, app: &App) {
    let block = Block::default().borders(Borders::ALL).title("detail");
    let lines: Vec<Line> = match app.selected_host() {
        None => vec![Line::from(Span::styled(
            "no hosts configured — see config.example.toml",
            Style::default().fg(Color::Red),
        ))],
        Some(h) => {
            let mut v = vec![
                kv("name", &h.name),
                kv("provider", h.provider.label()),
                kv("ssh alias", &h.ssh_alias),
                kv("hostname", h.display_hostname()),
                kv("user", h.display_user()),
            ];
            if !h.notes.is_empty() {
                v.push(Line::from(""));
                v.push(Line::from(Span::styled(
                    h.notes.clone(),
                    Style::default().fg(Color::Gray),
                )));
            }
            if let Some(cache) = app.vultr_cache.as_ref() {
                if let Some(inst) = cache.instance_for_ip(h.display_hostname()) {
                    v.push(Line::from(""));
                    let cost = cache
                        .cost_for(&inst.plan)
                        .map(|c| format!("${c:.2}/mo"))
                        .unwrap_or_else(|| "?".into());
                    v.push(kv(
                        "vultr",
                        &format!(
                            "region={}  plan={}  {}",
                            inst.region, inst.plan, cost
                        ),
                    ));
                    v.push(kv(
                        "",
                        &format!(
                            "status={}  power={}  id={}",
                            inst.status, inst.power_status, inst.id
                        ),
                    ));
                }
            }
            let bizes: Vec<_> = app
                .config
                .businesses
                .iter()
                .filter(|b| b.host == h.name)
                .collect();
            if !bizes.is_empty() {
                v.push(Line::from(""));
                v.push(Line::from(Span::styled(
                    "businesses on this host:",
                    Style::default().add_modifier(Modifier::BOLD),
                )));
                for b in bizes {
                    v.push(Line::from(vec![
                        Span::raw("  • "),
                        Span::styled(
                            b.name.clone(),
                            Style::default().add_modifier(Modifier::BOLD),
                        ),
                        Span::raw("  "),
                        Span::styled(
                            b.primary_domain.clone(),
                            Style::default().fg(Color::Cyan),
                        ),
                    ]));
                }
            }
            v
        }
    };
    let p = Paragraph::new(lines).block(block).wrap(Wrap { trim: false });
    f.render_widget(p, area);
}

fn kv(k: &str, v: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("  {:<10}", k),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(v.to_string()),
    ])
}
