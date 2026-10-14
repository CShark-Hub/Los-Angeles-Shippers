//! `probe list`, `probe quarantine` and `probe release`: the admin's view of the probes, and the
//! hand on the switch. They work on the database, like every admin command.

use std::fmt::Write as _;
use std::io::Write;

use chrono::{DateTime, Utc};
use mercury_schema::ProjectId;
use serde_json::json;
use sqlx::SqliteExecutor;

use super::ProbeStatus;
use crate::admin::AdminError;
use crate::audit;
use crate::cli::ProbeCommand;
use crate::db::{self, Db};

/// A probe as `probe list` shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeLine {
    pub project: String,
    /// At the time of the listing: a healthy probe silent for three minutes shows as stale.
    pub status: ProbeStatus,
    pub access: String,
    pub access_limited_by: Option<String>,
    pub last_seen_at: String,
    pub quarantine_reason: Option<String>,
}

/// What `probe quarantine` or `probe release` did.
#[derive(Debug, PartialEq, Eq)]
pub enum Switched {
    Now,
    Already,
}

fn project_id(text: &str) -> Result<String, AdminError> {
    ProjectId::parse(text.trim())
        .map(|id| id.as_str().to_owned())
        .map_err(|_| {
            AdminError::Invalid("a project id is lower-case letters, digits and '-', like delta".into())
        })
}

fn no_probe() -> AdminError {
    AdminError::Invalid("no probe has registered for that project".into())
}

/// `probe list`: every probe that has registered, by project.
pub async fn list(db: &Db, now: DateTime<Utc>) -> Result<Vec<ProbeLine>, AdminError> {
    Ok(sqlx::query!(
        "SELECT project, status, access, access_limited_by, last_seen_at, quarantine_reason
         FROM probes ORDER BY project"
    )
    .fetch_all(db.pool())
    .await?
    .into_iter()
    .map(|row| ProbeLine {
        status: ProbeStatus::parse(&row.status)
            .unwrap_or(ProbeStatus::Stale)
            .at(&row.last_seen_at, now),
        project: row.project,
        access: row.access,
        access_limited_by: row.access_limited_by,
        last_seen_at: row.last_seen_at,
        quarantine_reason: row.quarantine_reason,
    })
    .collect())
}

/// `probe quarantine`: the core stops calling the probe until it is released. The switch and its
/// audit row are one transaction.
pub async fn quarantine(
    db: &Db,
    project: &str,
    reason: &str,
    now: DateTime<Utc>,
) -> Result<Switched, AdminError> {
    let project = project_id(project)?;
    let reason = reason.trim();
    if reason.is_empty() || reason.chars().count() > 200 || reason.chars().any(char::is_control) {
        return Err(AdminError::Invalid(
            "the reason must be 1 to 200 characters on one line".into(),
        ));
    }
    let stored = format!("an admin: {reason}");
    let quarantined = ProbeStatus::Quarantined.as_str();
    let mut tx = db.pool().begin_with("BEGIN IMMEDIATE").await?;
    let changed = sqlx::query!(
        "UPDATE probes SET status = ?, quarantine_reason = ? WHERE project = ? AND status <> ?",
        quarantined,
        stored,
        project,
        quarantined
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    let switched = if changed == 1 {
        Switched::Now
    } else {
        known(&mut *tx, &project).await?;
        Switched::Already
    };
    let id = ProjectId::parse(&project).ok();
    let payload = json!({ "by": "admin", "reason": reason, "already": switched == Switched::Already });
    audit::action_in(&mut tx, "probe_quarantined", id.as_ref(), &payload, now).await?;
    tx.commit().await?;
    Ok(switched)
}

/// `probe release`: the core may call the probe again. It counts as unchecked (`stale`) until
/// its next registration passes the tools check. The switch and its audit row are one transaction.
pub async fn release(db: &Db, project: &str, now: DateTime<Utc>) -> Result<Switched, AdminError> {
    let project = project_id(project)?;
    let (quarantined, stale) = (ProbeStatus::Quarantined.as_str(), ProbeStatus::Stale.as_str());
    let mut tx = db.pool().begin_with("BEGIN IMMEDIATE").await?;
    let changed = sqlx::query!(
        "UPDATE probes SET status = ?, quarantine_reason = NULL WHERE project = ? AND status = ?",
        stale,
        project,
        quarantined
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    let switched = if changed == 1 {
        Switched::Now
    } else {
        known(&mut *tx, &project).await?;
        Switched::Already
    };
    let id = ProjectId::parse(&project).ok();
    let payload = json!({ "already": switched == Switched::Already });
    audit::action_in(&mut tx, "probe_released", id.as_ref(), &payload, now).await?;
    tx.commit().await?;
    Ok(switched)
}

async fn known<'e>(conn: impl SqliteExecutor<'e>, project: &str) -> Result<(), AdminError> {
    sqlx::query!("SELECT project FROM probes WHERE project = ?", project)
        .fetch_optional(conn)
        .await?
        .map(|_| ())
        .ok_or_else(no_probe)
}

/// A stored time as `2026-10-02 09:31`, in UTC.
fn short(text: &str) -> String {
    db::parse_time(text).map_or_else(|| text.to_owned(), |at| at.format("%Y-%m-%d %H:%M").to_string())
}

/// The `probe list` table.
pub fn render(lines: &[ProbeLine]) -> String {
    if lines.is_empty() {
        return "No probe has registered.\n".into();
    }
    let rows: Vec<[String; 5]> = lines
        .iter()
        .map(|line| {
            [
                line.project.clone(),
                line.status.as_str().to_owned(),
                match &line.access_limited_by {
                    Some(by) => format!("{} (limited by {by})", line.access),
                    None => line.access.clone(),
                },
                short(&line.last_seen_at),
                line.quarantine_reason.clone().unwrap_or_default(),
            ]
        })
        .collect();
    let header = ["PROJECT", "STATUS", "ACCESS", "LAST SEEN", "QUARANTINED FOR"];
    let mut widths = header.map(|cell| cell.chars().count());
    for row in &rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let mut out = String::new();
    for row in std::iter::once(header.map(str::to_owned)).chain(rows) {
        let mut text = String::new();
        for (cell, width) in row.iter().zip(widths) {
            let _ = write!(text, "{cell:<width$}  ");
        }
        out.push_str(text.trim_end());
        out.push('\n');
    }
    out
}

/// Runs a `probe` command at `now`; what it shows goes to `out`.
pub async fn run(
    command: ProbeCommand,
    db: &Db,
    out: &mut dyn Write,
    now: DateTime<Utc>,
) -> Result<(), AdminError> {
    // each command leaves one audit row without a question, the project in its column
    match command {
        ProbeCommand::List => {
            let lines = list(db, now).await?;
            let payload = json!({ "probes": lines.len() });
            audit::action(db, "probes_listed", None, &payload, now).await?;
            write!(out, "{}", render(&lines))?;
        }
        ProbeCommand::Quarantine { project, reason } => {
            let switched = quarantine(db, &project, &reason, now).await?;
            match switched {
                Switched::Now => writeln!(out, "quarantined the probe of {}", project.trim())?,
                Switched::Already => {
                    writeln!(out, "the probe of {} was quarantined already", project.trim())?
                }
            }
        }
        ProbeCommand::Release { project } => {
            let switched = release(db, &project, now).await?;
            match switched {
                Switched::Now => writeln!(
                    out,
                    "released the probe of {}; it is checked at its next registration",
                    project.trim()
                )?,
                Switched::Already => writeln!(out, "the probe of {} isn't quarantined", project.trim())?,
            }
        }
    }
    Ok(())
}
