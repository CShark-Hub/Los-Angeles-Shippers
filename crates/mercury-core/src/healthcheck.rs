//! `mercury-core healthcheck`: asks the running core's public listener for `/v1/health`, for the
//! container health check. Plain HTTP/1.1 over a socket, so the image needs no other tool.

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::time::Duration;

/// How long each step may take; the container's check allows 3 s in all.
pub const TIMEOUT: Duration = Duration::from_secs(2);

/// `Ok` when the core at `addr` answers `/v1/health` with 200 and `"status":"ok"`. An address on
/// every interface (`0.0.0.0`, `[::]`) is asked on the loopback of the same family.
pub fn check(addr: SocketAddr) -> Result<(), String> {
    let target = match addr.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => SocketAddr::new(Ipv4Addr::LOCALHOST.into(), addr.port()),
        IpAddr::V6(ip) if ip.is_unspecified() => SocketAddr::new(Ipv6Addr::LOCALHOST.into(), addr.port()),
        _ => addr,
    };
    let mut stream = TcpStream::connect_timeout(&target, TIMEOUT)
        .map_err(|err| format!("cannot reach {target}: {err}"))?;
    stream
        .set_read_timeout(Some(TIMEOUT))
        .and_then(|()| stream.set_write_timeout(Some(TIMEOUT)))
        .and_then(|()| {
            stream.write_all(b"GET /v1/health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        })
        .map_err(|err| format!("cannot ask {target}: {err}"))?;
    let mut reply = String::new();
    stream
        .read_to_string(&mut reply)
        .map_err(|err| format!("no answer from {target}: {err}"))?;
    let status = reply.lines().next().unwrap_or_default();
    if status.starts_with("HTTP/1.1 200") && reply.contains(r#""status":"ok""#) {
        Ok(())
    } else {
        Err(format!("{target} answered {status:?}"))
    }
}

#[cfg(test)]
mod tests {
    use tokio::net::TcpListener;

    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn a_running_core_passes() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, crate::router()).await });
        let on_every_interface = SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), addr.port());
        let result = tokio::task::spawn_blocking(move || check(on_every_interface))
            .await
            .unwrap();
        assert_eq!(result, Ok(()));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_wrong_answer_fails() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, axum::Router::new()).await });
        let result = tokio::task::spawn_blocking(move || check(addr)).await.unwrap();
        assert!(result.unwrap_err().contains("404"));
    }

    #[test]
    fn a_core_that_isnt_there_fails() {
        let free = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        assert!(check(free).unwrap_err().contains("cannot reach"));
    }
}
