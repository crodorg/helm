mod agent;
mod browse;
mod buyvm;
mod dns;
mod health;
mod history;
mod log_picker;
mod log_tail;
mod money;
mod processes;
mod runner;
mod services;
mod shortcuts;
mod vultr;

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::app::{App, InputFocus, Mode};

pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);

    draw_header(f, chunks[0], app);
    match app.mode {
        Mode::Browse => browse::draw(f, chunks[1], app),
        Mode::Runner => runner::draw(f, chunks[1], app),
        Mode::Services => services::draw(f, chunks[1], app),
        Mode::Shortcuts => {
            // Render Browse beneath, then overlay the shortcut palette modal.
            browse::draw(f, chunks[1], app);
            shortcuts::draw(f, chunks[1], app);
        }
        Mode::AgentTail => agent::draw(f, chunks[1], app),
        Mode::Processes => processes::draw(f, chunks[1], app),
        Mode::Health => health::draw(f, chunks[1], app),
        Mode::Vultr => vultr::draw(f, chunks[1], app),
        Mode::Buyvm => buyvm::draw(f, chunks[1], app),
        Mode::Money => money::draw(f, chunks[1], app),
        Mode::LogPicker => {
            // Render Browse beneath, then overlay the log picker modal.
            browse::draw(f, chunks[1], app);
            log_picker::draw(f, chunks[1], app);
        }
        Mode::LogTail => log_tail::draw(f, chunks[1], app),
        Mode::History => history::draw(f, chunks[1], app),
        Mode::Dns => dns::draw(f, chunks[1], app),
    }
    draw_footer(f, chunks[2], app);
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let mode_label = match app.mode {
        Mode::Browse => " browse ",
        Mode::Runner => " runner ",
        Mode::Services => " services ",
        Mode::Shortcuts => " shortcuts ",
        Mode::AgentTail => " agent ",
        Mode::Processes => " processes ",
        Mode::Health => " health ",
        Mode::Vultr => " vultr ",
        Mode::Buyvm => " buyvm ",
        Mode::Money => " money ",
        Mode::LogPicker => " logs ",
        Mode::LogTail => " logs ",
        Mode::History => " history ",
        Mode::Dns => " dns ",
    };
    let agent_active = app.agent_active.is_some();
    let agent_chip_bg = if agent_active {
        Color::Yellow
    } else {
        Color::DarkGray
    };
    let p = Paragraph::new(Line::from(vec![
        Span::styled(
            " helm ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            mode_label,
            Style::default().fg(Color::Black).bg(Color::Magenta),
        ),
        Span::raw("  "),
        Span::styled(
            format!(" {} ", app.agent_indicator()),
            Style::default()
                .fg(Color::Black)
                .bg(agent_chip_bg)
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    f.render_widget(p, area);
}

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    let hints = match (app.mode, app.runner.focus) {
        (Mode::Browse, _) => " [j/k] move   [enter] ssh   [r] run cmd   [s] services   [p] processes   [h] health   [v] vultr   [b] buyvm   [m] money   [l] logs   [t] history   [d] dns   [a] shortcuts   [c] agent tail   [q] quit ",
        (Mode::Runner, Some(InputFocus::Password)) => " typing password — [enter] submit   [esc] cancel ",
        (Mode::Runner, Some(InputFocus::Command)) => " typing command — [enter] run   [esc] back ",
        (Mode::Runner, None) => " [j/k] scroll   [pgup/pgdn] page   [g/G] top/bottom   [r] new cmd   [esc] back ",
        (Mode::Services, _) => " [j/k] scroll   [pgup/pgdn] page   [g/G] top/bottom   [r] refresh   [esc] back ",
        (Mode::Shortcuts, _) => " press a shortcut key   [esc] cancel ",
        (Mode::AgentTail, _) => " [j/k] scroll   [pgup/pgdn] page   [g/G] top/bottom   [esc] back ",
        (Mode::Processes, _) => " [j/k] scroll   [pgup/pgdn] page   [r] refresh   [esc] back ",
        (Mode::Health, _) => " [j/k] scroll   [pgup/pgdn] page   [r] refresh   [esc] back ",
        (Mode::Vultr, _) => " [j/k] scroll   [pgup/pgdn] page   [r] refresh   [esc] back ",
        (Mode::Buyvm, _) => " [j/k] scroll   [pgup/pgdn] page   [r] refresh   [esc] back ",
        (Mode::Money, _) => " [r] refresh   [esc] back ",
        (Mode::LogPicker, _) => " press a log key   [esc] cancel ",
        (Mode::LogTail, _) => " [j/k] scroll   [pgup/pgdn] page   [g/G] top/bottom   [esc] kill tail + back ",
        (Mode::History, _) => " [j/k] move   [enter] replay (loads cmd into runner)   [r] refresh   [esc] back ",
        (Mode::Dns, _) => " [j/k] scroll   [pgup/pgdn] page   [r] refresh   [esc] back ",
    };

    let line = if app.status.is_empty() {
        Line::from(Span::styled(hints, Style::default().fg(Color::DarkGray)))
    } else {
        Line::from(vec![
            Span::styled(hints, Style::default().fg(Color::DarkGray)),
            Span::raw("  "),
            Span::styled(app.status.clone(), Style::default().fg(Color::Yellow)),
        ])
    };
    f.render_widget(Paragraph::new(line), area);
}
