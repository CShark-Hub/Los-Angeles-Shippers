//! `admin status`: what an admin looks at daily. The alerts that hold, the probes and their state,
//! the backend of each of the core's model roles, today's spend (UTC) against the caps, and which
//! judge answered the questions that ended in the last 24 hours, counted from their last audit
//! rows; with `--since`, the questions of a window by channel and outcome. `--check` says only the
//! alerts, for cron. It reads the database, `models.yaml` and `projects.yaml` only and asks no
//! model and no probe, so it answers the same while the models are down.

use std::fmt::Write as _;
use std::io::Write;
use std::path::Path;

use chrono::{DateTime, TimeDelta, Utc};
use mercury_schema::ProjectId;
use mercury_schema::config::{JudgeKind, ModelsConfig};
use serde_json::{Value, json};

use crate::admin::AdminError;
use crate::alerts::{self, Alert};
use crate::audit::{self, AuditError};
use crate::config::{self, Problem};
use crate::db::{self, Db};
use crate::orchestrator::budget::{self, EVERYONE};
use crate::probes::admin::{self as probes, ProbeLine};

/// How far back the judge modes are counted.
pub const WINDOW: TimeDelta = TimeDelta::hours(24);

/// A day's spend at this share of its cap or more is called out.
pub const NEAR_THE_CAP: f64 = 0.8;

/// How the questions that ended in the window were judged: by the weakest judge any stage had, as
/// their last row says; `none` when no judge answered (a project the policy refused).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modes {
    pub jev: u64,
    pub chat: u64,
    pub rules: u64,
    pub none: u64,
}

impl Modes {
    pub fn total(&self) -> u64 {
        self.jev + self.chat + self.rules + self.none
    }

    fn count(&mut self, payload: Option<&str>) {
        match mode_of(payload).as_deref() {
            Some("jev") => self.jev += 1,
            Some("chat") => self.chat += 1,
            Some("rules") => self.rules += 1,
            _ => self.none += 1,
        }
    }
}

/// The judge a question's ending names in its audit payload, if it names one.
pub(crate) fn mode_of(payload: Option<&str>) -> Option<String> {
    payload
        .and_then(|json| serde_json::from_str::<Value>(json).ok())
        .and_then(|data| data.get("judge_mode").and_then(Value::as_str).map(str::to_owned))
}

/// What became of a question: each one is in exactly one of these. The admin panel's overview
/// counts by the same rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Delivered, judged by Jev.
    Answered,
    Refused,
    /// Delivered, judged by `chat` or `rules`.
    Degraded,
    /// It went to a sign-off, whatever was decided.
    Held,
    /// A clarify, a failure, or still running.
    Other,
}

impl Outcome {
    /// From the question's `status` and `judge_mode`, and whether the audit has its
    /// `pending_signoff` row: a hold wins, then a refusal.
    pub(crate) fn of(status: &str, judge_mode: Option<&str>, held: bool) -> Self {
        match (held, status) {
            (true, _) => Outcome::Held,
            (false, "refused") => Outcome::Refused,
            (false, "delivered") if matches!(judge_mode, Some("chat" | "rules")) => Outcome::Degraded,
            (false, "delivered") => Outcome::Answered,
            (false, _) => Outcome::Other,
        }
    }
}

/// The questions of one channel, by outcome.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Outcomes {
    pub answered: u64,
    pub refused: u64,
    pub degraded: u64,
    pub held: u64,
    pub other: u64,
}

impl Outcomes {
    pub fn total(&self) -> u64 {
        self.answered + self.refused + self.degraded + self.held + self.other
    }

    fn add(&mut self, outcome: Outcome, n: u64) {
        let slot = match outcome {
            Outcome::Answered => &mut self.answered,
            Outcome::Refused => &mut self.refused,
            Outcome::Degraded => &mut self.degraded,
            Outcome::Held => &mut self.held,
            Outcome::Other => &mut self.other,
        };
        *slot += n;
    }

    fn row(&self, name: &str) -> Vec<String> {
        vec![
            name.to_owned(),
            self.answered.to_string(),
            self.refused.to_string(),
            self.degraded.to_string(),
            self.held.to_string(),
            self.other.to_string(),
            self.total().to_string(),
        ]
    }
}

/// The questions asked since a time, by channel (`cli`, `web`, `mcp`, then any other the rows
/// name) and outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    pub since: DateTime<Utc>,
    pub channels: Vec<(String, Outcomes)>,
}

impl Window {
    pub fn total(&self) -> Outcomes {
        let mut all = Outcomes::default();
        for (_, outcomes) in &self.channels {
            all.answered += outcomes.answered;
            all.refused += outcomes.refused;
            all.degraded += outcomes.degraded;
            all.held += outcomes.held;
            all.other += outcomes.other;
        }
        all
    }
}

/// What `status` was asked beyond the daily view.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Options {
    /// Say only the alerts that hold, and fail when one does.
    pub check: bool,
    /// Count the questions asked since this time, by channel and outcome.
    pub since: Option<DateTime<Utc>>,
}

/// The questions created since `since`, counted by channel and outcome from their rows and the
/// audit's `pending_signoff` rows.
async fn window(db: &Db, since: DateTime<Utc>) -> Result<Window, AdminError> {
    let from = db::time(since);
    let rows = sqlx::query!(
        r#"SELECT q.channel AS "channel!", q.status AS "status!", q.judge_mode AS "judge_mode?",
                  EXISTS (SELECT 1 FROM audit_events a
                          WHERE a.question_id = q.id AND a.type = 'pending_signoff') AS "held!: bool",
                  COUNT(*) AS "n!: i64"
           FROM questions q WHERE q.created_at >= ?
           GROUP BY 1, 2, 3, 4"#,
        from
    )
    .fetch_all(db.pool())
    .await?;
    let mut channels: Vec<(String, Outcomes)> = ["cli", "web", "mcp"]
        .iter()
        .map(|name| ((*name).to_owned(), Outcomes::default()))
        .collect();
    for row in rows {
        let outcome = Outcome::of(&row.status, row.judge_mode.as_deref(), row.held);
        let n = u64::try_from(row.n).unwrap_or(0);
        match channels.iter_mut().find(|(name, _)| *name == row.channel) {
            Some((_, outcomes)) => outcomes.add(outcome, n),
            None => {
                let mut outcomes = Outcomes::default();
                outcomes.add(outcome, n);
                channels.push((row.channel, outcomes));
            }
        }
    }
    Ok(Window { since, channels })
}

/// One person's spend of the day: their e-mail address (or their id, if they are gone).
#[derive(Debug, Clone, PartialEq)]
pub struct Spender {
    pub who: String,
    pub usd: f64,
}

/// Everything `status` shows.
#[derive(Debug)]
pub struct Status {
    /// The alerts that hold now.
    pub alerts: Vec<Alert>,
    /// What the alerts couldn't look at, one line each: a file they are judged against that can't
    /// be read.
    pub unchecked: Vec<String>,
    pub probes: Vec<ProbeLine>,
    /// `models.yaml`, or what is wrong with it.
    pub models: Result<ModelsConfig, Vec<Problem>>,
    /// The UTC day the spend is counted on.
    pub day: String,
    pub everyone: f64,
    /// The people who spent anything today, the most first.
    pub people: Vec<Spender>,
    pub modes: Modes,
    /// The questions since the time `--since` named, if it named one.
    pub window: Option<Window>,
}

/// The daily budget the spend alert is judged against: `global_daily_usd`, if `models.yaml` can be
/// read.
fn daily_budget(models: &Result<ModelsConfig, Vec<Problem>>) -> Option<f64> {
    models
        .as_ref()
        .ok()
        .map(|models| models.budgets.global_daily_usd.get())
}

/// The projects the alerts expect a probe for: the ids `projects.yaml` lists, if it can be read.
fn listed(projects: &Result<Vec<ProjectId>, Vec<Problem>>) -> Option<&[ProjectId]> {
    projects.as_ref().ok().map(Vec::as_slice)
}

/// What the alerts can't look at, one line each, with what is wrong in short: the spend when
/// `models.yaml` can't be read, the probes that never registered when `projects.yaml` can't. A
/// check that can't look is never a pass.
fn unchecked(
    models: &Result<ModelsConfig, Vec<Problem>>,
    projects: &Result<Vec<ProjectId>, Vec<Problem>>,
) -> Vec<String> {
    let mut lines = Vec::new();
    if let Err(problems) = models {
        lines.push(format!("spend: can't be checked: {}", in_short(problems)));
    }
    if let Err(problems) = projects {
        lines.push(format!(
            "probes: those that never registered can't be checked: {}",
            in_short(problems)
        ));
    }
    lines
}

/// A file's problems on one line.
fn in_short(problems: &[Problem]) -> String {
    let problems: Vec<String> = problems.iter().map(ToString::to_string).collect();
    problems.join("; ")
}

/// How the questions that ended since `since` were judged, from the audit rows that end them: each
/// holds what it cost and which judge answered.
pub(crate) async fn modes_since(db: &Db, since: DateTime<Utc>) -> Result<Modes, sqlx::Error> {
    let last_day = db::time(since);
    let endings = sqlx::query_scalar!(
        "SELECT payload_json FROM audit_events
         WHERE question_id IS NOT NULL AND created_at >= ?
           AND type IN ('done', 'refused', 'clarify', 'error')",
        last_day
    )
    .fetch_all(db.pool())
    .await?;
    let mut modes = Modes::default();
    for payload in &endings {
        modes.count(payload.as_deref());
    }
    Ok(modes)
}

/// Reads what `status` shows at `now`; the questions since `since`, if asked.
pub async fn read(
    db: &Db,
    config_dir: &Path,
    now: DateTime<Utc>,
    since: Option<DateTime<Utc>>,
) -> Result<Status, AdminError> {
    let models = config::models(config_dir);
    let projects = config::project_ids(config_dir);
    let day = budget::day(now);
    let rows = sqlx::query!(
        r#"SELECT s.user_id, s.spent_usd, u.email AS "email?" FROM spend s
           LEFT JOIN users u ON u.id = s.user_id
           WHERE s.day = ? ORDER BY s.spent_usd DESC, s.user_id"#,
        day
    )
    .fetch_all(db.pool())
    .await?;
    let mut everyone = 0.0;
    let mut people = Vec::new();
    for row in rows {
        if row.user_id == EVERYONE {
            everyone = row.spent_usd;
        } else {
            people.push(Spender {
                who: row.email.unwrap_or(row.user_id),
                usd: row.spent_usd,
            });
        }
    }
    let modes = modes_since(db, now - WINDOW).await?;
    let window = match since {
        Some(since) => Some(window(db, since).await?),
        None => None,
    };
    Ok(Status {
        alerts: alerts::check(db, daily_budget(&models), listed(&projects), now).await?,
        unchecked: unchecked(&models, &projects),
        probes: probes::list(db, now).await?,
        models,
        day,
        everyone,
        people,
        modes,
        window,
    })
}

fn usd(amount: f64) -> String {
    format!("${amount:.4}")
}

fn cap(amount: f64) -> String {
    format!("${amount:.2}")
}

/// A share as a whole percentage, rounded down, so 79.9% never reads as 80%.
pub(crate) fn percent(part: f64, whole: f64) -> String {
    if whole <= 0.0 {
        return "-".to_owned();
    }
    format!("{}%", ((part / whole) * 100.0).floor())
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
        let mut line = String::new();
        for (cell, width) in row.iter().zip(&widths) {
            let _ = write!(line, "{cell:<width$}  ");
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

fn backends(models: &ModelsConfig) -> String {
    let roles = &models.roles;
    let mut rows = vec![vec![
        "judge".to_owned(),
        roles.judge.backend.as_str().to_owned(),
        kind(roles.judge.kind).to_owned(),
        roles.judge.model.as_str().to_owned(),
    ]];
    if let Some(fallback) = &roles.judge_fallback {
        rows.push(vec![
            "judge_fallback".to_owned(),
            fallback.backend.as_str().to_owned(),
            kind(fallback.kind).to_owned(),
            fallback.model.as_str().to_owned(),
        ]);
    }
    for (name, role) in [("planner", &roles.planner), ("composer", &roles.composer)] {
        rows.push(vec![
            name.to_owned(),
            role.backend.as_str().to_owned(),
            "chat".to_owned(),
            role.model.as_str().to_owned(),
        ]);
    }
    table(&["ROLE", "BACKEND", "KIND", "MODEL"], &rows)
}

fn kind(kind: JudgeKind) -> &'static str {
    match kind {
        JudgeKind::Jev => "jev",
        JudgeKind::Chat => "chat",
    }
}

fn spend_line(who: &str, spent: f64, limit: Option<f64>) -> Vec<String> {
    let (limit_text, share) = match limit {
        // a cap of 0 has no share: every question it counts is refused
        Some(limit) if limit <= 0.0 => (cap(limit), "a cap of 0, nothing can be asked".to_owned()),
        Some(limit) => {
            let mut share = percent(spent, limit);
            if spent >= limit * NEAR_THE_CAP {
                share.push_str(", 80% of the cap or more");
            }
            (cap(limit), share)
        }
        None => ("-".to_owned(), "-".to_owned()),
    };
    vec![who.to_owned(), usd(spent), limit_text, share]
}

/// The alerts that hold, one line each, then what they couldn't look at.
fn alert_lines(alerts: &[Alert], unchecked: &[String]) -> String {
    let mut out = String::new();
    for alert in alerts {
        let _ = writeln!(out, "{}", alert.line());
    }
    for line in unchecked {
        let _ = writeln!(out, "{line}");
    }
    out
}

/// The questions of a window, by channel and outcome.
fn window_view(window: &Window) -> String {
    let all = window.total();
    let mut out = format!(
        "\nQuestions since {} (UTC): {}\n",
        window.since.format("%Y-%m-%d %H:%M"),
        all.total()
    );
    let mut rows: Vec<Vec<String>> = window.channels.iter().map(|(name, n)| n.row(name)).collect();
    rows.push(all.row("all"));
    out.push_str(&table(
        &[
            "CHANNEL", "ANSWERED", "REFUSED", "DEGRADED", "HELD", "OTHER", "TOTAL",
        ],
        &rows,
    ));
    out.push_str(
        "Answered: delivered by Jev. Degraded: delivered by chat or rules. Held: went to a sign-off. \
         Other: a clarify, a failure, or still running.\n",
    );
    out
}

/// The `status` view.
pub fn render(status: &Status) -> String {
    let mut out = String::from("Alerts\n");
    if status.alerts.is_empty() && status.unchecked.is_empty() {
        out.push_str("No alert holds.\n");
    } else {
        out.push_str(&alert_lines(&status.alerts, &status.unchecked));
    }

    out.push_str("\nProbes\n");
    out.push_str(&probes::render(&status.probes));

    out.push_str("\nModel backends\n");
    let budgets = match &status.models {
        Ok(models) => {
            out.push_str(&backends(models));
            Some(models.budgets)
        }
        Err(problems) => {
            for problem in problems {
                let _ = writeln!(out, "models.yaml can't be read: {problem}");
            }
            None
        }
    };

    let _ = writeln!(out, "\nSpend on {} (UTC)", status.day);
    let mut rows = vec![spend_line(
        "everyone",
        status.everyone,
        budgets.map(|b| b.global_daily_usd.get()),
    )];
    for person in &status.people {
        rows.push(spend_line(
            &person.who,
            person.usd,
            budgets.map(|b| b.per_user_daily_usd.get()),
        ));
    }
    out.push_str(&table(&["WHO", "SPENT", "CAP", "SHARE"], &rows));
    if let Some(budgets) = budgets {
        let _ = writeln!(
            out,
            "A question may cost at most {}.",
            cap(budgets.per_question_usd.get())
        );
    }

    let modes = status.modes;
    let total = modes.total();
    let _ = writeln!(
        out,
        "\nJudge modes of the questions that ended in the last 24 hours: {total}"
    );
    if total > 0 {
        let whole = total as f64;
        let rows: Vec<Vec<String>> = [
            ("jev", modes.jev),
            ("chat", modes.chat),
            ("rules", modes.rules),
            ("none", modes.none),
        ]
        .iter()
        .map(|(mode, n)| vec![(*mode).to_owned(), n.to_string(), percent(*n as f64, whole)])
        .collect();
        out.push_str(&table(&["MODE", "QUESTIONS", "SHARE"], &rows));
        let _ = writeln!(
            out,
            "Degraded (chat or rules): {}; none: nothing was judged.",
            percent((modes.chat + modes.rules) as f64, whole)
        );
    }
    if let Some(window) = &status.window {
        out.push_str(&window_view(window));
    }
    out
}

/// The audit row `status` writes before it shows anything. A full disk is said as such.
async fn record(db: &Db, payload: &Value, now: DateTime<Utc>) -> Result<(), AdminError> {
    match audit::action(db, "status_shown", None, payload, now).await {
        Ok(_) => Ok(()),
        Err(err) if is_disk_full(&err) => Err(AdminError::DiskFull),
        Err(err) => Err(err.into()),
    }
}

/// SQLite's `SQLITE_FULL` (13): "database or disk is full".
fn is_disk_full(err: &AuditError) -> bool {
    match err {
        AuditError::Write(sqlx::Error::Database(error)) => {
            error.code().as_deref() == Some("13") || error.message().contains("disk is full")
        }
        _ => false,
    }
}

/// `admin status` at `now`: the view to `out`, and one audit row without a question saying what it
/// counted. The row comes first: no audit, no answer, so a disk too full for the row shows nothing
/// and says so.
///
/// With `options.check` only the alerts are said, one line each, and the command fails when there
/// is one; when none holds it shows nothing and writes nothing. What the check can't look at (a
/// file it can't read) is a line of its own and fails it the same way: a check that can't look is
/// never a pass.
pub async fn run(
    db: &Db,
    config_dir: &Path,
    out: &mut dyn Write,
    now: DateTime<Utc>,
    options: Options,
) -> Result<(), AdminError> {
    if options.check {
        let models = config::models(config_dir);
        let projects = config::project_ids(config_dir);
        let found = alerts::check(db, daily_budget(&models), listed(&projects), now).await?;
        let unchecked = unchecked(&models, &projects);
        if found.is_empty() && unchecked.is_empty() {
            return Ok(());
        }
        let payload = json!({ "check": true, "alerts": found.len(), "unchecked": unchecked.len() });
        record(db, &payload, now).await?;
        write!(out, "{}", alert_lines(&found, &unchecked))?;
        return Err(AdminError::Alerts(found.len() + unchecked.len()));
    }
    let status = read(db, config_dir, now, options.since).await?;
    let payload = json!({
        "probes": status.probes.len(),
        "questions": status.modes.total(),
        "alerts": status.alerts.len(),
    });
    record(db, &payload, now).await?;
    write!(out, "{}", render(&status))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use mercury_schema::api::{ErrorCode, Refusal, RefusalCode, Usd};
    use mercury_schema::events::{ComposerMode, Done, Event, EventError, Refused};
    use mercury_schema::{JudgeMode, QuestionId};

    use super::*;
    use crate::db::testing;

    fn at(text: &str) -> DateTime<Utc> {
        db::parse_time(text).unwrap()
    }

    const NOW: &str = "2026-10-05T12:00:00.000Z";

    fn examples() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config-examples")
    }

    /// A configuration directory with the examples' `models.yaml` and `projects.yaml`, each line
    /// `from` of `models.yaml` written as `to`.
    fn examples_with(changes: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let mut models = std::fs::read_to_string(examples().join(config::MODELS)).unwrap();
        for (from, to) in changes {
            assert!(models.contains(from), "{from} in models.yaml");
            models = models.replace(from, to);
        }
        std::fs::write(dir.path().join(config::MODELS), models).unwrap();
        std::fs::copy(
            examples().join(config::PROJECTS),
            dir.path().join(config::PROJECTS),
        )
        .unwrap();
        dir
    }

    async fn ended(db: &Db, n: u32, event: Event, when: &str) {
        let id = QuestionId::parse(&format!("q_01J9Z3QK7M8N9P0R1S2T3V{n:04}")).unwrap();
        let _recorded = audit::record(db, &id, None, event, at(when)).await.unwrap();
    }

    fn done(mode: JudgeMode, composer: ComposerMode) -> Event {
        Event::Done(Done {
            duration_ms: 1_200,
            cost_usd: Usd::new(0.004).unwrap(),
            judge_mode: mode,
            composer,
            audit_id: 1,
        })
    }

    fn refused(mode: Option<JudgeMode>) -> Event {
        let refusal = Refusal {
            code: RefusalCode::ForbiddenProject,
            message: "You can't ask about that project.".to_owned(),
            hint: "Ask about delta.".to_owned(),
        };
        Event::Refused(Refused::new(refusal, Usd::default(), mode))
    }

    async fn seeded() -> (tempfile::TempDir, Db) {
        let (dir, db) = testing::db().await;
        let at = db::time(at(NOW));
        sqlx::query(
            "INSERT INTO users (id, email, display_name, role, created_at)
             VALUES ('u_1', 'pm@example.com', 'Pat Example', 'pm', ?)",
        )
        .bind(&at)
        .execute(db.pool())
        .await
        .unwrap();
        for (who, usd) in [("u_1", 0.85), ("*", 1.25)] {
            sqlx::query("INSERT INTO spend (day, user_id, spent_usd) VALUES ('2026-10-05', ?, ?)")
                .bind(who)
                .bind(usd)
                .execute(db.pool())
                .await
                .unwrap();
        }
        // yesterday's spend is another day's
        sqlx::query("INSERT INTO spend (day, user_id, spent_usd) VALUES ('2026-10-04', 'u_1', 0.99)")
            .execute(db.pool())
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO probes (project, url, access, access_limited_by, status, schema, manifest_hash,
                                 manifest_json, tools_json, registered_at, last_seen_at)
             VALUES ('delta', 'http://probe-delta:7000/mcp', 'code', NULL, 'healthy', 'mercury.facts/0.1',
                     'h', '{}', '[]', '2026-10-05T11:59:00.000Z', '2026-10-05T11:59:00.000Z')",
        )
        .execute(db.pool())
        .await
        .unwrap();
        // the last day's endings, and one before it
        ended(
            &db,
            1,
            done(JudgeMode::Jev, ComposerMode::Model),
            "2026-10-05T09:00:00.000Z",
        )
        .await;
        ended(
            &db,
            2,
            done(JudgeMode::Jev, ComposerMode::Model),
            "2026-10-05T10:00:00.000Z",
        )
        .await;
        ended(
            &db,
            3,
            done(JudgeMode::Rules, ComposerMode::Template),
            "2026-10-05T11:00:00.000Z",
        )
        .await;
        ended(&db, 4, refused(None), "2026-10-05T11:30:00.000Z").await;
        let error = Event::Error(EventError {
            code: ErrorCode::BudgetExceeded,
            message: "The question reached its spending limit, so it was stopped.".to_owned(),
            cost_usd: Usd::new(0.05).unwrap(),
            judge_mode: Some(JudgeMode::Chat),
        });
        ended(&db, 5, error, "2026-10-04T13:00:00.000Z").await;
        ended(
            &db,
            6,
            done(JudgeMode::Jev, ComposerMode::Model),
            "2026-10-04T11:59:00.000Z",
        )
        .await;
        (dir, db)
    }

    #[tokio::test]
    async fn the_status_shows_the_probes_the_backends_the_days_spend_and_the_judge_modes() {
        let (_dir, db) = seeded().await;
        let mut out = Vec::new();
        run(&db, &examples(), &mut out, at(NOW), Options::default())
            .await
            .unwrap();
        let text = String::from_utf8(out).unwrap();

        // the alerts come first: the last question Jev judged is the one at 10:00; the next judged
        // one was rules, at 11:00, and an hour is more than five minutes
        assert!(
            text.starts_with(
                "Alerts\njudge: answering in a degraded mode (rules) for 60 minutes, \
                 since 2026-10-05 11:00 UTC; no question has ended on Jev since\n\nProbes\n"
            ),
            "{text}"
        );
        // the probe as `probe list` shows it
        assert!(text.contains("delta    healthy  code"), "{text}");
        // every role of the examples, with its backend
        for line in [
            "judge           openrouter  jev   typesafe/jev-1.13",
            "judge_fallback  openrouter  chat  openai/gpt-4.1-mini-2025-04-14",
            "planner         openrouter  chat  openai/gpt-4.1-mini-2025-04-14",
            "composer        openrouter  chat  anthropic/claude-4.6-sonnet-20260217",
        ] {
            assert!(text.contains(line), "{line} in {text}");
        }
        // today's rows against their caps; yesterday's isn't counted
        assert!(text.contains("Spend on 2026-10-05 (UTC)"), "{text}");
        assert!(text.contains("everyone        $1.2500  $2.00  62%"), "{text}");
        assert!(
            text.contains("pm@example.com  $0.8500  $1.00  85%, 80% of the cap or more"),
            "{text}"
        );
        assert!(!text.contains("0.99"), "{text}");
        assert!(text.contains("A question may cost at most $0.15."), "{text}");
        // the five endings of the last 24 hours, by the judge their row names
        assert!(
            text.contains("Judge modes of the questions that ended in the last 24 hours: 5"),
            "{text}"
        );
        for line in [
            "jev    2          40%",
            "chat   1          20%",
            "rules  1          20%",
            "none   1          20%",
        ] {
            assert!(text.contains(line), "{line} in {text}");
        }
        assert!(text.contains("Degraded (chat or rules): 40%"), "{text}");

        // one audit row, without a question
        let rows: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT type, question_id, payload_json FROM audit_events WHERE type = 'status_shown'",
        )
        .fetch_all(db.pool())
        .await
        .unwrap();
        assert_eq!(rows.len(), 1);
        let (_, question, payload) = &rows[0];
        assert_eq!(question, &None);
        let payload: Value = serde_json::from_str(payload.as_deref().unwrap()).unwrap();
        assert_eq!(payload, json!({ "probes": 1, "questions": 5, "alerts": 1 }));
    }

    #[tokio::test]
    async fn a_models_file_that_cant_be_read_still_shows_the_rest() {
        let (_dir, db) = seeded().await;
        let empty = tempfile::tempdir().unwrap();
        let mut out = Vec::new();
        run(&db, empty.path(), &mut out, at(NOW), Options::default())
            .await
            .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("models.yaml can't be read: models.yaml: the file is missing"),
            "{text}"
        );
        assert!(
            text.contains("\nspend: can't be checked: models.yaml: the file is missing\n"),
            "the alerts say what they couldn't look at: {text}"
        );
        assert!(text.contains("delta    healthy  code"), "{text}");
        assert!(text.contains("everyone        $1.2500  -    -"), "{text}");
        assert!(text.contains("rules  1"), "{text}");
    }

    #[tokio::test]
    async fn a_daily_budget_of_zero_is_an_alert_and_has_no_share() {
        let (_dir, db) = seeded().await;
        let zero = examples_with(&[
            ("global_daily_usd: 2.00", "global_daily_usd: 0"),
            ("per_user_daily_usd: 1.00", "per_user_daily_usd: 0"),
        ]);
        let mut out = Vec::new();
        let check = Options {
            check: true,
            since: None,
        };
        let failed = run(&db, zero.path(), &mut out, at(NOW), check).await.unwrap_err();
        assert!(matches!(failed, AdminError::Alerts(2)), "{failed:?}");
        let text = String::from_utf8(out).unwrap();
        assert_eq!(
            text.lines().next(),
            Some("spend: the daily budget is $0.00, so nothing can be asked ($1.25 spent, 2026-10-05 UTC)"),
            "{text}"
        );

        let mut out = Vec::new();
        run(&db, zero.path(), &mut out, at(NOW), Options::default())
            .await
            .unwrap();
        let view = String::from_utf8(out).unwrap();
        assert!(view.starts_with(&format!("Alerts\n{text}\nProbes\n")), "{view}");
        for line in [
            "everyone        $1.2500  $0.00  a cap of 0, nothing can be asked",
            "pm@example.com  $0.8500  $0.00  a cap of 0, nothing can be asked",
        ] {
            assert!(view.contains(line), "{line} in {view}");
        }
    }

    #[tokio::test]
    async fn a_quiet_day_shows_nothing_spent_and_no_question() {
        let (_dir, db) = testing::db().await;
        let mut out = Vec::new();
        run(&db, &examples(), &mut out, at(NOW), Options::default())
            .await
            .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("Alerts\nNo alert holds.\n\nProbes\n"), "{text}");
        assert!(text.contains("No probe has registered."), "{text}");
        assert!(text.contains("everyone  $0.0000  $2.00  0%"), "{text}");
        assert!(
            text.ends_with("Judge modes of the questions that ended in the last 24 hours: 0\n"),
            "{text}"
        );
    }

    async fn status_rows(db: &Db) -> Vec<Value> {
        let payloads: Vec<Option<String>> = sqlx::query_scalar(
            "SELECT payload_json FROM audit_events WHERE type = 'status_shown' ORDER BY seq",
        )
        .fetch_all(db.pool())
        .await
        .unwrap();
        payloads
            .iter()
            .map(|payload| serde_json::from_str(payload.as_deref().unwrap()).unwrap())
            .collect()
    }

    #[tokio::test]
    async fn the_check_says_only_the_alerts_and_fails_while_one_holds() {
        let (_dir, db) = seeded().await;
        let mut out = Vec::new();
        let check = Options {
            check: true,
            since: None,
        };
        let failed = run(&db, &examples(), &mut out, at(NOW), check).await.unwrap_err();
        assert!(matches!(failed, AdminError::Alerts(1)), "{failed:?}");
        assert_eq!(failed.to_string(), "1 alert(s) hold");
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "judge: answering in a degraded mode (rules) for 60 minutes, since 2026-10-05 11:00 UTC; \
             no question has ended on Jev since\n"
        );
        // what it showed is audited, first
        assert_eq!(
            status_rows(&db).await,
            vec![json!({ "check": true, "alerts": 1, "unchecked": 0 })]
        );

        // every threshold at once, one line each: the spend (everyone's, at 90%), the judge, a probe
        sqlx::query("UPDATE spend SET spent_usd = 1.8 WHERE day = '2026-10-05' AND user_id = '*'")
            .execute(db.pool())
            .await
            .unwrap();
        sqlx::query("UPDATE probes SET last_seen_at = '2026-10-05T11:30:00.000Z'")
            .execute(db.pool())
            .await
            .unwrap();
        let mut out = Vec::new();
        let failed = run(&db, &examples(), &mut out, at(NOW), check).await.unwrap_err();
        assert!(matches!(failed, AdminError::Alerts(3)), "{failed:?}");
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "{text}");
        assert_eq!(
            lines[0],
            "spend: 90% of the daily budget is spent ($1.80 of $2.00, 2026-10-05 UTC)"
        );
        assert!(
            lines[1].starts_with("judge: answering in a degraded mode (rules) for 60 minutes"),
            "{text}"
        );
        assert_eq!(
            lines[2],
            "probe delta: silent for 30 minutes, last seen 2026-10-05 11:30 UTC"
        );

        // the full view lists the same three
        let mut out = Vec::new();
        run(&db, &examples(), &mut out, at(NOW), Options::default())
            .await
            .unwrap();
        let view = String::from_utf8(out).unwrap();
        assert!(view.starts_with(&format!("Alerts\n{text}\nProbes\n")), "{view}");
    }

    #[tokio::test]
    async fn the_check_with_nothing_to_say_says_and_writes_nothing() {
        let (_dir, db) = testing::db().await;
        let mut out = Vec::new();
        let check = Options {
            check: true,
            since: None,
        };
        run(&db, &examples(), &mut out, at(NOW), check).await.unwrap();
        assert!(out.is_empty());
        assert_eq!(
            status_rows(&db).await,
            Vec::<Value>::new(),
            "it showed nothing: no row"
        );
    }

    #[tokio::test]
    async fn a_check_that_cant_be_audited_shows_nothing() {
        let (_dir, db) = seeded().await;
        sqlx::query(
            "CREATE TRIGGER no_status BEFORE INSERT ON audit_events WHEN NEW.type = 'status_shown' \
             BEGIN SELECT RAISE(ABORT, 'the disk is full'); END",
        )
        .execute(db.pool())
        .await
        .unwrap();
        let mut out = Vec::new();
        let check = Options {
            check: true,
            since: None,
        };
        let failed = run(&db, &examples(), &mut out, at(NOW), check).await.unwrap_err();
        assert!(matches!(failed, AdminError::DiskFull), "{failed:?}");
        assert!(out.is_empty(), "nothing shown without its row");
    }

    /// A configuration directory with the examples' `projects.yaml` and the given `models.yaml`
    /// text, or none.
    fn models_only(text: Option<&str>) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::copy(
            examples().join(config::PROJECTS),
            dir.path().join(config::PROJECTS),
        )
        .unwrap();
        if let Some(text) = text {
            std::fs::write(dir.path().join(config::MODELS), text).unwrap();
        }
        dir
    }

    #[tokio::test]
    async fn a_check_without_a_readable_models_file_says_so_and_fails_besides_the_other_alerts() {
        let (_dir, db) = seeded().await;
        sqlx::query("UPDATE spend SET spent_usd = 1.9 WHERE day = '2026-10-05' AND user_id = '*'")
            .execute(db.pool())
            .await
            .unwrap();
        let missing = models_only(None);
        let mut out = Vec::new();
        let check = Options {
            check: true,
            since: None,
        };
        let failed = run(&db, missing.path(), &mut out, at(NOW), check)
            .await
            .unwrap_err();
        assert!(
            matches!(failed, AdminError::Alerts(2)),
            "the judge, and the spend it can't check: {failed:?}"
        );
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert!(lines[0].starts_with("judge: "), "{text}");
        assert_eq!(
            lines[1],
            "spend: can't be checked: models.yaml: the file is missing"
        );
        assert_eq!(
            status_rows(&db).await,
            vec![json!({ "check": true, "alerts": 1, "unchecked": 1 })]
        );

        // the full view says the same at its top
        let mut out = Vec::new();
        run(&db, missing.path(), &mut out, at(NOW), Options::default())
            .await
            .unwrap();
        let view = String::from_utf8(out).unwrap();
        assert!(view.starts_with(&format!("Alerts\n{text}\nProbes\n")), "{view}");
    }

    #[tokio::test]
    async fn a_check_that_cant_read_the_models_file_is_never_a_pass() {
        let (_dir, db) = testing::db().await;
        let broken = models_only(Some("budgets: {}\nbackends: {}\nroles: {}\nextra: 1\n"));
        let check = Options {
            check: true,
            since: None,
        };

        // its row comes first, as for a breach: a disk too full for it shows nothing
        sqlx::query(
            "CREATE TRIGGER no_status BEFORE INSERT ON audit_events WHEN NEW.type = 'status_shown' \
             BEGIN SELECT RAISE(ABORT, 'the disk is full'); END",
        )
        .execute(db.pool())
        .await
        .unwrap();
        let mut out = Vec::new();
        let failed = run(&db, broken.path(), &mut out, at(NOW), check)
            .await
            .unwrap_err();
        assert!(matches!(failed, AdminError::DiskFull), "{failed:?}");
        assert!(out.is_empty(), "nothing shown without its row");
        sqlx::query("DROP TRIGGER no_status")
            .execute(db.pool())
            .await
            .unwrap();

        // no alert holds, and still it fails: it couldn't look at the spend
        let mut out = Vec::new();
        let failed = run(&db, broken.path(), &mut out, at(NOW), check)
            .await
            .unwrap_err();
        assert!(matches!(failed, AdminError::Alerts(1)), "{failed:?}");
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text.lines().count(), 1, "{text}");
        assert!(
            text.starts_with("spend: can't be checked: models.yaml"),
            "the problem, in short: {text}"
        );
        assert_eq!(
            status_rows(&db).await,
            vec![json!({ "check": true, "alerts": 0, "unchecked": 1 })]
        );
    }

    #[tokio::test]
    async fn a_listed_probe_that_never_registered_is_an_alert_and_an_unreadable_list_is_said() {
        let (_dir, db) = testing::db().await;
        // the stack's first row is an hour old, and delta, which the examples list, never registered
        ended(
            &db,
            1,
            done(JudgeMode::Jev, ComposerMode::Model),
            "2026-10-05T11:00:00.000Z",
        )
        .await;
        let check = Options {
            check: true,
            since: None,
        };
        let mut out = Vec::new();
        let failed = run(&db, &examples(), &mut out, at(NOW), check).await.unwrap_err();
        assert!(matches!(failed, AdminError::Alerts(1)), "{failed:?}");
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "probe delta: listed in projects.yaml, and its probe has never registered\n"
        );

        // a projects.yaml it can't read: a line of its own, and the check fails all the same
        let broken = examples_with(&[]);
        std::fs::write(broken.path().join(config::PROJECTS), "projects: [\n").unwrap();
        let mut out = Vec::new();
        let failed = run(&db, broken.path(), &mut out, at(NOW), check)
            .await
            .unwrap_err();
        assert!(matches!(failed, AdminError::Alerts(1)), "{failed:?}");
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text.lines().count(), 1, "{text}");
        assert!(
            text.starts_with("probes: those that never registered can't be checked: projects.yaml"),
            "{text}"
        );
        assert_eq!(
            status_rows(&db).await,
            vec![
                json!({ "check": true, "alerts": 1, "unchecked": 0 }),
                json!({ "check": true, "alerts": 0, "unchecked": 1 })
            ]
        );

        // the full view says it at its top, and the rest still shows
        let mut out = Vec::new();
        run(&db, broken.path(), &mut out, at(NOW), Options::default())
            .await
            .unwrap();
        let view = String::from_utf8(out).unwrap();
        assert!(view.starts_with(&format!("Alerts\n{text}\nProbes\n")), "{view}");
        assert!(view.contains("everyone  $0.0000  $2.00  0%"), "{view}");
    }

    /// A question as the core stores it, with the audit's `pending_signoff` row when it was held.
    async fn asked(
        db: &Db,
        n: u32,
        channel: &str,
        status: &str,
        judge: Option<&str>,
        when: &str,
        held: bool,
    ) {
        let id = format!("q_01J9Z3QK7M8N9P0R1S2T3V{n:04}");
        sqlx::query(
            "INSERT INTO questions (id, user_id, token_public_id, channel, status, text_redacted, judge_mode,
                                    created_at)
             VALUES (?, 'u_1', 'tok_1', ?, ?, 'a question', ?, ?)",
        )
        .bind(&id)
        .bind(channel)
        .bind(status)
        .bind(judge)
        .bind(when)
        .execute(db.pool())
        .await
        .unwrap();
        if held {
            let event = Event::PendingSignoff(mercury_schema::events::PendingSignoff {
                approval_id: mercury_schema::ids::ApprovalId::parse(&format!(
                    "ap_01JB2M3N4P5Q6R7S8T9V0W{n:04}"
                ))
                .unwrap(),
                reason: mercury_schema::api::SignoffReason::Class,
            });
            let question = QuestionId::parse(&id).unwrap();
            let _recorded = audit::record(db, &question, None, event, at(when)).await.unwrap();
            // the hold moved the question to `pending_signoff`; a decision since put it back
            sqlx::query("UPDATE questions SET status = ? WHERE id = ?")
                .bind(status)
                .bind(&id)
                .execute(db.pool())
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn the_questions_since_a_date_are_counted_by_channel_and_outcome() {
        let (_dir, db) = seeded().await;
        sqlx::query(
            "INSERT INTO tokens (public_id, user_id, secret_sha256, label, created_at, expires_at)
             VALUES ('tok_1', 'u_1', zeroblob(32), 'test', '2026-10-01T00:00:00.000Z', '2027-01-01T00:00:00.000Z')",
        )
        .execute(db.pool())
        .await
        .unwrap();
        let since = at("2026-10-05T00:00:00.000Z");
        // before the window: counted nowhere
        asked(
            &db,
            1,
            "cli",
            "delivered",
            Some("jev"),
            "2026-10-04T23:59:59.000Z",
            false,
        )
        .await;
        // cli: two answered, one refused (judged by chat: still a refusal), one clarify
        asked(
            &db,
            2,
            "cli",
            "delivered",
            Some("jev"),
            "2026-10-05T08:00:00.000Z",
            false,
        )
        .await;
        asked(
            &db,
            3,
            "cli",
            "delivered",
            Some("jev"),
            "2026-10-05T08:30:00.000Z",
            false,
        )
        .await;
        asked(
            &db,
            4,
            "cli",
            "refused",
            Some("chat"),
            "2026-10-05T09:00:00.000Z",
            false,
        )
        .await;
        asked(&db, 5, "cli", "clarify", None, "2026-10-05T09:30:00.000Z", false).await;
        // web: a degraded answer, a held one still waiting, a held one that was approved, a failure
        asked(
            &db,
            6,
            "web",
            "delivered",
            Some("rules"),
            "2026-10-05T10:00:00.000Z",
            false,
        )
        .await;
        asked(
            &db,
            7,
            "web",
            "pending_signoff",
            None,
            "2026-10-05T10:30:00.000Z",
            true,
        )
        .await;
        asked(
            &db,
            8,
            "web",
            "delivered",
            Some("jev"),
            "2026-10-05T11:00:00.000Z",
            true,
        )
        .await;
        asked(&db, 9, "web", "failed", None, "2026-10-05T11:15:00.000Z", false).await;
        // mcp: one answer, one still running
        asked(
            &db,
            10,
            "mcp",
            "delivered",
            Some("chat"),
            "2026-10-05T11:30:00.000Z",
            false,
        )
        .await;
        asked(&db, 11, "mcp", "running", None, "2026-10-05T11:59:00.000Z", false).await;

        let counted = window(&db, since).await.unwrap();
        let outcomes = |name: &str| {
            counted
                .channels
                .iter()
                .find(|(channel, _)| channel == name)
                .unwrap()
                .1
        };
        assert_eq!(
            outcomes("cli"),
            Outcomes {
                answered: 2,
                refused: 1,
                degraded: 0,
                held: 0,
                other: 1
            }
        );
        assert_eq!(
            outcomes("web"),
            Outcomes {
                answered: 0,
                refused: 0,
                degraded: 1,
                held: 2,
                other: 1
            }
        );
        assert_eq!(
            outcomes("mcp"),
            Outcomes {
                answered: 0,
                refused: 0,
                degraded: 1,
                held: 0,
                other: 1
            }
        );
        assert_eq!(
            counted.total().total(),
            10,
            "the one before the window isn't counted"
        );

        let mut out = Vec::new();
        let options = Options {
            check: false,
            since: Some(since),
        };
        run(&db, &examples(), &mut out, at(NOW), options).await.unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.ends_with(
                "\nQuestions since 2026-10-05 00:00 (UTC): 10\n\
                 CHANNEL  ANSWERED  REFUSED  DEGRADED  HELD  OTHER  TOTAL\n\
                 cli      2         1        0         0     1      4\n\
                 web      0         0        1         2     1      4\n\
                 mcp      0         0        1         0     1      2\n\
                 all      2         1        2         2     3      10\n\
                 Answered: delivered by Jev. Degraded: delivered by chat or rules. Held: went to a sign-off. \
                 Other: a clarify, a failure, or still running.\n"
            ),
            "{text}"
        );
        // without --since the view has no such section
        let mut out = Vec::new();
        run(&db, &examples(), &mut out, at(NOW), Options::default())
            .await
            .unwrap();
        assert!(!String::from_utf8(out).unwrap().contains("Questions since"));
    }

    #[tokio::test]
    async fn a_window_with_no_question_still_has_its_table() {
        let (_dir, db) = testing::db().await;
        let counted = window(&db, at("2026-10-05T00:00:00.000Z")).await.unwrap();
        let names: Vec<&str> = counted.channels.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["cli", "web", "mcp"]);
        assert_eq!(counted.total().total(), 0);
    }

    #[tokio::test]
    async fn a_status_that_cant_be_audited_shows_nothing() {
        let (_dir, db) = seeded().await;
        sqlx::query(
            "CREATE TRIGGER no_status BEFORE INSERT ON audit_events WHEN NEW.type = 'status_shown' \
             BEGIN SELECT RAISE(ABORT, 'the disk is full'); END",
        )
        .execute(db.pool())
        .await
        .unwrap();
        let mut out = Vec::new();
        assert!(
            run(&db, &examples(), &mut out, at(NOW), Options::default())
                .await
                .is_err()
        );
        assert!(out.is_empty(), "nothing shown without its row");
    }
}
