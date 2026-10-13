//! `mercury-probe reach TARGET [--timeout SECS]`: the deployment's network checks, run inside a
//! container whose image has no other tool. It answers whether a route exists, and nothing more:
//!
//! - `http://host[:port][/path]`: a GET of the path, straight to the host. Any HTTP answer counts,
//!   and its status line is printed.
//! - `https://host[:port][/...]`: through the proxy `HTTPS_PROXY` (or `https_proxy`) names, unless
//!   `NO_PROXY` (or `no_proxy`) names the host: the proxy's answer to `CONNECT host:port` counts
//!   when it is 2xx, and its status line is printed either way. Without a proxy, a TCP connection to
//!   the port (443 by default). No TLS: the route is the question, not the host's certificate.
//! - `host:port`: a TCP connection.
//!
//! Names resolve with the system's resolver, and each step (resolving, connecting, asking, reading
//! the answer) has the timeout.

use std::io::Write;
use std::time::Duration;

use super::{HostPort, connect, describe, read_status_line, status_code};

/// The timeout of each step when `--timeout` isn't given.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// The longest `--timeout`, in seconds.
const MAX_TIMEOUT_SECS: u64 = 600;

/// What `reach` is asked to reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// `http://host[:port][/path]`: a GET of the path.
    Http { to: HostPort, path: String },
    /// `https://host[:port][/...]`: a route to the port, through the proxy when there is one.
    Https(HostPort),
    /// `host:port`: a TCP connection.
    Tcp(HostPort),
}

impl Target {
    pub fn parse(value: &str) -> Result<Self, String> {
        let Some((scheme, rest)) = value.split_once("://") else {
            return HostPort::parse(value, None).map(Target::Tcp).map_err(|problem| {
                format!("the target must be http://..., https://... or host:port: {problem}")
            });
        };
        let (authority, path) = match rest.find(['/', '?', '#']) {
            Some(at) => rest.split_at(at),
            None => (rest, ""),
        };
        if authority.contains('@') {
            return Err("the target's URL carries credentials, which reach never sends".to_owned());
        }
        match scheme.to_ascii_lowercase().as_str() {
            "http" => {
                let to = HostPort::parse(authority, Some(80))?;
                let path = path.split('#').next().unwrap_or_default();
                let path = if path.starts_with('/') {
                    path.to_owned()
                } else {
                    format!("/{path}")
                };
                if path.chars().any(|c| c.is_whitespace() || c.is_control()) {
                    return Err("the target's path can't hold spaces or control characters".to_owned());
                }
                Ok(Target::Http { to, path })
            }
            "https" => HostPort::parse(authority, Some(443)).map(Target::Https),
            other => Err(format!(
                "reach knows http://, https:// and host:port, not {other}://"
            )),
        }
    }
}

/// `reach`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReachArgs {
    pub target: Target,
    /// How long each step may take.
    pub timeout: Duration,
}

/// `TARGET [--timeout SECS]`, in either order.
pub fn parse_args(args: impl IntoIterator<Item = String>) -> Result<ReachArgs, String> {
    let mut args = args.into_iter();
    let mut target = None;
    let mut timeout = DEFAULT_TIMEOUT;
    while let Some(arg) = args.next() {
        if arg == "--timeout" {
            let value = args.next().ok_or("--timeout needs a value")?;
            timeout = match value.parse::<u64>() {
                Ok(secs) if (1..=MAX_TIMEOUT_SECS).contains(&secs) => Duration::from_secs(secs),
                _ => {
                    return Err(format!(
                        "--timeout must be whole seconds from 1 to {MAX_TIMEOUT_SECS}, not {value:?}"
                    ));
                }
            };
        } else if arg.starts_with("--") {
            return Err(format!("unknown option {arg:?}"));
        } else if target.is_some() {
            return Err(format!("reach takes one target, not also {arg:?}"));
        } else {
            target = Some(Target::parse(&arg)?);
        }
    }
    let target = target.ok_or("reach needs a target: http://..., https://... or host:port")?;
    Ok(ReachArgs { target, timeout })
}

/// The proxy an `https://` target on `host` goes through: `HTTPS_PROXY` (or `https_proxy`), an
/// `http://host:port` address, unless `NO_PROXY` (or `no_proxy`) names the host. An error names the
/// variable, never its value.
pub fn https_proxy(host: &str, var: impl Fn(&str) -> Option<String>) -> Result<Option<HostPort>, String> {
    let set = |name: &str| var(name).map(|v| v.trim().to_owned()).filter(|v| !v.is_empty());
    let Some(proxy) = set("HTTPS_PROXY").or_else(|| set("https_proxy")) else {
        return Ok(None);
    };
    if [set("NO_PROXY"), set("no_proxy")]
        .into_iter()
        .flatten()
        .any(|list| no_proxy_names(&list, host))
    {
        return Ok(None);
    }
    let rest = match proxy.split_once("://") {
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("http") => rest,
        Some(_) => return Err("HTTPS_PROXY must name an http:// proxy".to_owned()),
        None => proxy.as_str(),
    };
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.contains('@') {
        return Err("HTTPS_PROXY carries credentials, which reach never sends".to_owned());
    }
    HostPort::parse(authority, Some(80))
        .map(Some)
        .map_err(|_| "HTTPS_PROXY isn't an http://host:port address".to_owned())
}

/// Whether a `NO_PROXY` list names `host`: `*`, the host itself, or a domain it is in
/// (`example.com` and `.example.com` both cover `api.example.com`). A port on an entry is ignored.
fn no_proxy_names(list: &str, host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    list.split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .any(|entry| {
            if entry == "*" {
                return true;
            }
            let entry = entry.trim_start_matches("*.").trim_start_matches('.');
            let entry = match entry.strip_prefix('[').and_then(|v6| v6.split_once(']')) {
                Some((v6, _)) => v6,
                None => match entry.rsplit_once(':') {
                    Some((name, port))
                        if !name.contains(':') && port.bytes().all(|byte| byte.is_ascii_digit()) =>
                    {
                        name
                    }
                    _ => entry,
                },
            };
            let entry = entry.to_ascii_lowercase();
            !entry.is_empty() && (host == entry || host.ends_with(&format!(".{entry}")))
        })
}

/// What `reach` found.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// The status line to print: the server's (`http://`) or the proxy's (`https://` through one).
    pub status: Option<String>,
    /// Why the target counts as unreachable; `None` when it was reached.
    pub failure: Option<String>,
}

impl Outcome {
    fn failed(problem: String) -> Self {
        Self {
            status: None,
            failure: Some(problem),
        }
    }
}

/// `reach` with its arguments, the proxy taken from `var` (the environment).
pub fn run(args: &ReachArgs, var: impl Fn(&str) -> Option<String>) -> Outcome {
    let proxy = match &args.target {
        Target::Https(to) => match https_proxy(&to.host, var) {
            Ok(proxy) => proxy,
            Err(problem) => return Outcome::failed(problem),
        },
        Target::Http { .. } | Target::Tcp(_) => None,
    };
    reach(&args.target, args.timeout, proxy.as_ref())
}

/// Reaches `target`, an `https://` one through `proxy` when it is given.
pub fn reach(target: &Target, timeout: Duration, proxy: Option<&HostPort>) -> Outcome {
    match (target, proxy) {
        (Target::Http { to, path }, _) => {
            let request = format!(
                "GET {path} HTTP/1.1\r\nHost: {to}\r\nUser-Agent: mercury-probe\r\nAccept: */*\r\nConnection: close\r\n\r\n"
            );
            match ask(to, &request, timeout) {
                Ok(status) => Outcome {
                    status: Some(status),
                    failure: None,
                },
                Err(problem) => Outcome::failed(problem),
            }
        }
        (Target::Https(to), Some(proxy)) => {
            let request = format!("CONNECT {to} HTTP/1.1\r\nHost: {to}\r\n\r\n");
            match ask(proxy, &request, timeout) {
                Ok(status) if status_code(&status).is_some_and(|code| (200..300).contains(&code)) => {
                    Outcome {
                        status: Some(status),
                        failure: None,
                    }
                }
                Ok(status) => Outcome {
                    status: Some(status),
                    failure: Some(format!("the proxy {proxy} refused CONNECT {to}")),
                },
                Err(problem) => Outcome::failed(problem),
            }
        }
        // `host:port` never goes through a proxy
        (Target::Https(to), None) | (Target::Tcp(to), _) => match connect(to, timeout) {
            Ok(_) => Outcome::default(),
            Err(problem) => Outcome::failed(problem),
        },
    }
}

/// Sends `request` to `server` and returns the status line of its answer, when the answer is HTTP.
fn ask(server: &HostPort, request: &str, timeout: Duration) -> Result<String, String> {
    let mut stream = connect(server, timeout)?;
    stream
        .write_all(request.as_bytes())
        .map_err(|err| format!("cannot ask {server}: {}", describe(&err)))?;
    let status = read_status_line(&mut stream, timeout)
        .map_err(|err| format!("no answer from {server}: {}", describe(&err)))?;
    if status_code(&status).is_some() {
        Ok(status)
    } else if status.is_empty() {
        Err(format!("{server} closed the connection without an answer"))
    } else {
        Err(format!("{server} didn't answer in HTTP"))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn parse(line: &str) -> Result<ReachArgs, String> {
        parse_args(line.split_whitespace().map(str::to_owned))
    }

    fn host_port(host: &str, port: u16) -> HostPort {
        HostPort {
            host: host.to_owned(),
            port,
        }
    }

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let vars: HashMap<String, String> = pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        move |name| vars.get(name).cloned()
    }

    #[test]
    fn an_http_target_keeps_its_path() {
        let parsed = Target::parse("http://core:8081/internal/probes/register").unwrap();
        assert_eq!(
            parsed,
            Target::Http {
                to: host_port("core", 8081),
                path: "/internal/probes/register".to_owned()
            }
        );
        assert_eq!(
            Target::parse("http://reader-egress:8888").unwrap(),
            Target::Http {
                to: host_port("reader-egress", 8888),
                path: "/".to_owned()
            }
        );
        assert_eq!(
            Target::parse("HTTP://example.com?x=1#top").unwrap(),
            Target::Http {
                to: host_port("example.com", 80),
                path: "/?x=1".to_owned()
            }
        );
    }

    #[test]
    fn an_https_target_is_its_host_and_port() {
        assert_eq!(
            Target::parse("https://example.com").unwrap(),
            Target::Https(host_port("example.com", 443))
        );
        assert_eq!(
            Target::parse("https://api.github.com/repos?per_page=1").unwrap(),
            Target::Https(host_port("api.github.com", 443))
        );
        assert_eq!(
            Target::parse("https://[::1]:8443/").unwrap(),
            Target::Https(host_port("::1", 8443))
        );
    }

    #[test]
    fn a_bare_target_needs_its_port() {
        assert_eq!(
            Target::parse("egress-proxy:8888").unwrap(),
            Target::Tcp(host_port("egress-proxy", 8888))
        );
        assert!(Target::parse("egress-proxy").is_err());
    }

    #[test]
    fn other_targets_are_refused() {
        for value in [
            "ftp://example.com",
            "https://user:secret@example.com",
            "http://exa mple.com/",
            "http://example.com:0/",
            "https://",
            "",
        ] {
            assert!(Target::parse(value).is_err(), "{value}");
        }
    }

    #[test]
    fn the_timeout_is_whole_seconds_on_either_side_of_the_target() {
        let args = parse("https://example.com").unwrap();
        assert_eq!(args.timeout, DEFAULT_TIMEOUT);
        assert_eq!(
            parse("https://example.com --timeout 2").unwrap().timeout,
            Duration::from_secs(2)
        );
        assert_eq!(
            parse("--timeout 3 example.com:443").unwrap().timeout,
            Duration::from_secs(3)
        );
        for line in [
            "",
            "--timeout 5",
            "example.com:443 --timeout",
            "example.com:443 --timeout 0",
            "example.com:443 --timeout 1.5",
            "example.com:443 --timeout 601",
            "example.com:443 example.org:443",
            "example.com:443 --verbose",
        ] {
            assert!(parse(line).is_err(), "{line}");
        }
    }

    #[test]
    fn https_goes_through_the_proxy_the_environment_names() {
        let proxy = Some(host_port("egress-proxy", 8888));
        let upper = env(&[("HTTPS_PROXY", "http://egress-proxy:8888")]);
        assert_eq!(https_proxy("api.github.com", upper), Ok(proxy.clone()));
        let lower = env(&[("https_proxy", "http://egress-proxy:8888/")]);
        assert_eq!(https_proxy("api.github.com", lower), Ok(proxy.clone()));
        let bare = env(&[("HTTPS_PROXY", "egress-proxy:8888")]);
        assert_eq!(https_proxy("api.github.com", bare), Ok(proxy));
        let default_port = env(&[("HTTPS_PROXY", "http://egress-proxy")]);
        assert_eq!(
            https_proxy("api.github.com", default_port),
            Ok(Some(host_port("egress-proxy", 80)))
        );
        assert_eq!(https_proxy("api.github.com", env(&[])), Ok(None));
        assert_eq!(
            https_proxy("api.github.com", env(&[("HTTPS_PROXY", " ")])),
            Ok(None)
        );
    }

    #[test]
    fn a_proxy_it_cant_use_is_an_error_that_shows_no_value() {
        for value in [
            "https://egress-proxy:8888",
            "socks5://egress-proxy:1080",
            "http://user:secret@proxy:8888",
        ] {
            let problem = https_proxy("example.com", env(&[("HTTPS_PROXY", value)])).unwrap_err();
            assert!(problem.starts_with("HTTPS_PROXY"), "{problem}");
            assert!(!problem.contains("secret"), "{problem}");
        }
        assert!(https_proxy("example.com", env(&[("HTTPS_PROXY", "http://:8888")])).is_err());
    }

    #[test]
    fn no_proxy_names_hosts_and_their_domains() {
        let names = |list: &str, host: &str| {
            https_proxy(
                host,
                env(&[("HTTPS_PROXY", "http://proxy:8888"), ("NO_PROXY", list)]),
            )
            .unwrap()
            .is_none()
        };
        assert!(names("*", "example.com"));
        assert!(names("example.com", "example.com"));
        assert!(names("example.com", "api.example.com"));
        assert!(names(".example.com", "api.example.com"));
        assert!(names("*.example.com", "api.example.com"));
        assert!(names("localhost, core:8081", "core"));
        assert!(names("[::1]:443", "::1"));
        assert!(names("EXAMPLE.com", "example.COM"));
        assert!(!names("example.com", "notexample.com"));
        assert!(!names("api.example.com", "example.com"));
        assert!(!names("", "example.com"));
        assert!(!names(".", "example.com"));
        let lower = env(&[("https_proxy", "http://proxy:8888"), ("no_proxy", "example.com")]);
        assert_eq!(https_proxy("example.com", lower), Ok(None));
    }
}
