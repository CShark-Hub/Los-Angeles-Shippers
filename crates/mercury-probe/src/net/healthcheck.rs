//! `mercury-probe healthcheck PORT`: asks the probe in the same container for `/health` on the
//! loopback, for the container's health check (`serve` and `dev` listen on 7000, the code reader on
//! 7100). Plain HTTP/1.1 over a socket, so the image needs no other tool.

use std::io::Write;
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

use super::{describe, parse_port, read_status_line, status_code};

/// How long each step may take; the container's check allows 3 s in all.
pub const TIMEOUT: Duration = Duration::from_secs(2);

/// `healthcheck`'s arguments: the port, alone.
pub fn parse_args(args: impl IntoIterator<Item = String>) -> Result<u16, String> {
    let mut args = args.into_iter();
    let port = args
        .next()
        .ok_or("healthcheck needs the port the probe listens on")?;
    if let Some(extra) = args.next() {
        return Err(format!("healthcheck takes the port alone, not also {extra:?}"));
    }
    parse_port(&port).map_err(|()| format!("the port must be a number from 1 to 65535, not {port:?}"))
}

/// `Ok` when `http://127.0.0.1:PORT/health` answers 200; otherwise what went wrong, in a few words.
pub fn check(port: u16) -> Result<(), String> {
    let target = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port);
    let mut stream = TcpStream::connect_timeout(&target, TIMEOUT)
        .map_err(|err| format!("cannot reach {target}: {}", describe(&err)))?;
    stream
        .set_write_timeout(Some(TIMEOUT))
        .and_then(|()| {
            stream.write_all(
                format!("GET /health HTTP/1.1\r\nHost: {target}\r\nConnection: close\r\n\r\n").as_bytes(),
            )
        })
        .map_err(|err| format!("cannot ask {target}: {}", describe(&err)))?;
    let status = read_status_line(&mut stream, TIMEOUT)
        .map_err(|err| format!("no answer from {target}: {}", describe(&err)))?;
    match status_code(&status) {
        Some(200) => Ok(()),
        _ if status.is_empty() => Err(format!("{target} closed the connection without an answer")),
        _ => Err(format!("{target} answered {status:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str) -> Result<u16, String> {
        parse_args(line.split_whitespace().map(str::to_owned))
    }

    #[test]
    fn the_port_comes_alone() {
        assert_eq!(parse("7000"), Ok(7000));
        assert_eq!(parse("7100"), Ok(7100));
        assert!(parse("").unwrap_err().contains("needs the port"));
        assert!(parse("7000 7100").unwrap_err().contains("alone"));
        for line in ["0", "65536", "http://127.0.0.1:7000/health", "--port"] {
            assert!(parse(line).is_err(), "{line}");
        }
    }
}
