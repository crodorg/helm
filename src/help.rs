//! Central key-binding registry.
//!
//! Every mode's footer hint *and* the in-TUI help modal (`?`) read from
//! the same table here. If you add a new keybind, add a Binding entry —
//! the footer will pick it up automatically and the help modal will list
//! it.

use crate::app::{InputFocus, Mode};

#[derive(Debug, Clone, Copy)]
pub struct Binding {
    pub key: &'static str,
    pub action: &'static str,
}

const fn b(key: &'static str, action: &'static str) -> Binding {
    Binding { key, action }
}

pub fn bindings_for(mode: Mode, focus: Option<InputFocus>) -> &'static [Binding] {
    match (mode, focus) {
        (Mode::Browse, _) => &BROWSE,
        (Mode::Runner, Some(InputFocus::Password)) => &RUNNER_PW,
        (Mode::Runner, Some(InputFocus::Command)) => &RUNNER_CMD,
        (Mode::Runner, None) => &RUNNER,
        (Mode::Services, _) => &SERVICES,
        (Mode::Shortcuts, _) => &SHORTCUTS,
        (Mode::AgentTail, _) => &AGENT_TAIL,
        (Mode::Processes, _) => &PROCESSES,
        (Mode::Health, _) => &HEALTH,
        (Mode::Vultr, _) => &VULTR,
        (Mode::Money, _) => &MONEY,
        (Mode::LogPicker, _) => &LOG_PICKER,
        (Mode::LogTail, _) => &LOG_TAIL,
        (Mode::History, _) => &HISTORY,
        (Mode::Dns, _) => &DNS,
        (Mode::ShellSessions, _) => &SHELL_SESSIONS,
        (Mode::Help, _) => &HELP,
    }
}

pub fn mode_title(mode: Mode) -> &'static str {
    match mode {
        Mode::Browse => "browse",
        Mode::Runner => "runner",
        Mode::Services => "services",
        Mode::Shortcuts => "shortcuts",
        Mode::AgentTail => "agent",
        Mode::Processes => "processes",
        Mode::Health => "health",
        Mode::Vultr => "vultr",
        Mode::Money => "money",
        Mode::LogPicker => "logs",
        Mode::LogTail => "logs",
        Mode::History => "history",
        Mode::Dns => "dns",
        Mode::ShellSessions => "sessions",
        Mode::Help => "help",
    }
}

const BROWSE: [Binding; 18] = [
    b("j/k", "move"),
    b("enter", "helm shell"),
    b("r", "run cmd"),
    b("R", "refresh-all overlays"),
    b("F5", "reload config.toml"),
    b("s", "services"),
    b("S", "shell sessions"),
    b("p", "processes"),
    b("H", "health"),
    b("v", "vultr"),
    b("m", "money"),
    b("l", "logs"),
    b("t", "history"),
    b("d", "dns"),
    b("a", "shortcuts"),
    b("c", "agent tail"),
    b("?", "help"),
    b("q", "quit"),
];

const RUNNER_PW: [Binding; 2] = [
    b("enter", "submit password"),
    b("esc", "cancel"),
];

const RUNNER_CMD: [Binding; 2] = [
    b("enter", "run"),
    b("h/esc", "back"),
];

const RUNNER: [Binding; 7] = [
    b("j/k", "scroll"),
    b("pgup/pgdn", "page"),
    b("g/G", "top/bottom"),
    b("r", "new cmd"),
    b("?", "help"),
    b("h/esc", "back"),
    b("q", "back"),
];

const SERVICES: [Binding; 7] = [
    b("j/k", "scroll"),
    b("pgup/pgdn", "page"),
    b("g/G", "top/bottom"),
    b("r", "refresh"),
    b("?", "help"),
    b("h/esc", "back"),
    b("q", "back"),
];

const SHORTCUTS: [Binding; 2] = [
    b("a-z", "fire shortcut"),
    b("esc", "cancel"),
];

const AGENT_TAIL: [Binding; 5] = [
    b("j/k", "scroll"),
    b("pgup/pgdn", "page"),
    b("g/G", "top/bottom"),
    b("?", "help"),
    b("h/esc", "back"),
];

const PROCESSES: [Binding; 5] = [
    b("j/k", "scroll"),
    b("pgup/pgdn", "page"),
    b("r", "refresh"),
    b("?", "help"),
    b("h/esc", "back"),
];

const HEALTH: [Binding; 5] = [
    b("j/k", "scroll"),
    b("pgup/pgdn", "page"),
    b("r", "refresh"),
    b("?", "help"),
    b("h/esc", "back"),
];

const VULTR: [Binding; 8] = [
    b("j/k", "select"),
    b("R", "reboot"),
    b("H", "halt"),
    b("S", "start"),
    b("N", "snapshot (BILLABLE)"),
    b("r", "refresh"),
    b("?", "help"),
    b("h/esc", "back"),
];

const MONEY: [Binding; 4] = [
    b("r", "refresh"),
    b("f", "cycle filter (per business)"),
    b("?", "help"),
    b("h/esc", "back"),
];

const LOG_PICKER: [Binding; 2] = [
    b("a-z", "tail this log"),
    b("esc", "cancel"),
];

const LOG_TAIL: [Binding; 5] = [
    b("j/k", "scroll"),
    b("pgup/pgdn", "page"),
    b("g/G", "top/bottom"),
    b("?", "help"),
    b("h/esc", "kill tail + back"),
];

const HISTORY: [Binding; 5] = [
    b("j/k", "move"),
    b("enter", "replay (load into runner)"),
    b("r", "refresh"),
    b("?", "help"),
    b("h/esc", "back"),
];

const DNS: [Binding; 5] = [
    b("j/k", "scroll"),
    b("pgup/pgdn", "page"),
    b("r", "refresh"),
    b("?", "help"),
    b("h/esc", "back"),
];

const SHELL_SESSIONS: [Binding; 7] = [
    b("j/k", "move"),
    b("enter", "open (exec into terminal)"),
    b("d", "ensure detached"),
    b("r", "refresh"),
    b("?", "help"),
    b("h/esc", "back"),
    b("q", "back"),
];

const HELP: [Binding; 3] = [
    b("?", "close"),
    b("esc", "close"),
    b("q", "close"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mode_has_bindings() {
        for m in [
            Mode::Browse,
            Mode::Runner,
            Mode::Services,
            Mode::Shortcuts,
            Mode::AgentTail,
            Mode::Processes,
            Mode::Health,
            Mode::Vultr,
            Mode::Money,
            Mode::LogPicker,
            Mode::LogTail,
            Mode::History,
            Mode::Dns,
            Mode::ShellSessions,
            Mode::Help,
        ] {
            assert!(!bindings_for(m, None).is_empty(), "no bindings for {m:?}");
        }
    }

    #[test]
    fn every_non_input_mode_documents_help_key() {
        // Excludes input-capture modes (Runner Password/Command, Shortcuts,
        // LogPicker) where `?` is a literal character, and Help itself.
        for m in [
            Mode::Browse,
            Mode::Runner,
            Mode::Services,
            Mode::AgentTail,
            Mode::Processes,
            Mode::Health,
            Mode::Vultr,
            Mode::Money,
            Mode::LogTail,
            Mode::History,
            Mode::Dns,
            Mode::ShellSessions,
        ] {
            assert!(
                bindings_for(m, None).iter().any(|b| b.key == "?"),
                "{m:?} missing [?] binding"
            );
        }
    }
}
