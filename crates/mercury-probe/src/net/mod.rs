//! The small network tools the probe's images run, so they need no other program: `healthcheck`
//! (the container's health check), `reach` (the deployment's network checks) and `egress` (the
//! forward proxy between a network with no route out and the hosts its services may reach).

pub mod egress;
pub mod healthcheck;
pub mod reach;

use std::fmt;
use std::io::{self, Read};
use std::net::{IpAddr, Ipv6Addr, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// The most of a reply's first line that is read.
const MAX_STATUS_LINE: usize = 1024;

/// The longest host name DNS allows.
const MAX_HOST: usize = 253;

/// A host and a port, as a request line or a URL names them: `example.com:443`, `127.0.0.1:80`,
/// `[::1]:8080`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostPort {
    /// A name in lower case (letters, digits, dots, hyphens and underscores), or an IP address; an
    /// IPv6 address without its brackets.
    pub host: String,
    pub port: u16,
}

impl HostPort {
    /// `host:port`, `[v6]:port`, or the host alone when `default_port` stands in for its port.
    pub fn parse(value: &str, default_port: Option<u16>) -> Result<Self, String> {
        let (host, port) = match value.strip_prefix('[') {
            Some(rest) => {
                let (host, after) = rest
                    .split_once(']')
                    .ok_or_else(|| format!("{value:?} opens a bracket it doesn't close"))?;
                if host.parse::<Ipv6Addr>().is_err() {
                    return Err(format!("{host:?} isn't an IPv6 address"));
                }
                let port = match after {
                    "" => None,
                    _ => Some(
                        after
                            .strip_prefix(':')
                            .ok_or_else(|| format!("{value:?} has more than a port after the address"))?,
                    ),
                };
                (host, port)
            }
            None => {
                let (host, port) = match value.rsplit_once(':') {
                    Some((host, port)) => (host, Some(port)),
                    None => (value, None),
                };
                if !is_name(host) {
                    return Err(format!("{host:?} isn't a host name or an IP address"));
                }
                (host, port)
            }
        };
        let port = match port {
            Some(port) => parse_port(port).map_err(|()| format!("{port:?} isn't a port"))?,
            None => default_port.ok_or_else(|| format!("{value:?} names no port"))?,
        };
        Ok(Self {
            host: host.to_ascii_lowercase(),
            port,
        })
    }
}

impl fmt::Display for HostPort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.host.contains(':') {
            write!(f, "[{}]:{}", self.host, self.port)
        } else {
            write!(f, "{}:{}", self.host, self.port)
        }
    }
}

/// Letters, digits, dots, hyphens and underscores: a DNS name, or an IPv4 address.
fn is_name(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= MAX_HOST
        && host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

/// A port from 1 to 65535, in digits alone.
pub(crate) fn parse_port(value: &str) -> Result<u16, ()> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(());
    }
    value.parse::<u16>().ok().filter(|&port| port != 0).ok_or(())
}

/// The status code of an HTTP status line: `HTTP/1.1 200 OK` is 200; `None` when the line isn't one.
pub fn status_code(line: &str) -> Option<u16> {
    let mut parts = line.split(' ');
    let version = parts.next()?;
    let code = parts.next()?;
    if !version.starts_with("HTTP/") || code.len() != 3 || !code.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    code.parse().ok()
}

/// An I/O error in a few words; a read timeout says so on every system.
pub(crate) fn describe(err: &io::Error) -> String {
    match err.kind() {
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => "timed out".to_owned(),
        _ => err.to_string(),
    }
}

/// A TCP connection to `target`: the name resolved with the system's resolver within `timeout`,
/// then each of its addresses tried until one answers, all of them within `timeout`. The stream
/// reads and writes with `timeout` too.
pub(crate) fn connect(target: &HostPort, timeout: Duration) -> Result<TcpStream, String> {
    let addrs = resolve(target, timeout)?;
    let deadline = Instant::now() + timeout;
    let mut last = None;
    for addr in addrs {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        match TcpStream::connect_timeout(&addr, left) {
            Ok(stream) => {
                stream
                    .set_read_timeout(Some(timeout))
                    .and_then(|()| stream.set_write_timeout(Some(timeout)))
                    .map_err(|err| format!("cannot use the connection to {target}: {err}"))?;
                return Ok(stream);
            }
            Err(err) => last = Some(err),
        }
    }
    Err(format!(
        "cannot reach {target}: {}",
        last.map_or_else(|| "timed out".to_owned(), |err| describe(&err))
    ))
}

/// The addresses of `target`. An IP address needs no resolver; a name is looked up on a thread of
/// its own, because the system's resolver can't be told to stop: one that outlives `timeout` is
/// left behind, and ends with the process.
fn resolve(target: &HostPort, timeout: Duration) -> Result<Vec<SocketAddr>, String> {
    if let Ok(ip) = target.host.parse::<IpAddr>() {
        return Ok(vec![SocketAddr::new(ip, target.port)]);
    }
    let (sender, receiver) = mpsc::channel();
    let name = (target.host.clone(), target.port);
    std::thread::spawn(move || {
        let _ = sender.send(name.to_socket_addrs().map(|addrs| addrs.collect::<Vec<_>>()));
    });
    match receiver.recv_timeout(timeout) {
        Ok(Ok(addrs)) if !addrs.is_empty() => Ok(addrs),
        Ok(Ok(_)) => Err(format!("{} has no address", target.host)),
        Ok(Err(err)) => Err(format!("cannot resolve {}: {err}", target.host)),
        Err(_) => Err(format!(
            "cannot resolve {}: no answer within {} s",
            target.host,
            timeout.as_secs_f32()
        )),
    }
}

/// The first line of the reply on `stream`, read within `timeout` in all; at most 1 KiB of it, with
/// control characters made spaces. Empty when the other end closed without a byte.
pub(crate) fn read_status_line(stream: &mut TcpStream, timeout: Duration) -> io::Result<String> {
    let deadline = Instant::now() + timeout;
    let mut line = Vec::new();
    let mut chunk = [0u8; 256];
    while !line.contains(&b'\n') && line.len() < MAX_STATUS_LINE {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        stream.set_read_timeout(Some(left))?;
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        line.extend_from_slice(&chunk[..read]);
    }
    let end = line
        .iter()
        .position(|&byte| byte == b'\n')
        .unwrap_or(line.len())
        .min(MAX_STATUS_LINE);
    let text: String = String::from_utf8_lossy(&line[..end])
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    Ok(text.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(value: &str, default_port: Option<u16>) -> (String, u16) {
        let parsed = HostPort::parse(value, default_port).unwrap();
        (parsed.host, parsed.port)
    }

    #[test]
    fn a_host_and_a_port_are_read_in_every_form() {
        assert_eq!(
            parsed("api.github.com:443", None),
            ("api.github.com".to_owned(), 443)
        );
        assert_eq!(
            parsed("API.GitHub.com:443", None),
            ("api.github.com".to_owned(), 443)
        );
        assert_eq!(parsed("127.0.0.1:8888", None), ("127.0.0.1".to_owned(), 8888));
        assert_eq!(parsed("[::1]:7000", None), ("::1".to_owned(), 7000));
        assert_eq!(
            parsed("reader-egress", Some(80)),
            ("reader-egress".to_owned(), 80)
        );
        assert_eq!(parsed("[::1]", Some(443)), ("::1".to_owned(), 443));
        assert_eq!(
            HostPort::parse("[::1]:7000", None).unwrap().to_string(),
            "[::1]:7000"
        );
        assert_eq!(
            HostPort::parse("example.com:443", None).unwrap().to_string(),
            "example.com:443"
        );
    }

    #[test]
    fn a_malformed_host_or_port_is_refused() {
        for value in [
            "example.com",
            "example.com:",
            "example.com:0",
            "example.com:65536",
            "example.com:+443",
            "example.com:https",
            ":443",
            "::1:443",
            "[::1",
            "[::1]x",
            "[example.com]:443",
            "user@example.com:443",
            "exa mple.com:443",
            "example.com/x:443",
            "example.com\r\n:443",
        ] {
            assert!(HostPort::parse(value, None).is_err(), "{value}");
        }
        assert!(HostPort::parse(&format!("{}:443", "a".repeat(254)), None).is_err());
    }

    #[test]
    fn a_status_code_comes_from_a_status_line_only() {
        assert_eq!(status_code("HTTP/1.1 200 OK"), Some(200));
        assert_eq!(status_code("HTTP/1.0 405 Method Not Allowed"), Some(405));
        assert_eq!(status_code("HTTP/1.1 200 Connection established"), Some(200));
        assert_eq!(status_code("HTTP/1.1 200"), Some(200));
        for line in [
            "",
            "SSH-2.0-OpenSSH",
            "HTTP/1.1",
            "HTTP/1.1 20 OK",
            "HTTP/1.1 2000",
            "http 200",
        ] {
            assert_eq!(status_code(line), None, "{line}");
        }
    }

    #[test]
    fn ports_are_digits_from_1_to_65535() {
        assert_eq!(parse_port("443"), Ok(443));
        assert_eq!(parse_port("65535"), Ok(65535));
        for value in ["", "0", "65536", "-1", "+1", " 1", "1a"] {
            assert_eq!(parse_port(value), Err(()), "{value}");
        }
    }
}
