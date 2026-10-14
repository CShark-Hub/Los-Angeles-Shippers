//! The chain the core judges with: Jev, then the chat judge, then the rules. Each answers only
//! when the one before it couldn't, and the outcome's mode names who answered, so the verdicts
//! can tighten for a weaker judge. An error of the rules, the last one, stays an error.
//!
//! A circuit breaker skips Jev for a while after it failed often, so a dead endpoint doesn't
//! cost every question its timeout.

use std::collections::VecDeque;
use std::fmt;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use async_trait::async_trait;

use crate::{Batch, Judge, JudgeError, Outcome};

/// This many failures of Jev…
pub const BREAKER_FAILURES: usize = 3;
/// …within this window…
pub const BREAKER_WINDOW: Duration = Duration::from_secs(30);
/// …skip it for this long.
pub const BREAKER_OPEN: Duration = Duration::from_secs(60);

/// Where the chain reads the time. Tests turn it by hand.
pub type Clock = Box<dyn Fn() -> Instant + Send + Sync>;

/// Counts Jev's failures: [`BREAKER_FAILURES`] within [`BREAKER_WINDOW`] open it for
/// [`BREAKER_OPEN`]. It counts failures only, so a success in between wipes none of them; once
/// the open time is over, it starts afresh.
#[derive(Debug, Default)]
pub struct CircuitBreaker {
    failures: VecDeque<Instant>,
    open_until: Option<Instant>,
}

impl CircuitBreaker {
    /// Whether Jev may be asked at `now`.
    pub fn allows(&mut self, now: Instant) -> bool {
        match self.open_until {
            Some(until) if now < until => false,
            Some(_) => {
                self.open_until = None;
                self.failures.clear();
                true
            }
            None => true,
        }
    }

    /// Records a failure at `now`; `true` when it opens the breaker.
    pub fn failed(&mut self, now: Instant) -> bool {
        self.failures
            .retain(|&at| now.saturating_duration_since(at) < BREAKER_WINDOW);
        self.failures.push_back(now);
        if self.failures.len() < BREAKER_FAILURES {
            return false;
        }
        self.open_until = Some(now + BREAKER_OPEN);
        self.failures.clear();
        true
    }
}

/// Jev → the chat judge → the rules. Without Jev (the judge role is a chat judge, as in the
/// `strict` preset) the chain starts at the chat judge; without a chat judge it drops from Jev
/// straight to the rules.
pub struct ResilientJudge {
    jev: Option<Box<dyn Judge>>,
    chat: Option<Box<dyn Judge>>,
    rules: Box<dyn Judge>,
    breaker: Mutex<CircuitBreaker>,
    clock: Clock,
}

impl ResilientJudge {
    pub fn new(jev: Option<Box<dyn Judge>>, chat: Option<Box<dyn Judge>>, rules: Box<dyn Judge>) -> Self {
        Self {
            jev,
            chat,
            rules,
            breaker: Mutex::default(),
            clock: Box::new(Instant::now),
        }
    }

    /// The same chain on another clock.
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// Whether the breaker skips Jev now.
    pub fn skips_jev(&self) -> bool {
        let now = (self.clock)();
        !self.breaker().allows(now)
    }

    fn breaker(&self) -> MutexGuard<'_, CircuitBreaker> {
        self.breaker.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl fmt::Debug for ResilientJudge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResilientJudge")
            .field("jev", &self.jev.is_some())
            .field("chat", &self.chat.is_some())
            .field("breaker", &*self.breaker())
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl Judge for ResilientJudge {
    async fn decide(&self, batch: &Batch) -> Result<Outcome, JudgeError> {
        // the caller's mistake: no judge may answer it, so none is asked and none fails for it
        batch.validate()?;
        if let Some(jev) = &self.jev {
            let allowed = self.breaker().allows((self.clock)());
            if allowed {
                match jev.decide(batch).await {
                    Ok(outcome) => return Ok(outcome),
                    Err(err) => {
                        let opened = self.breaker().failed((self.clock)());
                        tracing::warn!(error = %err, "Jev failed; the next judge answers");
                        if opened {
                            tracing::warn!(
                                failures = BREAKER_FAILURES,
                                window_s = BREAKER_WINDOW.as_secs(),
                                skipped_s = BREAKER_OPEN.as_secs(),
                                "Jev failed too often; it is skipped for a while"
                            );
                        }
                    }
                }
            }
        }
        if let Some(chat) = &self.chat {
            match chat.decide(batch).await {
                Ok(outcome) => return Ok(outcome),
                Err(err) => tracing::warn!(error = %err, "the chat judge failed; the rules answer"),
            }
        }
        self.rules.decide(batch).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: Duration = Duration::from_secs(1);

    #[test]
    fn three_failures_in_thirty_seconds_open_the_breaker_for_sixty() {
        let mut breaker = CircuitBreaker::default();
        let start = Instant::now();
        assert!(!breaker.failed(start));
        assert!(!breaker.failed(start + 10 * SECOND));
        assert!(breaker.failed(start + 29 * SECOND), "the third within 30 s");
        let opened = start + 29 * SECOND;
        assert!(!breaker.allows(opened));
        assert!(!breaker.allows(opened + 59 * SECOND));
        assert!(
            breaker.allows(opened + 60 * SECOND),
            "open for 60 s, then Jev is asked again"
        );
        assert!(
            !breaker.failed(opened + 61 * SECOND),
            "afresh: one failure doesn't reopen it"
        );
    }

    #[test]
    fn failures_further_apart_than_the_window_never_open_it() {
        let mut breaker = CircuitBreaker::default();
        let start = Instant::now();
        for n in 0..10 {
            assert!(!breaker.failed(start + n * 16 * SECOND), "failure {n}");
            assert!(breaker.allows(start + n * 16 * SECOND));
        }
    }

    #[test]
    fn a_success_in_between_wipes_no_failure() {
        // successes aren't recorded at all: three failures in the window open it
        let mut breaker = CircuitBreaker::default();
        let start = Instant::now();
        breaker.failed(start);
        assert!(breaker.allows(start + SECOND));
        breaker.failed(start + 2 * SECOND);
        assert!(breaker.allows(start + 3 * SECOND));
        assert!(breaker.failed(start + 4 * SECOND));
    }
}
