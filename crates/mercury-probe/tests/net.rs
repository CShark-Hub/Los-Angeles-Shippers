//! The network tools of the probe's images, on the loopback: `healthcheck` against the probe's own
//! `/health` and against servers that fail; `reach` against HTTP servers, bare ports and a stub
//! proxy; `egress` between a client and a local echo server that stands in for an allowed host.
//! Each through the library, and once as the binary the images run, with the exit codes and the
//! output the deployment's checks read.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "the fixtures are literals; a panic is the test failing"
)]

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener as StdListener};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mercury_probe::net::egress::{self, Allowlist, MAX_HEAD_BYTES, Policy};
use mercury_probe::net::reach::{self, Outcome, Target};
use mercury_probe::net::{HostPort, healthcheck};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

const BIN: &str = env!("CARGO_BIN_EXE_mercury-probe");

const WAIT: Duration = Duration::from_secs(5);

/// How long the proxy may take to answer: a refused connection takes a Windows host about 2 s.
const ANSWER: Duration = Duration::from_secs(15);

/// A port nothing listens on: bound and released.
fn free_port() -> u16 {
    StdListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn loopback(port: u16) -> HostPort {
    HostPort {
        host: "127.0.0.1".to_owned(),
        port,
    }
}

/// A server on the loopback that reads each request's head and answers it with `reply`; it keeps
/// the request lines it was sent.
fn stub(reply: &'static str) -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
    let listener = StdListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let kept = Arc::clone(&seen);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut head = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            if head.read_line(&mut line).is_err() {
                continue;
            }
            kept.lock().unwrap().push(line.trim_end().to_owned());
            loop {
                let mut header = String::new();
                match head.read_line(&mut header) {
                    Ok(0) | Err(_) => break,
                    Ok(_) if header.trim().is_empty() => break,
                    Ok(_) => {}
                }
            }
            let _ = stream.write_all(reply.as_bytes());
        }
    });
    (addr, seen)
}

/// `mercury-probe` with `args`, in an environment without proxies but `https_proxy`.
fn probe(args: &[&str], https_proxy: Option<SocketAddr>) -> Output {
    let mut command = Command::new(BIN);
    command.args(args);
    for name in [
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "ALL_PROXY",
        "all_proxy",
        "NO_PROXY",
        "no_proxy",
    ] {
        command.env_remove(name);
    }
    if let Some(proxy) = https_proxy {
        command.env("HTTPS_PROXY", format!("http://{proxy}"));
    }
    command.output().unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

// ---------------------------------------------------------------- healthcheck

#[tokio::test(flavor = "multi_thread")]
async fn the_probes_own_health_check_passes() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, mercury_probe::router()).await });
    let checked = tokio::task::spawn_blocking(move || healthcheck::check(port))
        .await
        .unwrap();
    assert_eq!(checked, Ok(()));
}

#[test]
fn a_health_check_asks_for_health_and_fails_on_anything_but_200() {
    let (ok, seen) = stub("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
    assert_eq!(healthcheck::check(ok.port()), Ok(()));
    assert_eq!(*seen.lock().unwrap(), ["GET /health HTTP/1.1"]);

    let (failing, _) = stub("HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n");
    let problem = healthcheck::check(failing.port()).unwrap_err();
    assert!(problem.contains("500"), "{problem}");

    let (not_found, _) = stub("HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
    assert!(healthcheck::check(not_found.port()).is_err());
}

#[test]
fn a_health_check_with_nothing_listening_fails() {
    let problem = healthcheck::check(free_port()).unwrap_err();
    assert!(problem.starts_with("cannot reach 127.0.0.1:"), "{problem}");
}

#[test]
fn the_healthcheck_command_exits_0_on_200_and_1_otherwise() {
    let (ok, _) = stub("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
    let passed = probe(&["healthcheck", &ok.port().to_string()], None);
    assert!(passed.status.success(), "{}", stderr(&passed));

    let (failing, _) = stub("HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n");
    let failed = probe(&["healthcheck", &failing.port().to_string()], None);
    assert_eq!(failed.status.code(), Some(1));
    assert!(
        stderr(&failed).starts_with("mercury-probe: 127.0.0.1:"),
        "{}",
        stderr(&failed)
    );

    let nothing = probe(&["healthcheck", &free_port().to_string()], None);
    assert_eq!(nothing.status.code(), Some(1));
    assert!(stderr(&nothing).contains("cannot reach"), "{}", stderr(&nothing));

    let usage = probe(&["healthcheck"], None);
    assert_eq!(usage.status.code(), Some(2));
}

// ---------------------------------------------------------------- reach

#[test]
fn reach_prints_the_status_line_of_any_http_answer() {
    let (server, seen) = stub("HTTP/1.1 405 Method Not Allowed\r\nAllow: POST\r\nContent-Length: 0\r\n\r\n");
    let target = Target::parse(&format!("http://{server}/internal/probes/register")).unwrap();
    assert_eq!(
        reach::reach(&target, WAIT, None),
        Outcome {
            status: Some("HTTP/1.1 405 Method Not Allowed".to_owned()),
            failure: None
        }
    );
    assert_eq!(*seen.lock().unwrap(), ["GET /internal/probes/register HTTP/1.1"]);
}

#[test]
fn reach_fails_on_an_answer_that_isnt_http_or_no_answer() {
    let (ssh, _) = stub("SSH-2.0-OpenSSH_9.6\r\n");
    let outcome = reach::reach(&Target::parse(&format!("http://{ssh}/")).unwrap(), WAIT, None);
    assert_eq!(outcome.status, None);
    assert!(outcome.failure.unwrap().contains("didn't answer in HTTP"));

    let closed = Target::parse(&format!("http://127.0.0.1:{}/", free_port())).unwrap();
    let outcome = reach::reach(&closed, WAIT, None);
    assert_eq!(outcome.status, None);
    assert!(outcome.failure.unwrap().starts_with("cannot reach 127.0.0.1:"));
}

#[test]
fn reach_gives_up_on_a_server_that_never_answers_after_the_timeout() {
    // bound and never accepting: the connection opens in the listen queue, and no byte comes back
    let silent = StdListener::bind("127.0.0.1:0").unwrap();
    let target = Target::parse(&format!("http://{}/", silent.local_addr().unwrap())).unwrap();
    let started = Instant::now();
    let outcome = reach::reach(&target, Duration::from_millis(300), None);
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(outcome.status, None);
    assert!(outcome.failure.unwrap().contains("timed out"));
}

#[test]
fn reach_opens_a_tcp_connection_or_fails() {
    let open = StdListener::bind("127.0.0.1:0").unwrap();
    let port = open.local_addr().unwrap().port();
    assert_eq!(
        reach::reach(&Target::Tcp(loopback(port)), WAIT, None),
        Outcome::default()
    );
    // https without a proxy is the same question, on its port
    assert_eq!(
        reach::reach(&Target::Https(loopback(port)), WAIT, None),
        Outcome::default()
    );
    let closed = free_port();
    assert!(
        reach::reach(&Target::Tcp(loopback(closed)), WAIT, None)
            .failure
            .is_some()
    );
    assert!(
        reach::reach(&Target::Https(loopback(closed)), WAIT, None)
            .failure
            .is_some()
    );
}

#[test]
fn reach_asks_the_proxy_to_connect_and_only_2xx_passes() {
    let target = Target::parse("https://upstream.example.com/v1/models").unwrap();

    let (refusing, seen) = stub("HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n");
    let outcome = reach::reach(&target, WAIT, Some(&loopback(refusing.port())));
    assert_eq!(outcome.status.as_deref(), Some("HTTP/1.1 403 Forbidden"));
    assert!(
        outcome
            .failure
            .unwrap()
            .contains("refused CONNECT upstream.example.com:443")
    );
    assert_eq!(
        *seen.lock().unwrap(),
        ["CONNECT upstream.example.com:443 HTTP/1.1"]
    );

    let (letting, _) = stub("HTTP/1.1 200 Connection established\r\n\r\n");
    assert_eq!(
        reach::reach(&target, WAIT, Some(&loopback(letting.port()))),
        Outcome {
            status: Some("HTTP/1.1 200 Connection established".to_owned()),
            failure: None
        }
    );

    let gone = reach::reach(&target, WAIT, Some(&loopback(free_port())));
    assert_eq!(gone.status, None);
    assert!(gone.failure.unwrap().starts_with("cannot reach 127.0.0.1:"));
}

#[test]
fn the_reach_command_prints_status_lines_and_exits_0_or_1() {
    let (server, _) = stub("HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\n\r\n");
    let url = format!("http://{server}/");
    let answered = probe(&["reach", &url, "--timeout", "5"], None);
    assert!(answered.status.success(), "{}", stderr(&answered));
    assert_eq!(stdout(&answered), "HTTP/1.1 405 Method Not Allowed\n");

    // http:// goes straight to the host, whatever proxy the environment names
    let mut command = Command::new(BIN);
    let dead = format!("http://127.0.0.1:{}", free_port());
    let direct = command
        .args(["reach", &url, "--timeout", "5"])
        .env("HTTPS_PROXY", &dead)
        .env("HTTP_PROXY", &dead)
        .env("http_proxy", &dead)
        .output()
        .unwrap();
    assert!(direct.status.success(), "{}", stderr(&direct));

    let (proxy, _) = stub("HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n");
    let refused = probe(&["reach", "https://example.com", "--timeout", "5"], Some(proxy));
    assert_eq!(refused.status.code(), Some(1));
    assert_eq!(stdout(&refused), "HTTP/1.1 403 Forbidden\n");
    assert!(
        stderr(&refused).starts_with("mercury-probe: the proxy"),
        "{}",
        stderr(&refused)
    );

    let closed = format!("127.0.0.1:{}", free_port());
    let unreachable = probe(&["reach", &closed, "--timeout", "5"], None);
    assert_eq!(unreachable.status.code(), Some(1));
    assert_eq!(stdout(&unreachable), "");
    assert!(
        stderr(&unreachable).contains("cannot reach"),
        "{}",
        stderr(&unreachable)
    );

    assert_eq!(probe(&["reach"], None).status.code(), Some(2));
    assert_eq!(
        probe(&["reach", "ftp://example.com"], None).status.code(),
        Some(2)
    );
}

// ---------------------------------------------------------------- egress

/// An echo server: what a connection sends comes back, until it closes.
async fn echo() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let (mut from, mut to) = stream.split();
                let _ = tokio::io::copy(&mut from, &mut to).await;
            });
        }
    });
    addr
}

/// `egress` on the loopback with `allowlist` and `ports`, and the lines it logs.
async fn start_egress(allowlist: &str, ports: &[u16]) -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
    let policy = Policy::new(Allowlist::parse(allowlist).unwrap(), ports);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let lines = Arc::new(Mutex::new(Vec::new()));
    let kept = Arc::clone(&lines);
    let log: egress::Log = Arc::new(move |line| kept.lock().unwrap().push(line.to_owned()));
    tokio::spawn(egress::serve(listener, Arc::new(policy), log));
    (addr, lines)
}

/// Sends `request` to the proxy; the head of its reply, read a byte at a time so that nothing
/// after it is taken, and the stream.
async fn ask(proxy: SocketAddr, request: &[u8]) -> (String, TcpStream) {
    let mut stream = TcpStream::connect(proxy).await.unwrap();
    stream.write_all(request).await.unwrap();
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if timeout(ANSWER, stream.read(&mut byte)).await.unwrap().unwrap() == 0 {
            break;
        }
        head.push(byte[0]);
    }
    (String::from_utf8(head).unwrap(), stream)
}

/// The log's lines once there are `count` of them, sorted.
async fn logged(lines: &Mutex<Vec<String>>, count: usize) -> Vec<String> {
    let deadline = Instant::now() + WAIT;
    loop {
        let mut now = lines.lock().unwrap().clone();
        if now.len() >= count || Instant::now() > deadline {
            now.sort();
            return now;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_allowed_connect_gets_200_and_a_tunnel_both_ways() {
    let upstream = echo().await;
    let port = upstream.port();
    let (proxy, log) = start_egress(r"^127\.0\.0\.1$", &[port]).await;

    // what a client sends right after its head (the start of a TLS handshake) is passed on too
    let request = format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\nearly,");
    let (head, stream) = ask(proxy, request.as_bytes()).await;
    assert_eq!(head, "HTTP/1.1 200 Connection established\r\n\r\n");

    // a quarter of a megabyte each way at once: neither direction waits for the other
    let payload: Vec<u8> = (0..256 * 1024).map(|at| (at % 251) as u8).collect();
    let (mut from, mut to) = stream.into_split();
    let sent = payload.clone();
    let writer = tokio::spawn(async move {
        to.write_all(&sent).await.unwrap();
        // the client's end closes the tunnel's way up; the echo then closes its way down
        to.shutdown().await.unwrap();
    });
    let mut echoed = Vec::new();
    timeout(WAIT, from.read_to_end(&mut echoed))
        .await
        .unwrap()
        .unwrap();
    writer.await.unwrap();
    assert_eq!(&echoed[..6], b"early,");
    assert!(echoed[6..] == payload[..], "the tunnel changed the bytes");

    assert_eq!(
        logged(&log, 1).await,
        [format!("egress: CONNECT 127.0.0.1:{port}: 200, allowed")]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn refused_requests_get_403_405_400_or_502_and_one_line_each() {
    let upstream = echo().await;
    let open = upstream.port();
    let closed = free_port();
    let (proxy, log) = start_egress(
        "^127\\.0\\.0\\.1$ # the echo server\n^localhost$\n",
        &[open, closed],
    )
    .await;
    let status = |head: String| head.lines().next().unwrap_or_default().to_owned();

    let (head, _) = ask(
        proxy,
        format!("CONNECT api.github.com:{open} HTTP/1.1\r\n\r\n").as_bytes(),
    )
    .await;
    assert_eq!(status(head), "HTTP/1.1 403 Forbidden");
    let (head, _) = ask(proxy, b"CONNECT 127.0.0.1:443 HTTP/1.1\r\n\r\n").await;
    assert_eq!(status(head), "HTTP/1.1 403 Forbidden");
    let (head, _) = ask(proxy, b"GET / HTTP/1.1\r\nHost: reader-egress:8888\r\n\r\n").await;
    assert!(head.starts_with("HTTP/1.1 405 Method Not Allowed\r\n"), "{head}");
    assert!(head.contains("\r\nAllow: CONNECT\r\n"), "{head}");
    let (head, _) = ask(proxy, b"hello there\r\n\r\n").await;
    assert_eq!(status(head), "HTTP/1.1 400 Bad Request");
    // a head over the cap: refused once the cap is passed, every byte of it read
    let (head, _) = ask(proxy, &vec![b'a'; MAX_HEAD_BYTES + 1]).await;
    assert_eq!(status(head), "HTTP/1.1 400 Bad Request");
    let (head, _) = ask(
        proxy,
        format!("CONNECT 127.0.0.1:{closed} HTTP/1.1\r\n\r\n").as_bytes(),
    )
    .await;
    assert_eq!(status(head), "HTTP/1.1 502 Bad Gateway");
    // a connection that sends nothing (a port check) is answered with nothing and logs nothing
    drop(TcpStream::connect(proxy).await.unwrap());

    let mut expected = vec![
        format!("egress: CONNECT api.github.com:{open}: 403, the host isn't on the allowlist"),
        "egress: CONNECT 127.0.0.1:443: 403, the port isn't allowed".to_owned(),
        "egress: a request other than CONNECT: 405".to_owned(),
        "egress: a malformed request: 400".to_owned(),
        "egress: a malformed request: 400".to_owned(),
        format!("egress: CONNECT 127.0.0.1:{closed}: 502, the host can't be reached"),
    ];
    expected.sort();
    assert_eq!(logged(&log, expected.len()).await, expected);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(log.lock().unwrap().len(), expected.len(), "one line per request");
}

/// A `mercury-probe egress` process, stopped when it goes out of scope.
struct EgressProcess {
    child: Child,
    addr: SocketAddr,
    /// The lines it logs after its first.
    lines: Receiver<String>,
    _dir: tempfile::TempDir,
}

impl EgressProcess {
    fn start(allowlist: &str, ports: &[u16]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let allow = dir.path().join("allowlist");
        std::fs::write(&allow, allowlist).unwrap();
        let mut command = Command::new(BIN);
        command
            .args(["egress", "--listen", "127.0.0.1:0", "--allow"])
            .arg(&allow);
        for port in ports {
            command.args(["--port", &port.to_string()]);
        }
        let mut child = command
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stderr = child.stderr.take().unwrap();
        let (sender, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        // the first line names the address it listens on
        let first = lines
            .recv_timeout(Duration::from_secs(20))
            .expect("egress didn't start");
        let addr = first
            .split("CONNECT on ")
            .nth(1)
            .and_then(|rest| rest.split(' ').next())
            .and_then(|addr| addr.parse().ok())
            .unwrap_or_else(|| panic!("no address in {first:?}"));
        Self {
            child,
            addr,
            lines,
            _dir: dir,
        }
    }
}

impl Drop for EgressProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn the_egress_command_and_reach_check_routes_the_way_the_deployment_does() {
    // an upstream that accepts: the listen queue is enough, since reach sends nothing through
    let upstream = StdListener::bind("127.0.0.1:0").unwrap();
    let port = upstream.local_addr().unwrap().port();
    let egress = EgressProcess::start("# the loopback alone\n^127\\.0\\.0\\.1$\n", &[port]);

    let allowed = probe(
        &["reach", &format!("https://127.0.0.1:{port}/"), "--timeout", "5"],
        Some(egress.addr),
    );
    assert!(allowed.status.success(), "{}", stderr(&allowed));
    assert_eq!(stdout(&allowed), "HTTP/1.1 200 Connection established\n");

    let refused = probe(
        &["reach", &format!("https://example.com:{port}"), "--timeout", "5"],
        Some(egress.addr),
    );
    assert_eq!(refused.status.code(), Some(1));
    assert_eq!(stdout(&refused), "HTTP/1.1 403 Forbidden\n");

    // a plain GET gets a status line back, which is what a check that the proxy is reachable reads
    let get = probe(
        &["reach", &format!("http://{}/", egress.addr), "--timeout", "5"],
        None,
    );
    assert!(get.status.success(), "{}", stderr(&get));
    assert_eq!(stdout(&get), "HTTP/1.1 405 Method Not Allowed\n");

    // the container's health check: a TCP connection, logged by nobody
    let health = probe(&["reach", &egress.addr.to_string(), "--timeout", "5"], None);
    assert!(health.status.success(), "{}", stderr(&health));

    let mut lines: Vec<String> = (0..3)
        .map(|_| egress.lines.recv_timeout(WAIT).expect("a line per request"))
        .collect();
    lines.sort();
    let mut expected = vec![
        format!("mercury-probe: egress: CONNECT 127.0.0.1:{port}: 200, allowed"),
        format!("mercury-probe: egress: CONNECT example.com:{port}: 403, the host isn't on the allowlist"),
        "mercury-probe: egress: a request other than CONNECT: 405".to_owned(),
    ];
    expected.sort();
    assert_eq!(lines, expected);
    assert!(egress.lines.recv_timeout(Duration::from_millis(300)).is_err());
}

#[test]
fn egress_doesnt_start_without_a_readable_allowlist() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("allowlist");
    let ran = probe(
        &[
            "egress",
            "--listen",
            "127.0.0.1:0",
            "--allow",
            missing.to_str().unwrap(),
        ],
        None,
    );
    assert_eq!(ran.status.code(), Some(1));
    assert!(
        stderr(&ran).contains("cannot read the allowlist"),
        "{}",
        stderr(&ran)
    );

    std::fs::write(&missing, "^api\\.github\\.com$\n^(broken$\n").unwrap();
    let ran = probe(
        &[
            "egress",
            "--listen",
            "127.0.0.1:0",
            "--allow",
            missing.to_str().unwrap(),
        ],
        None,
    );
    assert_eq!(ran.status.code(), Some(1));
    assert!(stderr(&ran).contains("line 2"), "{}", stderr(&ran));

    assert_eq!(
        probe(&["egress", "--listen", "127.0.0.1:0"], None).status.code(),
        Some(2)
    );
}
