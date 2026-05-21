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

    // Left column splits vertically: hosts on top, key palette below.
    // Palette height: one row per binding + 2 borders, capped so the
    // host list always has at least 5 visible rows even on small terms.
    let raw = crate::help::bindings_for(app.mode, app.runner.focus);
    let bindings: Vec<crate::help::Binding> = raw
        .iter()
        .filter(|b| {
            app.mode != crate::app::Mode::Browse
                || app.config.features.browse_key_enabled(b.key)
        })
        .copied()
        .collect();
    let palette_h = (bindings.len() as u16 + 2)
        .min(cols[0].height.saturating_sub(5))
        .max(3);

    let left = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(palette_h)])
        .split(cols[0]);

    draw_hosts(f, left[0], app);
    draw_keys_palette(f, left[1], &bindings);
    draw_detail(f, cols[1], app);
}

fn draw_keys_palette(f: &mut Frame, area: Rect, bindings: &[crate::help::Binding]) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" keys (?) ")
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Single column — keeps long action labels readable. The ? overlay
    // exists for when the palette gets truncated by a tiny terminal.
    let max_key = bindings.iter().map(|b| b.key.len()).max().unwrap_or(0);
    let lines: Vec<Line<'static>> = bindings
        .iter()
        .map(|b| {
            Line::from(vec![
                Span::raw(" "),
                Span::styled(
                    format!("[{}]", b.key),
                    Style::default().fg(Color::Yellow),
                ),
                Span::raw(" ".repeat(max_key.saturating_sub(b.key.len()) + 2)),
                Span::styled(b.action, Style::default().fg(Color::White)),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
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
                .fg(Color::Black)
                .bg(Color::Yellow)
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
                    push_money_lines(&mut v, b, app);
                    push_postmark_lines(&mut v, b, app);
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

/// Render at most two indented lines per business — Mercury balance (if
/// linked + cache loaded) and a Stripe linkage badge (if id set). Caller
/// supplies the parent line vector.
fn push_money_lines(v: &mut Vec<Line<'static>>, b: &crate::config::Business, app: &App) {
    let cache = match app.money_cache.as_ref() {
        Some(c) => c,
        _ => {
            // No cache yet: show a placeholder only if any linkage is set,
            // so the user knows the row is intentional and pending.
            if b.stripe_account_id.is_some() || b.mercury_account_id.is_some() {
                v.push(money_line("money", "(fetching… press m to force)", Color::DarkGray));
            }
            return;
        }
    };
    if let Some(id) = b.mercury_account_id.as_ref() {
        let label = match cache.mercury_for_id(id) {
            Some(acc) => format!(
                "{} — avail ${:.2}  curr ${:.2}",
                acc.name, acc.available_balance, acc.current_balance
            ),
            _ => format!("(mercury id '{id}' not in account list)"),
        };
        v.push(money_line("mercury", &label, Color::Green));
    }
    if let Some(id) = b.stripe_account_id.as_ref() {
        let stripe_status = if let Some(s) = cache.stripe_for_connect(id) {
            let cur = s.currency.to_uppercase();
            format!(
                "{id} — avail {} pending {}  total {}",
                fmt_amount(s.available_cents, &cur),
                fmt_amount(s.pending_cents, &cur),
                fmt_amount(s.available_cents + s.pending_cents, &cur),
            )
        } else if let Some(e) = cache.stripe_error_for_connect(id) {
            format!("{id} — fetch error: {e}")
        } else {
            format!("{id} — (fetching…)")
        };
        v.push(money_line("stripe", &stripe_status, Color::Magenta));
    }
}

fn fmt_amount(cents: i64, currency: &str) -> String {
    let dollars = cents as f64 / 100.0;
    if currency == "USD" {
        format!("${:.2}", dollars)
    } else {
        format!("{:.2} {}", dollars, currency)
    }
}

/// Render a single Postmark line per business when a token is configured.
/// Handles three states: not-yet-fetched, error, and success. Renders
/// nothing when the business has no token.
pub fn push_postmark_lines(v: &mut Vec<Line<'static>>, b: &crate::config::Business, app: &App) {
    if b.postmark_server_token.is_none() {
        return;
    }
    let body = match app.postmark_results.get(&b.name) {
        Some(Ok(s)) => format!(
            "{} sent  {} bounced ({:.1}%)  {} spam ({:.1}%)   {}→{}",
            s.sent, s.bounced, s.bounce_rate, s.spam_complaints, s.spam_rate,
            s.from_date, s.to_date,
        ),
        Some(Err(e)) => format!("error: {e}"),
        _ if app.postmark_rx.is_some() => "(fetching…)".to_string(),
        _ if app.postmark_fetch_attempted => "(no result — token may be empty)".to_string(),
        _ => "(pending — press refresh)".to_string(),
    };
    let color = match app.postmark_results.get(&b.name) {
        Some(Err(_)) => Color::Red,
        _ => Color::Yellow,
    };
    v.push(money_line("postmark", &body, color));
}

fn money_line(label: &str, body: &str, accent: Color) -> Line<'static> {
    Line::from(vec![
        Span::raw("      "),
        Span::styled(
            format!("{:<8}", label),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::raw(body.to_string()),
    ])
}
