//! `mercury-probe egress`: the forward proxy between a network with no route out and the hosts its
//! services may reach (`sync`'s GitHub API, the code reader's one upstream). It speaks one method,
//! `CONNECT host:port`, and lets through only the ports it is given (443 by default) and the hosts
//! its allowlist matches; everything else is refused. After `200 Connection established` it copies
//! bytes both ways and reads none of them: TLS runs end to end between the client and the host.
//!
//! The allowlist is a file of extended regular expressions, one per line, matched against the host
//! alone; a line ends at its first space or unescaped `#`, and blank lines and comments are skipped.
//! Anchor each one (`^api\.github\.com$`): an expression matches any host that contains it.
//!
//! It writes no file, and logs one line per request: the host, the port and the verdict.

use std::collections::BTreeSet;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use regex::{Regex, RegexBuilder};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio::time::{Instant, timeout};

use super::{HostPort, parse_port};

/// The port a `CONNECT` may name when no `--port` is given.
pub const DEFAULT_PORT: u16 = 443;

/// How many connections are served at once; the next ones wait in the listen queue.
pub const MAX_CONNECTIONS: usize = 50;

/// A tunnel that moves no byte either way for this long is closed.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(600);

/// The most a request's head (its request line and headers) may take.
pub const MAX_HEAD_BYTES: usize = 8 * 1024;

/// How long a client has to send its request's head.
const HEAD_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the host of an allowed `CONNECT` has to accept the connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a refusal may take to write.
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);

/// The pause after a failed accept (out of file descriptors, say), so the loop doesn't spin.
const ACCEPT_PAUSE: Duration = Duration::from_millis(100);

const ESTABLISHED: &[u8] = b"HTTP/1.1 200 Connection established\r\n\r\n";
const BAD_REQUEST: &str = "400 Bad Request";
const FORBIDDEN: &str = "403 Forbidden";
const METHOD_NOT_ALLOWED: &str = "405 Method Not Allowed";
const BAD_GATEWAY: &str = "502 Bad Gateway";

/// A line of the proxy's log goes here.
pub type Log = Arc<dyn Fn(&str) + Send + Sync>;

/// `egress`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressArgs {
    pub listen: SocketAddr,
    /// The allowlist file.
    pub allow: PathBuf,
    /// The ports a `CONNECT` may name; `[443]` when no `--port` was given.
    pub ports: Vec<u16>,
}

/// `--listen ADDR --allow FILE [--port N]...`.
pub fn parse_args(args: impl IntoIterator<Item = String>) -> Result<EgressArgs, String> {
    let mut args = args.into_iter();
    let (mut listen, mut allow, mut ports) = (None, None, Vec::new());
    while let Some(flag) = args.next() {
        let known = matches!(flag.as_str(), "--listen" | "--allow" | "--port");
        let value = match (known, args.next()) {
            (false, _) => return Err(format!("unknown option {flag:?}")),
            (true, None) => return Err(format!("{flag} needs a value")),
            (true, Some(value)) => value,
        };
        match flag.as_str() {
            "--listen" => listen = Some(crate::listen_addr(&value)?),
            "--allow" => allow = Some(PathBuf::from(value)),
            _ => ports.push(
                parse_port(&value)
                    .map_err(|()| format!("--port must be a number from 1 to 65535, not {value:?}"))?,
            ),
        }
    }
    if ports.is_empty() {
        ports.push(DEFAULT_PORT);
    }
    Ok(EgressArgs {
        listen: listen.ok_or("egress needs --listen ADDR")?,
        allow: allow.ok_or("egress needs --allow FILE, its allowlist")?,
        ports,
    })
}

/// The hosts a `CONNECT` may name: extended regular expressions, matched against the host alone.
#[derive(Debug, Clone)]
pub struct Allowlist {
    patterns: Vec<Regex>,
}

impl Allowlist {
    /// The patterns of an allowlist's text. An error names the line.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut patterns = Vec::new();
        for (number, line) in text.lines().enumerate() {
            let Some(pattern) = pattern(line) else {
                continue;
            };
            // a host name is the same in any case
            let regex = RegexBuilder::new(pattern)
                .case_insensitive(true)
                .build()
                .map_err(|_| format!("line {} isn't a regular expression", number + 1))?;
            patterns.push(regex);
        }
        Ok(Self { patterns })
    }

    /// The allowlist in the file at `path`.
    pub fn read(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|err| format!("cannot read the allowlist {}: {err}", path.display()))?;
        Self::parse(&text).map_err(|problem| format!("the allowlist {}: {problem}", path.display()))
    }

    /// Whether one of the patterns matches `host`.
    pub fn allows(&self, host: &str) -> bool {
        self.patterns.iter().any(|pattern| pattern.is_match(host))
    }

    pub fn len(&self) -> usize {
        self.patterns.len()
    }

    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }
}

/// The pattern of an allowlist line: up to its first space or unescaped `#`. `None` for a blank line
/// or a comment.
fn pattern(line: &str) -> Option<&str> {
    let line = line.trim_start();
    let mut end = line.len();
    let mut escaped = false;
    for (at, c) in line.char_indices() {
        if c.is_whitespace() || (c == '#' && !escaped) {
            end = at;
            break;
        }
        escaped = c == '\\' && !escaped;
    }
    Some(&line[..end]).filter(|pattern| !pattern.is_empty())
}

/// What the proxy lets through: the allowlist's hosts, on the given ports.
#[derive(Debug, Clone)]
pub struct Policy {
    hosts: Allowlist,
    ports: BTreeSet<u16>,
}

/// The policy's answer to a `CONNECT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Allowed,
    /// A port that isn't one of `--port`.
    PortRefused,
    /// A host no pattern of the allowlist matches.
    HostRefused,
}

impl Policy {
    pub fn new(hosts: Allowlist, ports: &[u16]) -> Self {
        Self {
            hosts,
            ports: ports.iter().copied().collect(),
        }
    }

    pub fn verdict(&self, target: &HostPort) -> Verdict {
        if !self.ports.contains(&target.port) {
            Verdict::PortRefused
        } else if !self.hosts.allows(&target.host) {
            Verdict::HostRefused
        } else {
            Verdict::Allowed
        }
    }
}

/// Why a request is refused before the policy is asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BadRequest {
    /// Not a request line and headers this proxy can read: 400.
    Malformed,
    /// A method other than `CONNECT`: 405.
    Method,
}

/// The target of a `CONNECT host:port HTTP/1.x` request's head (its request line and headers, up
/// to the blank line). Its headers are checked for form and otherwise ignored.
pub fn parse_head(head: &[u8]) -> Result<HostPort, BadRequest> {
    let head = std::str::from_utf8(head).map_err(|_| BadRequest::Malformed)?;
    let mut lines = head
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line));
    let request = lines.next().unwrap_or_default();
    let mut parts = request.split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(BadRequest::Malformed);
    };
    let token = !method.is_empty()
        && method
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte));
    if !token || !matches!(version, "HTTP/1.1" | "HTTP/1.0") {
        return Err(BadRequest::Malformed);
    }
    for line in lines.filter(|line| !line.is_empty()) {
        match line.split_once(':') {
            Some((name, _)) if !name.is_empty() && !name.contains([' ', '\t']) => {}
            _ => return Err(BadRequest::Malformed),
        }
    }
    if method != "CONNECT" {
        return Err(BadRequest::Method);
    }
    HostPort::parse(target, None).map_err(|_| BadRequest::Malformed)
}

/// Where the head of a request ends in `buffer`: the end of its last line, and the start of what
/// follows the blank line after it.
fn head_end(buffer: &[u8]) -> Option<(usize, usize)> {
    buffer.iter().enumerate().find_map(|(at, &byte)| {
        if byte != b'\n' {
            return None;
        }
        match (buffer.get(at + 1), buffer.get(at + 2)) {
            (Some(b'\n'), _) => Some((at + 1, at + 2)),
            (Some(b'\r'), Some(b'\n')) => Some((at + 1, at + 3)),
            _ => None,
        }
    })
}

/// What a client sent before the proxy answers.
enum Head {
    /// The head, and whatever the client sent after it (the start of its TLS handshake, say).
    Read { head: Vec<u8>, rest: Vec<u8> },
    /// Over [`MAX_HEAD_BYTES`], or cut off by the client's end of the connection.
    Bad,
    /// Closed before a byte.
    Nothing,
}

async fn read_head(stream: &mut TcpStream) -> io::Result<Head> {
    let mut buffer = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Ok(if buffer.is_empty() {
                Head::Nothing
            } else {
                Head::Bad
            });
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some((end, start)) = head_end(&buffer) {
            if end > MAX_HEAD_BYTES {
                return Ok(Head::Bad);
            }
            let rest = buffer.split_off(start);
            buffer.truncate(end);
            return Ok(Head::Read { head: buffer, rest });
        }
        if buffer.len() > MAX_HEAD_BYTES {
            return Ok(Head::Bad);
        }
    }
}

/// The line that starts the log.
pub fn start_line(addr: SocketAddr, policy: &Policy, allow: &Path) -> String {
    let ports: Vec<String> = policy.ports.iter().map(u16::to_string).collect();
    format!(
        "mercury-probe {} (egress): CONNECT on {addr} to ports {} of the hosts the {} patterns of {} match; at most {MAX_CONNECTIONS} connections, closed after {} s idle",
        env!("CARGO_PKG_VERSION"),
        ports.join(", "),
        policy.hosts.len(),
        allow.display(),
        IDLE_TIMEOUT.as_secs(),
    )
}

/// Serves `listener` until the process is stopped.
pub async fn serve(listener: TcpListener, policy: Arc<Policy>, log: Log) -> Result<(), String> {
    let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    loop {
        let slot = Arc::clone(&slots)
            .acquire_owned()
            .await
            .map_err(|_| "the proxy's connection slots were closed".to_owned())?;
        let stream = match listener.accept().await {
            Ok((stream, _)) => stream,
            Err(err) => {
                log(&format!("egress: a connection couldn't be accepted: {err}"));
                tokio::time::sleep(ACCEPT_PAUSE).await;
                continue;
            }
        };
        let (policy, log) = (Arc::clone(&policy), Arc::clone(&log));
        tokio::spawn(async move {
            handle(stream, &policy, &log).await;
            drop(slot);
        });
    }
}

/// One client: its request, the policy's verdict, and the tunnel when it is allowed.
async fn handle(mut client: TcpStream, policy: &Policy, log: &Log) {
    let (head, rest) = match timeout(HEAD_TIMEOUT, read_head(&mut client)).await {
        Ok(Ok(Head::Read { head, rest })) => (head, rest),
        // nothing asked, nothing to answer or log
        Ok(Ok(Head::Nothing) | Err(_)) => return,
        Ok(Ok(Head::Bad)) => {
            refuse(&mut client, BAD_REQUEST).await;
            log("egress: a malformed request: 400");
            return;
        }
        Err(_) => {
            log(&format!(
                "egress: no request within {} s: closed",
                HEAD_TIMEOUT.as_secs()
            ));
            return;
        }
    };
    let target = match parse_head(&head) {
        Ok(target) => target,
        Err(BadRequest::Method) => {
            refuse(&mut client, METHOD_NOT_ALLOWED).await;
            log("egress: a request other than CONNECT: 405");
            return;
        }
        Err(BadRequest::Malformed) => {
            refuse(&mut client, BAD_REQUEST).await;
            log("egress: a malformed request: 400");
            return;
        }
    };
    match policy.verdict(&target) {
        Verdict::Allowed => {}
        Verdict::PortRefused => {
            refuse(&mut client, FORBIDDEN).await;
            log(&format!("egress: CONNECT {target}: 403, the port isn't allowed"));
            return;
        }
        Verdict::HostRefused => {
            refuse(&mut client, FORBIDDEN).await;
            log(&format!(
                "egress: CONNECT {target}: 403, the host isn't on the allowlist"
            ));
            return;
        }
    }
    let mut upstream = match timeout(
        CONNECT_TIMEOUT,
        TcpStream::connect((target.host.as_str(), target.port)),
    )
    .await
    {
        Ok(Ok(upstream)) => upstream,
        Ok(Err(_)) | Err(_) => {
            refuse(&mut client, BAD_GATEWAY).await;
            log(&format!(
                "egress: CONNECT {target}: 502, the host can't be reached"
            ));
            return;
        }
    };
    log(&format!("egress: CONNECT {target}: 200, allowed"));
    let _ = client.set_nodelay(true);
    let _ = upstream.set_nodelay(true);
    if client.write_all(ESTABLISHED).await.is_err() || upstream.write_all(&rest).await.is_err() {
        return;
    }
    relay(client, upstream, IDLE_TIMEOUT).await;
}

/// Answers with `status` and an empty body, then closes the connection.
async fn refuse(stream: &mut TcpStream, status: &str) {
    let allow = if status == METHOD_NOT_ALLOWED {
        "Allow: CONNECT\r\n"
    } else {
        ""
    };
    let reply = format!("HTTP/1.1 {status}\r\n{allow}Content-Length: 0\r\nConnection: close\r\n\r\n");
    // a client that has gone already needs no answer
    let _ = timeout(REPLY_TIMEOUT, async {
        stream.write_all(reply.as_bytes()).await?;
        stream.shutdown().await
    })
    .await;
}

/// Copies bytes both ways until both sides have closed, either fails, or nothing moves for `idle`.
/// One side's close is passed on to the other, which may still answer.
async fn relay(client: TcpStream, upstream: TcpStream, idle: Duration) {
    let started = Instant::now();
    let last = AtomicU64::new(0);
    let (mut client_in, mut client_out) = client.into_split();
    let (mut upstream_in, mut upstream_out) = upstream.into_split();
    let both = async {
        tokio::try_join!(
            one_way(&mut client_in, &mut upstream_out, &last, started),
            one_way(&mut upstream_in, &mut client_out, &last, started),
        )
    };
    tokio::select! {
        _ = both => {}
        () = idle_out(&last, started, idle) => {}
    }
}

/// Copies `from` to `to` until `from` closes, then closes `to`'s sending side. `last` is the time of
/// the last byte moved, in milliseconds after `started`.
async fn one_way(
    from: &mut OwnedReadHalf,
    to: &mut OwnedWriteHalf,
    last: &AtomicU64,
    started: Instant,
) -> io::Result<()> {
    let mut buffer = vec![0u8; 16 * 1024];
    loop {
        let read = from.read(&mut buffer).await?;
        if read == 0 {
            return to.shutdown().await;
        }
        to.write_all(&buffer[..read]).await?;
        last.store(millis_since(started), Ordering::Relaxed);
    }
}

/// Ends once nothing has moved for `idle`.
async fn idle_out(last: &AtomicU64, started: Instant, idle: Duration) {
    loop {
        let deadline = started + Duration::from_millis(last.load(Ordering::Relaxed)) + idle;
        if Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep_until(deadline).await;
    }
}

fn millis_since(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str) -> Result<EgressArgs, String> {
        parse_args(line.split_whitespace().map(str::to_owned))
    }

    fn target(host: &str, port: u16) -> HostPort {
        HostPort {
            host: host.to_owned(),
            port,
        }
    }

    #[test]
    fn the_listen_address_and_the_allowlist_are_needed_and_the_port_defaults_to_443() {
        let args = parse("--listen 0.0.0.0:8888 --allow /etc/mercury/allowlist").unwrap();
        assert_eq!(
            args,
            EgressArgs {
                listen: "0.0.0.0:8888".parse().unwrap(),
                allow: PathBuf::from("/etc/mercury/allowlist"),
                ports: vec![443],
            }
        );
        let args = parse("--port 443 --allow a --port 8443 --listen 127.0.0.1:0").unwrap();
        assert_eq!(args.ports, vec![443, 8443]);
        assert!(parse("--allow a").unwrap_err().contains("--listen"));
        assert!(parse("--listen 127.0.0.1:8888").unwrap_err().contains("--allow"));
        for line in [
            "--listen 8888 --allow a",
            "--listen 127.0.0.1:8888 --allow a --port 0",
            "--listen 127.0.0.1:8888 --allow a --port https",
            "--listen 127.0.0.1:8888 --allow",
            "--listen 127.0.0.1:8888 --allow a --verbose",
            "--listen 127.0.0.1:8888 --allow a extra",
        ] {
            assert!(parse(line).is_err(), "{line}");
        }
    }

    #[test]
    fn the_allowlist_skips_blank_lines_and_comments() {
        let list = Allowlist::parse(
            "# the GitHub API\n\n^api\\.github\\.com$\r\n  ^codeload\\.github\\.com$   # tarballs\n\t\n#^example\\.com$\n",
        )
        .unwrap();
        assert_eq!(list.len(), 2);
        assert!(list.allows("api.github.com"));
        assert!(list.allows("codeload.github.com"));
        assert!(!list.allows("example.com"));
        assert!(Allowlist::parse("").unwrap().is_empty());
        assert!(Allowlist::parse("# nothing\n\n").unwrap().is_empty());
    }

    #[test]
    fn an_escaped_hash_is_part_of_the_pattern() {
        assert_eq!(pattern(r"^a\#b$ # comment"), Some(r"^a\#b$"));
        assert_eq!(pattern(r"^a\\#b$"), Some(r"^a\\"));
        assert_eq!(pattern("   # comment"), None);
        assert_eq!(pattern(""), None);
    }

    #[test]
    fn an_anchored_pattern_matches_its_host_alone_in_any_case() {
        let list = Allowlist::parse("^openrouter\\.ai$").unwrap();
        assert!(list.allows("openrouter.ai"));
        assert!(list.allows("OpenRouter.AI"));
        for host in [
            "api.openrouter.ai",
            "openrouter.ai.example.com",
            "openrouterxai",
            "example.com",
        ] {
            assert!(!list.allows(host), "{host}");
        }
        // unanchored, a pattern matches any host that contains it
        let loose = Allowlist::parse("github\\.com").unwrap();
        assert!(loose.allows("api.github.com"));
        assert!(loose.allows("github.com.example.com"));
    }

    #[test]
    fn an_allowlist_line_that_isnt_an_expression_is_refused_by_number() {
        let problem = Allowlist::parse("^api\\.github\\.com$\n# fine\n^(unclosed$\n").unwrap_err();
        assert_eq!(problem, "line 3 isn't a regular expression");
    }

    #[test]
    fn the_policy_refuses_other_ports_and_hosts_off_the_list() {
        let policy = Policy::new(
            Allowlist::parse("^api\\.github\\.com$\n^codeload\\.github\\.com$").unwrap(),
            &[443],
        );
        assert_eq!(policy.verdict(&target("api.github.com", 443)), Verdict::Allowed);
        assert_eq!(
            policy.verdict(&target("codeload.github.com", 443)),
            Verdict::Allowed
        );
        assert_eq!(
            policy.verdict(&target("api.github.com", 22)),
            Verdict::PortRefused
        );
        assert_eq!(
            policy.verdict(&target("api.github.com", 80)),
            Verdict::PortRefused
        );
        assert_eq!(policy.verdict(&target("example.com", 443)), Verdict::HostRefused);
        assert_eq!(policy.verdict(&target("140.82.112.6", 443)), Verdict::HostRefused);
        let nothing = Policy::new(Allowlist::parse("").unwrap(), &[443]);
        assert_eq!(
            nothing.verdict(&target("api.github.com", 443)),
            Verdict::HostRefused
        );
        let more = Policy::new(Allowlist::parse("^replay$").unwrap(), &[443, 9000]);
        assert_eq!(more.verdict(&target("replay", 9000)), Verdict::Allowed);
        assert_eq!(more.verdict(&target("replay", 9001)), Verdict::PortRefused);
    }

    #[test]
    fn a_connect_head_names_its_target() {
        assert_eq!(
            parse_head(
                b"CONNECT api.github.com:443 HTTP/1.1\r\nHost: api.github.com:443\r\nUser-Agent: x\r\n"
            ),
            Ok(target("api.github.com", 443))
        );
        assert_eq!(
            parse_head(b"CONNECT [::1]:443 HTTP/1.0\n"),
            Ok(target("::1", 443))
        );
        assert_eq!(
            parse_head(b"CONNECT OpenRouter.ai:443 HTTP/1.1\r\n"),
            Ok(target("openrouter.ai", 443))
        );
    }

    #[test]
    fn other_methods_are_405_and_broken_heads_400() {
        for head in [
            &b"GET / HTTP/1.1\r\nHost: reader-egress:8888\r\n"[..],
            b"GET http://example.com/ HTTP/1.1\r\n",
            b"POST /x HTTP/1.0\r\n",
            b"connect api.github.com:443 HTTP/1.1\r\n",
        ] {
            assert_eq!(parse_head(head), Err(BadRequest::Method), "{head:?}");
        }
        for head in [
            &b""[..],
            b"\r\n",
            b"CONNECT api.github.com:443\r\n",
            b"CONNECT api.github.com:443 HTTP/2\r\n",
            b"CONNECT  api.github.com:443 HTTP/1.1\r\n",
            b"CONNECT api.github.com:443 HTTP/1.1 extra\r\n",
            b"CONNECT api.github.com HTTP/1.1\r\n",
            b"CONNECT api.github.com:0 HTTP/1.1\r\n",
            b"CONNECT user@upstream.example.com:443 HTTP/1.1\r\n",
            b"CONNECT http://api.github.com:443 HTTP/1.1\r\n",
            b"CONNECT api.github.com:443 HTTP/1.1\r\nno colon here\r\n",
            b"CONNECT api.github.com:443 HTTP/1.1\r\nBad Name: x\r\n",
            b"CONNECT api.github.com:443 HTTP/1.1\r\n: x\r\n",
            b"CONNECT \xff:443 HTTP/1.1\r\n",
            b"G<T / HTTP/1.1\r\n",
        ] {
            assert_eq!(parse_head(head), Err(BadRequest::Malformed), "{head:?}");
        }
    }

    #[test]
    fn a_head_ends_at_its_blank_line() {
        assert_eq!(head_end(b"CONNECT a:443 HTTP/1.1\r\n\r\n"), Some((24, 26)));
        assert_eq!(head_end(b"CONNECT a:443 HTTP/1.1\n\nTLS"), Some((23, 24)));
        assert_eq!(
            head_end(b"CONNECT a:443 HTTP/1.1\r\nHost: a\r\n\r\n\x16\x03"),
            Some((33, 35))
        );
        assert_eq!(head_end(b"CONNECT a:443 HTTP/1.1\r\nHost: a\r\n"), None);
        assert_eq!(head_end(b""), None);
    }

    #[tokio::test]
    async fn a_tunnel_where_nothing_moves_is_closed() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (client, accepted) = tokio::join!(TcpStream::connect(addr), listener.accept());
        let (mut client, server_side) = (client.unwrap(), accepted.unwrap().0);
        let upstream_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_addr = upstream_listener.local_addr().unwrap();
        let (upstream, accepted) =
            tokio::join!(TcpStream::connect(upstream_addr), upstream_listener.accept());
        let (upstream, _far_end) = (upstream.unwrap(), accepted.unwrap().0);
        let relayed = tokio::spawn(relay(server_side, upstream, Duration::from_millis(200)));
        timeout(Duration::from_secs(5), relayed).await.unwrap().unwrap();
        // the proxy's side is gone: the client reads the end of the stream
        let mut byte = [0u8; 1];
        assert_eq!(client.read(&mut byte).await.unwrap(), 0);
    }
}
