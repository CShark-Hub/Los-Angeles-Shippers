//! The budgets: what a question, a person and the installation may spend on paid model calls.
//!
//! Every paid call is counted as it returns, in one transaction that adds its cost to the
//! person's `spend` row and to the row `*` for the day, and writes the question's running cost.
//! A question starts only while both rows for the day are under their caps. The counters fail
//! closed: a row that can't be read or written is an error, never a free pass.

use chrono::{DateTime, NaiveTime, TimeDelta, Utc};
use mercury_schema::config::Budgets;
use serde::Serialize;
use sqlx::SqliteConnection;

use crate::db::Db;

/// The `spend` row that counts everyone.
pub const EVERYONE: &str = "*";

/// Which daily cap is spent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Cap {
    User,
    Global,
}

impl Cap {
    pub fn name(self) -> &'static str {
        match self {
            Cap::User => "user",
            Cap::Global => "global",
        }
    }
}

/// The day a cost is counted on, as `spend.day` holds it: the UTC date.
pub fn day(at: DateTime<Utc>) -> String {
    at.format("%Y-%m-%d").to_string()
}

/// How long until the next day's budget, at least a second.
pub fn until_tomorrow(at: DateTime<Utc>) -> TimeDelta {
    let tomorrow = (at.date_naive() + TimeDelta::days(1))
        .and_time(NaiveTime::MIN)
        .and_utc();
    (tomorrow - at).max(TimeDelta::seconds(1))
}

/// The daily cap a new question of `user_id` would go past, or `None` while both have room.
/// Read inside the caller's transaction, so the question it lets start is written on the same view.
pub async fn spent(
    conn: &mut SqliteConnection,
    budgets: &Budgets,
    user_id: &str,
    at: DateTime<Utc>,
) -> Result<Option<Cap>, sqlx::Error> {
    let day = day(at);
    let user = sqlx::query_scalar!(
        "SELECT spent_usd FROM spend WHERE day = ? AND user_id = ?",
        day,
        user_id
    )
    .fetch_optional(&mut *conn)
    .await?
    .unwrap_or(0.0);
    if user >= budgets.per_user_daily_usd.get() {
        return Ok(Some(Cap::User));
    }
    let everyone = sqlx::query_scalar!(
        "SELECT spent_usd FROM spend WHERE day = ? AND user_id = ?",
        day,
        EVERYONE
    )
    .fetch_optional(&mut *conn)
    .await?
    .unwrap_or(0.0);
    if everyone >= budgets.global_daily_usd.get() {
        return Ok(Some(Cap::Global));
    }
    Ok(None)
}

/// Counts one paid call of `cost`: the person's row and everyone's for the day, and the question's
/// running `total`, together or not at all.
pub async fn charge(
    db: &Db,
    question_id: &str,
    user_id: &str,
    cost: f64,
    total: f64,
    at: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let day = day(at);
    let mut tx = db.pool().begin_with("BEGIN IMMEDIATE").await?;
    for who in [user_id, EVERYONE] {
        sqlx::query!(
            "INSERT INTO spend (day, user_id, spent_usd) VALUES (?, ?, ?) \
             ON CONFLICT (day, user_id) DO UPDATE SET spent_usd = spent_usd + excluded.spent_usd",
            day,
            who,
            cost
        )
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query!(
        "UPDATE questions SET cost_usd = ? WHERE id = ?",
        total,
        question_id
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> DateTime<Utc> {
        text.parse().unwrap()
    }

    #[test]
    fn the_day_is_the_utc_date() {
        assert_eq!(day(at("2026-10-05T23:59:59Z")), "2026-10-05");
        assert_eq!(day(at("2026-10-05T22:30:00-02:00")), "2026-10-06");
    }

    #[test]
    fn the_next_budget_is_at_midnight_utc() {
        assert_eq!(until_tomorrow(at("2026-10-05T23:00:00Z")), TimeDelta::hours(1));
        assert_eq!(until_tomorrow(at("2026-10-05T00:00:00Z")), TimeDelta::days(1));
        assert_eq!(
            until_tomorrow(at("2026-10-05T23:59:59.900Z")),
            TimeDelta::seconds(1),
            "never less than a second"
        );
    }
}
