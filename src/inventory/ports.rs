//! Parse OpenBSD `netstat -na` output, extracting listening sockets.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListeningSocket {
    pub proto: String,
    pub local: String,
}

/// Parse stdout from `netstat -na`. Keeps only listening sockets:
///   - tcp / tcp6 rows whose state column is `LISTEN`
///   - udp / udp6 rows (datagram sockets — bound = listening)
///
/// Header lines, blank lines, and unix-domain rows are skipped.
pub fn parse(stdout: &str) -> Vec<ListeningSocket> {
    let mut out = Vec::new();
    for line in stdout.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        let toks: Vec<&str> = line.split_whitespace().collect();
        if toks.is_empty() {
            continue;
        }
        let proto = toks[0];
        match proto {
            "tcp" | "tcp6" => {
                // Format: proto recv-q send-q local foreign state
                if toks.len() < 6 {
                    continue;
                }
                let state = toks[5];
                if state == "LISTEN" {
                    out.push(ListeningSocket {
                        proto: proto.to_string(),
                        local: toks[3].to_string(),
                    });
                }
            }
            "udp" | "udp6" => {
                // Format: proto recv-q send-q local foreign
                if toks.len() < 5 {
                    continue;
                }
                out.push(ListeningSocket {
                    proto: proto.to_string(),
                    local: toks[3].to_string(),
                });
            }
            _ => {}
        }
    }
    out.sort_by(|a, b| a.proto.cmp(&b.proto).then(a.local.cmp(&b.local)));
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_listening_tcp_and_udp() {
        let raw = "\
Active Internet connections (including servers)
Proto Recv-Q Send-Q  Local Address          Foreign Address        (state)
tcp          0      0  *.22                   *.*                    LISTEN
tcp          0      0  127.0.0.1.25           *.*                    LISTEN
tcp          0      0  10.0.0.1.443           198.51.100.5.40404     ESTABLISHED
tcp6         0      0  *.22                   *.*                    LISTEN
udp          0      0  *.53                   *.*
udp          0      0  127.0.0.1.514          *.*
";
        let sockets = parse(raw);
        let descriptors: Vec<String> =
            sockets.iter().map(|s| format!("{}/{}", s.proto, s.local)).collect();
        // ESTABLISHED tcp is filtered; five listening sockets remain.
        assert_eq!(descriptors.len(), 5);
        assert!(descriptors.contains(&"tcp/*.22".to_string()));
        assert!(descriptors.contains(&"tcp/127.0.0.1.25".to_string()));
        assert!(descriptors.contains(&"tcp6/*.22".to_string()));
        assert!(descriptors.contains(&"udp/*.53".to_string()));
    }

    #[test]
    fn skips_unix_sockets_and_blanks() {
        let raw = "\
Active UNIX domain sockets
Address  Type   Recv-Q Send-Q ...
ffff     stream      0      0 /var/run/something
";
        assert!(parse(raw).is_empty());
    }

    #[test]
    fn empty_input() {
        assert!(parse("").is_empty());
    }
}
