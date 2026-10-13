//! Personal tokens and the caller they identify.
//!
//! A token is `mcy_<public_id>_<secret>`: a 12-character public id to find it by, and a secret of
//! 32 random bytes in base64url. The database keeps only the SHA-256 of the secret, and digests
//! are compared in constant time. A 256-bit random secret needs no slow hash.

use std::fmt;

use axum::extract::{FromRef, FromRequestParts};
use axum::http::HeaderValue;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, TimeDelta, Utc};
use mercury_schema::Role;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::db::{self, Db};
use crate::error::ApiError;

/// What every token starts with, so secret scanners can spot a leaked one.
pub const PREFIX: &str = "mcy_";

/// The length of a token's public id.
pub const PUBLIC_ID_LEN: usize = 12;

const SECRET_BYTES: usize = 32;

/// 32 bytes in base64url without padding.
const SECRET_LEN: usize = 43;

const ALPHABET: &[u8; 62] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

/// `last_used_at` is written at most this often.
const LAST_USED_EVERY: TimeDelta = TimeDelta::minutes(1);

/// A token as it is issued: the one moment its secret exists outside the person's hands.
pub struct NewToken {
    public_id: String,
    secret: [u8; SECRET_BYTES],
}

impl NewToken {
    /// A token with a random public id and secret from the operating system.
    pub fn generate() -> Result<Self, getrandom::Error> {
        let mut secret = [0; SECRET_BYTES];
        getrandom::fill(&mut secret)?;
        Ok(Self {
            public_id: random_public_id()?,
            secret,
        })
    }

    pub fn public_id(&self) -> &str {
        &self.public_id
    }

    /// What the database stores.
    pub fn secret_sha256(&self) -> [u8; 32] {
        Sha256::digest(self.secret).into()
    }

    /// The whole token, to print once.
    pub fn reveal(&self) -> String {
        format!(
            "{PREFIX}{}_{}",
            self.public_id,
            URL_SAFE_NO_PAD.encode(self.secret)
        )
    }
}

impl fmt::Debug for NewToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NewToken")
            .field("public_id", &self.public_id)
            .finish_non_exhaustive()
    }
}

/// 12 characters from `ALPHABET`, drawn without bias: a byte below 248 (4 × 62) picks
/// `ALPHABET[byte % 62]`, any other byte is drawn again.
fn random_public_id() -> Result<String, getrandom::Error> {
    let mut id = String::with_capacity(PUBLIC_ID_LEN);
    let mut bytes = [0; 32];
    while id.len() < PUBLIC_ID_LEN {
        getrandom::fill(&mut bytes)?;
        for byte in bytes.iter().filter(|&&byte| byte < 248) {
            if id.len() < PUBLIC_ID_LEN {
                id.push(char::from(ALPHABET[usize::from(byte % 62)]));
            }
        }
    }
    Ok(id)
}

/// A token someone presented, in the shape of one, not yet checked against the database.
struct Presented {
    public_id: String,
    secret_sha256: [u8; 32],
}

/// Splits `mcy_<public_id>_<secret>`; anything else is `None`.
fn parse(text: &str) -> Option<Presented> {
    let rest = text.strip_prefix(PREFIX)?;
    if !rest.is_ascii() || rest.len() != PUBLIC_ID_LEN + 1 + SECRET_LEN {
        return None;
    }
    let (public_id, rest) = rest.split_at(PUBLIC_ID_LEN);
    let secret = rest.strip_prefix('_')?;
    if !public_id.bytes().all(|byte| ALPHABET.contains(&byte)) {
        return None;
    }
    let secret: [u8; SECRET_BYTES] = URL_SAFE_NO_PAD.decode(secret).ok()?.try_into().ok()?;
    Some(Presented {
        public_id: public_id.to_owned(),
        secret_sha256: Sha256::digest(secret).into(),
    })
}

/// The token of an `Authorization: Bearer <token>` header.
fn bearer(header: Option<&HeaderValue>) -> Option<&str> {
    let (scheme, token) = header?.to_str().ok()?.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
}

/// Compares two digests in constant time.
fn digests_match(stored: &[u8], presented: &[u8; 32]) -> bool {
    stored.ct_eq(presented.as_slice()).into()
}

/// The name a role has in the database and the contract.
pub fn role_name(role: Role) -> &'static str {
    match role {
        Role::Pm => "pm",
        Role::TechLead => "tech_lead",
        Role::Board => "board",
        Role::Admin => "admin",
    }
}

/// The role a name stands for.
pub fn parse_role(name: &str) -> Option<Role> {
    [Role::Pm, Role::TechLead, Role::Board, Role::Admin]
        .into_iter()
        .find(|role| role_name(*role) == name)
}

/// Whether a person with `role` may approve held answers: only a project manager or a tech lead.
/// An approver flag on anyone else, left from before that rule, grants nothing where it is used.
/// The approvals' queries say the same in SQL, as `role IN ('pm', 'tech_lead')`.
pub fn may_approve(role: Role) -> bool {
    matches!(role, Role::Pm | Role::TechLead)
}

/// A project the caller may ask about, or `portfolio`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub project: String,
    /// The approver flag: in effect on a [`Caller`], as stored in the admin commands, which show a
    /// flag that grants nothing too, so that it can be taken back.
    pub approver: bool,
}

/// The token a request came with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenRef {
    pub public_id: String,
    pub expires_at: DateTime<Utc>,
}

/// How a request proved who sent it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    /// `Authorization: Bearer <token>`: the CLI, a script, an agent.
    Bearer,
    /// The web client's session cookie.
    Session,
}

/// Where a question was asked from: its `questions.channel`, the schema's one type for it.
pub use mercury_schema::Channel;

/// Who sent a request.
#[derive(Clone, PartialEq, Eq)]
pub struct Caller {
    pub user_id: String,
    pub email: String,
    pub display_name: String,
    pub role: Role,
    pub projects: Vec<Grant>,
    pub token: TokenRef,
    pub via: Via,
}

impl Caller {
    /// The channel of what this caller asks through `POST /v1/questions`.
    pub fn channel(&self) -> Channel {
        match self.via {
            Via::Bearer => Channel::Cli,
            Via::Session => Channel::Web,
        }
    }
}

/// Leaves out the e-mail address and the name, so a stray `{:?}` logs neither.
impl fmt::Debug for Caller {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Caller")
            .field("user_id", &self.user_id)
            .field("role", &self.role)
            .field("projects", &self.projects)
            .field("token", &self.token)
            .field("via", &self.via)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// No token, a malformed or unknown one, a wrong secret, a revoked token or a disabled user.
    #[error("no valid token")]
    Unauthenticated,
    /// The right secret, past its expiry.
    #[error("the token has expired")]
    Expired,
    #[error("a stored token or user doesn't have the expected shape")]
    Corrupt,
    #[error("the database failed: {0}")]
    Db(#[from] sqlx::Error),
}

impl From<AuthError> for ApiError {
    fn from(err: AuthError) -> Self {
        match err {
            AuthError::Unauthenticated => ApiError::unauthenticated(),
            AuthError::Expired => ApiError::token_expired(),
            AuthError::Corrupt | AuthError::Db(_) => {
                tracing::error!(error = %err, "cannot check a token");
                ApiError::internal()
            }
        }
    }
}

/// The caller behind an `Authorization` header at `now`. The secret is checked before anything
/// else is revealed: only the right secret learns that its token expired.
pub async fn authenticate(
    db: &Db,
    header: Option<&HeaderValue>,
    now: DateTime<Utc>,
) -> Result<Caller, AuthError> {
    authenticate_token(db, bearer(header).ok_or(AuthError::Unauthenticated)?, now).await
}

/// The caller of a token as its text, which a Bearer header carries and sign-in is sent. Checked
/// like any request: the secret first, then revocation and a disabled user, then the expiry.
pub async fn authenticate_token(db: &Db, text: &str, now: DateTime<Utc>) -> Result<Caller, AuthError> {
    let presented = parse(text).ok_or(AuthError::Unauthenticated)?;
    let row = sqlx::query!(
        r#"SELECT t.secret_sha256, t.expires_at, t.revoked_at, t.last_used_at,
                  u.id AS user_id, u.email, u.display_name, u.role, u.disabled_at
           FROM tokens t JOIN users u ON u.id = t.user_id
           WHERE t.public_id = ?"#,
        presented.public_id
    )
    .fetch_optional(db.pool())
    .await?
    .ok_or(AuthError::Unauthenticated)?;
    if !digests_match(&row.secret_sha256, &presented.secret_sha256) {
        return Err(AuthError::Unauthenticated);
    }
    if row.revoked_at.is_some() || row.disabled_at.is_some() {
        return Err(AuthError::Unauthenticated);
    }
    let expires_at = db::parse_time(&row.expires_at).ok_or(AuthError::Corrupt)?;
    if now >= expires_at {
        return Err(AuthError::Expired);
    }
    touch_token(db, &presented.public_id, row.last_used_at.as_deref(), now).await?;
    caller_of(
        db,
        row.user_id,
        row.email,
        row.display_name,
        &row.role,
        TokenRef {
            public_id: presented.public_id,
            expires_at,
        },
        Via::Bearer,
    )
    .await
}

/// The caller behind a session: the SHA-256 of the 32 bytes of the id its cookie carries. A session
/// that is unknown, revoked or expired, or whose token is revoked or expired or whose person is
/// disabled, is the same `Unauthenticated` as a wrong token: the web client goes back to Sign in.
pub async fn authenticate_session(
    db: &Db,
    id_sha256: &[u8; 32],
    now: DateTime<Utc>,
) -> Result<Caller, AuthError> {
    let id = id_sha256.as_slice();
    let row = sqlx::query!(
        r#"SELECT s.token_public_id, s.expires_at AS session_expires_at, s.revoked_at AS session_revoked_at,
                  s.last_seen_at, t.expires_at AS token_expires_at, t.revoked_at AS token_revoked_at,
                  t.last_used_at, u.id AS user_id, u.email, u.display_name, u.role, u.disabled_at
           FROM sessions s
           JOIN tokens t ON t.public_id = s.token_public_id
           JOIN users u ON u.id = s.user_id
           WHERE s.id_sha256 = ?"#,
        id
    )
    .fetch_optional(db.pool())
    .await?
    .ok_or(AuthError::Unauthenticated)?;
    if row.session_revoked_at.is_some() || row.token_revoked_at.is_some() || row.disabled_at.is_some() {
        return Err(AuthError::Unauthenticated);
    }
    let session_expires_at = db::parse_time(&row.session_expires_at).ok_or(AuthError::Corrupt)?;
    let token_expires_at = db::parse_time(&row.token_expires_at).ok_or(AuthError::Corrupt)?;
    if now >= session_expires_at || now >= token_expires_at {
        return Err(AuthError::Unauthenticated);
    }
    let seen = row.last_seen_at.as_deref().and_then(db::parse_time);
    if seen.is_none_or(|at| now - at >= LAST_USED_EVERY) {
        let seen_at = db::time(now);
        let stale_before = db::time(now - LAST_USED_EVERY);
        sqlx::query!(
            "UPDATE sessions SET last_seen_at = ?
             WHERE id_sha256 = ? AND (last_seen_at IS NULL OR last_seen_at <= ?)",
            seen_at,
            id,
            stale_before
        )
        .execute(db.pool())
        .await?;
    }
    // the token's own last use moves with its sessions', so `token list` shows the web client's too
    touch_token(db, &row.token_public_id, row.last_used_at.as_deref(), now).await?;
    caller_of(
        db,
        row.user_id,
        row.email,
        row.display_name,
        &row.role,
        TokenRef {
            public_id: row.token_public_id,
            expires_at: token_expires_at,
        },
        Via::Session,
    )
    .await
}

/// `tokens.last_used_at`, written at most once a minute.
async fn touch_token(
    db: &Db,
    public_id: &str,
    last_used_at: Option<&str>,
    now: DateTime<Utc>,
) -> Result<(), AuthError> {
    let last_used = last_used_at.and_then(db::parse_time);
    if last_used.is_none_or(|at| now - at >= LAST_USED_EVERY) {
        let used_at = db::time(now);
        let stale_before = db::time(now - LAST_USED_EVERY);
        sqlx::query!(
            "UPDATE tokens SET last_used_at = ?
             WHERE public_id = ? AND (last_used_at IS NULL OR last_used_at <= ?)",
            used_at,
            public_id,
            stale_before
        )
        .execute(db.pool())
        .await?;
    }
    Ok(())
}

/// The caller a checked token or session stands for, with their grants. A grant's approver flag is
/// the one in effect: a stored flag counts only for a role that [`may_approve`].
async fn caller_of(
    db: &Db,
    user_id: String,
    email: String,
    display_name: String,
    role: &str,
    token: TokenRef,
    via: Via,
) -> Result<Caller, AuthError> {
    let role = parse_role(role).ok_or(AuthError::Corrupt)?;
    let approves = may_approve(role);
    let projects = sqlx::query!(
        "SELECT project, approver FROM user_projects WHERE user_id = ? ORDER BY project",
        user_id
    )
    .fetch_all(db.pool())
    .await?
    .into_iter()
    .map(|grant| Grant {
        project: grant.project,
        approver: approves && grant.approver != 0,
    })
    .collect();
    Ok(Caller {
        user_id,
        email,
        display_name,
        role,
        projects,
        token,
        via,
    })
}

/// The public id a token text claims, when it has the issued shape: for the audit of a refused
/// sign-in, which can say which token was tried. Nothing of the secret.
pub fn claimed_public_id(text: &str) -> Option<String> {
    parse(text).map(|presented| presented.public_id)
}

/// The caller of a route, from its `Authorization` header. A layer that already authenticated
/// the request (the rate limit) leaves the caller in the request's extensions, and it is taken
/// from there.
impl<S> FromRequestParts<S> for Caller
where
    Db: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        if let Some(caller) = parts.extensions.get::<Caller>() {
            return Ok(caller.clone());
        }
        let db = Db::from_ref(state);
        Ok(authenticate(&db, parts.headers.get(AUTHORIZATION), db::now()).await?)
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use chrono::{DateTime, Utc};

    use super::NewToken;
    use crate::db::{self, Db};

    /// Adds a person with `email`, granted `delta` (approver) and `gamma`, and a token that
    /// expires at `expires_at`. Returns the token's text.
    pub(crate) async fn issue(db: &Db, email: &str, expires_at: DateTime<Utc>) -> String {
        let token = NewToken::generate().unwrap();
        let user_id = format!("u_{}", token.public_id());
        let created_at = db::time(expires_at - chrono::TimeDelta::days(90));
        sqlx::query(
            "INSERT INTO users (id, email, display_name, role, created_at) VALUES (?, ?, 'Pat', 'pm', ?)",
        )
        .bind(&user_id)
        .bind(email)
        .bind(&created_at)
        .execute(db.pool())
        .await
        .unwrap();
        for (project, approver) in [("delta", 1), ("gamma", 0)] {
            sqlx::query("INSERT INTO user_projects (user_id, project, approver) VALUES (?, ?, ?)")
                .bind(&user_id)
                .bind(project)
                .bind(approver)
                .execute(db.pool())
                .await
                .unwrap();
        }
        sqlx::query(
            "INSERT INTO tokens (public_id, user_id, secret_sha256, label, created_at, expires_at) \
             VALUES (?, ?, ?, 'laptop', ?, ?)",
        )
        .bind(token.public_id())
        .bind(&user_id)
        .bind(token.secret_sha256().as_slice())
        .bind(&created_at)
        .bind(db::time(expires_at))
        .execute(db.pool())
        .await
        .unwrap();
        token.reveal()
    }
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::body::Body;
    use axum::http::header::WWW_AUTHENTICATE;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;
    use crate::db::testing;

    fn at(text: &str) -> DateTime<Utc> {
        db::parse_time(text).unwrap()
    }

    fn header(token: &str) -> HeaderValue {
        HeaderValue::from_str(&format!("Bearer {token}")).unwrap()
    }

    const NOW: &str = "2026-10-02T10:00:00.000Z";
    const LATER: &str = "2026-12-31T10:00:00.000Z";

    #[test]
    fn a_new_token_has_the_issued_shape_and_parses_back_to_its_digest() {
        let token = NewToken::generate().unwrap();
        let text = token.reveal();
        assert!(text.starts_with("mcy_"));
        assert_eq!(text.len(), 4 + 12 + 1 + 43);
        assert!(token.public_id().bytes().all(|byte| byte.is_ascii_alphanumeric()));
        let presented = parse(&text).unwrap();
        assert_eq!(presented.public_id, token.public_id());
        assert_eq!(presented.secret_sha256, token.secret_sha256());
    }

    #[test]
    fn two_tokens_share_neither_id_nor_secret() {
        let (one, two) = (NewToken::generate().unwrap(), NewToken::generate().unwrap());
        assert_ne!(one.public_id(), two.public_id());
        assert_ne!(one.secret_sha256(), two.secret_sha256());
    }

    /// `mcy_` and the public id, and the secret after the `_` that follows them. Split by
    /// position: base64url secrets contain `_` too.
    fn split(token: &str) -> (&str, &str) {
        (&token[..16], &token[17..])
    }

    #[test]
    fn the_debug_form_never_shows_the_secret() {
        let token = NewToken::generate().unwrap();
        let text = token.reveal();
        let (_, secret) = split(&text);
        let debug = format!("{token:?}");
        assert!(!debug.contains(secret), "{debug}");
        assert!(debug.contains(token.public_id()));
    }

    #[test]
    fn anything_but_the_issued_shape_is_refused() {
        let good = NewToken::generate().unwrap().reveal();
        let (head, secret) = split(&good);
        let cases = [
            String::new(),
            good.replacen("mcy_", "mcx_", 1),
            good.replacen("mcy_", "", 1),
            format!("{good}A"),
            good[..good.len() - 1].to_owned(),
            format!("{head}-{secret}"),
            format!("mcy_k3Xq9AbCdE-G_{secret}"),
            format!("mcy_k3Xq9AbCdEfé_{secret}"),
            format!("{head}_{}", "!".repeat(43)),
            format!("{head}_{}=", &secret[..42]),
            // 43 characters whose last one carries bits a 32-byte value can't have
            format!("{head}_{}B", &secret[..42]),
        ];
        for case in cases {
            assert!(parse(&case).is_none(), "{case:?}");
        }
    }

    #[test]
    fn only_a_bearer_header_carries_a_token() {
        let value = |text: &str| HeaderValue::from_str(text).unwrap();
        assert_eq!(bearer(Some(&value("Bearer mcy_x"))), Some("mcy_x"));
        assert_eq!(bearer(Some(&value("bearer  mcy_x "))), Some("mcy_x"));
        assert_eq!(bearer(Some(&value("Basic bWN5X3g="))), None);
        assert_eq!(bearer(Some(&value("Bearer"))), None);
        assert_eq!(bearer(None), None);
    }

    #[test]
    fn digests_match_only_when_equal() {
        let digest = [7; 32];
        assert!(digests_match(&digest, &digest));
        let mut other = digest;
        other[31] = 8;
        assert!(!digests_match(&other, &digest));
        assert!(!digests_match(&digest[..31], &digest));
    }

    #[test]
    fn roles_round_trip_through_their_names() {
        for role in [Role::Pm, Role::TechLead, Role::Board, Role::Admin] {
            assert_eq!(parse_role(role_name(role)), Some(role));
        }
        assert_eq!(parse_role("Pm"), None);
        assert_eq!(parse_role("approver"), None);
    }

    #[tokio::test]
    async fn the_right_token_gives_the_caller_with_role_projects_and_token() {
        let (_dir, db) = testing::db().await;
        let token = testing_issue(&db, "pm@example.com", LATER).await;
        let caller = authenticate(&db, Some(&header(&token)), at(NOW)).await.unwrap();
        assert_eq!(caller.email, "pm@example.com");
        assert_eq!(caller.role, Role::Pm);
        assert_eq!(
            caller.projects,
            [
                Grant {
                    project: "delta".into(),
                    approver: true
                },
                Grant {
                    project: "gamma".into(),
                    approver: false
                }
            ]
        );
        assert_eq!(caller.token.public_id, &token[4..16]);
        assert_eq!(caller.token.expires_at, at(LATER));
        let debug = format!("{caller:?}");
        assert!(
            !debug.contains("pm@example.com") && !debug.contains("Pat"),
            "{debug}"
        );
    }

    async fn testing_issue(db: &Db, email: &str, expires_at: &str) -> String {
        super::testing::issue(db, email, at(expires_at)).await
    }

    /// An approver flag counts only for a project manager or a tech lead: one left on a board or
    /// an admin person, as stored before that rule, is no flag on their caller.
    #[tokio::test]
    async fn only_a_project_manager_or_a_tech_lead_is_an_approver() {
        let (_dir, db) = testing::db().await;
        let token = testing_issue(&db, "pat@example.com", LATER).await;
        for (role, approves) in [
            ("tech_lead", true),
            ("board", false),
            ("admin", false),
            ("pm", true),
        ] {
            sqlx::query("UPDATE users SET role = ?")
                .bind(role)
                .execute(db.pool())
                .await
                .unwrap();
            let caller = authenticate(&db, Some(&header(&token)), at(NOW)).await.unwrap();
            let flags: Vec<(&str, bool)> = caller
                .projects
                .iter()
                .map(|g| (g.project.as_str(), g.approver))
                .collect();
            assert_eq!(flags, [("delta", approves), ("gamma", false)], "{role}");
        }
    }

    #[tokio::test]
    async fn a_wrong_secret_or_an_unknown_token_is_unauthenticated() {
        let (_dir, db) = testing::db().await;
        let token = testing_issue(&db, "pm@example.com", LATER).await;
        let other_secret = NewToken::generate().unwrap().reveal();
        let wrong_secret = format!("{}{}", &token[..17], &other_secret[17..]);
        let unknown = NewToken::generate().unwrap().reveal();
        for presented in [wrong_secret, unknown, "mcy_short".to_owned()] {
            let result = authenticate(&db, Some(&header(&presented)), at(NOW)).await;
            assert!(matches!(result, Err(AuthError::Unauthenticated)), "{result:?}");
        }
        let result = authenticate(&db, None, at(NOW)).await;
        assert!(matches!(result, Err(AuthError::Unauthenticated)));
    }

    #[tokio::test]
    async fn a_token_expires_at_its_expiry_and_only_its_secret_learns_so() {
        let (_dir, db) = testing::db().await;
        let token = testing_issue(&db, "pm@example.com", NOW).await;
        let just_before = at(NOW) - TimeDelta::milliseconds(1);
        assert!(
            authenticate(&db, Some(&header(&token)), just_before)
                .await
                .is_ok()
        );
        let result = authenticate(&db, Some(&header(&token)), at(NOW)).await;
        assert!(matches!(result, Err(AuthError::Expired)), "{result:?}");

        let other_secret = NewToken::generate().unwrap().reveal();
        let wrong_secret = format!("{}{}", &token[..17], &other_secret[17..]);
        let result = authenticate(&db, Some(&header(&wrong_secret)), at(NOW)).await;
        assert!(matches!(result, Err(AuthError::Unauthenticated)), "{result:?}");
    }

    #[tokio::test]
    async fn a_revoked_token_is_unauthenticated_even_past_its_expiry() {
        let (_dir, db) = testing::db().await;
        let token = testing_issue(&db, "pm@example.com", LATER).await;
        sqlx::query("UPDATE tokens SET revoked_at = ?")
            .bind(NOW)
            .execute(db.pool())
            .await
            .unwrap();
        for when in [NOW, LATER, "2027-01-01T00:00:00.000Z"] {
            let result = authenticate(&db, Some(&header(&token)), at(when)).await;
            assert!(
                matches!(result, Err(AuthError::Unauthenticated)),
                "{when}: {result:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_disabled_users_token_is_unauthenticated() {
        let (_dir, db) = testing::db().await;
        let token = testing_issue(&db, "pm@example.com", LATER).await;
        sqlx::query("UPDATE users SET disabled_at = ?")
            .bind(NOW)
            .execute(db.pool())
            .await
            .unwrap();
        let result = authenticate(&db, Some(&header(&token)), at(NOW)).await;
        assert!(matches!(result, Err(AuthError::Unauthenticated)), "{result:?}");
    }

    async fn last_used(db: &Db) -> Option<String> {
        sqlx::query_scalar("SELECT last_used_at FROM tokens")
            .fetch_one(db.pool())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn last_used_at_moves_at_most_once_a_minute() {
        let (_dir, db) = testing::db().await;
        let token = testing_issue(&db, "pm@example.com", LATER).await;
        assert_eq!(last_used(&db).await, None);

        let first = at(NOW);
        authenticate(&db, Some(&header(&token)), first).await.unwrap();
        assert_eq!(last_used(&db).await.as_deref(), Some(NOW));

        let within = first + TimeDelta::seconds(59);
        authenticate(&db, Some(&header(&token)), within).await.unwrap();
        assert_eq!(
            last_used(&db).await.as_deref(),
            Some(NOW),
            "not again within the minute"
        );

        let a_minute_on = first + TimeDelta::minutes(1);
        authenticate(&db, Some(&header(&token)), a_minute_on)
            .await
            .unwrap();
        assert_eq!(last_used(&db).await, Some(db::time(a_minute_on)));
    }

    async fn whoami(caller: Caller) -> String {
        role_name(caller.role).to_owned()
    }

    async fn get_with(db: &Db, authorization: Option<String>) -> (StatusCode, String, Option<String>) {
        let app = Router::new().route("/whoami", get(whoami)).with_state(db.clone());
        let mut request = Request::builder().uri("/whoami");
        if let Some(value) = authorization {
            request = request.header(AUTHORIZATION, value);
        }
        let response = app.oneshot(request.body(Body::empty()).unwrap()).await.unwrap();
        let status = response.status();
        let challenge = response
            .headers()
            .get(WWW_AUTHENTICATE)
            .map(|value| value.to_str().unwrap().to_owned());
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(body.to_vec()).unwrap(), challenge)
    }

    #[tokio::test]
    async fn the_extractor_answers_with_the_contracts_error_body() {
        let (_dir, db) = testing::db().await;
        let token = testing_issue(&db, "pm@example.com", "2099-01-01T00:00:00.000Z").await;
        let expired = testing_issue(&db, "old@example.com", "2020-01-01T00:00:00.000Z").await;

        let (status, body, _) = get_with(&db, Some(format!("Bearer {token}"))).await;
        assert_eq!((status, body.as_str()), (StatusCode::OK, "pm"));

        for (authorization, code) in [
            (None, "unauthenticated"),
            (Some(format!("Bearer {token}x")), "unauthenticated"),
            (Some(format!("Basic {token}")), "unauthenticated"),
            (Some(format!("Bearer {expired}")), "token_expired"),
        ] {
            let (status, body, challenge) = get_with(&db, authorization.clone()).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{authorization:?}");
            let json: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert_eq!(json["error"]["code"], code, "{authorization:?}");
            assert_eq!(challenge.as_deref(), Some("Bearer"));
            assert!(!body.contains(&token) && !body.contains(&expired));
        }
    }
}
