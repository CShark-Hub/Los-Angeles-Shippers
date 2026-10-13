//! Where the server and the token come from. `MERCURY_URL` and `MERCURY_TOKEN` come first, for CI
//! and containers, which have no keychain; otherwise what `mercury login` kept in the OS keychain:
//! Credential Manager on Windows, Keychain Services on macOS, the Secret Service elsewhere. The
//! token travels as a `TokenSecret`, whose `Debug` never shows it.

use std::cell::RefCell;
use std::fmt;
use std::net::IpAddr;

use mercury_schema::api::TokenSecret;
use reqwest::Url;
use serde::{Deserialize, Serialize};

use crate::error::CliError;

/// The server's address, for scripts and CI.
pub const URL_VAR: &str = "MERCURY_URL";
/// The token, for scripts and CI; an argument would land in the shell's history.
pub const TOKEN_VAR: &str = "MERCURY_TOKEN";

/// The keychain entry `mercury login` writes. It is one entry, so `logout` leaves nothing behind.
pub const SERVICE: &str = "mercury";
pub const ACCOUNT: &str = "login";

const MAX_TOKEN_LEN: usize = 256;

/// A Mercury server's address: http or https, no user, password, query or fragment. Plain http
/// only to this machine, so a token never crosses a network in clear.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Server(String);

impl Server {
    pub fn parse(raw: &str) -> Result<Self, CliError> {
        let url = Url::parse(raw.trim()).map_err(|_| CliError::BadUrl("that isn't a URL"))?;
        if !url.username().is_empty() || url.password().is_some() {
            return Err(CliError::BadUrl(
                "the address must not carry a user or a password",
            ));
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err(CliError::BadUrl(
                "the address must not carry a query or a fragment",
            ));
        }
        let host = url
            .host_str()
            .ok_or(CliError::BadUrl("the address has no host"))?;
        match url.scheme() {
            "https" => {}
            "http" if is_loopback(host) => {}
            "http" => {
                return Err(CliError::BadUrl(
                    "use https://: plain http would carry the token in clear",
                ));
            }
            _ => return Err(CliError::BadUrl("the address must start with https://")),
        }
        Ok(Self(url.as_str().trim_end_matches('/').to_owned()))
    }

    /// The URL of an API path such as `/v1/me`.
    pub fn endpoint(&self, path: &str) -> String {
        format!("{}{path}", self.0)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Server {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn is_loopback(host: &str) -> bool {
    let host = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();
    host == "localhost"
        || host.ends_with(".localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// A pasted token, trimmed and checked for its shape: `mcy_`, then letters, digits, `_` and `-`.
/// The server checks the rest; this catches a paste that cut the token or took more with it.
pub fn token(raw: &str) -> Result<TokenSecret, CliError> {
    let token = raw.trim();
    let shaped = token.len() <= MAX_TOKEN_LEN
        && token.strip_prefix("mcy_").is_some_and(|rest| {
            !rest.is_empty()
                && rest
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        });
    if shaped {
        Ok(TokenSecret::new(token))
    } else {
        Err(CliError::BadToken)
    }
}

/// A server and the token for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Login {
    pub server: Server,
    pub token: TokenSecret,
}

/// The keychain entry's value. No `Debug`: it holds the token in plain text.
#[derive(Serialize, Deserialize)]
struct Stored {
    url: String,
    token: String,
}

/// Where `mercury login` keeps the sign-in.
pub trait Store {
    fn load(&self) -> Result<Option<Login>, CliError>;
    fn save(&self, login: &Login) -> Result<(), CliError>;
    /// `true` when there was a sign-in to remove.
    fn clear(&self) -> Result<bool, CliError>;
}

/// The OS keychain.
pub struct Keychain;

impl Keychain {
    fn entry() -> Result<keyring::Entry, CliError> {
        keyring::Entry::new(SERVICE, ACCOUNT).map_err(|err| keychain(&err))
    }
}

impl Store for Keychain {
    fn load(&self) -> Result<Option<Login>, CliError> {
        match Self::entry()?.get_password() {
            Ok(text) => {
                let stored: Stored = serde_json::from_str(&text)
                    .map_err(|_| CliError::Keychain("its Mercury entry can't be read"))?;
                Ok(Some(Login {
                    server: Server::parse(&stored.url)?,
                    token: token(&stored.token)?,
                }))
            }
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(keychain(&err)),
        }
    }

    fn save(&self, login: &Login) -> Result<(), CliError> {
        let stored = Stored {
            url: login.server.as_str().to_owned(),
            token: login.token.expose().to_owned(),
        };
        let text =
            serde_json::to_string(&stored).map_err(|_| CliError::Keychain("the entry can't be written"))?;
        Self::entry()?.set_password(&text).map_err(|err| keychain(&err))
    }

    fn clear(&self) -> Result<bool, CliError> {
        match Self::entry()?.delete_credential() {
            Ok(()) => Ok(true),
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(err) => Err(keychain(&err)),
        }
    }
}

/// Fixed words for the keychain's errors: some of them carry what the entry held.
fn keychain(err: &keyring::Error) -> CliError {
    CliError::Keychain(match err {
        keyring::Error::NoDefaultStore => "there is none on this machine",
        keyring::Error::NoStorageAccess(_) => "it is locked or refused access",
        keyring::Error::BadEncoding(_)
        | keyring::Error::BadDataFormat(..)
        | keyring::Error::BadStoreFormat(_) => "its Mercury entry can't be read",
        _ => "it failed",
    })
}

/// A store in memory, for tests.
#[derive(Debug, Default)]
pub struct MemoryStore(RefCell<Option<Login>>);

impl MemoryStore {
    pub fn holding(login: Login) -> Self {
        Self(RefCell::new(Some(login)))
    }

    pub fn current(&self) -> Option<Login> {
        self.0.borrow().clone()
    }
}

impl Store for MemoryStore {
    fn load(&self) -> Result<Option<Login>, CliError> {
        Ok(self.current())
    }

    fn save(&self, login: &Login) -> Result<(), CliError> {
        *self.0.borrow_mut() = Some(login.clone());
        Ok(())
    }

    fn clear(&self) -> Result<bool, CliError> {
        Ok(self.0.borrow_mut().take().is_some())
    }
}

/// The sign-in a command uses. With `MERCURY_TOKEN` set, it and `MERCURY_URL`, and the keychain
/// isn't touched; otherwise the keychain's. `MERCURY_URL` alone may only repeat the server signed
/// in to: the token is never sent to another one.
pub fn resolve(var: &dyn Fn(&str) -> Option<String>, store: &dyn Store) -> Result<Login, CliError> {
    let set = |name: &str| var(name).filter(|value| !value.trim().is_empty());
    let url = set(URL_VAR);
    if let Some(raw) = set(TOKEN_VAR) {
        let url = url.ok_or(CliError::NoUrl)?;
        return Ok(Login {
            server: Server::parse(&url)?,
            token: token(&raw)?,
        });
    }
    let login = store.load()?.ok_or(CliError::NotSignedIn)?;
    if let Some(url) = url
        && Server::parse(&url)? != login.server
    {
        return Err(CliError::OtherServer);
    }
    Ok(login)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "mcy_AbCdEf123456_0123456789abcdefghijklmnopqrstuvwxyzABCDEFG";

    fn login(url: &str) -> Login {
        Login {
            server: Server::parse(url).unwrap(),
            token: token(TOKEN).unwrap(),
        }
    }

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    #[test]
    fn plain_http_only_to_this_machine() {
        for good in [
            "https://mercury.example.com",
            "https://mercury.example.com/",
            "http://localhost:8080",
            "http://127.0.0.1:8080",
            "http://[::1]:8080",
            "http://mercury.localhost",
            "HTTPS://Mercury.Example.com/base/",
        ] {
            assert!(Server::parse(good).is_ok(), "{good}");
        }
        for bad in [
            "http://mercury.example.com",
            "http://10.0.0.5:8080",
            "ftp://mercury.example.com",
            "https://someone:secret@example.com",
            "https://mercury.example.com/?token=x",
            "https://mercury.example.com/#x",
            "mercury.example.com",
            "",
        ] {
            assert!(Server::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn an_address_is_kept_without_its_trailing_slash() {
        let server = Server::parse(" https://Mercury.example.com/base/ ").unwrap();
        assert_eq!(server.as_str(), "https://mercury.example.com/base");
        assert_eq!(
            server.endpoint("/v1/me"),
            "https://mercury.example.com/base/v1/me"
        );
        assert_eq!(
            Server::parse("http://localhost:8080").unwrap().endpoint("/v1/me"),
            "http://localhost:8080/v1/me"
        );
    }

    #[test]
    fn a_token_is_trimmed_and_checked_for_its_shape() {
        assert_eq!(token(&format!("  {TOKEN}\r\n")).unwrap().expose(), TOKEN);
        for bad in [
            "",
            "mcy_",
            "Bearer mcy_abc",
            "mcx_AbCdEf123456_secret",
            "mcy_abc def",
            "mcy_abc\u{1b}[2J",
        ] {
            assert!(matches!(token(bad), Err(CliError::BadToken)), "{bad:?}");
        }
        assert!(token(&format!("mcy_{}", "a".repeat(300))).is_err());
    }

    #[test]
    fn the_environment_comes_first_and_needs_both_variables() {
        let store = MemoryStore::holding(login("https://mercury.example.com"));
        let both = [(URL_VAR, "http://localhost:8080"), (TOKEN_VAR, TOKEN)];
        let found = resolve(&env(&both), &store).unwrap();
        assert_eq!(found.server.as_str(), "http://localhost:8080");

        let token_only = [(TOKEN_VAR, TOKEN)];
        assert!(matches!(resolve(&env(&token_only), &store), Err(CliError::NoUrl)));
    }

    #[test]
    fn the_keychain_token_never_goes_to_another_server() {
        let store = MemoryStore::holding(login("https://mercury.example.com"));
        assert_eq!(
            resolve(&env(&[]), &store).unwrap().server.as_str(),
            "https://mercury.example.com"
        );
        let same = [(URL_VAR, "https://mercury.example.com/")];
        assert!(resolve(&env(&same), &store).is_ok());
        let other = [(URL_VAR, "https://elsewhere.example.com")];
        assert!(matches!(
            resolve(&env(&other), &store),
            Err(CliError::OtherServer)
        ));
    }

    #[test]
    fn nothing_anywhere_is_not_signed_in() {
        let empty = [(URL_VAR, " "), (TOKEN_VAR, "")];
        assert!(matches!(
            resolve(&env(&empty), &MemoryStore::default()),
            Err(CliError::NotSignedIn)
        ));
    }

    #[test]
    fn the_debug_form_never_shows_the_token() {
        let shown = format!("{:?}", login("https://mercury.example.com"));
        assert!(!shown.contains("0123456789abcdef"), "{shown}");
    }
}
