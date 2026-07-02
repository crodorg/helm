//! Shared flag/argument parsing for the `helm shell` and `helm pane` surfaces.
//!
//! The two surfaces deliberately expose the same `-n LINES`, `--timeout SECS`,
//! and single-line-command semantics. The validators live here — as pure
//! functions returning a bare reason string — so a fix (notably the `-n 0`
//! rejection, which had to be found twice while the logic was duplicated) can't
//! drift between them. Each caller prepends its own command prefix to the
//! returned reason, preserving the surface-specific error text.

/// Why a run command is not a valid single line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandError {
    /// No command words (after trimming).
    Empty,
    /// Contains a newline.
    MultiLine,
}

/// Parse a `-n LINES` value: a strictly positive integer. `-S -0` captures the
/// whole visible pane, not "0 lines", so 0 is rejected to match the promise the
/// flag makes. `Err` is the bare reason; callers prepend their command prefix.
pub fn parse_lines(val: &str) -> Result<u32, &'static str> {
    match val.parse::<u32>() {
        Ok(n) if n > 0 => Ok(n),
        _ => Err("-n requires a positive integer"),
    }
}

/// Parse a `--timeout SECS` value: a strictly positive integer.
pub fn parse_timeout(val: &str) -> Result<u32, &'static str> {
    match val.parse::<u32>() {
        Ok(s) if s > 0 => Ok(s),
        _ => Err("--timeout requires a positive integer"),
    }
}

/// Join command words into a single line, rejecting empty and multi-line input.
/// A newline would detach the sentinel `printf` from the command (the shell
/// submits the first line on its own), so `$?` would report the wrong exit and
/// the poll could hang — reject it up front.
pub fn single_line_command(parts: &[String]) -> Result<String, CommandError> {
    let cmd = parts.join(" ");
    if cmd.trim().is_empty() {
        return Err(CommandError::Empty);
    }
    if cmd.contains('\n') {
        return Err(CommandError::MultiLine);
    }
    Ok(cmd)
}

/// The process exit byte a `run` returns: the remote command's own exit for a
/// completed run (clamped to the 0..=255 a process code occupies), 1 for a busy
/// or gone session/pane, 124 (the GNU `timeout` convention) for a timeout. Both
/// `helm shell run` and `helm pane run` map through this so their exit contract
/// stays identical.
pub fn run_exit_byte(busy: bool, gone: bool, exit: Option<i32>) -> u8 {
    if busy || gone {
        1
    } else {
        match exit {
            Some(code) => code.clamp(0, 255) as u8,
            None => 124,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_lines_rejects_zero_and_nonpositive() {
        assert_eq!(parse_lines("200"), Ok(200));
        assert_eq!(parse_lines("1"), Ok(1));
        assert!(parse_lines("0").is_err()); // `-S -0` is the whole pane, not 0 lines
        assert!(parse_lines("-5").is_err());
        assert!(parse_lines("abc").is_err());
    }

    #[test]
    fn parse_timeout_rejects_zero_and_nonpositive() {
        assert_eq!(parse_timeout("30"), Ok(30));
        assert!(parse_timeout("0").is_err());
        assert!(parse_timeout("x").is_err());
    }

    #[test]
    fn single_line_command_joins_and_validates() {
        assert_eq!(
            single_line_command(&["echo".into(), "hi".into()]).unwrap(),
            "echo hi"
        );
        assert_eq!(single_line_command(&[]), Err(CommandError::Empty));
        assert_eq!(
            single_line_command(&["   ".into()]),
            Err(CommandError::Empty)
        );
        assert_eq!(
            single_line_command(&["a\nb".into()]),
            Err(CommandError::MultiLine)
        );
    }

    #[test]
    fn run_exit_byte_maps_each_state() {
        assert_eq!(run_exit_byte(false, false, Some(0)), 0);
        assert_eq!(run_exit_byte(false, false, Some(2)), 2);
        assert_eq!(run_exit_byte(false, false, Some(300)), 255); // clamped
        assert_eq!(run_exit_byte(false, false, None), 124); // timeout
        assert_eq!(run_exit_byte(true, false, None), 1); // busy
        assert_eq!(run_exit_byte(false, true, None), 1); // gone
    }
}
