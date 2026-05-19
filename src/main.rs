mod app;
mod config;
mod history;
mod inventory;
mod ipc;
mod money;
mod postmark;
mod ssh;
mod tmux;
mod ui;
mod vultr;

use std::io;
use std::process::Command;
use std::time::Duration;

use anyhow::Result;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, MouseButton,
        MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
};

use crate::app::{App, InputFocus, Mode, OutputLine};
use crate::config::Config;
use crate::ipc::protocol::Request as IpcRequest;
use crate::ssh::spawn_remote;

fn main() -> std::process::ExitCode {
    let argv: Vec<String> = std::env::args().collect();
    if let Some(sub) = argv.get(1) {
        match sub.as_str() {
            "exec" => return run_exec_cli(&argv[2..]),
            "shell" => return run_shell_cli(&argv[2..]),
            "--help" | "-h" | "help" => {
                print_help();
                return std::process::ExitCode::SUCCESS;
            }
            other if !other.starts_with('-') => {
                eprintln!("helm: unknown subcommand `{other}`");
                print_help();
                return std::process::ExitCode::from(2);
            }
            _ => {}
        }
    }
    match run_tui() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("helm: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn print_help() {
    eprintln!(
        "helm — TUI fleet manager + remote command bridge

usage:
  helm                          launch the TUI
  helm exec <alias> <cmd...>    run a one-shot command on a host through the
                                running TUI (refuses if no TUI is open)
  helm shell <subcommand>       drive a persistent tmux-backed shell session
                                per VPS (see `helm shell help`)
  helm help                     this help"
    );
}

fn print_shell_help() {
    eprintln!(
        "helm shell — persistent VPS-side tmux shell sessions

Sessions live on the VPS, not your machine. Each `<target>` is `<alias>`
(default session `helm`) or `<alias>:<label>` (session `helm-<label>`).
Persistence survives helm restarts, network drops, and operator-machine
reboots — only a VPS reboot or remote `tmux kill-server` tears them down.

usage:
  helm shell open <target>            attach this terminal to the remote
                                      session (creates it if missing)
  helm shell open -d <target>         create the remote session detached;
                                      do not attach
  helm shell send <target> <text...>  send a line of text (auto-Enter) to
                                      the remote session's active pane;
                                      creates the session if missing
  helm shell read <target> [-n LINES] capture the active pane's scrollback
                                      (default 1000); creates if missing
  helm shell list <alias>             list helm-* sessions on the alias's
                                      remote tmux server
  helm shell close <target>           kill the remote session"
    );
}

fn run_shell_cli(args: &[String]) -> std::process::ExitCode {
    let Some(sub) = args.first() else {
        print_shell_help();
        return std::process::ExitCode::from(2);
    };
    match sub.as_str() {
        "help" | "--help" | "-h" => {
            print_shell_help();
            std::process::ExitCode::SUCCESS
        }
        "open" => shell_open(&args[1..]),
        "send" => shell_send(&args[1..]),
        "read" => shell_read(&args[1..]),
        "list" => shell_list(&args[1..]),
        "close" => shell_close(&args[1..]),
        other => {
            eprintln!("helm shell: unknown subcommand `{other}`");
            print_shell_help();
            std::process::ExitCode::from(2)
        }
    }
}

fn shell_open(args: &[String]) -> std::process::ExitCode {
    // Parse optional -d flag.
    let (detached, target) = match args {
        [flag, t] if flag == "-d" => (true, t.as_str()),
        [t] => (false, t.as_str()),
        _ => {
            eprintln!("usage: helm shell open [-d] <target>");
            return std::process::ExitCode::from(2);
        }
    };
    let (alias, session) = tmux::parse_target(target);
    if detached {
        if let Err(e) = tmux::ensure_session(target) {
            eprintln!("helm: {e}");
            return std::process::ExitCode::FAILURE;
        }
        eprintln!(
            "helm: remote session ready on {alias} — attach with `helm shell open {target}`"
        );
        return std::process::ExitCode::SUCCESS;
    }
    // Replace current process with `ssh -t <alias> 'tmux new-session -A -s
    // <session>'`. The `-A` flag makes new-session idempotent: attach if
    // exists, create otherwise. Never returns on success.
    use std::os::unix::process::CommandExt;
    let remote = format!("tmux new-session -A -s {session}");
    let err = std::process::Command::new("ssh")
        .arg("-t")
        .arg(&alias)
        .arg(&remote)
        .exec();
    eprintln!("helm: exec ssh attach failed: {err}");
    std::process::ExitCode::FAILURE
}

fn shell_send(args: &[String]) -> std::process::ExitCode {
    if args.len() < 2 {
        eprintln!("usage: helm shell send <target> <text...>");
        return std::process::ExitCode::from(2);
    }
    let target = &args[0];
    let text = args[1..].join(" ");
    if let Err(e) = tmux::ensure_session(target) {
        eprintln!("helm: {e}");
        return std::process::ExitCode::FAILURE;
    }
    if let Err(e) = tmux::send_keys(target, &text) {
        eprintln!("helm: {e}");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}

fn shell_read(args: &[String]) -> std::process::ExitCode {
    // helm shell read <target> [-n LINES]
    let (target, lines) = match args {
        [t] => (t.as_str(), tmux::DEFAULT_CAPTURE_LINES),
        [t, flag, n] if flag == "-n" => match n.parse::<u32>() {
            Ok(parsed) => (t.as_str(), parsed),
            Err(_) => {
                eprintln!("helm shell read: -n requires a positive integer");
                return std::process::ExitCode::from(2);
            }
        },
        _ => {
            eprintln!("usage: helm shell read <target> [-n LINES]");
            return std::process::ExitCode::from(2);
        }
    };
    if let Err(e) = tmux::ensure_session(target) {
        eprintln!("helm: {e}");
        return std::process::ExitCode::FAILURE;
    }
    match tmux::capture(target, lines) {
        Ok(s) => {
            print!("{s}");
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("helm: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn shell_list(args: &[String]) -> std::process::ExitCode {
    let Some(alias) = args.first() else {
        eprintln!("usage: helm shell list <alias>");
        return std::process::ExitCode::from(2);
    };
    match tmux::list(alias) {
        Ok(targets) => {
            if targets.is_empty() {
                eprintln!("(no helm-* tmux sessions on {alias})");
            } else {
                for t in targets {
                    println!("{t}");
                }
            }
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("helm: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn shell_close(args: &[String]) -> std::process::ExitCode {
    let Some(target) = args.first() else {
        eprintln!("usage: helm shell close <target>");
        return std::process::ExitCode::from(2);
    };
    match tmux::kill(target) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("helm: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run_exec_cli(args: &[String]) -> std::process::ExitCode {
    if args.len() < 2 {
        eprintln!("usage: helm exec <alias> <cmd...>");
        return std::process::ExitCode::from(2);
    }
    let alias = args[0].clone();
    let cmd = args[1..].join(" ");
    ipc::client::run(&IpcRequest::Exec { alias, cmd })
}

fn run_tui() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(io::stderr)
        .init();

    let mut cfg = Config::load()?;
    let mut ssh_hosts = Vec::new();
    if cfg.ssh_config.enabled {
        let path = cfg
            .ssh_config
            .path
            .clone()
            .or_else(crate::ssh::sshconfig::default_config_path);
        if let Some(p) = path {
            if p.exists() {
                match crate::ssh::sshconfig::load_from(&p) {
                    Ok(hs) => {
                        ssh_hosts = hs.clone();
                        cfg.merge_ssh_hosts(hs);
                    }
                    Err(e) => tracing::warn!("ssh config {}: {}", p.display(), e),
                }
            }
        }
    }
    let agent_status = crate::ssh::agent::check(&ssh_hosts);
    if let Some(msg) = crate::ssh::agent::render_blocker(&agent_status, &ssh_hosts) {
        eprintln!("{msg}");
        std::process::exit(1);
    }
    let mut app = App::new(cfg);
    match history::HistoryStore::open_default() {
        Ok(store) => app.attach_history(store, 100),
        Err(e) => eprintln!("helm: warning — could not open history db: {e}"),
    }
    app.start_vultr_fetch();
    // Eager money fetch when any business declares Stripe/Mercury linkage,
    // so the Browse detail panel renders balances without forcing the
    // operator to press `m` first.
    let any_money_linkage = app
        .config
        .businesses
        .iter()
        .any(|b| b.stripe_account_id.is_some() || b.mercury_account_id.is_some());
    if any_money_linkage {
        app.start_money_fetch();
    }
    // Same pattern for Postmark — fire on startup if any business sets a
    // token, so the Browse detail panel populates without operator action.
    let any_postmark = app
        .config
        .businesses
        .iter()
        .any(|b| b.postmark_server_token.is_some());
    if any_postmark {
        app.start_postmark_fetch();
    }

    // IPC server — bind socket, hand jobs to the App via mpsc. `_guard`
    // lives until the end of run_tui so its Drop removes the socket file
    // when helm exits cleanly.
    let socket = crate::ipc::socket_path();
    let _guard = match crate::ipc::server::start(socket.clone()) {
        Ok((guard, jobs_rx)) => {
            eprintln!("helm: control socket at {}", guard.socket_path.display());
            app.jobs_rx = Some(jobs_rx);
            Some(guard)
        }
        Err(e) => {
            eprintln!("helm: warning — could not bind control socket: {e}");
            None
        }
    };

    let mut terminal = setup_terminal()?;
    let res = run(&mut terminal, &mut app);
    restore_terminal(&mut terminal)?;
    res
}

type Term = Terminal<CrosstermBackend<io::Stdout>>;

fn setup_terminal() -> Result<Term> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn restore_terminal(terminal: &mut Term) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}

fn run(terminal: &mut Term, app: &mut App) -> Result<()> {
    while !app.should_quit {
        app.ingest_run_events();
        app.ingest_services_events();
        app.ingest_processes_events();
        app.ingest_health_events();
        app.ingest_vultr_events();
        app.ingest_money_events();
        app.ingest_dns_events();
        app.ingest_postmark_events();
        app.ingest_log_tail_events();
        app.ingest_jobs();
        app.ingest_agent_events();

        terminal.draw(|f| ui::draw(f, app))?;

        if event::poll(Duration::from_millis(120))? {
            match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press => handle_key(app, k.code),
                Event::Mouse(m) => {
                    let area = terminal.size().map(|s| Rect::new(0, 0, s.width, s.height))?;
                    handle_mouse(app, m, area);
                }
                _ => {}
            }
        }

        if let Some(alias) = app.launch_ssh.take() {
            run_ssh(terminal, app, &alias)?;
        }
    }
    Ok(())
}

fn handle_key(app: &mut App, code: KeyCode) {
    match app.mode {
        Mode::Browse => handle_browse(app, code),
        Mode::Runner => handle_runner(app, code),
        Mode::Services => handle_services(app, code),
        Mode::Shortcuts => handle_shortcuts(app, code),
        Mode::AgentTail => handle_agent_tail(app, code),
        Mode::Processes => handle_processes(app, code),
        Mode::Health => handle_health(app, code),
        Mode::Vultr => handle_vultr(app, code),
        Mode::Money => handle_money(app, code),
        Mode::LogPicker => handle_log_picker(app, code),
        Mode::LogTail => handle_log_tail(app, code),
        Mode::History => handle_history(app, code),
        Mode::Dns => handle_dns(app, code),
    }
}

fn handle_log_picker(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Esc => app.close_log_picker(),
        KeyCode::Char(c) => {
            let _ = app.fire_log(c);
        }
        _ => {}
    }
}

fn handle_log_tail(app: &mut App, code: KeyCode) {
    if let Some(state) = app.log_tail.as_ref() {
        if handle_scroll_keys(&state.scroll, code) {
            return;
        }
    }
    if matches!(code, KeyCode::Esc) {
        app.close_log_tail();
    }
}

/// Dispatch j/k/PgUp/PgDn/g/G to a ScrollState. Returns true if the key
/// was consumed (so the caller doesn't double-handle Esc/etc.).
fn handle_scroll_keys(scroll: &crate::app::ScrollState, code: KeyCode) -> bool {
    match code {
        KeyCode::Char('j') | KeyCode::Down => {
            scroll.line_down();
            true
        }
        KeyCode::Char('k') | KeyCode::Up => {
            scroll.line_up();
            true
        }
        KeyCode::PageDown => {
            scroll.page_down();
            true
        }
        KeyCode::PageUp => {
            scroll.page_up();
            true
        }
        KeyCode::Char('g') => {
            scroll.to_top();
            true
        }
        KeyCode::Char('G') => {
            scroll.to_bottom();
            true
        }
        _ => false,
    }
}

fn handle_money(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Esc => app.close_money(),
        KeyCode::Char('r') => app.refresh_money(),
        _ => {}
    }
}

fn handle_dns(app: &mut App, code: KeyCode) {
    if let Some(state) = app.dns_pane.as_ref() {
        if handle_scroll_keys(&state.scroll, code) {
            return;
        }
    }
    match code {
        KeyCode::Esc => app.close_dns(),
        KeyCode::Char('r') => app.refresh_dns(),
        _ => {}
    }
}

fn handle_history(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Esc => app.close_history(),
        KeyCode::Char('r') => app.refresh_history(),
        KeyCode::Char('j') | KeyCode::Down => app.history_next(),
        KeyCode::Char('k') | KeyCode::Up => app.history_prev(),
        KeyCode::Enter => app.replay_selected_history(),
        _ => {}
    }
}

fn handle_vultr(app: &mut App, code: KeyCode) {
    if handle_scroll_keys(&app.vultr_scroll, code) {
        return;
    }
    match code {
        KeyCode::Esc => app.close_vultr(),
        KeyCode::Char('r') => app.refresh_vultr(),
        _ => {}
    }
}

fn handle_health(app: &mut App, code: KeyCode) {
    if let Some(state) = app.health_pane.as_ref() {
        if handle_scroll_keys(&state.scroll, code) {
            return;
        }
    }
    match code {
        KeyCode::Esc => app.close_health(),
        KeyCode::Char('r') => app.refresh_health(),
        _ => {}
    }
}

fn handle_processes(app: &mut App, code: KeyCode) {
    if let Some(state) = app.processes_pane.as_ref() {
        if handle_scroll_keys(&state.scroll, code) {
            return;
        }
    }
    match code {
        KeyCode::Esc => app.close_processes(),
        KeyCode::Char('r') => app.refresh_processes(),
        _ => {}
    }
}

fn handle_agent_tail(app: &mut App, code: KeyCode) {
    if handle_scroll_keys(&app.agent_tail_scroll, code) {
        return;
    }
    if matches!(code, KeyCode::Esc | KeyCode::Char('q')) {
        app.close_agent_tail();
    }
}

fn handle_browse(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Char('q') | KeyCode::Esc => app.quit(),
        KeyCode::Char('j') | KeyCode::Down => app.next_host(),
        KeyCode::Char('k') | KeyCode::Up => app.prev_host(),
        KeyCode::Enter => app.request_ssh(),
        KeyCode::Char('r') => app.open_runner(),
        KeyCode::Char('s') => app.open_services(),
        KeyCode::Char('p') => app.open_processes(),
        KeyCode::Char('h') => app.open_health(),
        KeyCode::Char('v') => app.open_vultr(),
        KeyCode::Char('m') => app.open_money(),
        KeyCode::Char('l') => app.open_log_picker(),
        KeyCode::Char('t') => app.open_history(),
        KeyCode::Char('d') => app.open_dns(),
        KeyCode::Char('a') => app.open_shortcuts(),
        KeyCode::Char('c') => app.open_agent_tail(),
        _ => {}
    }
}

fn handle_shortcuts(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Esc => app.close_shortcuts(),
        // Unknown chars leave the palette open until Esc.
        KeyCode::Char(c) => {
            let _ = app.fire_shortcut(c);
        }
        _ => {}
    }
}

fn handle_services(app: &mut App, code: KeyCode) {
    if let Some(state) = app.services.as_ref() {
        if handle_scroll_keys(&state.scroll, code) {
            return;
        }
    }
    match code {
        KeyCode::Esc => app.close_services(),
        KeyCode::Char('r') => app.refresh_services(),
        _ => {}
    }
}

fn handle_mouse(app: &mut App, m: MouseEvent, term: Rect) {
    match (app.mode, m.kind) {
        // Scroll wheel = host navigation everywhere it makes sense.
        (Mode::Browse, MouseEventKind::ScrollDown) => app.next_host(),
        (Mode::Browse, MouseEventKind::ScrollUp) => app.prev_host(),

        // Click in the browse host list → select that row.
        (Mode::Browse, MouseEventKind::Down(MouseButton::Left)) => {
            if let Some(idx) = browse_row_at(term, m.column, m.row, app.hosts().len()) {
                app.selected = idx;
            }
        }

        _ => {}
    }
}

/// Recompute the browse host-list bounds from terminal area and translate a
/// click coordinate to a host index. Mirrors the layout in `ui::draw` +
/// `ui::browse::draw`. Returns None if the click is outside the list rows.
fn browse_row_at(term: Rect, col: u16, row: u16, host_count: usize) -> Option<usize> {
    if host_count == 0 {
        return None;
    }
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(term);
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(outer[1]);
    let list_area = cols[0];

    // Subtract the bordered Block's 1-row top/bottom and 1-col left/right.
    let inner_x = list_area.x + 1;
    let inner_y = list_area.y + 1;
    let inner_w = list_area.width.saturating_sub(2);
    let inner_h = list_area.height.saturating_sub(2);

    if col < inner_x || col >= inner_x + inner_w {
        return None;
    }
    if row < inner_y || row >= inner_y + inner_h {
        return None;
    }
    let idx = (row - inner_y) as usize;
    if idx < host_count { Some(idx) } else { None }
}

fn handle_runner(app: &mut App, code: KeyCode) {
    // PgUp/PgDn always scroll, regardless of focus (they're not text input
    // characters). j/k/g/G only when not actively typing — they'd be
    // captured as command/password characters otherwise.
    match code {
        KeyCode::PageUp => {
            app.runner.scroll.page_up();
            return;
        }
        KeyCode::PageDown => {
            app.runner.scroll.page_down();
            return;
        }
        _ => {}
    }
    if app.runner.focus.is_none() && handle_scroll_keys(&app.runner.scroll, code) {
        return;
    }
    match app.runner.focus {
        Some(InputFocus::Password) => match code {
            KeyCode::Esc => {
                app.runner.password.clear();
                app.runner.focus = Some(InputFocus::Command);
            }
            KeyCode::Enter => {
                let pw = std::mem::take(&mut app.runner.password);
                if let Some(h) = app.run_handle.as_mut() {
                    if let Err(e) = h.send_line(&pw) {
                        app.runner
                            .output
                            .push(OutputLine::System(format!("write password failed: {e}")));
                    }
                }
                app.runner.focus = None;
            }
            KeyCode::Backspace => {
                app.runner.password.pop();
            }
            KeyCode::Char(c) => app.runner.password.push(c),
            _ => {}
        },
        Some(InputFocus::Command) => match code {
            KeyCode::Esc => app.close_runner(),
            KeyCode::Enter => submit_command(app),
            KeyCode::Backspace => {
                app.runner.input.pop();
            }
            KeyCode::Char(c) => app.runner.input.push(c),
            _ => {}
        },
        None => match code {
            KeyCode::Esc => app.close_runner(),
            KeyCode::Char('r') => {
                app.runner.input.clear();
                app.runner.output.clear();
                app.runner.exit_code = None;
                app.runner.focus = Some(InputFocus::Command);
            }
            _ => {}
        },
    }
}

fn submit_command(app: &mut App) {
    let cmd = app.runner.input.trim().to_string();
    if cmd.is_empty() {
        return;
    }
    let Some(host) = app.selected_host() else {
        app.runner
            .output
            .push(OutputLine::System("no host selected".into()));
        return;
    };
    let alias = host.ssh_alias.clone();

    app.runner.output.push(OutputLine::System(format!(
        "$ ssh {alias} '{cmd}'"
    )));

    match spawn_remote(&alias, &cmd) {
        Ok(handle) => {
            app.run_handle = Some(handle);
            app.runner.running = true;
            app.runner.focus = None;
            app.runner.exit_code = None;
            app.runner.input.clear();
            app.runner.current_alias = Some(alias);
            app.runner.current_cmd = Some(cmd);
            app.runner.current_started_at = Some(std::time::Instant::now());
            app.runner.current_started_at_unix = Some(unix_now());
        }
        Err(e) => {
            app.runner
                .output
                .push(OutputLine::System(format!("spawn failed: {e}")));
        }
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn run_ssh(terminal: &mut Term, app: &mut App, alias: &str) -> Result<()> {
    restore_terminal(terminal)?;

    let status = Command::new("ssh").arg(alias).status();

    enable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        EnterAlternateScreen,
        EnableMouseCapture
    )?;
    terminal.clear()?;

    match status {
        Ok(s) if s.success() => app.status = format!("ssh {alias} ok"),
        Ok(s) => app.status = format!("ssh {alias} exit {}", s.code().unwrap_or(-1)),
        Err(e) => app.status = format!("ssh {alias} failed: {e}"),
    }
    Ok(())
}
