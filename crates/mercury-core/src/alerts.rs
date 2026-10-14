//! The three alerts that docs/deploy.md describes under "Keeping it running", read from the
//! database alone: today's spend at 80% of the daily budget (at once when that budget is 0), the
//! judge degraded for more than five minutes, a probe silent for more than fifteen (or a project of
//! `projects.yaml` whose probe has never registered, once the audit is fifteen minutes old). The
//! thresholds are fixed: no key sets them.
//!
//! One rule, in two places. The core checks it every minute and logs each breach in fixed words (the
//! kind and its numbers or the project: never a question's text, a fact or an answer), at most once
//! an hour while the breach holds. `admin status` lists the alerts that hold and `admin status
//! --check` says them for cron. Neither asks a model or a probe.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use mercury_schema::ProjectId;
use mercury_schema::config::PORTFOLIO;

use crate::db::{self, Db};
use crate::orchestrator::budget::{self, EVERYONE};
use crate::status;

/// The share of the daily budget at which the spend alerts.
pub const SPEND_SHARE: f64 = status::NEAR_THE_CAP;

/// How long the judge has to be degraded before it alerts.
pub const JUDGE_DEGRADED_AFTER: TimeDelta = TimeDelta::minutes(5);

/// How long a probe has to be silent before it alerts.
pub const PROBE_SILENT_AFTER: TimeDelta = TimeDelta::minutes(15);

/// How often a breach that still holds is logged again.
pub const REPEAT_AFTER: TimeDelta = TimeDelta::hours(1);

/// How often the core checks.
const EVERY: Duration = Duration::from_secs(60);

/// A judge that is not Jev, which has answered the questions of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Degraded {
    Chat,
    Rules,
}

impl Degraded {
    pub fn as_str(self) -> &'static str {
        match self {
            Degraded::Chat => "chat",
            Degraded::Rules => "rules",
        }
    }
}

/// A threshold that is crossed now.
#[derive(Debug, Clone, PartialEq)]
pub enum Alert {
    /// Everyone's spend today (UTC) is at 80% of the daily budget or more, or the daily budget is
    /// 0, so nothing can be asked.
    Spend { day: String, spent: f64, limit: f64 },
    /// The questions that ended last were judged by `mode`, the first of them `age` ago, and none
    /// has ended on Jev since.
    JudgeDegraded {
        mode: Degraded,
        since: DateTime<Utc>,
        age: TimeDelta,
    },
    /// A probe that is not quarantined has not registered for `age`; or, when `registered` is
    /// false, a project `projects.yaml` lists has no probe that ever registered, and `last_seen`
    /// and `age` are `None`.
    ProbeSilent {
        project: String,
        registered: bool,
        last_seen: Option<DateTime<Utc>>,
        age: Option<TimeDelta>,
    },
}

impl Alert {
    /// What names the breach: the same key while it holds, a new one when another breach starts.
    fn key(&self) -> String {
        match self {
            Alert::Spend { .. } => "spend".to_owned(),
            Alert::JudgeDegraded { .. } => "judge".to_owned(),
            Alert::ProbeSilent { project, .. } => format!("probe:{project}"),
        }
    }

    /// The line `status` and `status --check` show.
    pub fn line(&self) -> String {
        match self {
            Alert::Spend { day, spent, limit } if *limit <= 0.0 => format!(
                "spend: the daily budget is $0.00, so nothing can be asked (${spent:.2} spent, {day} UTC)"
            ),
            Alert::Spend { day, spent, limit } => format!(
                "spend: {} of the daily budget is spent (${spent:.2} of ${limit:.2}, {day} UTC)",
                status::percent(*spent, *limit)
            ),
            Alert::JudgeDegraded { mode, since, age } => format!(
                "judge: answering in a degraded mode ({}) for {}, since {}; no question has ended on Jev since",
                mode.as_str(),
                span(*age),
                moment(*since)
            ),
            Alert::ProbeSilent {
                project,
                registered: false,
                ..
            } => format!("probe {project}: listed in projects.yaml, and its probe has never registered"),
            Alert::ProbeSilent {
                project,
                last_seen,
                age,
                ..
            } => {
                let mut line = format!("probe {project}: silent for ");
                match age {
                    Some(age) => line.push_str(&span(*age)),
                    None => line.push_str("more than 15 minutes"),
                }
                if let Some(seen) = last_seen {
                    let _ = write!(line, ", last seen {}", moment(*seen));
                }
                line
            }
        }
    }

    /// The warning for the core's log: fixed words, with the kind and its numbers or the project.
    pub fn warn(&self) {
        match self {
            Alert::Spend { limit, .. } if *limit <= 0.0 => tracing::warn!(
                alert = "spend",
                "alert: the daily budget is 0, so nothing can be asked"
            ),
            Alert::Spend { spent, limit, .. } => tracing::warn!(
                alert = "spend",
                share = %status::percent(*spent, *limit),
                "alert: today's spend is at 80% of the daily budget or more"
            ),
            Alert::JudgeDegraded { mode, age, .. } => tracing::warn!(
                alert = "judge",
                mode = mode.as_str(),
                minutes = age.num_minutes(),
                "alert: the judge has been degraded for more than 5 minutes"
            ),
            Alert::ProbeSilent {
                project,
                registered: false,
                ..
            } => tracing::warn!(
                alert = "probe",
                project = %project,
                "alert: a project of projects.yaml has a probe that has never registered"
            ),
            Alert::ProbeSilent { project, age, .. } => tracing::warn!(
                alert = "probe",
                project = %project,
                minutes = age.map(|age| age.num_minutes()),
                "alert: a probe has been silent for more than 15 minutes"
            ),
        }
    }
}

/// A length of time in the unit a person reads: minutes, then hours, then days.
fn span(delta: TimeDelta) -> String {
    let minutes = delta.num_minutes();
    if minutes < 120 {
        format!("{minutes} minutes")
    } else if minutes < 48 * 60 {
        format!("{} hours", minutes / 60)
    } else {
        format!("{} days", minutes / (24 * 60))
    }
}

fn moment(at: DateTime<Utc>) -> String {
    at.format("%Y-%m-%d %H:%M UTC").to_string()
}

/// How the judge answered a question that ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Judged {
    Jev,
    Degraded(Degraded),
}

fn judged(payload: Option<&str>) -> Option<Judged> {
    match status::mode_of(payload)?.as_str() {
        "jev" => Some(Judged::Jev),
        "chat" => Some(Judged::Degraded(Degraded::Chat)),
        "rules" => Some(Judged::Degraded(Degraded::Rules)),
        _ => None,
    }
}

/// Everyone's spend at `now`, against `limit`. A daily budget of 0 alerts whatever was spent: every
/// question is refused while it holds.
fn spend(day: String, spent: f64, limit: f64) -> Option<Alert> {
    (limit <= 0.0 || spent >= limit * SPEND_SHARE).then_some(Alert::Spend { day, spent, limit })
}

/// The judge's state from the questions that ended, oldest first, each with its time and how it
/// was judged (`None` when no judge answered it, which says nothing about the judge). The questions
/// held for a sign-off are not among them: they end when an approver decides. Degraded when the
/// latest was judged by `chat` or `rules` and the run of such questions since the last `jev` began
/// more than five minutes ago.
fn judge(endings: &[(DateTime<Utc>, Option<Judged>)], now: DateTime<Utc>) -> Option<Alert> {
    let mut run: Option<(DateTime<Utc>, Degraded)> = None;
    for (at, how) in endings {
        match how {
            Some(Judged::Jev) => run = None,
            Some(Judged::Degraded(mode)) => run = Some((run.map_or(*at, |(since, _)| since), *mode)),
            None => {}
        }
    }
    let (since, mode) = run?;
    let age = now - since;
    (age > JUDGE_DEGRADED_AFTER).then_some(Alert::JudgeDegraded { mode, since, age })
}

/// The alerts that hold at `now`. `daily_budget` is `models.yaml`'s `global_daily_usd`, or `None`
/// when that file can't be read, in which case the spend can't be judged. `projects` are the ids
/// `projects.yaml` lists, or `None` when that file can't be read, in which case a probe that never
/// registered can't be told from one that isn't expected.
pub async fn check(
    db: &Db,
    daily_budget: Option<f64>,
    projects: Option<&[ProjectId]>,
    now: DateTime<Utc>,
) -> Result<Vec<Alert>, sqlx::Error> {
    let mut alerts = Vec::new();

    if let Some(limit) = daily_budget {
        let day = budget::day(now);
        let spent = sqlx::query_scalar!(
            "SELECT spent_usd FROM spend WHERE day = ? AND user_id = ?",
            day,
            EVERYONE
        )
        .fetch_optional(db.pool())
        .await?;
        alerts.extend(spend(day, spent.unwrap_or(0.0), limit));
    }

    // A question held for a sign-off ends when an approver decides or the hold expires, hours
    // later, with the judge of its own time: it says nothing about the judge at its ending.
    let since = db::time(now - status::WINDOW);
    let endings = sqlx::query!(
        "SELECT e.created_at, e.payload_json FROM audit_events e
         WHERE e.question_id IS NOT NULL AND e.created_at >= ?
           AND e.type IN ('done', 'refused', 'clarify', 'error')
           AND NOT EXISTS (SELECT 1 FROM audit_events h
                           WHERE h.question_id = e.question_id AND h.type = 'pending_signoff')
         ORDER BY e.created_at, e.seq",
        since
    )
    .fetch_all(db.pool())
    .await?;
    let endings: Vec<(DateTime<Utc>, Option<Judged>)> = endings
        .iter()
        .filter_map(|row| {
            Some((
                db::parse_time(&row.created_at)?,
                judged(row.payload_json.as_deref()),
            ))
        })
        .collect();
    alerts.extend(judge(&endings, now));

    let cutoff = db::time(now - PROBE_SILENT_AFTER);
    let silent = sqlx::query!(
        "SELECT project, last_seen_at FROM probes
         WHERE status != 'quarantined' AND last_seen_at < ? ORDER BY project",
        cutoff
    )
    .fetch_all(db.pool())
    .await?;
    let mut probes = Vec::new();
    for row in silent {
        let last_seen = db::parse_time(&row.last_seen_at);
        probes.push(Alert::ProbeSilent {
            project: row.project,
            registered: true,
            age: last_seen.map(|seen| now - seen),
            last_seen,
        });
    }
    if let Some(projects) = projects {
        for project in never_registered(db, projects, now).await? {
            probes.push(Alert::ProbeSilent {
                project,
                registered: false,
                last_seen: None,
                age: None,
            });
        }
    }
    // by project, whichever way each is silent
    probes.sort_by_key(Alert::key);
    alerts.extend(probes);
    Ok(alerts)
}

/// The projects of `projects` with no probe that ever registered, by id, once the audit's first
/// row is at least fifteen minutes old: a stack that has just started gives its probes the time a
/// running one gives a silent probe. A stack whose audit is empty has done nothing yet.
async fn never_registered(
    db: &Db,
    projects: &[ProjectId],
    now: DateTime<Utc>,
) -> Result<Vec<String>, sqlx::Error> {
    let first = sqlx::query_scalar!("SELECT created_at FROM audit_events ORDER BY seq LIMIT 1")
        .fetch_optional(db.pool())
        .await?;
    let begun = first.as_deref().and_then(db::parse_time);
    if !begun.is_some_and(|begun| now - begun >= PROBE_SILENT_AFTER) {
        return Ok(Vec::new());
    }
    // a quarantined probe registered once: it is no probe that never did
    let registered: HashSet<String> = sqlx::query_scalar!("SELECT project FROM probes")
        .fetch_all(db.pool())
        .await?
        .into_iter()
        .collect();
    let mut missing: Vec<String> = projects
        .iter()
        .map(|project| project.as_str().to_owned())
        .filter(|project| project != PORTFOLIO && !registered.contains(project))
        .collect();
    missing.sort();
    missing.dedup();
    Ok(missing)
}

/// Which breaches the log has said, and when: each once an hour while it holds.
#[derive(Debug, Default)]
pub struct Throttle {
    warned: HashMap<String, DateTime<Utc>>,
}

impl Throttle {
    /// The alerts to log at `now`: the ones not logged within the hour. A breach that no longer
    /// holds is forgotten, so one that comes back is new and is logged at once.
    pub fn due<'a>(&mut self, alerts: &'a [Alert], now: DateTime<Utc>) -> Vec<&'a Alert> {
        let holding: HashSet<String> = alerts.iter().map(Alert::key).collect();
        self.warned.retain(|key, _| holding.contains(key));
        let mut due = Vec::new();
        for alert in alerts {
            let key = alert.key();
            if self.warned.get(&key).is_some_and(|at| now - *at < REPEAT_AFTER) {
                continue;
            }
            self.warned.insert(key, now);
            due.push(alert);
        }
        due
    }
}

/// The core's check: every minute, the alerts that hold, each logged once an hour. `daily_budget`
/// is `global_daily_usd` of the `models.yaml` the core started with, and `projects` the ids its
/// `projects.yaml` lists. The first check is a minute after the start, when the probes have
/// registered: a probe that was silent while the core was down is not an alert of the core's.
pub async fn watch(db: Db, daily_budget: f64, projects: Vec<ProjectId>) {
    let mut ticks = tokio::time::interval_at(tokio::time::Instant::now() + EVERY, EVERY);
    let mut throttle = Throttle::default();
    loop {
        ticks.tick().await;
        let now = db::now();
        match check(&db, Some(daily_budget), Some(&projects), now).await {
            Ok(alerts) => {
                for alert in throttle.due(&alerts, now) {
                    alert.warn();
                }
            }
            Err(err) => tracing::warn!(error = %err, "cannot check the alerts; the next minute tries again"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::{Arc, Mutex};

    use mercury_schema::api::{SignoffReason, Usd};
    use mercury_schema::events::{ComposerMode, Done, Event, PendingSignoff};
    use mercury_schema::ids::ApprovalId;
    use mercury_schema::{JudgeMode, QuestionId};
    use tracing_subscriber::fmt::MakeWriter;

    use super::*;
    use crate::audit;
    use crate::db::testing;

    fn at(text: &str) -> DateTime<Utc> {
        db::parse_time(text).unwrap()
    }

    const NOW: &str = "2026-10-05T12:00:00.000Z";

    async fn ended(db: &Db, n: u32, mode: JudgeMode, when: &str) {
        let id = QuestionId::parse(&format!("q_01J9Z3QK7M8N9P0R1S2T3V{n:04}")).unwrap();
        let event = Event::Done(Done {
            duration_ms: 1_200,
            cost_usd: Usd::new(0.004).unwrap(),
            judge_mode: mode,
            composer: ComposerMode::Model,
            audit_id: 1,
        });
        let _recorded = audit::record(db, &id, None, event, at(when)).await.unwrap();
    }

    /// The audit's `pending_signoff` row of question `n`: it was held for a sign-off at `when`.
    async fn held(db: &Db, n: u32, when: &str) {
        let id = QuestionId::parse(&format!("q_01J9Z3QK7M8N9P0R1S2T3V{n:04}")).unwrap();
        let event = Event::PendingSignoff(PendingSignoff {
            approval_id: ApprovalId::parse(&format!("ap_01JB2M3N4P5Q6R7S8T9V0W{n:04}")).unwrap(),
            reason: SignoffReason::Class,
        });
        let _recorded = audit::record(db, &id, None, event, at(when)).await.unwrap();
    }

    async fn probe(db: &Db, project: &str, status: &str, seen: &str) {
        sqlx::query(
            "INSERT INTO probes (project, url, access, access_limited_by, status, schema, manifest_hash,
                                 manifest_json, tools_json, registered_at, last_seen_at)
             VALUES (?, 'http://probe:7000/mcp', 'code', NULL, ?, 'mercury.facts/0.1',
                     'h', '{}', '[]', ?, ?)",
        )
        .bind(project)
        .bind(status)
        .bind(seen)
        .bind(seen)
        .execute(db.pool())
        .await
        .unwrap();
    }

    async fn spent(db: &Db, day: &str, who: &str, usd: f64) {
        sqlx::query("INSERT INTO spend (day, user_id, spent_usd) VALUES (?, ?, ?)")
            .bind(day)
            .bind(who)
            .bind(usd)
            .execute(db.pool())
            .await
            .unwrap();
    }

    #[test]
    fn the_spend_alerts_at_eighty_percent_of_the_budget_and_not_below() {
        let day = || "2026-10-05".to_owned();
        assert!(spend(day(), 1.59, 2.0).is_none());
        let alert = spend(day(), 1.6, 2.0).unwrap();
        assert_eq!(
            alert.line(),
            "spend: 80% of the daily budget is spent ($1.60 of $2.00, 2026-10-05 UTC)"
        );
        assert!(spend(day(), 5.0, 2.0).is_some(), "over the cap is over 80%");
    }

    #[test]
    fn a_daily_budget_of_zero_alerts_at_once_because_nothing_can_be_asked() {
        let day = || "2026-10-05".to_owned();
        let alert = spend(day(), 0.0, 0.0).expect("nothing spent, and still nothing can be asked");
        assert_eq!(
            alert.line(),
            "spend: the daily budget is $0.00, so nothing can be asked ($0.00 spent, 2026-10-05 UTC)"
        );
        let spent = spend(day(), 0.25, 0.0).unwrap();
        assert_eq!(
            spent.line(),
            "spend: the daily budget is $0.00, so nothing can be asked ($0.25 spent, 2026-10-05 UTC)",
            "no share of nothing"
        );
    }

    #[test]
    fn the_judge_alerts_once_a_degraded_run_is_older_than_five_minutes() {
        let chat = Some(Judged::Degraded(Degraded::Chat));
        let rules = Some(Judged::Degraded(Degraded::Rules));
        let jev = Some(Judged::Jev);
        let now = at(NOW);
        let ago = |minutes: i64| now - TimeDelta::minutes(minutes);

        assert!(judge(&[], now).is_none(), "no question, nothing known");
        assert!(judge(&[(ago(30), jev)], now).is_none());
        assert!(
            judge(&[(ago(5), chat)], now).is_none(),
            "exactly five minutes is not more than five"
        );
        let alert = judge(&[(ago(6), chat)], now).unwrap();
        assert_eq!(
            alert,
            Alert::JudgeDegraded {
                mode: Degraded::Chat,
                since: ago(6),
                age: TimeDelta::minutes(6)
            }
        );
        // the run counts from its first question, and says the judge the latest one had
        let run = [(ago(40), jev), (ago(20), chat), (ago(10), rules), (ago(1), chat)];
        let alert = judge(&run, now).unwrap();
        assert_eq!(
            alert,
            Alert::JudgeDegraded {
                mode: Degraded::Chat,
                since: ago(20),
                age: TimeDelta::minutes(20)
            }
        );
        // a question that ended on Jev ends it; a new run starts later and is young
        let back = [(ago(40), chat), (ago(30), jev), (ago(2), chat)];
        assert!(judge(&back, now).is_none(), "the new run is two minutes old");
        let recovered = [(ago(40), chat), (ago(1), jev)];
        assert!(judge(&recovered, now).is_none());
        // a question no judge answered says nothing about the judge, either way
        let refused_by_policy = [(ago(20), chat), (ago(10), None)];
        assert!(judge(&refused_by_policy, now).is_some());
        let only_none = [(ago(20), None)];
        assert!(judge(&only_none, now).is_none());
    }

    #[test]
    fn every_alert_has_its_own_line() {
        let judge = Alert::JudgeDegraded {
            mode: Degraded::Rules,
            since: at("2026-10-05T11:53:00.000Z"),
            age: TimeDelta::minutes(7),
        };
        assert_eq!(
            judge.line(),
            "judge: answering in a degraded mode (rules) for 7 minutes, since 2026-10-05 11:53 UTC; \
             no question has ended on Jev since"
        );
        let probe = Alert::ProbeSilent {
            project: "delta".to_owned(),
            registered: true,
            last_seen: Some(at("2026-10-05T11:43:00.000Z")),
            age: Some(TimeDelta::minutes(17)),
        };
        assert_eq!(
            probe.line(),
            "probe delta: silent for 17 minutes, last seen 2026-10-05 11:43 UTC"
        );
        let old = Alert::ProbeSilent {
            project: "delta".to_owned(),
            registered: true,
            last_seen: Some(at("2026-10-03T11:00:00.000Z")),
            age: Some(TimeDelta::hours(49)),
        };
        assert!(
            old.line().starts_with("probe delta: silent for 2 days,"),
            "{}",
            old.line()
        );
        let long = Alert::ProbeSilent {
            project: "delta".to_owned(),
            registered: true,
            last_seen: None,
            age: None,
        };
        assert_eq!(long.line(), "probe delta: silent for more than 15 minutes");
        let never = Alert::ProbeSilent {
            project: "omega".to_owned(),
            registered: false,
            last_seen: None,
            age: None,
        };
        assert_eq!(
            never.line(),
            "probe omega: listed in projects.yaml, and its probe has never registered"
        );
    }

    #[tokio::test]
    async fn each_threshold_breached_in_the_database_is_found() {
        let (_dir, db) = testing::db().await;
        let now = at(NOW);

        // nothing breached
        assert_eq!(check(&db, Some(2.0), None, now).await.unwrap(), vec![]);

        // spend: today's everyone row at 85% of $2.00; yesterday's and a person's row don't count
        spent(&db, "2026-10-05", "*", 1.7).await;
        spent(&db, "2026-10-04", "*", 9.0).await;
        let alerts = check(&db, Some(2.0), None, now).await.unwrap();
        assert_eq!(
            alerts,
            vec![Alert::Spend {
                day: "2026-10-05".to_owned(),
                spent: 1.7,
                limit: 2.0
            }]
        );
        assert_eq!(
            check(&db, None, None, now).await.unwrap(),
            vec![],
            "without a budget the spend can't be judged"
        );
        assert_eq!(
            check(&db, Some(0.0), None, now).await.unwrap(),
            vec![Alert::Spend {
                day: "2026-10-05".to_owned(),
                spent: 1.7,
                limit: 0.0
            }],
            "a budget of 0 refuses every question"
        );

        // the judge: one question judged by chat 6 minutes ago, the one before it by Jev
        ended(&db, 1, JudgeMode::Jev, "2026-10-05T11:30:00.000Z").await;
        ended(&db, 2, JudgeMode::Chat, "2026-10-05T11:54:00.000Z").await;
        let alerts = check(&db, Some(2.0), None, now).await.unwrap();
        assert_eq!(alerts.len(), 2, "{alerts:?}");
        assert_eq!(
            alerts[1],
            Alert::JudgeDegraded {
                mode: Degraded::Chat,
                since: at("2026-10-05T11:54:00.000Z"),
                age: TimeDelta::minutes(6)
            }
        );
        ended(&db, 3, JudgeMode::Jev, "2026-10-05T11:58:00.000Z").await;
        assert_eq!(
            check(&db, Some(2.0), None, now).await.unwrap().len(),
            1,
            "Jev answered again"
        );

        // probes: silent for 16 minutes, one for 14, one quarantined and silent for a day
        probe(&db, "alpha", "stale", "2026-10-05T11:44:00.000Z").await;
        probe(&db, "bravo", "healthy", "2026-10-05T11:46:00.000Z").await;
        probe(&db, "charlie", "quarantined", "2026-10-04T12:00:00.000Z").await;
        let alerts = check(&db, Some(2.0), None, now).await.unwrap();
        assert_eq!(alerts.len(), 2, "{alerts:?}");
        assert_eq!(
            alerts[1],
            Alert::ProbeSilent {
                project: "alpha".to_owned(),
                registered: true,
                last_seen: Some(at("2026-10-05T11:44:00.000Z")),
                age: Some(TimeDelta::minutes(16))
            }
        );
    }

    #[tokio::test]
    async fn a_listed_project_whose_probe_never_registered_is_silent_once_the_audit_is_old_enough() {
        let (_dir, db) = testing::db().await;
        let now = at(NOW);
        let listed: Vec<ProjectId> = ["alpha", "bravo", "charlie", "omega", "portfolio", "zulu"]
            .iter()
            .map(|id| ProjectId::parse(id).unwrap())
            .collect();
        // the registered probes follow their own rule: alpha silent for 16 minutes, zulu for 30,
        // bravo for 14, charlie quarantined and silent for a day; omega's never registered
        probe(&db, "alpha", "stale", "2026-10-05T11:44:00.000Z").await;
        probe(&db, "bravo", "healthy", "2026-10-05T11:46:00.000Z").await;
        probe(&db, "charlie", "quarantined", "2026-10-04T12:00:00.000Z").await;
        probe(&db, "zulu", "stale", "2026-10-05T11:30:00.000Z").await;
        let registered = |project: &str, seen: &str, at_now: DateTime<Utc>| {
            let seen = at(seen);
            Alert::ProbeSilent {
                project: project.to_owned(),
                registered: true,
                last_seen: Some(seen),
                age: Some(at_now - seen),
            }
        };
        let silent_now = vec![
            registered("alpha", "2026-10-05T11:44:00.000Z", now),
            registered("zulu", "2026-10-05T11:30:00.000Z", now),
        ];

        // an empty audit: the stack has done nothing yet
        assert_eq!(
            check(&db, Some(2.0), Some(&listed), now).await.unwrap(),
            silent_now
        );

        // the stack's first row is 14 minutes old: its probes still have time to register
        ended(&db, 1, JudgeMode::Jev, "2026-10-05T11:46:00.000Z").await;
        assert_eq!(
            check(&db, Some(2.0), Some(&listed), now).await.unwrap(),
            silent_now,
            "a fresh audit raises nothing for omega"
        );

        // a minute later it is 15 minutes old: omega is silent, among the others by project
        let later = now + TimeDelta::minutes(1);
        assert_eq!(
            check(&db, Some(2.0), Some(&listed), later).await.unwrap(),
            vec![
                registered("alpha", "2026-10-05T11:44:00.000Z", later),
                Alert::ProbeSilent {
                    project: "omega".to_owned(),
                    registered: false,
                    last_seen: None,
                    age: None,
                },
                registered("zulu", "2026-10-05T11:30:00.000Z", later),
            ],
            "bravo is silent for exactly 15 minutes, which is not more; portfolio has no probe of its own"
        );

        // without the list (projects.yaml can't be read) omega can't be told from a project nobody
        // expects
        assert_eq!(
            check(&db, Some(2.0), None, later).await.unwrap(),
            vec![
                registered("alpha", "2026-10-05T11:44:00.000Z", later),
                registered("zulu", "2026-10-05T11:30:00.000Z", later),
            ]
        );
    }

    #[tokio::test]
    async fn a_held_question_decided_later_says_nothing_about_the_judge() {
        let (_dir, db) = testing::db().await;
        let now = at(NOW);

        // Jev answered at 11:30; a question held at 10:00, when chat judged, is approved at 11:54
        ended(&db, 1, JudgeMode::Jev, "2026-10-05T11:30:00.000Z").await;
        held(&db, 2, "2026-10-05T10:00:00.000Z").await;
        ended(&db, 2, JudgeMode::Chat, "2026-10-05T11:54:00.000Z").await;
        assert_eq!(
            check(&db, Some(2.0), None, now).await.unwrap(),
            vec![],
            "its ending starts no degraded run"
        );

        // the rules judged at 11:40; a question held at 09:00, when Jev judged, is approved at 11:58
        ended(&db, 3, JudgeMode::Rules, "2026-10-05T11:40:00.000Z").await;
        held(&db, 4, "2026-10-05T09:00:00.000Z").await;
        ended(&db, 4, JudgeMode::Jev, "2026-10-05T11:58:00.000Z").await;
        assert_eq!(
            check(&db, Some(2.0), None, now).await.unwrap(),
            vec![Alert::JudgeDegraded {
                mode: Degraded::Rules,
                since: at("2026-10-05T11:40:00.000Z"),
                age: TimeDelta::minutes(20)
            }],
            "and its ending ends no degraded run"
        );
    }

    #[tokio::test]
    async fn questions_older_than_a_day_say_nothing_about_the_judge() {
        let (_dir, db) = testing::db().await;
        ended(&db, 1, JudgeMode::Rules, "2026-10-04T11:00:00.000Z").await;
        assert_eq!(check(&db, Some(2.0), None, at(NOW)).await.unwrap(), vec![]);
    }

    #[test]
    fn a_breach_is_logged_once_an_hour_while_it_holds() {
        let spend = Alert::Spend {
            day: "2026-10-05".to_owned(),
            spent: 1.7,
            limit: 2.0,
        };
        let probe = Alert::ProbeSilent {
            project: "delta".to_owned(),
            registered: true,
            last_seen: None,
            age: None,
        };
        let mut throttle = Throttle::default();
        let start = at(NOW);
        let both = [spend.clone(), probe.clone()];

        assert_eq!(throttle.due(&both, start).len(), 2, "both are new");
        assert!(throttle.due(&both, start + TimeDelta::minutes(1)).is_empty());
        assert!(throttle.due(&both, start + TimeDelta::minutes(59)).is_empty());
        assert_eq!(
            throttle.due(&both, start + TimeDelta::minutes(60)).len(),
            2,
            "an hour later"
        );
        assert!(throttle.due(&both, start + TimeDelta::minutes(61)).is_empty());

        // the probe comes back, the spend stays: the spend is not repeated, the probe is new when it returns
        let only_spend = [spend];
        assert!(
            throttle
                .due(&only_spend, start + TimeDelta::minutes(62))
                .is_empty()
        );
        let again = throttle.due(&both, start + TimeDelta::minutes(63));
        assert_eq!(again, vec![&both[1]], "a breach that came back is a new one");
    }

    /// A subscriber's writer that keeps what it is given.
    #[derive(Clone, Default)]
    struct Lines(Arc<Mutex<Vec<u8>>>);

    impl io::Write for Lines {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Lines {
        type Writer = Lines;

        fn make_writer(&'a self) -> Lines {
            self.clone()
        }
    }

    #[test]
    fn the_warnings_are_fixed_words_with_numbers_and_a_project_and_nothing_more() {
        let lines = Lines::default();
        tracing::subscriber::with_default(crate::logging::subscriber(lines.clone()), || {
            for alert in [
                Alert::Spend {
                    day: "2026-10-05".to_owned(),
                    spent: 1.7,
                    limit: 2.0,
                },
                Alert::JudgeDegraded {
                    mode: Degraded::Chat,
                    since: at("2026-10-05T11:53:00.000Z"),
                    age: TimeDelta::minutes(7),
                },
                Alert::ProbeSilent {
                    project: "delta".to_owned(),
                    registered: true,
                    last_seen: Some(at("2026-10-05T11:43:00.000Z")),
                    age: Some(TimeDelta::minutes(17)),
                },
                Alert::Spend {
                    day: "2026-10-05".to_owned(),
                    spent: 0.0,
                    limit: 0.0,
                },
                Alert::ProbeSilent {
                    project: "omega".to_owned(),
                    registered: false,
                    last_seen: None,
                    age: None,
                },
            ] {
                alert.warn();
            }
        });
        let text = String::from_utf8(lines.0.lock().unwrap().clone()).unwrap();
        let logged: Vec<serde_json::Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(logged.len(), 5, "{text}");
        let shown: Vec<(&str, &serde_json::Value)> = logged
            .iter()
            .map(|line| (line["level"].as_str().unwrap(), &line["fields"]))
            .collect();
        assert!(shown.iter().all(|(level, _)| *level == "WARN"));
        assert_eq!(
            shown[0].1,
            &serde_json::json!({
                "message": "alert: today's spend is at 80% of the daily budget or more",
                "alert": "spend",
                "share": "85%"
            })
        );
        assert_eq!(
            shown[1].1,
            &serde_json::json!({
                "message": "alert: the judge has been degraded for more than 5 minutes",
                "alert": "judge",
                "mode": "chat",
                "minutes": 7
            })
        );
        assert_eq!(
            shown[2].1,
            &serde_json::json!({
                "message": "alert: a probe has been silent for more than 15 minutes",
                "alert": "probe",
                "project": "delta",
                "minutes": 17
            })
        );
        assert_eq!(
            shown[3].1,
            &serde_json::json!({
                "message": "alert: the daily budget is 0, so nothing can be asked",
                "alert": "spend"
            }),
            "no share of a budget of 0"
        );
        assert_eq!(
            shown[4].1,
            &serde_json::json!({
                "message": "alert: a project of projects.yaml has a probe that has never registered",
                "alert": "probe",
                "project": "omega"
            })
        );
    }
}
