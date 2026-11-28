//! goodbyedpi command-line handling: tokenising, TTL rewriting, and the flag
//! reference shown in the Flags tab.

use std::net::SocketAddr;

/// Starting point. No TTL here on purpose: TTL search adds `--set-ttl N` after it
/// has *measured* which N gets past the DPI.
#[cfg(windows)]
pub const DEFAULT_FLAGS: &str = "-e 2 --reverse-frag --max-payload --wrong-chksum";
#[cfg(not(windows))]
pub const DEFAULT_FLAGS: &str = "--tlsrec 1 -s 1+s -d 1+s";

pub fn tokenize(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_q = false;
    let mut has = false;
    for c in s.chars() {
        match c {
            '"' => {
                in_q = !in_q;
                has = true;
            }
            c if c.is_whitespace() && !in_q => {
                if has {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            c => {
                cur.push(c);
                has = true;
            }
        }
    }
    if has {
        out.push(cur);
    }
    out
}

pub fn join(tokens: &[String]) -> String {
    tokens
        .iter()
        .map(|t| if t.is_empty() || t.contains(char::is_whitespace) { format!("\"{t}\"") } else { t.clone() })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn get_ttl(flags: &str) -> Option<u8> {
    let t = tokenize(flags);
    t.iter().position(|x| x == "--set-ttl" || x == "-t" || x == "--ttl").and_then(|i| t.get(i + 1)).and_then(|v| v.parse().ok())
}

/// Returns `flags` with exactly one `--set-ttl ttl` (or `-t ttl`) and no `--auto-ttl`
/// (the two conflict). Everything else is preserved in order.
pub fn with_ttl(flags: &str, ttl: u8) -> String {
    let is_byedpi = uses(flags, "-t") || uses(flags, "--ttl") || uses(flags, "--tlsrec") || (!uses(flags, "--set-ttl") && uses(flags, "-f"));
    let without = remove(&remove(&remove(&remove(flags, "--set-ttl"), "--auto-ttl"), "-t"), "--ttl");
    let mut t = tokenize(&without);
    if is_byedpi { t.push("-t".into()); } else { t.push("--set-ttl".into()); }
    t.push(ttl.to_string());
    join(&t)
}

pub fn proxy_addr(flags: &str) -> SocketAddr {
    format!("127.0.0.1:{}", parse_proxy_port(flags)).parse().unwrap()
}

pub fn parse_proxy_port(flags: &str) -> u16 {
    let tokens = tokenize(flags);
    for w in tokens.windows(2) {
        if (w[0] == "-p" || w[0] == "--port") && !w[1].starts_with('-') {
            if let Ok(p) = w[1].parse::<u16>() { return p; }
        }
    }
    1080
}

// ----------------------------------------------------------------- reference

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Value {
    None,
    Required,
    Optional,
}

pub struct FlagInfo {
    pub flag: &'static str,
    pub value: Value,
    /// Value inserted by "Add" (empty = none).
    pub example: &'static str,
    pub group: &'static str,
    pub text: &'static str,
}

const fn f(flag: &'static str, value: Value, example: &'static str, group: &'static str, text: &'static str) -> FlagInfo {
    FlagInfo { flag, value, example, group, text }
}

use Value::{None as N, Optional as O, Required as R};

/// Every goodbyedpi 0.2.3 option, most useful for this network first.
pub const ALL: &[FlagInfo] = &[
    f("--wrong-chksum", N, "", "Decoy", "Send a decoy hello with a broken TCP checksum. The DPI reads it; the server discards it."),
    f("--set-ttl", R, "3", "Decoy", "Send a decoy hello that expires after N hops: past the campus firewall, before the server. TTL search picks N."),
    f("--wrong-seq", N, "", "Decoy", "Send a decoy hello with a TCP sequence number in the past. Servers ignore it; some DPIs don't."),
    f("--frag-by-sni", N, "", "Split", "Split the ClientHello right before the site name (SNI). Closest goodbyedpi gets to the phone's --tlsrec 1+s."),
    f("-e", R, "2", "Split", "Split the first HTTPS packet (TLS ClientHello) at byte N. Weak alone: this campus DPI reassembles TCP."),
    f("--reverse-frag", N, "", "Split", "Send the split pieces in reverse order (second piece first). Confuses simple DPI."),
    f("--native-frag", N, "", "Split", "Split by sending smaller packets instead of shrinking the TCP window. Faster than the default way."),
    f("--max-payload", O, "", "Speed", "Ignore packets bigger than N bytes (default 1200). Big downloads skip goodbyedpi entirely: no slowdown."),
    f("-q", N, "", "Traffic", "Block QUIC/HTTP3 so apps fall back to TCP (the campus drops UDP 443 anyway)."),
    f("--auto-ttl", O, "", "Decoy", "Guess the decoy TTL from the server's distance. Unreliable here; TTL search replaces it with --set-ttl."),
    f("--min-ttl", R, "3", "Decoy", "Don't send TTL decoys to servers closer than N hops."),
    f("--fake-with-sni", R, "www.example.com", "Decoy", "Make decoy packets carry this site name instead of the default one."),
    f("--fake-gen", R, "5", "Decoy", "Send N random-filled decoy packets."),
    f("--fake-resend", R, "2", "Decoy", "Send each decoy N times (helps on lossy networks)."),
    f("--fake-from-hex", R, "160301", "Decoy", "Use this hex blob as the decoy packet."),
    f("-p", N, "", "Traffic", "Drop redirects/resets injected by passive DPI."),
    f("--port", R, "8443", "Traffic", "Also process this TCP port (besides 80 and 443)."),
    f("-w", N, "", "Traffic", "Look for HTTP on every processed port, not only 80."),
    f("--ip-id", R, "54321", "Traffic", "Drop injected packets carrying this IP ID."),
    f("--blacklist", R, "\"C:\\sites.txt\"", "Filter", "Only apply tricks to host names listed in this text file (one per line)."),
    f("--allow-no-sni", N, "", "Filter", "With --blacklist: also apply tricks when no site name is found."),
    f("--dns-addr", R, "1.1.1.1", "DNS", "Redirect IPv4 DNS to this server. Not needed here: campus DNS is clean."),
    f("--dns-port", R, "53", "DNS", "Port for --dns-addr."),
    f("--dnsv6-addr", R, "2606:4700:4700::1111", "DNS", "Redirect IPv6 DNS to this server."),
    f("--dnsv6-port", R, "53", "DNS", "Port for --dnsv6-addr."),
    f("--dns-verb", N, "", "DNS", "Print DNS redirection details in the log."),
    f("-r", N, "", "HTTP only", "Rewrite 'Host:' as 'hoSt:'. Plain HTTP only; game stores use HTTPS."),
    f("-s", N, "", "HTTP only", "Remove the space after 'Host:'."),
    f("-m", N, "", "HTTP only", "Mix the case of the host name."),
    f("-a", N, "", "HTTP only", "Extra space after the HTTP method. May break sites."),
    f("-f", R, "2", "HTTP only", "Split HTTP requests at byte N."),
    f("-k", R, "2", "HTTP only", "Split keep-alive HTTP requests at byte N."),
    f("-n", N, "", "HTTP only", "With -k: don't wait for the first segment's ACK."),
    f("-9", N, "", "Preset", "goodbyedpi's default mode: -f 2 -e 2 --wrong-seq --wrong-chksum --reverse-frag --max-payload -q."),
    f("-8", N, "", "Preset", "Like -9 without -q."),
    f("-7", N, "", "Preset", "-f 2 -e 2 --wrong-chksum --reverse-frag --max-payload."),
    f("-6", N, "", "Preset", "-f 2 -e 2 --wrong-seq --reverse-frag --max-payload."),
    f("-5", N, "", "Preset", "-f 2 -e 2 --auto-ttl --reverse-frag --max-payload."),
];

pub const BYEDPI_FLAGS: &[FlagInfo] = &[
    f("--tlsrec", R, "1", "TLS Record", "Split TLS record at offset N (e.g. 1 or 1+s)."),
    f("-s", R, "1+s", "Split", "Split data stream at offset N."),
    f("-d", R, "1+s", "Disorder", "Send stream split parts out of order."),
    f("-f", R, "1+s", "Fake Packet", "Inject fake packet at offset N."),
    f("-t", R, "3", "TTL", "Set TTL for fake injected packets."),
    f("-p", R, "1080", "Proxy Port", "SOCKS5 proxy port to listen on (default 1080)."),
];

pub fn info(flag: &str) -> Option<&'static FlagInfo> {
    ALL.iter().find(|i| i.flag == flag).or_else(|| BYEDPI_FLAGS.iter().find(|i| i.flag == flag))
}

fn takes_value(info: Option<&FlagInfo>, next: Option<&String>) -> bool {
    match info.map(|i| i.value) {
        Some(Value::Required) => next.is_some(),
        Some(Value::Optional) => next.map_or(false, |v| !v.starts_with('-') || v.parse::<i64>().is_ok() && v.len() > 2),
        _ => false,
    }
}

pub fn uses(flags: &str, flag: &str) -> bool {
    tokenize(flags).iter().any(|t| t == flag)
}

/// Removes every occurrence of `flag` (and its value, if it takes one).
pub fn remove(flags: &str, flag: &str) -> String {
    let t = tokenize(flags);
    let mut out = Vec::new();
    let mut i = 0;
    while i < t.len() {
        if t[i] == flag {
            i += if takes_value(info(flag), t.get(i + 1)) { 2 } else { 1 };
        } else {
            out.push(t[i].clone());
            i += 1;
        }
    }
    join(&out)
}

/// Appends `flag` with its example value (if it needs one). No-op if present.
pub fn add(flags: &str, flag: &str) -> String {
    if uses(flags, flag) {
        return flags.trim().to_string();
    }
    let mut s = flags.trim().to_string();
    if !s.is_empty() {
        s.push(' ');
    }
    s.push_str(flag);
    if let Some(i) = info(flag) {
        if i.value == Value::Required && !i.example.is_empty() {
            s.push(' ');
            s.push_str(i.example);
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ttl_rewrite() {
        assert_eq!(with_ttl("-e 2 --auto-ttl --wrong-chksum", 6), "-e 2 --wrong-chksum --set-ttl 6");
        assert_eq!(with_ttl("--auto-ttl 1-4-10 -q", 3), "-q --set-ttl 3");
        assert_eq!(with_ttl("--set-ttl 5 --wrong-seq", 7), "--wrong-seq --set-ttl 7");
        assert_eq!(get_ttl("--wrong-seq --set-ttl 7"), Some(7));
        assert_eq!(get_ttl(DEFAULT_FLAGS), None);
    }
    #[test]
    fn byedpi_ttl() {
        assert_eq!(with_ttl("--tlsrec 1 -s 1+s -d 1+s", 4), "--tlsrec 1 -s 1+s -d 1+s -t 4");
        assert_eq!(with_ttl("-s 1+s -t 2", 8), "-s 1+s -t 8");
        assert_eq!(get_ttl("--tlsrec 1 -t 5"), Some(5));
    }
    #[test]
    fn quoting() {
        let t = tokenize(r#"--blacklist "C:\my lists\a.txt" -q"#);
        assert_eq!(t, vec!["--blacklist", r"C:\my lists\a.txt", "-q"]);
        assert_eq!(join(&t), r#"--blacklist "C:\my lists\a.txt" -q"#);
    }
    #[test]
    fn add_remove() {
        assert_eq!(add("-e 2", "--frag-by-sni"), "-e 2 --frag-by-sni");
        assert_eq!(add("-e 2", "--fake-gen"), "-e 2 --fake-gen 5");
        assert_eq!(add("-e 2", "-e"), "-e 2");
        assert_eq!(remove("-e 2 --max-payload --wrong-chksum", "--max-payload"), "-e 2 --wrong-chksum");
        assert_eq!(remove("--max-payload 1400 -q", "--max-payload"), "-q");
        assert_eq!(remove("-e 2 --wrong-chksum", "-e"), "--wrong-chksum");
        assert!(uses("-e 2 --set-ttl 2", "--set-ttl"));
        assert!(!uses("-e 2", "--set-ttl"));
    }
}
