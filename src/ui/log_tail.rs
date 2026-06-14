use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState},
};

use crate::app::{App, LogLine};

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let Some(state) = app.log_tail.as_ref() else {
        let block = Block::default().borders(Borders::ALL).title("logs");
        f.render_widget(block, area);
        return;
    };

    let title = format!(
        " logs › {} › {} ({}){} ",
        state.alias,
        state.label,
        state.path,
        match state.exit {
            Some(c) => format!(" — exit {c}"),
            None => String::new(),
        }
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(if state.error.is_some() {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::DarkGray)
        });
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Window the buffer through the shared ScrollState. Renderer-side
    // call to `render_start` clamps the offset against the current total
    // and may re-enable sticky-bottom if the user scrolled back down to
    // the end. Sticky-bottom default means new lines auto-follow until
    // the operator presses k / PgUp / g.
    let visible: usize = inner.height as usize;
    let total = state.lines.len();
    let start = state.scroll.render_start(total, visible);
    let end = (start + visible).min(total);
    let items: Vec<ListItem> = state.lines[start..end].iter().map(render_line).collect();
    let list = List::new(items);
    let mut list_state = ListState::default();
    if !state.lines[start..end].is_empty() {
        list_state.select(Some(end - start - 1));
    }
    f.render_stateful_widget(list, inner, &mut list_state);
}

fn render_line(line: &LogLine) -> ListItem<'_> {
    match line {
        LogLine::Out(s) => ListItem::new(Line::from(Span::raw(s.clone()))),
        LogLine::Err(s) => ListItem::new(Line::from(Span::styled(
            s.clone(),
            Style::default().fg(Color::Red),
        ))),
        LogLine::System(s) => ListItem::new(Line::from(Span::styled(
            s.clone(),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        ))),
    }
}
