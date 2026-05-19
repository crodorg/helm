//! Parse OpenBSD `ps -axo %cpu,%mem,rss,pid,user,command` output.

#[derive(Debug, Clone, PartialEq)]
pub struct Process {
    pub cpu: f32,
    pub mem: f32,
    pub rss_kb: u64,
    pub pid: u32,
    pub user: String,
    pub command: String,
}

/// Parse stdout from `ps -axo %cpu,%mem,rss,pid,user,command`. The first
/// line is the column header and is skipped. Lines that don't tokenize into
/// at least 6 columns are silently ignored.
pub fn parse(stdout: &str) -> Vec<Process> {
    let mut out = Vec::new();
    for line in stdout.lines().skip(1) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut toks = line.split_whitespace();
        let Some(cpu) = toks.next().and_then(|s| s.parse::<f32>().ok()) else {
            continue;
        };
        let Some(mem) = toks.next().and_then(|s| s.parse::<f32>().ok()) else {
            continue;
        };
        let Some(rss) = toks.next().and_then(|s| s.parse::<u64>().ok()) else {
            continue;
        };
        let Some(pid) = toks.next().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let Some(user) = toks.next() else { continue };
        let command = toks.collect::<Vec<_>>().join(" ");
        if command.is_empty() {
            continue;
        }
        out.push(Process {
            cpu,
            mem,
            rss_kb: rss,
            pid,
            user: user.to_string(),
            command,
        });
    }
    out
}

/// Return the top `n` processes by CPU usage (descending).
pub fn top_by_cpu(procs: &[Process], n: usize) -> Vec<Process> {
    let mut sorted = procs.to_vec();
    sorted.sort_by(|a, b| {
        b.cpu
            .partial_cmp(&a.cpu)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    sorted.into_iter().take(n).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_ps_output() {
        let raw = "\
%CPU %MEM   RSS    PID USER     COMMAND
12.3  4.5  6789   1234 root     /usr/sbin/httpd
 0.0  0.1   512    900 _pflogd  pflogd: [priv]
 5.0  2.0  2048   2222 admin    ssh user@host
";
        let procs = parse(raw);
        assert_eq!(procs.len(), 3);
        assert_eq!(procs[0].pid, 1234);
        assert_eq!(procs[0].cpu, 12.3);
        assert_eq!(procs[0].user, "root");
        assert_eq!(procs[0].command, "/usr/sbin/httpd");
        assert_eq!(procs[2].command, "ssh user@host");
    }

    #[test]
    fn top_by_cpu_sorts_desc() {
        let raw = "\
%CPU %MEM   RSS    PID USER     COMMAND
 1.0  0.0   100      1 root     a
50.0  0.0   100      2 root     b
20.0  0.0   100      3 root     c
";
        let procs = parse(raw);
        let top = top_by_cpu(&procs, 2);
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].pid, 2);
        assert_eq!(top[1].pid, 3);
    }

    #[test]
    fn empty_or_header_only_yields_empty() {
        assert!(parse("").is_empty());
        assert!(parse("%CPU %MEM   RSS    PID USER     COMMAND\n").is_empty());
    }
}
