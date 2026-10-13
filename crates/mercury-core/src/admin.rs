//! The admin commands: people, their roles, their grants, their tokens, and who has access to what.
//! They work on the database directly, and every change is made here: the web client's admin panel
//! only reads. Nothing here logs: what a command shows goes to its output, and only `token create`
//! ever shows a secret, once.

use std::collections::{BTreeSet, HashMap};
use std::fmt::Write as _;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, TimeDelta, Utc};
use mercury_schema::config::{PORTFOLIO, Policies, ProjectsConfig};
use mercury_schema::manifest::ProbeManifest;
use mercury_schema::{ProjectId, QuestionClass, Role};
use sqlx::SqliteExecutor;
use sqlx::error::ErrorKind;

use serde_json::json;

use crate::audit::{self, AuditError};
use crate::cli::{AccessCommand, Admin, AuditCommand, TokenCommand, UserCommand};
use crate::config;
use crate::db::{self, Db};
use crate::identity::{Grant, NewToken, PUBLIC_ID_LEN, may_approve, parse_role, role_name};
use crate::policy::{EffectivePolicy, class_name};
use crate::session;

/// Admin errors. None of them repeats an e-mail address or a token.
#[derive(Debug, thiserror::Error)]
pub enum AdminError {
    #[error("no person has that e-mail address")]
    NoSuchUser,
    #[error("a person with that e-mail address exists already")]
    EmailTaken,
    #[error("that person is disabled")]
    UserDisabled,
    #[error("no token has that public id")]
    NoSuchToken,
    #[error("{0}")]
    Invalid(String),
    #[error("a stored person or token doesn't have the expected shape")]
    Corrupt,
    #[error("cannot draw random bytes: {0}")]
    Random(#[from] getrandom::Error),
    #[error("the database failed: {0}")]
    Db(#[from] sqlx::Error),
    #[error("cannot write the output: {0}")]
    Output(#[from] std::io::Error),
    #[error("{0}")]
    Audit(#[from] AuditError),
    #[error("the audit chain is broken at row {0}")]
    ChainBroken(u64),
    #[error(
        "the disk is full: the audit row can't be written, so nothing is shown. Free space on the data volume and run it again"
    )]
    DiskFull,
    #[error("{0} alert(s) hold")]
    Alerts(usize),
    #[error("{0}")]
    Export(#[from] audit::ExportError),
    #[error("{} exists already: a backup never overwrites a file, so nothing was written", .0.display())]
    BackupExists(PathBuf),
    #[error("cannot write the backup {}: {source}", path.display())]
    Backup {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

fn invalid(message: impl Into<String>) -> AdminError {
    AdminError::Invalid(message.into())
}

fn is_unique_violation(err: &sqlx::Error) -> bool {
    matches!(err, sqlx::Error::Database(err) if err.kind() == ErrorKind::UniqueViolation)
}

/// An e-mail address, trimmed and in lower case.
fn email_address(text: &str) -> Result<String, AdminError> {
    let email = text.trim().to_lowercase();
    let well_formed = email.len() <= 254
        && !email.chars().any(|c| c.is_whitespace() || c.is_control())
        && email
            .split_once('@')
            .is_some_and(|(local, domain)| !local.is_empty() && !domain.is_empty() && !domain.contains('@'));
    if well_formed {
        Ok(email)
    } else {
        Err(invalid("that is not an e-mail address"))
    }
}

/// A short line of text for a name or a label: trimmed, `1..=max` characters, no control characters.
fn line(text: &str, what: &str, max: usize) -> Result<String, AdminError> {
    let text = text.trim();
    let length = text.chars().count();
    if length == 0 || length > max || text.chars().any(char::is_control) {
        return Err(invalid(format!(
            "the {what} must be 1 to {max} characters on one line"
        )));
    }
    Ok(text.to_owned())
}

fn project_id(text: &str) -> Result<String, AdminError> {
    ProjectId::parse(text.trim())
        .map(|id| id.as_str().to_owned())
        .map_err(|_| invalid("a project id is lower-case letters, digits and '-', like delta"))
}

/// The projects a grant may name: those `projects.yaml` lists, and `portfolio`, which the board role
/// is granted and which has no entry there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownProjects(BTreeSet<String>);

impl KnownProjects {
    pub fn new<'a>(ids: impl IntoIterator<Item = &'a str>) -> Self {
        let mut known: BTreeSet<String> = ids.into_iter().map(str::to_owned).collect();
        known.insert(PORTFOLIO.to_owned());
        Self(known)
    }

    /// The projects in `dir`'s `projects.yaml`. Only that file is read, so a slip elsewhere in the
    /// configuration doesn't stop a grant; a file that can't be read does, since nothing can then be
    /// checked, and a grant is never given unchecked.
    pub fn read(dir: &Path) -> Result<Self, AdminError> {
        let ids = config::project_ids(dir).map_err(|problems| {
            let lines: Vec<String> = problems.iter().map(ToString::to_string).collect();
            invalid(format!(
                "cannot tell which projects exist, so nothing is granted: {}",
                lines.join("; ")
            ))
        })?;
        Ok(Self::new(ids.iter().map(ProjectId::as_str)))
    }

    /// Refuses the projects this doesn't know, naming the ones it does.
    fn check(&self, projects: &BTreeSet<String>) -> Result<(), AdminError> {
        let unknown: Vec<&str> = projects
            .iter()
            .filter(|project| !self.0.contains(*project))
            .map(String::as_str)
            .collect();
        if unknown.is_empty() {
            return Ok(());
        }
        let known: Vec<&str> = self.0.iter().map(String::as_str).collect();
        Err(invalid(format!(
            "projects.yaml doesn't list {}; the known projects are {}",
            unknown.join(", "),
            known.join(", ")
        )))
    }
}

fn public_id(text: &str) -> Result<&str, AdminError> {
    let text = text.trim();
    if text.len() == PUBLIC_ID_LEN && text.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        Ok(text)
    } else {
        Err(invalid(
            "a public id is 12 letters and digits, as `token list` shows it",
        ))
    }
}

/// A ULID: 48 bits of milliseconds since the epoch, then 80 random bits, in Crockford's base 32.
pub(crate) fn encode_ulid(millis: u64, random: [u8; 10]) -> String {
    const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut value = u128::from(millis & ((1 << 48) - 1)) << 80;
    for (i, byte) in random.iter().enumerate() {
        value |= u128::from(*byte) << (8 * (9 - i));
    }
    (0..26)
        .rev()
        .map(|digit| char::from(CROCKFORD[usize::from((value >> (5 * digit)) as u8 & 31)]))
        .collect()
}

fn new_user_id(now: DateTime<Utc>) -> Result<String, AdminError> {
    let mut random = [0; 10];
    getrandom::fill(&mut random)?;
    let millis = u64::try_from(now.timestamp_millis()).unwrap_or(0);
    Ok(format!("u_{}", encode_ulid(millis, random)))
}

struct UserRow {
    id: String,
    disabled_at: Option<String>,
}

async fn find_user<'e>(conn: impl SqliteExecutor<'e>, email: &str) -> Result<UserRow, AdminError> {
    let row = sqlx::query!("SELECT id, disabled_at FROM users WHERE email = ?", email)
        .fetch_optional(conn)
        .await?
        .ok_or(AdminError::NoSuchUser)?;
    Ok(UserRow {
        id: row.id,
        disabled_at: row.disabled_at,
    })
}

async fn find_active_user<'e>(conn: impl SqliteExecutor<'e>, email: &str) -> Result<UserRow, AdminError> {
    let user = find_user(conn, email).await?;
    match user.disabled_at {
        Some(_) => Err(AdminError::UserDisabled),
        None => Ok(user),
    }
}

/// `user add`. Returns the new person's id. The person and the audit row are one transaction.
pub async fn add_user(
    db: &Db,
    email: &str,
    name: &str,
    role: Role,
    now: DateTime<Utc>,
) -> Result<String, AdminError> {
    let email = email_address(email)?;
    let name = line(name, "name", 100)?;
    let id = new_user_id(now)?;
    let role = role_name(role);
    let created_at = db::time(now);
    let mut tx = db.pool().begin_with("BEGIN IMMEDIATE").await?;
    sqlx::query!(
        "INSERT INTO users (id, email, display_name, role, created_at) VALUES (?, ?, ?, ?, ?)",
        id,
        email,
        name,
        role,
        created_at
    )
    .execute(&mut *tx)
    .await
    .map_err(|err| {
        if is_unique_violation(&err) {
            AdminError::EmailTaken
        } else {
            err.into()
        }
    })?;
    let payload = json!({ "user": id, "email": email, "role": role });
    audit::action_in(&mut tx, "user_added", None, &payload, now).await?;
    tx.commit().await?;
    Ok(id)
}

/// `user grant`: the projects, each of which `known` must list. Returns the person's projects after
/// the grant. The grants and the audit row are one transaction. The row names, for each project of
/// this call, the approver flag before (`null` for a new grant) and after, so a plain grant that
/// takes a flag away shows it.
pub async fn grant(
    db: &Db,
    email: &str,
    projects: &[String],
    approver: bool,
    known: &KnownProjects,
    now: DateTime<Utc>,
) -> Result<Vec<Grant>, AdminError> {
    let email = email_address(email)?;
    let projects: BTreeSet<String> = projects
        .iter()
        .filter(|project| !project.trim().is_empty())
        .map(|project| project_id(project))
        .collect::<Result<_, _>>()?;
    if projects.is_empty() {
        return Err(invalid("name at least one project"));
    }
    known.check(&projects)?;
    let mut tx = db.pool().begin_with("BEGIN IMMEDIATE").await?;
    let user = find_active_user(&mut *tx, &email).await?;
    if approver {
        // a board approver would read a manager's item-level draft past `aggregates_only`, and an
        // admin one would release held answers on a grant they gave themselves
        let role = sqlx::query_scalar!("SELECT role FROM users WHERE id = ?", user.id)
            .fetch_one(&mut *tx)
            .await?;
        if !parse_role(&role).is_some_and(may_approve) {
            return Err(invalid(
                "an approver is a project manager or a tech lead; `user set-role` first",
            ));
        }
    }
    let before: HashMap<String, bool> = grants_of(&mut *tx, &user.id)
        .await?
        .into_iter()
        .map(|grant| (grant.project, grant.approver))
        .collect();
    let flag = i64::from(approver);
    for project in &projects {
        sqlx::query!(
            "INSERT INTO user_projects (user_id, project, approver) VALUES (?, ?, ?)
             ON CONFLICT (user_id, project) DO UPDATE SET approver = excluded.approver",
            user.id,
            project,
            flag
        )
        .execute(&mut *tx)
        .await?;
    }
    let grants = grants_of(&mut *tx, &user.id).await?;
    let granted: Vec<&str> = grants.iter().map(|g| g.project.as_str()).collect();
    // what this call did to sign-off authority, project by project
    let flags: Vec<serde_json::Value> = projects
        .iter()
        .map(|project| json!({ "project": project, "before": before.get(project), "after": approver }))
        .collect();
    let payload = json!({
        "email": email,
        "projects": granted,
        "approver": approver,
        "approver_flags": flags,
    });
    audit::action_in(&mut tx, "user_granted", None, &payload, now).await?;
    tx.commit().await?;
    Ok(grants)
}

async fn grants_of<'e>(conn: impl SqliteExecutor<'e>, user_id: &str) -> Result<Vec<Grant>, AdminError> {
    Ok(sqlx::query!(
        "SELECT project, approver FROM user_projects WHERE user_id = ? ORDER BY project",
        user_id
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(|grant| Grant {
        project: grant.project,
        approver: grant.approver != 0,
    })
    .collect())
}

/// What `user set-role` did.
#[derive(Debug, PartialEq, Eq)]
pub enum RoleSet {
    /// Changed now, from this role.
    Now { previous: Role },
    /// The person had that role already: nothing changed.
    Already,
}

/// `user set-role`: the person's role, which every request reads, so it holds from their next one.
/// The role they have already changes nothing and says so; either way it is one audit row, as for
/// `token revoke`. The change and the row are one transaction.
pub async fn set_role(db: &Db, email: &str, role: Role, now: DateTime<Utc>) -> Result<RoleSet, AdminError> {
    let email = email_address(email)?;
    let name = role_name(role);
    let mut tx = db.pool().begin_with("BEGIN IMMEDIATE").await?;
    let user = find_active_user(&mut *tx, &email).await?;
    let stored = sqlx::query_scalar!("SELECT role FROM users WHERE id = ?", user.id)
        .fetch_one(&mut *tx)
        .await?;
    let previous = parse_role(&stored).ok_or(AdminError::Corrupt)?;
    if !may_approve(role) {
        // an approver is a project manager or a tech lead, as `grant --approver` keeps it
        let approves = sqlx::query_scalar!(
            r#"SELECT count(*) AS "n!: i64" FROM user_projects WHERE user_id = ? AND approver = 1"#,
            user.id
        )
        .fetch_one(&mut *tx)
        .await?;
        if approves > 0 {
            return Err(invalid(
                "an approver is a project manager or a tech lead; grant the projects again without `--approver` first",
            ));
        }
    }
    let set = if previous == role {
        RoleSet::Already
    } else {
        sqlx::query!("UPDATE users SET role = ? WHERE id = ?", name, user.id)
            .execute(&mut *tx)
            .await?;
        RoleSet::Now { previous }
    };
    let payload = json!({ "email": email, "role": name, "previous": role_name(previous) });
    audit::action_in(&mut tx, "user_role_set", None, &payload, now).await?;
    tx.commit().await?;
    Ok(set)
}

/// What `user ungrant` did.
#[derive(Debug, PartialEq, Eq)]
pub struct Ungranted {
    /// The projects taken back, with their approver flags.
    pub removed: Vec<String>,
    /// The projects named that the person wasn't granted: nothing changed for them.
    pub not_granted: Vec<String>,
    /// The person's grants after.
    pub remaining: Vec<Grant>,
}

/// `user ungrant`: takes the projects back, approver flags with them. A project the person wasn't
/// granted is not an error, and nothing changes for it; `projects.yaml` isn't read, so a project
/// it no longer lists can still be taken back. The change and the audit row are one transaction,
/// and the row is written whatever was taken back, as for `token revoke`. The row names the grants
/// taken back that carried the approver flag.
pub async fn ungrant(
    db: &Db,
    email: &str,
    projects: &[String],
    now: DateTime<Utc>,
) -> Result<Ungranted, AdminError> {
    let email = email_address(email)?;
    let projects: BTreeSet<String> = projects
        .iter()
        .filter(|project| !project.trim().is_empty())
        .map(|project| project_id(project))
        .collect::<Result<_, _>>()?;
    if projects.is_empty() {
        return Err(invalid("name at least one project"));
    }
    let mut tx = db.pool().begin_with("BEGIN IMMEDIATE").await?;
    let user = find_active_user(&mut *tx, &email).await?;
    let approved: BTreeSet<String> = grants_of(&mut *tx, &user.id)
        .await?
        .into_iter()
        .filter(|grant| grant.approver)
        .map(|grant| grant.project)
        .collect();
    let (mut removed, mut not_granted) = (Vec::new(), Vec::new());
    for project in projects {
        let deleted = sqlx::query!(
            "DELETE FROM user_projects WHERE user_id = ? AND project = ?",
            user.id,
            project
        )
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if deleted == 0 {
            not_granted.push(project);
        } else {
            removed.push(project);
        }
    }
    let remaining = grants_of(&mut *tx, &user.id).await?;
    let left: Vec<&str> = remaining.iter().map(|g| g.project.as_str()).collect();
    let flags_removed: Vec<&String> = removed.iter().filter(|p| approved.contains(*p)).collect();
    let payload = json!({
        "email": email,
        "projects": removed,
        "remaining": left,
        "approver_flags_removed": flags_removed,
    });
    audit::action_in(&mut tx, "user_ungranted", None, &payload, now).await?;
    tx.commit().await?;
    Ok(Ungranted {
        removed,
        not_granted,
        remaining,
    })
}

/// What `user disable` did.
#[derive(Debug, PartialEq, Eq)]
pub enum Disabled {
    /// Disabled now, with this many tokens revoked in the same step.
    Now { tokens_revoked: u64 },
    /// Disabled before, at this time.
    Already { since: String },
}

/// `user disable`: the person, every token they hold and the audit row, in one transaction.
pub async fn disable_user(db: &Db, email: &str, now: DateTime<Utc>) -> Result<Disabled, AdminError> {
    let email = email_address(email)?;
    let at = db::time(now);
    let mut tx = db.pool().begin_with("BEGIN IMMEDIATE").await?;
    let user = sqlx::query!("SELECT id, disabled_at FROM users WHERE email = ?", email)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(AdminError::NoSuchUser)?;
    let mut sessions_ended = 0;
    let disabled = match user.disabled_at {
        Some(since) => Disabled::Already { since },
        None => {
            sqlx::query!("UPDATE users SET disabled_at = ? WHERE id = ?", at, user.id)
                .execute(&mut *tx)
                .await?;
            let tokens_revoked = sqlx::query!(
                "UPDATE tokens SET revoked_at = ? WHERE user_id = ? AND revoked_at IS NULL",
                at,
                user.id
            )
            .execute(&mut *tx)
            .await?
            .rows_affected();
            // their web sessions end with them, in the same step
            sessions_ended = session::end_for_user(&mut tx, &user.id, now).await?;
            Disabled::Now { tokens_revoked }
        }
    };
    let payload = match &disabled {
        Disabled::Now { tokens_revoked } => {
            json!({ "email": email, "tokens_revoked": tokens_revoked, "sessions_ended": sessions_ended })
        }
        Disabled::Already { .. } => json!({ "email": email, "already": true }),
    };
    audit::action_in(&mut tx, "user_disabled", None, &payload, now).await?;
    tx.commit().await?;
    Ok(disabled)
}

/// A token issued by `token create`.
#[derive(Debug)]
pub struct Created {
    pub token: NewToken,
    pub expires_at: DateTime<Utc>,
}

/// `token create`. The token's row and its audit row are one transaction, so a token is never
/// issued without its trail.
pub async fn create_token(
    db: &Db,
    email: &str,
    label: &str,
    valid_for: TimeDelta,
    now: DateTime<Utc>,
) -> Result<Created, AdminError> {
    let email = email_address(email)?;
    let label = line(label, "label", 64)?;
    if valid_for <= TimeDelta::zero() {
        return Err(invalid("a token must be valid for some time"));
    }
    let expires_at = now
        .checked_add_signed(valid_for)
        .ok_or_else(|| invalid("that expiry is too far ahead"))?;
    let (created_at, expires) = (db::time(now), db::time(expires_at));
    let mut tx = db.pool().begin_with("BEGIN IMMEDIATE").await?;
    let user = find_active_user(&mut *tx, &email).await?;
    // a clash of two public ids is next to impossible, but a second draw costs nothing
    for attempt in 1..=3 {
        let token = NewToken::generate()?;
        let public_id = token.public_id().to_owned();
        let digest = token.secret_sha256().to_vec();
        let inserted = sqlx::query!(
            "INSERT INTO tokens (public_id, user_id, secret_sha256, label, created_at, expires_at)
             VALUES (?, ?, ?, ?, ?, ?)",
            public_id,
            user.id,
            digest,
            label,
            created_at,
            expires
        )
        .execute(&mut *tx)
        .await;
        match inserted {
            Ok(_) => {
                let payload = json!({
                    "public_id": public_id,
                    "email": email,
                    "label": label,
                    "expires_at": expires,
                });
                audit::action_in(&mut tx, "token_created", None, &payload, now).await?;
                tx.commit().await?;
                return Ok(Created { token, expires_at });
            }
            Err(err) if is_unique_violation(&err) && attempt < 3 => {}
            Err(err) => return Err(err.into()),
        }
    }
    Err(AdminError::Corrupt)
}

/// A token as `token list` and `access list` show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenLine {
    pub public_id: String,
    pub email: String,
    pub label: String,
    pub created_at: String,
    pub expires_at: String,
    pub revoked_at: Option<String>,
    pub last_used_at: Option<String>,
}

async fn all_tokens(db: &Db) -> Result<Vec<TokenLine>, AdminError> {
    Ok(sqlx::query!(
        "SELECT t.public_id, u.email, t.label, t.created_at, t.expires_at, t.revoked_at, t.last_used_at
         FROM tokens t JOIN users u ON u.id = t.user_id
         ORDER BY u.email, t.created_at, t.public_id"
    )
    .fetch_all(db.pool())
    .await?
    .into_iter()
    .map(|row| TokenLine {
        public_id: row.public_id,
        email: row.email,
        label: row.label,
        created_at: row.created_at,
        expires_at: row.expires_at,
        revoked_at: row.revoked_at,
        last_used_at: row.last_used_at,
    })
    .collect())
}

/// `token list`.
pub async fn list_tokens(db: &Db, email: Option<&str>) -> Result<Vec<TokenLine>, AdminError> {
    let email = email.map(email_address).transpose()?;
    if let Some(email) = &email {
        find_user(db.pool(), email).await?;
    }
    let mut tokens = all_tokens(db).await?;
    tokens.retain(|token| email.as_ref().is_none_or(|email| &token.email == email));
    Ok(tokens)
}

/// What `token revoke` did.
#[derive(Debug, PartialEq, Eq)]
pub enum Revoked {
    Now,
    Already { since: String },
}

/// `token revoke`. The revocation and the audit row are one transaction.
pub async fn revoke_token(db: &Db, public_id: &str, now: DateTime<Utc>) -> Result<Revoked, AdminError> {
    let public_id = self::public_id(public_id)?;
    let at = db::time(now);
    let mut tx = db.pool().begin_with("BEGIN IMMEDIATE").await?;
    let changed = sqlx::query!(
        "UPDATE tokens SET revoked_at = ? WHERE public_id = ? AND revoked_at IS NULL",
        at,
        public_id
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    let revoked = if changed == 1 {
        Revoked::Now
    } else {
        let row = sqlx::query!("SELECT revoked_at FROM tokens WHERE public_id = ?", public_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(AdminError::NoSuchToken)?;
        Revoked::Already {
            since: row.revoked_at.ok_or(AdminError::Corrupt)?,
        }
    };
    // the token's web sessions end with it, in the same step, whether or not it was revoked before
    let sessions_ended = session::end_for_token(&mut tx, public_id, now).await?;
    let payload = json!({
        "public_id": public_id,
        "already": revoked != Revoked::Now,
        "sessions_ended": sessions_ended,
    });
    audit::action_in(&mut tx, "token_revoked", None, &payload, now).await?;
    tx.commit().await?;
    Ok(revoked)
}

/// A person as `access list` shows them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    pub email: String,
    pub display_name: String,
    pub role: Role,
    pub disabled_at: Option<String>,
    pub grants: Vec<Grant>,
}

/// A project's probe as the core last saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeLine {
    pub status: String,
    pub access: String,
    pub access_limited_by: Option<String>,
    pub last_seen_at: String,
    pub quarantine_reason: Option<String>,
}

/// What one grant lets a person ask: the policy's classes for their role and the project's manifest,
/// each marked when it is held for sign-off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Classes {
    Allowed(Vec<(QuestionClass, bool)>),
    /// The project has no manifest yet, from a probe or from `projects.yaml`: it can't be asked.
    NoManifest,
}

/// One grant and what it lets its person ask.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Effective {
    pub email: String,
    pub project: String,
    pub classes: Classes,
}

/// What `access list` shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Access {
    /// With `--project`: the project, and its probe if one has registered.
    pub project: Option<(String, Option<ProbeLine>)>,
    pub people: Vec<Person>,
    pub tokens: Vec<TokenLine>,
    /// What each grant lets its person ask; empty until [`effective_classes`] fills it.
    pub effective: Vec<Effective>,
    /// Why `effective` is empty when it can't be known: the files it reads are unreadable.
    pub effective_note: Option<String>,
}

/// `access list`: people with their grants and tokens; with `--project` only the people granted
/// that project, and only that grant; with `--email` only that person.
pub async fn access(db: &Db, project: Option<&str>, email: Option<&str>) -> Result<Access, AdminError> {
    let project = project.map(project_id).transpose()?;
    let email = email.map(email_address).transpose()?;
    if let Some(email) = &email {
        find_user(db.pool(), email).await?;
    }

    let mut grants: HashMap<String, Vec<Grant>> = HashMap::new();
    for row in sqlx::query!("SELECT user_id, project, approver FROM user_projects ORDER BY project")
        .fetch_all(db.pool())
        .await?
    {
        if project.as_ref().is_none_or(|project| &row.project == project) {
            grants.entry(row.user_id).or_default().push(Grant {
                project: row.project,
                approver: row.approver != 0,
            });
        }
    }

    let mut people = Vec::new();
    for row in sqlx::query!("SELECT id, email, display_name, role, disabled_at FROM users ORDER BY email")
        .fetch_all(db.pool())
        .await?
    {
        let grants = grants.remove(&row.id).unwrap_or_default();
        let wanted = email.as_ref().is_none_or(|email| &row.email == email)
            && (project.is_none() || !grants.is_empty());
        if wanted {
            people.push(Person {
                role: parse_role(&row.role).ok_or(AdminError::Corrupt)?,
                email: row.email,
                display_name: row.display_name,
                disabled_at: row.disabled_at,
                grants,
            });
        }
    }

    let mut tokens = all_tokens(db).await?;
    tokens.retain(|token| people.iter().any(|person| person.email == token.email));

    let project = match project {
        Some(project) => {
            let probe = sqlx::query!(
                "SELECT status, access, access_limited_by, last_seen_at, quarantine_reason
                 FROM probes WHERE project = ?",
                project
            )
            .fetch_optional(db.pool())
            .await?
            .map(|row| ProbeLine {
                status: row.status,
                access: row.access,
                access_limited_by: row.access_limited_by,
                last_seen_at: row.last_seen_at,
                quarantine_reason: row.quarantine_reason,
            });
            Some((project, probe))
        }
        None => None,
    };
    Ok(Access {
        project,
        people,
        tokens,
        effective: Vec::new(),
        effective_note: None,
    })
}

/// Fills in what each grant of `access` lets its person ask: the classes of their role in
/// `policies.yaml`, the project's manifest (the probe's, combined with the copy in `projects.yaml`,
/// the stricter winning) and both deny lists, as the core applies them to a question. A grant of
/// `portfolio` is read the same way. Files that can't be read leave a note in place of the classes,
/// and the rest of the listing stands.
pub async fn effective_classes(access: &mut Access, db: &Db, config_dir: &Path) -> Result<(), AdminError> {
    let (projects, policies) = match config::access_inputs(config_dir) {
        Ok(inputs) => inputs,
        Err(problems) => {
            let first = problems
                .first()
                .map_or_else(|| "the files".to_owned(), ToString::to_string);
            access.effective_note = Some(first);
            return Ok(());
        }
    };
    let mut registered: HashMap<String, ProbeManifest> = HashMap::new();
    for row in sqlx::query!("SELECT project, manifest_json FROM probes")
        .fetch_all(db.pool())
        .await?
    {
        if let Ok(manifest) = serde_json::from_str::<ProbeManifest>(&row.manifest_json) {
            registered.insert(row.project, manifest);
        }
    }
    let mut effective = Vec::new();
    for person in &access.people {
        for grant in &person.grants {
            let registered = registered.get(&grant.project);
            let classes = grant_classes(&projects, &policies, person.role, &grant.project, registered)?;
            effective.push(Effective {
                email: person.email.clone(),
                project: grant.project.clone(),
                classes,
            });
        }
    }
    access.effective = effective;
    Ok(())
}

/// What a grant of `project` lets a person of `role` ask, as the core applies it to a question:
/// the role's classes in `policies`, narrowed by the project's manifest (`registered`, the one its
/// probe registered, combined with the copy in `projects`) and both deny lists, each class marked
/// when it is held for sign-off. `portfolio` is read the same way, without a manifest.
pub(crate) fn grant_classes(
    projects: &ProjectsConfig,
    policies: &Policies,
    role: Role,
    project: &str,
    registered: Option<&ProbeManifest>,
) -> Result<Classes, AdminError> {
    let shown = |policy: EffectivePolicy| {
        Classes::Allowed(
            policy
                .allowed
                .iter()
                .map(|class| (*class, policy.signoff.contains(class)))
                .collect(),
        )
    };
    if project == PORTFOLIO {
        return Ok(shown(EffectivePolicy::for_portfolio(policies, role)));
    }
    let id = ProjectId::parse(project).map_err(|_| AdminError::Corrupt)?;
    Ok(match config::manifest_of(projects, &id, registered) {
        Some(manifest) => shown(EffectivePolicy::for_project(policies, role, &manifest)),
        None => Classes::NoManifest,
    })
}

/// A stored time as `2026-10-02 09:31`, in UTC; text that isn't a time is shown as it is.
fn short(text: &str) -> String {
    db::parse_time(text).map_or_else(|| text.to_owned(), |at| at.format("%Y-%m-%d %H:%M").to_string())
}

fn token_state(token: &TokenLine, now: DateTime<Utc>) -> String {
    match &token.revoked_at {
        Some(at) => format!("revoked {}", short(at)),
        None if db::parse_time(&token.expires_at).is_none_or(|expires| now >= expires) => "expired".into(),
        None => "active".into(),
    }
}

fn grants_text(grants: &[Grant]) -> String {
    if grants.is_empty() {
        return "none".into();
    }
    grants
        .iter()
        .map(|grant| match grant.approver {
            true => format!("{} (approver)", grant.project),
            false => grant.project.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Columns padded to their widest cell, two spaces apart.
fn table(header: &[&str], rows: &[Vec<String>]) -> String {
    let mut widths: Vec<usize> = header.iter().map(|cell| cell.chars().count()).collect();
    for row in rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let mut out = String::new();
    let header: Vec<String> = header.iter().map(|cell| (*cell).to_owned()).collect();
    for row in std::iter::once(&header).chain(rows) {
        let mut text = String::new();
        for (cell, width) in row.iter().zip(&widths) {
            let _ = write!(text, "{cell:<width$}  ");
        }
        out.push_str(text.trim_end());
        out.push('\n');
    }
    out
}

/// The `token list` table.
pub fn render_tokens(tokens: &[TokenLine], now: DateTime<Utc>) -> String {
    if tokens.is_empty() {
        return "No tokens.\n".into();
    }
    let rows: Vec<Vec<String>> = tokens
        .iter()
        .map(|token| {
            vec![
                token.public_id.clone(),
                token.email.clone(),
                token.label.clone(),
                token_state(token, now),
                short(&token.expires_at),
                token
                    .last_used_at
                    .as_deref()
                    .map_or_else(|| "never".into(), short),
            ]
        })
        .collect();
    table(
        &["TOKEN", "OWNER", "LABEL", "STATE", "EXPIRES", "LAST USED"],
        &rows,
    )
}

/// What each grant lets its person ask, as a table after the people: one row a grant.
fn render_effective(access: &Access) -> String {
    if let Some(note) = &access.effective_note {
        return format!("\nWhat each grant lets them ask isn't shown: {note}\n");
    }
    if access.effective.is_empty() {
        return String::new();
    }
    let rows: Vec<Vec<String>> = access
        .effective
        .iter()
        .map(|grant| {
            let classes = match &grant.classes {
                Classes::NoManifest => "nothing yet: the project has no manifest".to_owned(),
                Classes::Allowed(classes) if classes.is_empty() => "nothing".to_owned(),
                Classes::Allowed(classes) => classes
                    .iter()
                    .map(|(class, signoff)| match signoff {
                        true => format!("{} (held for sign-off)", class_name(*class)),
                        false => class_name(*class).to_owned(),
                    })
                    .collect::<Vec<_>>()
                    .join(", "),
            };
            vec![grant.email.clone(), grant.project.clone(), classes]
        })
        .collect();
    format!(
        "\nWHAT EACH GRANT LETS THEM ASK (the role's classes in policies.yaml, the project's manifest, both deny lists)\n{}",
        table(&["PERSON", "PROJECT", "CLASSES"], &rows)
    )
}

/// The `access list` view.
pub fn render_access(access: &Access, now: DateTime<Utc>) -> String {
    let mut out = String::new();
    if let Some((project, probe)) = &access.project {
        let probe = match probe {
            None => "not registered".to_owned(),
            Some(probe) => {
                let mut text = format!(
                    "{}, reads {}{}, last seen {}",
                    probe.status,
                    probe.access,
                    probe
                        .access_limited_by
                        .as_deref()
                        .map_or_else(String::new, |by| format!(" (limited by {by})")),
                    short(&probe.last_seen_at)
                );
                if let Some(reason) = &probe.quarantine_reason {
                    let _ = write!(text, "; quarantined: {reason}");
                }
                text
            }
        };
        let _ = writeln!(out, "PROJECT  {project}\nPROBE    {probe}\n");
    }
    if access.people.is_empty() {
        out.push_str("No one.\n");
    } else {
        let rows: Vec<Vec<String>> = access
            .people
            .iter()
            .map(|person| {
                vec![
                    person.email.clone(),
                    person.display_name.clone(),
                    role_name(person.role).to_owned(),
                    person
                        .disabled_at
                        .as_deref()
                        .map_or_else(|| "active".into(), |at| format!("disabled {}", short(at))),
                    grants_text(&person.grants),
                ]
            })
            .collect();
        out.push_str(&table(&["PERSON", "NAME", "ROLE", "STATE", "PROJECTS"], &rows));
    }
    out.push_str(&render_effective(access));
    out.push('\n');
    out.push_str(&render_tokens(&access.tokens, now));
    out
}

/// Runs an admin command at the current time. What it shows goes to `out`; `token create`
/// prints only the token there, so a script can take it, and its note to `note`. `config_dir` is
/// the configuration directory, read by the commands that name a project.
///
/// A command that changes the database writes its audit row in the same transaction: when the row
/// can't be written, nothing has changed and the command fails.
pub async fn run(
    command: Admin,
    db: &Db,
    config_dir: &Path,
    out: &mut dyn Write,
    note: &mut dyn Write,
) -> Result<(), AdminError> {
    let now = db::now();
    match command {
        Admin::User(UserCommand::Add { email, name, role }) => {
            let id = add_user(db, &email, &name, role.into(), now).await?;
            let email = email_address(&email)?;
            let role = role_name(role.into());
            writeln!(out, "added {email} as {role} ({id})")?;
        }
        Admin::User(UserCommand::Grant {
            email,
            projects,
            approver,
        }) => {
            let known = KnownProjects::read(config_dir)?;
            let grants = grant(db, &email, &projects, approver, &known, now).await?;
            let email = email_address(&email)?;
            writeln!(out, "{email} may ask about: {}", grants_text(&grants))?;
        }
        Admin::User(UserCommand::SetRole { email, role }) => {
            let role = Role::from(role);
            let set = set_role(db, &email, role, now).await?;
            let email = email_address(&email)?;
            let name = role_name(role);
            match set {
                RoleSet::Now { previous } => writeln!(
                    out,
                    "{email} is now {name} (was {}); it holds from their next request",
                    role_name(previous)
                )?,
                RoleSet::Already => writeln!(out, "{email} is {name} already; nothing changed")?,
            }
        }
        Admin::User(UserCommand::Ungrant { email, projects }) => {
            let ungranted = ungrant(db, &email, &projects, now).await?;
            let email = email_address(&email)?;
            if !ungranted.removed.is_empty() {
                writeln!(out, "took back from {email}: {}", ungranted.removed.join(", "))?;
            }
            if !ungranted.not_granted.is_empty() {
                writeln!(
                    out,
                    "{email} wasn't granted {}; nothing changed for it",
                    ungranted.not_granted.join(", ")
                )?;
            }
            writeln!(
                out,
                "{email} may ask about: {}",
                grants_text(&ungranted.remaining)
            )?;
        }
        Admin::User(UserCommand::Disable { email }) => {
            let email = email_address(&email)?;
            let disabled = disable_user(db, &email, now).await?;
            match disabled {
                Disabled::Now { tokens_revoked } => {
                    writeln!(out, "disabled {email}; revoked {tokens_revoked} token(s)")?;
                }
                Disabled::Already { since } => writeln!(out, "{email} was disabled on {}", short(&since))?,
            }
        }
        Admin::Token(TokenCommand::Create {
            email,
            label,
            expires,
        }) => {
            // the secret is shown only once its row is in the audit: the token and the row are
            // one transaction, so never a token without a trail
            let created = create_token(db, &email, &label, expires, now).await?;
            writeln!(out, "{}", created.token.reveal())?;
            writeln!(
                note,
                "token {} for {} expires {} UTC. It is shown only now: hand it over through the \
                 password manager or in person.",
                created.token.public_id(),
                email_address(&email)?,
                created.expires_at.format("%Y-%m-%d %H:%M")
            )?;
        }
        Admin::Token(TokenCommand::List { email }) => {
            let tokens = list_tokens(db, email.as_deref()).await?;
            let filter = email.as_deref().map(email_address).transpose()?;
            let payload = json!({ "email": filter, "tokens": tokens.len() });
            audit::action(db, "tokens_listed", None, &payload, now).await?;
            write!(out, "{}", render_tokens(&tokens, now))?;
        }
        Admin::Token(TokenCommand::Revoke { public_id }) => {
            let revoked = revoke_token(db, &public_id, now).await?;
            let public_id = public_id.trim();
            match revoked {
                Revoked::Now => writeln!(out, "revoked {public_id}")?,
                Revoked::Already { since } => writeln!(out, "{public_id} was revoked on {}", short(&since))?,
            }
        }
        Admin::Access(AccessCommand::List { project, email }) => {
            let mut access = access(db, project.as_deref(), email.as_deref()).await?;
            effective_classes(&mut access, db, config_dir).await?;
            let project = project
                .as_deref()
                .map(|p| ProjectId::parse(p.trim()))
                .transpose()
                .ok()
                .flatten();
            let filter = email.as_deref().map(email_address).transpose()?;
            // no project on the row: listing who has access is an admin's act, not something that
            // happened in a project; the project asked about is in the payload
            let payload = json!({ "project": project, "email": filter, "people": access.people.len() });
            audit::action(db, "access_listed", None, &payload, now).await?;
            write!(out, "{}", render_access(&access, now))?;
        }
        Admin::Audit(AuditCommand::Export {
            token,
            question,
            project,
            from,
            to,
        }) => {
            let filter = audit::Filter {
                project,
                token,
                question,
                from,
                to,
                ..audit::Filter::default()
            };
            // the rows up to the last one now; the export's own row comes after them
            let upto = audit::last_seq(db).await?;
            let payload = json!({ "by": "admin", "filters": filter.recorded(), "upto": upto });
            // no project on the row, as the API's export writes it: an admin's to read
            audit::action(db, "audit_exported", None, &payload, now).await?;
            let rows = audit::export_to(db, &filter, upto, out).await?;
            writeln!(note, "exported {rows} row(s), up to row {upto}")?;
        }
        Admin::Audit(AuditCommand::Verify) => {
            let verified = audit::verify(db).await?;
            match verified.broken {
                None => writeln!(out, "ok: {} row(s), the chain holds", verified.rows)?,
                Some(broken) => {
                    writeln!(out, "{broken}")?;
                    return Err(AdminError::ChainBroken(broken.seq));
                }
            }
        }
        Admin::Config(_) => {
            return Err(invalid(
                "`config check` reads the files only; the binary runs it without the database",
            ));
        }
        Admin::Shell => {
            return Err(invalid(
                "`shell` opens from the command line, and takes the commands from the prompt",
            ));
        }
        Admin::Probe(command) => crate::probes::admin::run(command, db, out, now).await?,
        Admin::Status { check, since } => {
            crate::status::run(db, config_dir, out, now, crate::status::Options { check, since }).await?;
        }
        // the row's time is read when the copy has finished, not when the command started
        Admin::Backup { to } => crate::backup::run(db, &to, out, crate::db::now).await?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;
    use clap::Parser;

    use super::*;
    use crate::cli::Cli;
    use crate::db::testing;
    use crate::identity::{self, AuthError};

    fn at(text: &str) -> DateTime<Utc> {
        db::parse_time(text).unwrap()
    }

    const NOW: &str = "2026-10-02T10:00:00.000Z";

    async fn authenticate(db: &Db, token: &str, now: DateTime<Utc>) -> Result<identity::Caller, AuthError> {
        let header = HeaderValue::from_str(&format!("Bearer {token}")).unwrap();
        identity::authenticate(db, Some(&header), now).await
    }

    /// A `projects.yaml` that lists delta and gamma.
    const PROJECTS: &str = "projects:
  delta:
    display_name: \"Delta\"
    aliases: [delta]
    access_max: code
    probe:
      url: http://probe-delta:7000/mcp
      token_file: /run/mercury/probe-tokens/delta.token
  gamma:
    display_name: \"Gamma\"
    aliases: [gamma]
    access_max: metadata
    probe:
      url: http://probe-gamma:7000/mcp
      token_file: /run/mercury/probe-tokens/gamma.token
";

    fn config_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(config::PROJECTS), PROJECTS).unwrap();
        dir
    }

    fn known() -> KnownProjects {
        KnownProjects::new(["delta", "gamma"])
    }

    /// `grant` with the projects of [`PROJECTS`], at [`NOW`].
    async fn grant_to(
        db: &Db,
        email: &str,
        projects: &[String],
        approver: bool,
    ) -> Result<Vec<Grant>, AdminError> {
        grant(db, email, projects, approver, &known(), at(NOW)).await
    }

    /// Runs a command line the way `main` does, with `config` as the configuration directory, and
    /// returns what it printed.
    async fn cli_in(db: &Db, config: &Path, args: &[&str]) -> Result<(String, String), AdminError> {
        let command = Cli::try_parse_from(["mercury-core", "admin"].iter().chain(args)).unwrap();
        let Some(crate::cli::Command::Admin(admin)) = command.command else {
            panic!("not an admin command");
        };
        let (mut out, mut note) = (Vec::new(), Vec::new());
        run(admin, db, config, &mut out, &mut note).await?;
        Ok((String::from_utf8(out).unwrap(), String::from_utf8(note).unwrap()))
    }

    /// [`cli_in`] with a configuration that lists delta and gamma.
    async fn cli(db: &Db, args: &[&str]) -> Result<(String, String), AdminError> {
        let dir = config_dir();
        cli_in(db, dir.path(), args).await
    }

    #[test]
    fn ulids_encode_the_time_first() {
        // the time part of the example in the ULID specification
        assert_eq!(
            encode_ulid(1_469_918_176_385, [0; 10]),
            "01ARYZ6S410000000000000000"
        );
        assert_eq!(encode_ulid(0, [0xff; 10]), "0000000000ZZZZZZZZZZZZZZZZ");
        let (earlier, later) = (encode_ulid(1_000, [0xff; 10]), encode_ulid(1_001, [0; 10]));
        assert!(earlier < later);
        assert_eq!(new_user_id(at(NOW)).unwrap().len(), 2 + 26);
    }

    #[test]
    fn e_mail_addresses_are_trimmed_lower_cased_and_checked() {
        assert_eq!(email_address(" PAT@example.com ").unwrap(), "pat@example.com");
        for wrong in [
            "",
            "pat",
            "@example.com",
            "pat@",
            "pat@@example.com",
            "pa t@example.com",
            "pat@exa\u{7}mple.com",
        ] {
            assert!(email_address(wrong).is_err(), "{wrong:?}");
        }
    }

    #[test]
    fn names_labels_project_ids_and_public_ids_are_checked() {
        assert_eq!(line("  Pat  ", "name", 100).unwrap(), "Pat");
        assert!(line("", "name", 100).is_err());
        assert!(line("Pat\nLee", "name", 100).is_err());
        assert!(line(&"x".repeat(65), "label", 64).is_err());
        assert_eq!(project_id("delta").unwrap(), "delta");
        assert_eq!(project_id("portfolio").unwrap(), "portfolio");
        assert!(project_id("Delta").is_err() && project_id("../x").is_err());
        assert!(public_id("k3Xq9AbCdEfG").is_ok());
        assert!(public_id("k3Xq9AbCdEf").is_err() && public_id("k3Xq9AbCdEf_").is_err());
    }

    #[tokio::test]
    async fn a_person_is_added_once_whatever_the_case_of_their_e_mail() {
        let (_dir, db) = testing::db().await;
        let id = add_user(&db, "Pat@example.com", "Pat", Role::Pm, at(NOW))
            .await
            .unwrap();
        assert!(id.starts_with("u_"));
        let again = add_user(&db, "pat@example.com", "Pat Again", Role::Board, at(NOW)).await;
        assert!(matches!(again, Err(AdminError::EmailTaken)), "{again:?}");
        let stored: (String, String) = sqlx::query_as("SELECT email, role FROM users")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(stored, ("pat@example.com".to_owned(), "pm".to_owned()));
        assert!(matches!(
            add_user(&db, "nobody", "X", Role::Pm, at(NOW)).await,
            Err(AdminError::Invalid(_))
        ));
    }

    #[tokio::test]
    async fn an_approver_is_a_project_manager_or_a_tech_lead() {
        let (_dir, db) = testing::db().await;
        for (email, role) in [
            ("pm@example.com", Role::Pm),
            ("lead@example.com", Role::TechLead),
            ("board@example.com", Role::Board),
            ("admin@example.com", Role::Admin),
        ] {
            add_user(&db, email, "X", role, at(NOW)).await.unwrap();
        }
        let before = audit_rows(&db).await.len();
        for email in ["board@example.com", "admin@example.com"] {
            let refused = grant_to(&db, email, &["delta".into()], true).await;
            assert!(
                matches!(&refused, Err(AdminError::Invalid(message)) if message.contains("`user set-role` first")),
                "{email}: {refused:?}"
            );
            // the plain grant is theirs to have
            assert!(grant_to(&db, email, &["delta".into()], false).await.is_ok());
        }
        // refused before writing: the two plain grants are all the audit gained
        assert_eq!(audit_rows(&db).await.len(), before + 2);
        for email in ["pm@example.com", "lead@example.com"] {
            let grants = grant_to(&db, email, &["delta".into()], true).await.unwrap();
            assert!(grants.iter().all(|grant| grant.approver), "{email}");
        }
    }

    #[tokio::test]
    async fn a_grant_adds_projects_and_sets_the_approver_flag_to_what_it_says() {
        let (_dir, db) = testing::db().await;
        add_user(&db, "pat@example.com", "Pat", Role::TechLead, at(NOW))
            .await
            .unwrap();
        let grants = grant_to(&db, "pat@example.com", &["delta".into(), "gamma".into()], true)
            .await
            .unwrap();
        assert!(grants.iter().all(|grant| grant.approver) && grants.len() == 2);
        let grants = grant_to(&db, "pat@example.com", &["gamma".into()], false)
            .await
            .unwrap();
        assert_eq!(
            grants,
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
        assert!(matches!(
            grant_to(&db, "nobody@example.com", &["delta".into()], false).await,
            Err(AdminError::NoSuchUser)
        ));
        assert!(matches!(
            grant_to(&db, "pat@example.com", &["Delta!".into()], false).await,
            Err(AdminError::Invalid(_))
        ));
        assert!(matches!(
            grant_to(&db, "pat@example.com", &[" ".into()], false).await,
            Err(AdminError::Invalid(_))
        ));
    }

    /// The grant rows say what happened to sign-off authority: `user_granted` names each project of
    /// the call with its approver flag before (`null` for a new grant) and after, so a plain grant
    /// that takes a flag away shows it, and `user_ungranted` names the grants taken back that
    /// carried the flag.
    #[tokio::test]
    async fn the_grant_rows_say_what_happened_to_the_approver_flags() {
        let (_dir, db) = testing::db().await;
        add_user(&db, "lee@example.com", "Lee", Role::TechLead, at(NOW))
            .await
            .unwrap();
        grant_to(&db, "lee@example.com", &["delta".into()], true)
            .await
            .unwrap();
        // a plain grant of a project Lee approves takes the flag away
        grant_to(&db, "lee@example.com", &["delta".into(), "gamma".into()], false)
            .await
            .unwrap();
        grant_to(
            &db,
            "lee@example.com",
            &["gamma".into(), "portfolio".into()],
            true,
        )
        .await
        .unwrap();
        ungrant(&db, "lee@example.com", &["delta".into(), "gamma".into()], at(NOW))
            .await
            .unwrap();

        let rows: Vec<(String, serde_json::Value)> = audit_rows(&db)
            .await
            .into_iter()
            .filter(|row| row.0 != "user_added")
            .map(|row| (row.0, serde_json::from_str(&row.3).unwrap()))
            .collect();
        let email = "lee@example.com";
        assert_eq!(
            rows,
            [
                (
                    "user_granted".to_owned(),
                    json!({ "email": email, "projects": ["delta"], "approver": true,
                            "approver_flags": [{ "project": "delta", "before": null, "after": true }] })
                ),
                (
                    "user_granted".to_owned(),
                    json!({ "email": email, "projects": ["delta", "gamma"], "approver": false,
                            "approver_flags": [{ "project": "delta", "before": true, "after": false },
                                               { "project": "gamma", "before": null, "after": false }] })
                ),
                (
                    "user_granted".to_owned(),
                    json!({ "email": email, "projects": ["delta", "gamma", "portfolio"], "approver": true,
                            "approver_flags": [{ "project": "gamma", "before": false, "after": true },
                                               { "project": "portfolio", "before": null, "after": true }] })
                ),
                (
                    "user_ungranted".to_owned(),
                    json!({ "email": email, "projects": ["delta", "gamma"], "remaining": ["portfolio"],
                            "approver_flags_removed": ["gamma"] })
                ),
            ]
        );
        assert_eq!(audit::verify(&db).await.unwrap().broken, None);
    }

    #[tokio::test]
    async fn a_created_token_works_until_it_expires() {
        let (_dir, db) = testing::db().await;
        add_user(&db, "pat@example.com", "Pat", Role::Pm, at(NOW))
            .await
            .unwrap();
        grant_to(&db, "pat@example.com", &["delta".into()], false)
            .await
            .unwrap();
        let created = create_token(&db, "pat@example.com", "laptop", TimeDelta::days(7), at(NOW))
            .await
            .unwrap();
        assert_eq!(created.expires_at, at("2026-10-09T10:00:00.000Z"));
        let text = created.token.reveal();
        let caller = authenticate(&db, &text, at(NOW)).await.unwrap();
        assert_eq!(caller.email, "pat@example.com");
        let a_week_on = at(NOW) + TimeDelta::days(7);
        assert!(matches!(
            authenticate(&db, &text, a_week_on).await,
            Err(AuthError::Expired)
        ));
        let stored: Vec<u8> = sqlx::query_scalar("SELECT secret_sha256 FROM tokens")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(stored, created.token.secret_sha256(), "only the digest is stored");
    }

    #[tokio::test]
    async fn no_token_for_a_stranger_a_disabled_person_or_no_time() {
        let (_dir, db) = testing::db().await;
        add_user(&db, "pat@example.com", "Pat", Role::Pm, at(NOW))
            .await
            .unwrap();
        let create = |email: &'static str, valid_for| create_token(&db, email, "laptop", valid_for, at(NOW));
        assert!(matches!(
            create("nobody@example.com", TimeDelta::days(1)).await,
            Err(AdminError::NoSuchUser)
        ));
        assert!(matches!(
            create("pat@example.com", TimeDelta::zero()).await,
            Err(AdminError::Invalid(_))
        ));
        assert!(matches!(
            create_token(&db, "pat@example.com", "", TimeDelta::days(1), at(NOW)).await,
            Err(AdminError::Invalid(_))
        ));
        disable_user(&db, "pat@example.com", at(NOW)).await.unwrap();
        assert!(matches!(
            create("pat@example.com", TimeDelta::days(1)).await,
            Err(AdminError::UserDisabled)
        ));
        assert!(matches!(
            grant_to(&db, "pat@example.com", &["delta".into()], false).await,
            Err(AdminError::UserDisabled)
        ));
    }

    #[tokio::test]
    async fn disabling_a_person_revokes_every_token_they_hold_at_once() {
        let (_dir, db) = testing::db().await;
        add_user(&db, "pat@example.com", "Pat", Role::Pm, at(NOW))
            .await
            .unwrap();
        add_user(&db, "lee@example.com", "Lee", Role::Pm, at(NOW))
            .await
            .unwrap();
        let mut texts = Vec::new();
        for label in ["laptop", "phone"] {
            let created = create_token(&db, "pat@example.com", label, TimeDelta::days(90), at(NOW))
                .await
                .unwrap();
            texts.push(created.token.reveal());
        }
        let lee = create_token(&db, "lee@example.com", "laptop", TimeDelta::days(90), at(NOW))
            .await
            .unwrap()
            .token
            .reveal();

        let later = at(NOW) + TimeDelta::hours(1);
        assert_eq!(
            disable_user(&db, "PAT@example.com", later).await.unwrap(),
            Disabled::Now { tokens_revoked: 2 }
        );
        for text in &texts {
            assert!(matches!(
                authenticate(&db, text, later).await,
                Err(AuthError::Unauthenticated)
            ));
        }
        assert!(authenticate(&db, &lee, later).await.is_ok(), "nobody else's");
        assert_eq!(
            disable_user(&db, "pat@example.com", later + TimeDelta::hours(1))
                .await
                .unwrap(),
            Disabled::Already {
                since: db::time(later)
            }
        );
        assert!(matches!(
            disable_user(&db, "nobody@example.com", later).await,
            Err(AdminError::NoSuchUser)
        ));
    }

    #[tokio::test]
    async fn a_revoked_token_stops_working_and_revoking_it_again_says_since_when() {
        let (_dir, db) = testing::db().await;
        add_user(&db, "pat@example.com", "Pat", Role::Pm, at(NOW))
            .await
            .unwrap();
        let created = create_token(&db, "pat@example.com", "laptop", TimeDelta::days(90), at(NOW))
            .await
            .unwrap();
        let (text, id) = (created.token.reveal(), created.token.public_id().to_owned());
        assert_eq!(revoke_token(&db, &id, at(NOW)).await.unwrap(), Revoked::Now);
        assert!(matches!(
            authenticate(&db, &text, at(NOW)).await,
            Err(AuthError::Unauthenticated)
        ));
        assert_eq!(
            revoke_token(&db, &id, at(NOW) + TimeDelta::days(1))
                .await
                .unwrap(),
            Revoked::Already { since: NOW.into() }
        );
        assert!(matches!(
            revoke_token(&db, "AAAAAAAAAAAA", at(NOW)).await,
            Err(AdminError::NoSuchToken)
        ));
        assert!(matches!(
            revoke_token(&db, "short", at(NOW)).await,
            Err(AdminError::Invalid(_))
        ));
    }

    #[tokio::test]
    async fn the_commands_print_what_they_did_and_the_secret_only_once() {
        let (_dir, db) = testing::db().await;
        let (out, _) = cli(
            &db,
            &[
                "user",
                "add",
                "--email",
                "Pat@example.com",
                "--name",
                "Pat",
                "--role",
                "pm",
            ],
        )
        .await
        .unwrap();
        assert!(out.starts_with("added pat@example.com as pm (u_"), "{out}");
        let (out, _) = cli(
            &db,
            &[
                "user",
                "grant",
                "--email",
                "pat@example.com",
                "--projects",
                "delta,gamma",
            ],
        )
        .await
        .unwrap();
        assert_eq!(out, "pat@example.com may ask about: delta, gamma\n");

        let (token, note) = cli(
            &db,
            &[
                "token",
                "create",
                "--email",
                "pat@example.com",
                "--label",
                "laptop",
            ],
        )
        .await
        .unwrap();
        let token = token.trim_end();
        assert!(token.starts_with("mcy_") && token.len() == 60, "{token}");
        let public_id = &token[4..16];
        let secret = &token[17..];
        assert!(note.contains(public_id) && !note.contains(secret), "{note}");
        assert!(authenticate(&db, token, db::now()).await.is_ok());

        for args in [
            &["token", "list"][..],
            &["token", "list", "--email", "pat@example.com"],
            &["access", "list"],
            &["access", "list", "--project", "delta"],
            &["access", "list", "--email", "pat@example.com"],
        ] {
            let (out, note) = cli(&db, args).await.unwrap();
            assert!(out.contains(public_id), "{args:?}: {out}");
            assert!(
                !out.contains(secret) && !note.contains(secret),
                "{args:?} showed the secret"
            );
        }

        let (out, _) = cli(&db, &["token", "revoke", public_id]).await.unwrap();
        assert_eq!(out, format!("revoked {public_id}\n"));
        let (out, _) = cli(&db, &["user", "disable", "--email", "pat@example.com"])
            .await
            .unwrap();
        assert_eq!(out, "disabled pat@example.com; revoked 0 token(s)\n");
        assert!(matches!(
            cli(&db, &["token", "list", "--email", "nobody@example.com"]).await,
            Err(AdminError::NoSuchUser)
        ));
    }

    #[tokio::test]
    async fn access_list_shows_people_grants_tokens_and_the_probe() {
        let (_dir, db) = testing::db().await;
        let now = at(NOW);
        add_user(&db, "pat@example.com", "Pat", Role::Pm, now)
            .await
            .unwrap();
        add_user(&db, "lee@example.com", "Lee", Role::TechLead, now)
            .await
            .unwrap();
        add_user(&db, "sam@example.com", "Sam", Role::Pm, now)
            .await
            .unwrap();
        grant_to(&db, "pat@example.com", &["delta".into(), "gamma".into()], false)
            .await
            .unwrap();
        grant_to(&db, "lee@example.com", &["delta".into()], true)
            .await
            .unwrap();
        grant_to(&db, "sam@example.com", &["gamma".into()], false)
            .await
            .unwrap();
        let pat = create_token(&db, "pat@example.com", "laptop", TimeDelta::days(90), now)
            .await
            .unwrap();
        let lee = create_token(&db, "lee@example.com", "phone", TimeDelta::days(1), now)
            .await
            .unwrap();
        revoke_token(&db, lee.token.public_id(), now).await.unwrap();
        disable_user(&db, "sam@example.com", now).await.unwrap();

        // the words of the first line that starts with `first`
        let row = |shown: &str, first: &str| -> Vec<String> {
            let line = shown.lines().find(|line| line.starts_with(first));
            line.unwrap_or_else(|| panic!("no {first} in\n{shown}"))
                .split_whitespace()
                .map(str::to_owned)
                .collect()
        };
        let everyone = render_access(&access(&db, None, None).await.unwrap(), now);
        assert_eq!(
            row(&everyone, "pat@example.com"),
            ["pat@example.com", "Pat", "pm", "active", "delta,", "gamma"]
        );
        assert_eq!(
            row(&everyone, "lee@example.com"),
            [
                "lee@example.com",
                "Lee",
                "tech_lead",
                "active",
                "delta",
                "(approver)"
            ]
        );
        assert_eq!(
            row(&everyone, "sam@example.com"),
            [
                "sam@example.com",
                "Sam",
                "pm",
                "disabled",
                "2026-10-02",
                "10:00",
                "gamma"
            ]
        );
        let pat_id = pat.token.public_id();
        assert_eq!(
            row(&everyone, pat_id),
            [
                pat_id,
                "pat@example.com",
                "laptop",
                "active",
                "2026-12-31",
                "10:00",
                "never"
            ]
        );
        let lee_id = lee.token.public_id();
        assert_eq!(
            row(&everyone, lee_id),
            [
                lee_id,
                "lee@example.com",
                "phone",
                "revoked",
                "2026-10-02",
                "10:00",
                "2026-10-03",
                "10:00",
                "never"
            ]
        );

        let delta = access(&db, Some("delta"), None).await.unwrap();
        let emails: Vec<&str> = delta.people.iter().map(|person| person.email.as_str()).collect();
        assert_eq!(
            emails,
            ["lee@example.com", "pat@example.com"],
            "Sam has no grant on delta"
        );
        assert!(delta.people.iter().all(|person| person.grants.len() == 1));
        let shown = render_access(&delta, now);
        assert!(
            shown.starts_with("PROJECT  delta\nPROBE    not registered\n"),
            "{shown}"
        );
        assert!(
            !shown.contains("gamma") && !shown.contains("sam@example.com"),
            "{shown}"
        );

        sqlx::query(
            "INSERT INTO probes (project, url, access, access_limited_by, status, schema, manifest_hash, \
             manifest_json, tools_json, registered_at, last_seen_at) \
             VALUES ('delta', 'http://probe-delta:7000/mcp', 'metadata', 'token', 'healthy', '0.1', 'h', '{}', '[]', ?, ?)",
        )
        .bind(NOW)
        .bind(NOW)
        .execute(db.pool())
        .await
        .unwrap();
        let shown = render_access(&access(&db, Some("delta"), None).await.unwrap(), now);
        assert!(
            shown.contains("PROBE    healthy, reads metadata (limited by token), last seen 2026-10-02 10:00"),
            "{shown}"
        );

        let one = access(&db, None, Some("Lee@example.com")).await.unwrap();
        assert_eq!(one.people.len(), 1);
        assert_eq!(one.tokens.len(), 1);
        assert!(render_access(&one, now).contains("phone"));

        let nobody = access(&db, Some("omega"), None).await.unwrap();
        assert!(render_access(&nobody, now).contains("No one.\n\nNo tokens.\n"));
        assert!(matches!(
            access(&db, None, Some("nobody@example.com")).await,
            Err(AdminError::NoSuchUser)
        ));
    }

    #[tokio::test]
    async fn an_expired_token_is_listed_as_expired() {
        let (_dir, db) = testing::db().await;
        add_user(&db, "pat@example.com", "Pat", Role::Pm, at(NOW))
            .await
            .unwrap();
        create_token(&db, "pat@example.com", "laptop", TimeDelta::hours(1), at(NOW))
            .await
            .unwrap();
        let tokens = list_tokens(&db, None).await.unwrap();
        let shown = render_tokens(&tokens, at(NOW) + TimeDelta::hours(2));
        assert!(shown.contains("expired"), "{shown}");
        assert!(shown.contains("never"), "{shown}");
    }

    /// Every audit row: type, whether it has a question, the project and the payload.
    async fn audit_rows(db: &Db) -> Vec<(String, bool, Option<String>, String)> {
        sqlx::query_as::<_, (String, Option<String>, Option<String>, String)>(
            "SELECT type, question_id, project, payload_json FROM audit_events ORDER BY seq",
        )
        .fetch_all(db.pool())
        .await
        .unwrap()
        .into_iter()
        .map(|(t, q, p, payload)| (t, q.is_some(), p, payload))
        .collect()
    }

    #[tokio::test]
    async fn every_admin_command_leaves_one_event_without_a_question() {
        let (_dir, db) = testing::db().await;
        let steps: [&[&str]; 7] = [
            &[
                "user",
                "add",
                "--email",
                "pm@example.com",
                "--name",
                "Pat",
                "--role",
                "pm",
            ],
            &[
                "user",
                "grant",
                "--email",
                "pm@example.com",
                "--projects",
                "delta",
            ],
            &[
                "token",
                "create",
                "--email",
                "pm@example.com",
                "--label",
                "laptop",
            ],
            &["token", "list"],
            &["access", "list", "--project", "delta"],
            &["token", "revoke", "PUBLIC"],
            &["user", "disable", "--email", "pm@example.com"],
        ];
        let mut secret = String::new();
        for (n, args) in steps.iter().enumerate() {
            let public_id;
            let args: Vec<&str> = if args[1] == "revoke" {
                public_id = secret.split('_').nth(1).unwrap().to_owned();
                vec!["token", "revoke", public_id.as_str()]
            } else {
                args.to_vec()
            };
            let (out, _) = cli(&db, &args).await.unwrap();
            if args[1] == "create" {
                secret = out.trim().to_owned();
            }
            assert_eq!(audit_rows(&db).await.len(), n + 1, "one row for {args:?}");
        }
        let rows = audit_rows(&db).await;
        let types: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
        assert_eq!(
            types,
            [
                "user_added",
                "user_granted",
                "token_created",
                "tokens_listed",
                "access_listed",
                "token_revoked",
                "user_disabled"
            ]
        );
        assert!(rows.iter().all(|r| !r.1), "no admin event has a question");
        assert_eq!(
            rows[4].2, None,
            "an access list is an admin's: no project on its row"
        );
        assert!(
            rows[4].3.contains("\"project\":\"delta\""),
            "the project asked about is in the payload: {}",
            rows[4].3
        );
        // `mcy_<public_id>_<secret>`: the secret is base64url, so it may hold `_` itself
        let secret_part = secret.trim().splitn(3, '_').nth(2).unwrap();
        assert!(secret_part.len() >= 40, "the whole secret, not a tail of it");
        assert!(
            rows.iter().all(|r| !r.3.contains(secret_part)),
            "the token's secret is in no payload"
        );
        assert!(rows[2].3.contains("\"label\":\"laptop\""), "{}", rows[2].3);
        let (out, _) = cli(&db, &["audit", "verify"]).await.unwrap();
        assert_eq!(
            out,
            "ok: 7 row(s), the chain holds
"
        );
    }

    #[tokio::test]
    async fn audit_verify_names_the_row_that_was_edited() {
        let (_dir, db) = testing::db().await;
        cli(
            &db,
            &[
                "user",
                "add",
                "--email",
                "pm@example.com",
                "--name",
                "Pat",
                "--role",
                "pm",
            ],
        )
        .await
        .unwrap();
        cli(&db, &["token", "list"]).await.unwrap();
        cli(&db, &["token", "list"]).await.unwrap();
        sqlx::query("UPDATE audit_events SET payload_json = '{\"email\":null,\"tokens\":9}' WHERE seq = 2")
            .execute(db.pool())
            .await
            .unwrap();
        let err = cli(&db, &["audit", "verify"]).await.unwrap_err();
        assert!(matches!(err, AdminError::ChainBroken(2)), "{err}");
        assert_eq!(err.to_string(), "the audit chain is broken at row 2");
    }

    #[tokio::test]
    async fn audit_export_by_token_writes_what_that_token_did_and_is_audited() {
        let (_dir, db) = testing::db().await;
        let mut public_ids = Vec::new();
        for email in ["pm@example.com", "lead@example.com"] {
            let add = ["user", "add", "--email", email, "--name", "Pat", "--role", "pm"];
            cli(&db, &add).await.unwrap();
            let (token, _) = cli(&db, &["token", "create", "--email", email, "--label", "laptop"])
                .await
                .unwrap();
            public_ids.push(token.trim().split('_').nth(1).unwrap().to_owned());
        }
        let pm = public_ids[0].clone();
        cli(&db, &["token", "revoke", &pm]).await.unwrap();

        let (out, note) = cli(&db, &["audit", "export", "--token", &pm]).await.unwrap();
        let rows: Vec<serde_json::Value> = out.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        let types: Vec<&str> = rows.iter().map(|r| r["type"].as_str().unwrap()).collect();
        assert_eq!(types, ["token_created", "token_revoked"]);
        assert!(
            rows.iter()
                .all(|r| r["token"] == pm.as_str() && r["prev_hash"].is_string())
        );
        assert_eq!(note, "exported 2 row(s), up to row 5\n");

        // the export is a row of its own, after the rows it covers
        let exported = audit_rows(&db).await.pop().unwrap();
        assert_eq!(exported.0, "audit_exported");
        let payload: serde_json::Value = serde_json::from_str(&exported.3).unwrap();
        assert_eq!(
            payload,
            json!({ "by": "admin", "filters": { "token": pm }, "upto": 5 })
        );
        let (verified, _) = cli(&db, &["audit", "verify"]).await.unwrap();
        assert_eq!(verified, "ok: 6 row(s), the chain holds\n");
    }

    async fn count(db: &Db, table: &str) -> i64 {
        sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
            .fetch_one(db.pool())
            .await
            .unwrap()
    }

    /// Makes the audit refuse the rows of one type, as a full disk would.
    async fn refuse_audit(db: &Db, event: &str) {
        let trigger = format!(
            "CREATE TRIGGER refuse_{event} BEFORE INSERT ON audit_events WHEN NEW.type = '{event}' \
             BEGIN SELECT RAISE(ABORT, 'the disk is full'); END"
        );
        sqlx::query(sqlx::AssertSqlSafe(trigger))
            .execute(db.pool())
            .await
            .unwrap();
    }

    async fn allow_audit(db: &Db, event: &str) {
        sqlx::query(sqlx::AssertSqlSafe(format!("DROP TRIGGER refuse_{event}")))
            .execute(db.pool())
            .await
            .unwrap();
    }

    /// A change and its audit row are one transaction: when the row can't be written, the change
    /// isn't made, and the command fails, so nothing is ever done without a trail.
    #[tokio::test]
    async fn an_admin_change_whose_audit_row_cant_be_written_is_not_made() {
        let (_dir, db) = testing::db().await;
        let now = at(NOW);

        refuse_audit(&db, "user_added").await;
        let added = add_user(&db, "pat@example.com", "Pat", Role::Pm, now).await;
        assert!(matches!(added, Err(AdminError::Audit(_))), "{added:?}");
        assert_eq!(count(&db, "users").await, 0, "no person without the row");
        allow_audit(&db, "user_added").await;
        add_user(&db, "pat@example.com", "Pat", Role::Pm, now)
            .await
            .unwrap();

        refuse_audit(&db, "user_granted").await;
        let granted = grant_to(&db, "pat@example.com", &["delta".into()], false).await;
        assert!(matches!(granted, Err(AdminError::Audit(_))), "{granted:?}");
        assert_eq!(count(&db, "user_projects").await, 0, "no grant without the row");
        allow_audit(&db, "user_granted").await;
        grant_to(&db, "pat@example.com", &["delta".into()], false)
            .await
            .unwrap();

        refuse_audit(&db, "token_created").await;
        let created = create_token(&db, "pat@example.com", "laptop", TimeDelta::days(1), now).await;
        assert!(matches!(created, Err(AdminError::Audit(_))), "{created:?}");
        assert_eq!(count(&db, "tokens").await, 0, "no token without the row");
        allow_audit(&db, "token_created").await;
        let created = create_token(&db, "pat@example.com", "laptop", TimeDelta::days(1), now)
            .await
            .unwrap();
        let (secret, public_id) = (created.token.reveal(), created.token.public_id().to_owned());

        refuse_audit(&db, "token_revoked").await;
        let revoked = revoke_token(&db, &public_id, now).await;
        assert!(matches!(revoked, Err(AdminError::Audit(_))), "{revoked:?}");
        assert!(
            authenticate(&db, &secret, now).await.is_ok(),
            "the token still works: the revocation wasn't made"
        );
        allow_audit(&db, "token_revoked").await;

        refuse_audit(&db, "user_disabled").await;
        let disabled = disable_user(&db, "pat@example.com", now).await;
        assert!(matches!(disabled, Err(AdminError::Audit(_))), "{disabled:?}");
        assert!(
            authenticate(&db, &secret, now).await.is_ok(),
            "the person and their token are as they were"
        );
        allow_audit(&db, "user_disabled").await;

        // what was done was audited, and the chain holds
        let types: Vec<String> = audit_rows(&db).await.into_iter().map(|row| row.0).collect();
        assert_eq!(types, ["user_added", "user_granted", "token_created"]);
        assert_eq!(audit::verify(&db).await.unwrap().broken, None);
    }

    /// A grant names projects `projects.yaml` lists, or `portfolio`; one it doesn't list is refused
    /// with the known ones, and nothing is granted, not even the others named with it.
    #[tokio::test]
    async fn a_grant_of_a_project_the_configuration_doesnt_list_is_refused_naming_the_known_ones() {
        let (_dir, db) = testing::db().await;
        add_user(&db, "pat@example.com", "Pat", Role::Pm, at(NOW))
            .await
            .unwrap();

        let refused = grant_to(&db, "pat@example.com", &["delta".into(), "omega".into()], false).await;
        let Err(AdminError::Invalid(message)) = refused else {
            panic!("{refused:?}");
        };
        assert!(
            message.contains("omega") && message.contains("delta, gamma, portfolio"),
            "{message}"
        );
        assert_eq!(count(&db, "user_projects").await, 0);
        assert_eq!(audit_rows(&db).await.len(), 1, "only the person's own row");

        let grants = grant_to(&db, "pat@example.com", &["portfolio".into()], true)
            .await
            .unwrap();
        assert_eq!(
            grants,
            [Grant {
                project: "portfolio".into(),
                approver: true
            }],
            "portfolio has no entry in projects.yaml and is always known"
        );
    }

    /// A role change holds from the person's next request, and the role they have already changes
    /// nothing; each is one audit row without a question, which names the role before.
    #[tokio::test]
    async fn a_role_change_holds_from_the_next_request_and_the_same_role_changes_nothing() {
        let (_dir, db) = testing::db().await;
        let now = at(NOW);
        add_user(&db, "anna@example.com", "Anna", Role::Pm, now)
            .await
            .unwrap();
        grant_to(&db, "anna@example.com", &["delta".into()], false)
            .await
            .unwrap();
        let token = create_token(&db, "anna@example.com", "laptop", TimeDelta::days(1), now)
            .await
            .unwrap()
            .token
            .reveal();
        assert_eq!(authenticate(&db, &token, now).await.unwrap().role, Role::Pm);

        let (out, _) = cli(
            &db,
            &[
                "user",
                "set-role",
                "--email",
                "Anna@example.com",
                "--role",
                "tech_lead",
            ],
        )
        .await
        .unwrap();
        assert_eq!(
            out,
            "anna@example.com is now tech_lead (was pm); it holds from their next request\n"
        );
        let caller = authenticate(&db, &token, now).await.unwrap();
        assert_eq!(caller.role, Role::TechLead, "the next request has the new role");
        assert_eq!(caller.projects.len(), 1, "the grants stay");

        let (out, _) = cli(
            &db,
            &[
                "user",
                "set-role",
                "--email",
                "anna@example.com",
                "--role",
                "tech_lead",
            ],
        )
        .await
        .unwrap();
        assert_eq!(out, "anna@example.com is tech_lead already; nothing changed\n");
        assert_eq!(
            set_role(&db, "anna@example.com", Role::TechLead, now)
                .await
                .unwrap(),
            RoleSet::Already
        );

        let rows: Vec<(String, bool, Option<String>, serde_json::Value)> = audit_rows(&db)
            .await
            .into_iter()
            .filter(|row| row.0 == "user_role_set")
            .map(|(t, q, p, payload)| (t, q, p, serde_json::from_str(&payload).unwrap()))
            .collect();
        assert_eq!(rows.len(), 3, "one row each time, the unchanged ones too");
        assert!(rows.iter().all(|row| !row.1 && row.2.is_none()));
        assert_eq!(
            rows[0].3,
            json!({ "email": "anna@example.com", "role": "tech_lead", "previous": "pm" })
        );
        assert_eq!(
            rows[1].3,
            json!({ "email": "anna@example.com", "role": "tech_lead", "previous": "tech_lead" })
        );
        assert_eq!(audit::verify(&db).await.unwrap().broken, None);
    }

    #[tokio::test]
    async fn an_approver_cant_be_moved_to_board_or_admin() {
        let (_dir, db) = testing::db().await;
        let now = at(NOW);
        add_user(&db, "lead@example.com", "Lead", Role::TechLead, now)
            .await
            .unwrap();
        grant_to(&db, "lead@example.com", &["delta".into()], true)
            .await
            .unwrap();
        let before = audit_rows(&db).await.len();
        for role in [Role::Board, Role::Admin] {
            let refused = set_role(&db, "lead@example.com", role, now).await;
            assert!(
                matches!(&refused, Err(AdminError::Invalid(message)) if message.contains("without `--approver` first")),
                "{role:?}: {refused:?}"
            );
        }
        // refused before writing: the role stands and the audit gained nothing
        assert_eq!(audit_rows(&db).await.len(), before);
        let role: String = sqlx::query_scalar("SELECT role FROM users")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(role, "tech_lead");
        // a pm keeps the flag; once it is gone, board is theirs to have
        assert!(set_role(&db, "lead@example.com", Role::Pm, now).await.is_ok());
        grant_to(&db, "lead@example.com", &["delta".into()], false)
            .await
            .unwrap();
        assert!(set_role(&db, "lead@example.com", Role::Board, now).await.is_ok());
    }

    #[tokio::test]
    async fn a_role_change_refuses_a_stranger_a_disabled_person_and_an_unknown_role() {
        let (_dir, db) = testing::db().await;
        let now = at(NOW);
        assert!(matches!(
            set_role(&db, "nobody@example.com", Role::Pm, now).await,
            Err(AdminError::NoSuchUser)
        ));
        add_user(&db, "anna@example.com", "Anna", Role::Pm, now)
            .await
            .unwrap();
        disable_user(&db, "anna@example.com", now).await.unwrap();
        assert!(matches!(
            set_role(&db, "anna@example.com", Role::Board, now).await,
            Err(AdminError::UserDisabled)
        ));
        let role: String = sqlx::query_scalar("SELECT role FROM users")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(role, "pm");
        let args = [
            "mercury-core",
            "admin",
            "user",
            "set-role",
            "--email",
            "anna@example.com",
        ];
        for role in ["approver", ""] {
            let parsed = Cli::try_parse_from(args.iter().copied().chain(["--role", role]));
            assert_eq!(parsed.unwrap_err().exit_code(), 2, "{role:?}");
        }
    }

    /// Taking a grant back removes it with its approver flag; a project the person wasn't granted
    /// changes nothing and says so, and the person's projects are printed after.
    #[tokio::test]
    async fn ungrant_takes_projects_back_with_their_approver_flags() {
        let (_dir, db) = testing::db().await;
        let now = at(NOW);
        add_user(&db, "anna@example.com", "Anna", Role::TechLead, now)
            .await
            .unwrap();
        grant_to(
            &db,
            "anna@example.com",
            &["delta".into(), "gamma".into(), "portfolio".into()],
            true,
        )
        .await
        .unwrap();
        let token = create_token(&db, "anna@example.com", "laptop", TimeDelta::days(1), now)
            .await
            .unwrap()
            .token
            .reveal();

        let (out, _) = cli(
            &db,
            &[
                "user",
                "ungrant",
                "--email",
                "anna@example.com",
                "--projects",
                "delta,omega",
            ],
        )
        .await
        .unwrap();
        assert_eq!(
            out,
            "took back from anna@example.com: delta\n\
             anna@example.com wasn't granted omega; nothing changed for it\n\
             anna@example.com may ask about: gamma (approver), portfolio (approver)\n"
        );
        let caller = authenticate(&db, &token, now).await.unwrap();
        let projects: Vec<&str> = caller.projects.iter().map(|g| g.project.as_str()).collect();
        assert_eq!(projects, ["gamma", "portfolio"], "delta is no longer theirs");

        // granted again without the flag, delta comes back without it: the flag went with the grant
        let grants = grant_to(&db, "anna@example.com", &["delta".into()], false)
            .await
            .unwrap();
        assert_eq!(
            grants[0],
            Grant {
                project: "delta".into(),
                approver: false
            }
        );

        let ungranted = ungrant(
            &db,
            "anna@example.com",
            &["portfolio".into(), "omega".into()],
            now,
        )
        .await
        .unwrap();
        assert_eq!(ungranted.removed, ["portfolio"]);
        assert_eq!(ungranted.not_granted, ["omega"]);
        let nothing = ungrant(&db, "anna@example.com", &["omega".into()], now)
            .await
            .unwrap();
        assert!(nothing.removed.is_empty());
        assert_eq!(nothing.remaining.len(), 2);

        let payloads: Vec<serde_json::Value> = audit_rows(&db)
            .await
            .into_iter()
            .filter(|row| row.0 == "user_ungranted" && !row.1 && row.2.is_none())
            .map(|row| serde_json::from_str(&row.3).unwrap())
            .collect();
        assert_eq!(
            payloads,
            [
                json!({ "email": "anna@example.com", "projects": ["delta"], "remaining": ["gamma", "portfolio"],
                        "approver_flags_removed": ["delta"] }),
                json!({ "email": "anna@example.com", "projects": ["portfolio"], "remaining": ["delta", "gamma"],
                        "approver_flags_removed": ["portfolio"] }),
                json!({ "email": "anna@example.com", "projects": [], "remaining": ["delta", "gamma"],
                        "approver_flags_removed": [] }),
            ],
            "one row each time, what was taken back, which of it approved, and what is left"
        );
        assert_eq!(audit::verify(&db).await.unwrap().broken, None);
    }

    #[tokio::test]
    async fn ungrant_refuses_a_stranger_a_disabled_person_and_no_project() {
        let (_dir, db) = testing::db().await;
        let now = at(NOW);
        assert!(matches!(
            ungrant(&db, "nobody@example.com", &["delta".into()], now).await,
            Err(AdminError::NoSuchUser)
        ));
        add_user(&db, "anna@example.com", "Anna", Role::Pm, now)
            .await
            .unwrap();
        grant_to(&db, "anna@example.com", &["delta".into(), "gamma".into()], false)
            .await
            .unwrap();
        for wrong in [&[" ".to_owned()][..], &["Delta!".to_owned()]] {
            assert!(matches!(
                ungrant(&db, "anna@example.com", wrong, now).await,
                Err(AdminError::Invalid(_))
            ));
        }
        // no configuration is read: a project projects.yaml no longer lists can still be taken back
        let empty = tempfile::tempdir().unwrap();
        let (out, _) = cli_in(
            &db,
            empty.path(),
            &[
                "user",
                "ungrant",
                "--email",
                "anna@example.com",
                "--projects",
                "gamma",
            ],
        )
        .await
        .unwrap();
        assert!(out.ends_with("anna@example.com may ask about: delta\n"), "{out}");
        disable_user(&db, "anna@example.com", now).await.unwrap();
        assert!(matches!(
            ungrant(&db, "anna@example.com", &["delta".into()], now).await,
            Err(AdminError::UserDisabled)
        ));
        assert_eq!(count(&db, "user_projects").await, 1, "nothing was taken back");
    }

    /// The role change and the grant taken back commit with their audit rows, as every admin change
    /// does: when the row can't be written, nothing changes.
    #[tokio::test]
    async fn a_role_change_or_an_ungrant_whose_audit_row_cant_be_written_is_not_made() {
        let (_dir, db) = testing::db().await;
        let now = at(NOW);
        add_user(&db, "anna@example.com", "Anna", Role::Pm, now)
            .await
            .unwrap();
        grant_to(&db, "anna@example.com", &["delta".into()], true)
            .await
            .unwrap();

        refuse_audit(&db, "user_role_set").await;
        // a tech lead: an approver can't be moved to board or admin at all
        let set = set_role(&db, "anna@example.com", Role::TechLead, now).await;
        assert!(matches!(set, Err(AdminError::Audit(_))), "{set:?}");
        let role: String = sqlx::query_scalar("SELECT role FROM users")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(role, "pm", "no role change without its row");

        refuse_audit(&db, "user_ungranted").await;
        let ungranted = ungrant(&db, "anna@example.com", &["delta".into()], now).await;
        assert!(matches!(ungranted, Err(AdminError::Audit(_))), "{ungranted:?}");
        assert_eq!(
            count(&db, "user_projects").await,
            1,
            "no grant taken back without its row"
        );
        allow_audit(&db, "user_role_set").await;
        allow_audit(&db, "user_ungranted").await;

        let types: Vec<String> = audit_rows(&db).await.into_iter().map(|row| row.0).collect();
        assert_eq!(types, ["user_added", "user_granted"]);
    }

    /// The command reads `projects.yaml` from the configuration directory, and with none it grants
    /// nothing: a grant is never given unchecked.
    #[tokio::test]
    async fn the_grant_command_reads_the_configuration_and_grants_nothing_without_it() {
        let (_dir, db) = testing::db().await;
        cli(
            &db,
            &[
                "user",
                "add",
                "--email",
                "pat@example.com",
                "--name",
                "Pat",
                "--role",
                "pm",
            ],
        )
        .await
        .unwrap();
        let grant_args = |projects: &'static str| {
            [
                "user",
                "grant",
                "--email",
                "pat@example.com",
                "--projects",
                projects,
            ]
        };

        let err = cli(&db, &grant_args("delta,omega")).await.unwrap_err();
        assert!(err.to_string().contains("delta, gamma, portfolio"), "{err}");

        let empty = tempfile::tempdir().unwrap();
        let err = cli_in(&db, empty.path(), &grant_args("delta")).await.unwrap_err();
        assert!(
            matches!(&err, AdminError::Invalid(m) if m.contains("projects.yaml: the file is missing")),
            "{err}"
        );
        assert_eq!(count(&db, "user_projects").await, 0);

        // the other commands need no configuration at all
        cli_in(&db, empty.path(), &["token", "list"]).await.unwrap();
    }
}
