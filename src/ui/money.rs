use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
};

use crate::app::App;
use crate::money::{BalanceField, MercuryAccount, MoneyCache, StripeSnapshot};

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let connect_rows = app
        .money_cache
        .as_ref()
        .map(|c| c.stripe_connect.len() + c.stripe_connect_errors.len())
        .unwrap_or(0) as u16;
    // 6 = 2 border + 3 platform rows + 1 spacer; +1 per Connect line.
    // Cap stripe block at 2/3 of pane so Mercury always has room.
    let stripe_h = (6 + connect_rows).min(area.height.saturating_mul(2) / 3).max(6);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(stripe_h), Constraint::Min(0)])
        .split(area);

    draw_stripe(f, chunks[0], app);
    draw_mercury(f, chunks[1], app);
}

fn draw_stripe(f: &mut Frame, area: Rect, app: &App) {
    let block = Block::default().borders(Borders::ALL).title("stripe");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let Some(cache) = app.money_cache.as_ref() else {
        let msg = if !app.money_fetch_attempted {
            "(press r to fetch)"
        } else {
            "(fetching…)"
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                msg,
                Style::default().fg(Color::DarkGray),
            ))),
            inner,
        );
        return;
    };
    if let Some(err) = cache.stripe_error.as_ref() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("error: {err}"),
                Style::default().fg(Color::Red),
            ))),
            inner,
        );
        return;
    }
    let Some(s) = cache.stripe.as_ref() else {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "(fetching…)",
                Style::default().fg(Color::DarkGray),
            ))),
            inner,
        );
        return;
    };

    let mut lines = stripe_lines(s);
    // Per-Connect rows: sort by acct id for stable rendering.
    if !cache.stripe_connect.is_empty() || !cache.stripe_connect_errors.is_empty() {
        lines.push(Line::from(""));
        let mut accts: Vec<&String> = cache
            .stripe_connect
            .keys()
            .chain(cache.stripe_connect_errors.keys())
            .collect();
        accts.sort();
        accts.dedup();
        for acct in accts {
            if let Some(snap) = cache.stripe_connect.get(acct) {
                let cur = snap.currency.to_uppercase();
                lines.push(kv(
                    acct,
                    &format!(
                        "avail {} pending {}",
                        format_amount(snap.available_cents, &cur),
                        format_amount(snap.pending_cents, &cur),
                    ),
                    Color::Magenta,
                ));
            } else if let Some(err) = cache.stripe_connect_errors.get(acct) {
                lines.push(kv(acct, &format!("error: {err}"), Color::Red));
            }
        }
    }
    let p = Paragraph::new(lines);
    f.render_widget(p, inner);
}

fn stripe_lines(s: &StripeSnapshot) -> Vec<Line<'static>> {
    let cur = s.currency.to_uppercase();
    vec![
        kv("available (platform)", &format_amount(s.available_cents, &cur), Color::Green),
        kv("pending   (platform)", &format_amount(s.pending_cents, &cur), Color::Yellow),
        kv(
            "total     (platform)",
            &format_amount(s.available_cents + s.pending_cents, &cur),
            Color::White,
        ),
    ]
}

fn draw_mercury(f: &mut Frame, area: Rect, app: &App) {
    let block = Block::default().borders(Borders::ALL).title("mercury");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let Some(cache) = app.money_cache.as_ref() else {
        let msg = if !app.money_fetch_attempted {
            "(press r to fetch)"
        } else {
            "(fetching…)"
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                msg,
                Style::default().fg(Color::DarkGray),
            ))),
            inner,
        );
        return;
    };
    if let Some(err) = cache.mercury_error.as_ref() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("error: {err}"),
                Style::default().fg(Color::Red),
            ))),
            inner,
        );
        return;
    }
    if cache.mercury.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "(no Mercury accounts returned)",
                Style::default().fg(Color::DarkGray),
            ))),
            inner,
        );
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0), Constraint::Length(1)])
        .split(inner);

    draw_mercury_header(f, chunks[0]);
    draw_mercury_body(f, chunks[1], &cache.mercury);
    draw_mercury_total(f, chunks[2], cache);
}

fn draw_mercury_header(f: &mut Frame, area: Rect) {
    let line = Line::from(vec![
        header_cell(&format!("{:<24}", "ACCOUNT")),
        Span::raw("  "),
        header_cell(&format!("{:<10}", "KIND")),
        Span::raw("  "),
        header_cell(&format!("{:>14}", "CURRENT")),
        Span::raw("  "),
        header_cell(&format!("{:>14}", "AVAILABLE")),
        Span::raw("  "),
        header_cell(&format!("{:<4}", "CCY")),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn draw_mercury_body(f: &mut Frame, area: Rect, accounts: &[MercuryAccount]) {
    let items: Vec<ListItem> = accounts.iter().map(account_row).collect();
    f.render_widget(List::new(items), area);
}

fn draw_mercury_total(f: &mut Frame, area: Rect, cache: &MoneyCache) {
    let current = cache.mercury_total(BalanceField::Current).unwrap_or(0.0);
    let avail = cache.mercury_total(BalanceField::Available).unwrap_or(0.0);
    let ccy = cache
        .mercury
        .first()
        .map(|a| a.currency.clone())
        .unwrap_or_else(|| "USD".into());
    let line = Line::from(vec![
        Span::styled(
            format!("{:<24}", "TOTAL"),
            Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::raw(format!("{:<10}", "")),
        Span::raw("  "),
        Span::styled(
            format!("{:>14}", format_decimal(current)),
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            format!("{:>14}", format_decimal(avail)),
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::raw(format!("{:<4}", ccy)),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn account_row<'a>(a: &MercuryAccount) -> ListItem<'a> {
    let name = format!("{:<24}", truncate(&a.name, 24));
    let kind = format!("{:<10}", truncate(&a.kind, 10));
    let current = format!("{:>14}", format_decimal(a.current_balance));
    let avail = format!("{:>14}", format_decimal(a.available_balance));
    let ccy = format!("{:<4}", a.currency);
    let amount_color = if a.available_balance < 0.0 {
        Color::Red
    } else {
        Color::Green
    };
    ListItem::new(Line::from(vec![
        Span::raw(name),
        Span::raw("  "),
        Span::raw(kind),
        Span::raw("  "),
        Span::styled(current, Style::default().fg(amount_color)),
        Span::raw("  "),
        Span::styled(avail, Style::default().fg(amount_color)),
        Span::raw("  "),
        Span::raw(ccy),
    ]))
}

fn kv(label: &str, value: &str, value_color: Color) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("  {label}  "),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            value.to_string(),
            Style::default().fg(value_color).add_modifier(Modifier::BOLD),
        ),
    ])
}

fn header_cell(s: &str) -> Span<'static> {
    Span::styled(
        s.to_string(),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    )
}

/// Format an integer minor-unit amount (e.g. cents) as a human dollars
/// string. Currency code shown alongside.
fn format_amount(minor: i64, currency_uppercase: &str) -> String {
    let major = minor as f64 / 100.0;
    format!("{} {currency_uppercase}", format_decimal(major))
}

fn format_decimal(v: f64) -> String {
    let neg = v < 0.0;
    let abs = v.abs();
    let whole = abs.trunc() as u64;
    let cents = (abs.fract() * 100.0).round() as u64;
    let mut whole_str = whole.to_string();
    // Insert thousands separators.
    if whole_str.len() > 3 {
        let bytes = whole_str.as_bytes().to_vec();
        let mut out = String::with_capacity(bytes.len() + bytes.len() / 3);
        let first = bytes.len() % 3;
        if first > 0 {
            out.push_str(std::str::from_utf8(&bytes[..first]).unwrap());
        }
        for (i, ch) in bytes[first..].chunks(3).enumerate() {
            if !(i == 0 && first == 0) {
                out.push(',');
            }
            out.push_str(std::str::from_utf8(ch).unwrap());
        }
        whole_str = out;
    }
    let sign = if neg { "-" } else { "" };
    format!("{sign}{whole_str}.{cents:02}")
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
    fn formats_small_decimals() {
        assert_eq!(format_decimal(0.0), "0.00");
        assert_eq!(format_decimal(1.5), "1.50");
        assert_eq!(format_decimal(123.456), "123.46");
        assert_eq!(format_decimal(-7.1), "-7.10");
    }

    #[test]
    fn formats_thousands_separators() {
        assert_eq!(format_decimal(1_000.0), "1,000.00");
        assert_eq!(format_decimal(1_234_567.89), "1,234,567.89");
        assert_eq!(format_decimal(12_345.67), "12,345.67");
    }

    #[test]
    fn formats_amount_cents_to_dollars() {
        assert_eq!(format_amount(0, "USD"), "0.00 USD");
        assert_eq!(format_amount(12345, "USD"), "123.45 USD");
        assert_eq!(format_amount(1_234_567, "EUR"), "12,345.67 EUR");
    }
}
