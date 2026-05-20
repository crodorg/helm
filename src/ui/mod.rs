mod agent;
mod browse;
mod dns;
mod health;
mod help;
mod history;
mod log_picker;
mod log_tail;
mod money;
mod processes;
mod runner;
mod services;
mod shell_sessions;
mod shortcuts;
#[cfg(test)]
mod snapshots;
mod vultr;

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::app::{App, Mode};

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
        Mode::Money => money::draw(f, chunks[1], app),
        Mode::LogPicker => {
            // Render Browse beneath, then overlay the log picker modal.
            browse::draw(f, chunks[1], app);
            log_picker::draw(f, chunks[1], app);
        }
        Mode::LogTail => log_tail::draw(f, chunks[1], app),
        Mode::History => history::draw(f, chunks[1], app),
        Mode::Dns => dns::draw(f, chunks[1], app),
        Mode::ShellSessions => shell_sessions::draw(f, chunks[1], app),
        Mode::Help => {
            // Render the underlying mode beneath, then overlay the help modal.
            // Re-dispatch by what the user opened it from.
            match app.help_origin {
                Some(Mode::Browse) | None => browse::draw(f, chunks[1], app),
                Some(Mode::Runner) => runner::draw(f, chunks[1], app),
                Some(Mode::Services) => services::draw(f, chunks[1], app),
                Some(Mode::Shortcuts) => shortcuts::draw(f, chunks[1], app),
                Some(Mode::AgentTail) => agent::draw(f, chunks[1], app),
                Some(Mode::Processes) => processes::draw(f, chunks[1], app),
                Some(Mode::Health) => health::draw(f, chunks[1], app),
                Some(Mode::Vultr) => vultr::draw(f, chunks[1], app),
                Some(Mode::Money) => money::draw(f, chunks[1], app),
                Some(Mode::LogPicker) => log_picker::draw(f, chunks[1], app),
                Some(Mode::LogTail) => log_tail::draw(f, chunks[1], app),
                Some(Mode::History) => history::draw(f, chunks[1], app),
                Some(Mode::Dns) => dns::draw(f, chunks[1], app),
                Some(Mode::ShellSessions) => shell_sessions::draw(f, chunks[1], app),
                Some(Mode::Help) => browse::draw(f, chunks[1], app),
            }
            help::draw(f, chunks[1], app);
        }
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
        Mode::Money => " money ",
        Mode::LogPicker => " logs ",
        Mode::LogTail => " logs ",
        Mode::History => " history ",
        Mode::Dns => " dns ",
        Mode::ShellSessions => " sessions ",
        Mode::Help => " help ",
    };
    let agent_active = app.engine.agent_active.is_some();
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
            format!(" {} ", app.engine.agent_indicator()),
            Style::default()
                .fg(Color::Black)
                .bg(agent_chip_bg)
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    f.render_widget(p, area);
}

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    // Key hints live in the Browse left-column palette + the `?`
    // overlay; the footer is now reserved for transient status only.
    let line = if app.status.is_empty() {
        Line::from(Span::styled(
            " press [?] for keys ",
            Style::default().fg(Color::DarkGray),
        ))
    } else {
        Line::from(Span::styled(
            app.status.clone(),
            Style::default().fg(Color::Yellow),
        ))
    };
    f.render_widget(Paragraph::new(line), area);
}
