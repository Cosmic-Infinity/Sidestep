//! Block detector: opens a raw TCP connection to <site>:443 and sends a TLS
//! ClientHello carrying the site's SNI. Because goodbyedpi intercepts all
//! outbound 443 traffic, this hello goes through the bypass exactly like a
//! browser's/launcher's would.
//!
//!   server answers with a TLS record     -> passed (DPI let it through)
//!   RST / close / silence after hello    -> blocked (what the campus DPI does)
//!   DNS or TCP connect fails             -> unreachable (network/IP issue, not SNI DPI)
//!
//! No TLS library: we only need the first 5 bytes of the server's reply.

use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verdict {
    Pass,
    Blocked,
    Unreachable,
}

pub struct SiteResult {
    pub host: String,
    pub verdict: Verdict,
    pub detail: String,
}

/// Accepts "epicgames.com", "https://store.steampowered.com/app/1/x", "gog.com:443"...
pub fn normalize_host(s: &str) -> Option<String> {
    let s = s.trim();
    let s = s.split("://").last().unwrap_or(s);
    let s = s.split(['/', '?', '#']).next().unwrap_or("");
    let s = s.rsplit('@').next().unwrap_or(s);
    let s = s.split(':').next().unwrap_or("");
    let s = s.trim().trim_end_matches('.').to_ascii_lowercase();
    if s.is_empty() || !s.contains('.') {
        None
    } else {
        Some(s)
    }
}

pub fn parse_sites(list: &str) -> Vec<String> {
    list.split([',', ' ', ';', '\n'])
        .filter_map(normalize_host)
        .collect()
}

/// Overall verdict: any block => Blocked; else any pass => Pass; else Unreachable.
pub fn probe_all(sites: &[String]) -> (Verdict, Vec<SiteResult>) {
    let results: Vec<SiteResult> = sites.iter().map(|h| probe_site(h)).collect();
    let v = if results.iter().any(|r| r.verdict == Verdict::Blocked) {
        Verdict::Blocked
    } else if results.iter().any(|r| r.verdict == Verdict::Pass) {
        Verdict::Pass
    } else {
        Verdict::Unreachable
    };
    (v, results)
}

pub fn probe_site(host: &str) -> SiteResult {
    let r = |verdict, detail: String| SiteResult { host: host.to_string(), verdict, detail };

    let addrs: Vec<SocketAddr> = match (host, 443).to_socket_addrs() {
        Ok(a) => a.filter(|a| a.is_ipv4()).collect(),
        Err(e) => return r(Verdict::Unreachable, format!("DNS lookup failed: {e}")),
    };
    let Some(addr) = addrs.first().copied() else {
        return r(Verdict::Unreachable, "no IPv4 address".into());
    };

    let mut s = match TcpStream::connect_timeout(&addr, Duration::from_secs(5)) {
        Ok(s) => s,
        Err(e) => return r(Verdict::Unreachable, format!("TCP connect to {addr} failed: {e}")),
    };
    let _ = s.set_nodelay(true);
    let _ = s.set_read_timeout(Some(Duration::from_secs(6)));
    let _ = s.set_write_timeout(Some(Duration::from_secs(5)));

    if let Err(e) = s.write_all(&client_hello(host)) {
        return r(Verdict::Blocked, format!("send failed after connect: {e}"));
    }

    let mut buf = [0u8; 5];
    match s.read(&mut buf) {
        Ok(0) => r(Verdict::Blocked, "connection closed right after ClientHello".into()),
        Ok(_) if buf[0] == 0x16 => r(Verdict::Pass, format!("ServerHello from {addr}")),
        Ok(_) if buf[0] == 0x15 => r(Verdict::Pass, format!("TLS alert from {addr} (server reached)")),
        Ok(_) => r(Verdict::Pass, format!("non-TLS reply from {addr} (server reached)")),
        Err(e) if matches!(e.kind(), ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted) => {
            r(Verdict::Blocked, "reset after ClientHello (DPI RST)".into())
        }
        Err(e) if matches!(e.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock) => {
            r(Verdict::Blocked, "no reply 6s after ClientHello (dropped)".into())
        }
        Err(e) => r(Verdict::Blocked, format!("read error after ClientHello: {e}")),
    }
}

// ---------------------------------------------------------------- ClientHello

fn rand32() -> [u8; 32] {
    static CTR: AtomicU64 = AtomicU64::new(0x9E37_79B9_7F4A_7C15);
    let t = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1);
    let mut x = t ^ CTR.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed) ^ (std::process::id() as u64) << 32;
    let mut out = [0u8; 32];
    for chunk in out.chunks_mut(8) {
        // xorshift64*
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        chunk.copy_from_slice(&x.wrapping_mul(0x2545_F491_4F6C_DD1D).to_le_bytes());
    }
    out
}

fn u16be(v: &mut Vec<u8>, n: usize) {
    v.extend_from_slice(&(n as u16).to_be_bytes());
}

fn ext(v: &mut Vec<u8>, ty: u16, body: &[u8]) {
    u16be(v, ty as usize);
    u16be(v, body.len());
    v.extend_from_slice(body);
}

fn client_hello(host: &str) -> Vec<u8> {
    let h = host.as_bytes();
    let mut e = Vec::new();

    // server_name
    let mut sni = Vec::new();
    u16be(&mut sni, h.len() + 3);
    sni.push(0);
    u16be(&mut sni, h.len());
    sni.extend_from_slice(h);
    ext(&mut e, 0x0000, &sni);
    // supported_groups: x25519, secp256r1
    ext(&mut e, 0x000a, &[0x00, 0x04, 0x00, 0x1d, 0x00, 0x17]);
    // ec_point_formats: uncompressed
    ext(&mut e, 0x000b, &[0x01, 0x00]);
    // signature_algorithms
    let mut sa = Vec::new();
    let algs: [u16; 8] = [0x0403, 0x0804, 0x0401, 0x0503, 0x0805, 0x0501, 0x0806, 0x0601];
    u16be(&mut sa, algs.len() * 2);
    for a in algs {
        u16be(&mut sa, a as usize);
    }
    ext(&mut e, 0x000d, &sa);
    // ALPN: http/1.1
    ext(&mut e, 0x0010, b"\x00\x09\x08http/1.1");
    // supported_versions: TLS 1.3, 1.2
    ext(&mut e, 0x002b, &[0x04, 0x03, 0x04, 0x03, 0x03]);
    // psk_key_exchange_modes: psk_dhe_ke
    ext(&mut e, 0x002d, &[0x01, 0x01]);
    // key_share: x25519 (any 32 bytes is a valid x25519 public value)
    let mut ks = Vec::new();
    u16be(&mut ks, 36);
    u16be(&mut ks, 0x001d);
    u16be(&mut ks, 32);
    ks.extend_from_slice(&rand32());
    ext(&mut e, 0x0033, &ks);

    let mut body = vec![0x03, 0x03];
    body.extend_from_slice(&rand32());
    body.push(32);
    body.extend_from_slice(&rand32());
    let suites: [u16; 7] = [0x1301, 0x1302, 0x1303, 0xc02b, 0xc02f, 0xc02c, 0xc030];
    u16be(&mut body, suites.len() * 2);
    for s in suites {
        u16be(&mut body, s as usize);
    }
    body.extend_from_slice(&[0x01, 0x00]);
    u16be(&mut body, e.len());
    body.extend_from_slice(&e);

    let mut hs = vec![0x01];
    hs.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
    hs.extend_from_slice(&body);

    let mut rec = vec![0x16, 0x03, 0x01];
    u16be(&mut rec, hs.len());
    rec.extend_from_slice(&hs);
    rec
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hosts() {
        assert_eq!(normalize_host("https://store.steampowered.com/app/1/x").unwrap(), "store.steampowered.com");
        assert_eq!(normalize_host("GOG.com:443").unwrap(), "gog.com");
        assert!(normalize_host("localhost").is_none());
    }
    #[test]
    fn hello_lengths_consistent() {
        let h = client_hello("epicgames.com");
        assert_eq!(h[0], 0x16);
        let rec_len = u16::from_be_bytes([h[3], h[4]]) as usize;
        assert_eq!(rec_len, h.len() - 5);
        let hs_len = u32::from_be_bytes([0, h[6], h[7], h[8]]) as usize;
        assert_eq!(hs_len, h.len() - 9);
    }
}
