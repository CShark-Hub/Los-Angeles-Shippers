//! Which hosts are private: one table for `sync`, `serve`, `dev` and the reader.
//!
//! The decision comes from the URL alone and never from a DNS lookup. On its internal networks
//! the reader can't resolve an outside name anyway, and a lookup would let DNS decide.

use std::net::{Ipv4Addr, Ipv6Addr};

use reqwest::Url;

/// Why a value isn't a URL the code reader may use. None of them quotes the value: a URL can carry
/// a secret by mistake.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum UrlProblem {
    #[error("it isn't a URL")]
    NotAUrl,
    #[error("it isn't an http or https URL")]
    NotHttp,
    #[error("it names no host")]
    NoHost,
    #[error("it carries a user name or a password")]
    Credentials,
}

/// An `http(s)://` URL with a host and no credentials.
pub fn parse_url(value: &str) -> Result<Url, UrlProblem> {
    let url = Url::parse(value.trim()).map_err(|_| UrlProblem::NotAUrl)?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(UrlProblem::NotHttp);
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err(UrlProblem::NoHost);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UrlProblem::Credentials);
    }
    Ok(url)
}

/// Whether the URL's host is private: an address of the networks no outside host can have, a name
/// with one label (a Compose service) or a name under a suffix that is local by convention.
pub fn is_private(url: &Url) -> bool {
    url.host_str().is_some_and(is_private_host)
}

/// The table, for a host as the URL gives it (an IPv6 address in brackets).
pub fn is_private_host(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if let Some(inner) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        return inner.parse::<Ipv6Addr>().is_ok_and(private_v6);
    }
    if let Ok(address) = host.parse::<Ipv4Addr>() {
        return private_v4(address);
    }
    if host.is_empty() {
        return false;
    }
    !host.contains('.')
        || [".internal", ".local", ".lan", ".home.arpa"]
            .iter()
            .any(|suffix| host.ends_with(suffix))
}

/// 10/8, 172.16/12, 192.168/16, 100.64/10 (the tailnet), 127/8 and 169.254/16.
fn private_v4(address: Ipv4Addr) -> bool {
    matches!(
        address.octets(),
        [10, ..] | [172, 16..=31, ..] | [192, 168, ..] | [100, 64..=127, ..] | [127, ..] | [169, 254, ..]
    )
}

/// `::1`, fc00::/7 and fe80::/10, and an IPv4 address written as an IPv6 one by what it is.
fn private_v6(address: Ipv6Addr) -> bool {
    let first = address.segments()[0];
    address.is_loopback()
        || (first & 0xfe00) == 0xfc00
        || (first & 0xffc0) == 0xfe80
        || address.to_ipv4_mapped().is_some_and(private_v4)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn private(url: &str) -> bool {
        is_private(&parse_url(url).unwrap())
    }

    #[test]
    fn the_private_host_table() {
        for url in [
            "http://10.0.0.1:7100",
            "http://10.255.255.255/",
            "http://172.16.0.1/",
            "http://172.31.255.255/",
            "http://192.168.1.20:8080/v1/chat",
            "http://100.64.0.1/",
            "http://100.127.255.254/",
            "http://127.0.0.1:9000",
            "http://127.1.2.3/",
            "http://169.254.169.254/",
            "http://[::1]:7100/",
            "http://[fc00::1]/",
            "http://[fdab:1234::9]/",
            "http://[fe80::1]/",
            "http://[febf::1]/",
            "http://[::ffff:10.1.2.3]/",
            // a name of one label: a Compose service
            "http://code-reader:7100",
            "http://llm/v1/chat/completions",
            "http://localhost:11434",
            // a name under a suffix that is local by convention
            "http://model.internal/",
            "http://gpu-1.corp.internal:8000/",
            "http://printer.local/",
            "http://box.lan/",
            "http://router.home.arpa/",
            "http://MODEL.Internal./",
            // a number written another way is the address it names
            "http://2130706433/",
            "http://0x7f.1/",
        ] {
            assert!(private(url), "{url} should be private");
        }
    }

    #[test]
    fn everything_else_is_outside() {
        for url in [
            "https://openrouter.ai/api/v1/chat/completions",
            "https://api.github.com/",
            "http://8.8.8.8/",
            "http://172.15.0.1/",
            "http://172.32.0.1/",
            "http://100.63.0.1/",
            "http://100.128.0.1/",
            "http://192.169.0.1/",
            "http://169.253.0.1/",
            "http://11.0.0.1/",
            "http://[2001:db8::1]/",
            "http://[fec0::1]/",
            "http://[::ffff:8.8.8.8]/",
            // a name that only looks local
            "https://model.internal.example.com/",
            "https://internal.example.com/",
            "https://local.example.com/",
            "https://10.0.0.1.example.com/",
            "https://example.lan.com/",
            "https://openrouter.ai./",
        ] {
            assert!(!private(url), "{url} should be outside");
        }
    }

    #[test]
    fn a_url_that_hides_its_host_behind_credentials_is_refused_not_trusted() {
        assert_eq!(
            parse_url("http://code-reader@evil.example.com/").unwrap_err(),
            UrlProblem::Credentials
        );
        assert_eq!(
            parse_url("http://a:b@10.0.0.1/").unwrap_err(),
            UrlProblem::Credentials
        );
        // a private-looking name before the "@" is a user name, and the host is the one after it
        assert_eq!(
            parse_url("http://10.0.0.1@evil.example.com/").unwrap_err(),
            UrlProblem::Credentials
        );
    }

    #[test]
    fn what_is_not_an_http_url_is_named_without_being_quoted() {
        assert_eq!(parse_url("not a url").unwrap_err(), UrlProblem::NotAUrl);
        assert_eq!(parse_url("").unwrap_err(), UrlProblem::NotAUrl);
        assert_eq!(parse_url("ftp://10.0.0.1/").unwrap_err(), UrlProblem::NotHttp);
        assert_eq!(parse_url("file:///etc/passwd").unwrap_err(), UrlProblem::NotHttp);
        assert!(parse_url("  http://code-reader:7100  ").is_ok());
        for problem in [
            UrlProblem::NotAUrl,
            UrlProblem::NotHttp,
            UrlProblem::NoHost,
            UrlProblem::Credentials,
        ] {
            assert!(!problem.to_string().contains("://"));
        }
    }
}
