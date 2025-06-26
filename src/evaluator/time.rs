use crate::achievement::AchievementContext;
use crate::evaluator::SingleEvaluator;
use chrono::{LocalResult, NaiveTime, Timelike};
use std::fmt::Debug;
use std::time::Duration;

/// Evaluates whether the current bloop's timestamp falls within a specified time window.
///
/// The `TimeEvaluator` checks if the `recorded_at` time of a bloop, converted to a configured
/// timezone, matches the given hour and/or minute, allowing for an optional leeway duration.
///
/// # Behavior
///
/// - If `hour` is `Some`, it checks if the time's hour equals.
/// - If `minute` is `Some`, it checks if the time's minute equals.
/// - If either `hour` or `minute` is `None`, that component is ignored
///   (i.e., any hour or minute matches).
/// - The time window starts at the exact configured hour/minute (or current hour/minute if `None`),
///   and extends forward by the `leeway` duration.
/// - Handles daylight saving time ambiguities by selecting the earliest matching local time.
/// - If the configured time does not exist on the day (e.g., during a DST gap), evaluation returns
///   false.
#[derive(Debug)]
pub struct TimeEvaluator {
    hour: Option<u32>,
    minute: Option<u32>,
    timezone: chrono_tz::Tz,
    leeway: Duration,
}

impl TimeEvaluator {
    /// Creates a new `TimeEvaluator`.
    ///
    /// Panics if `hour >= 24` or `minute >= 60`.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::time::Duration;
    /// use bloop_server_framework::evaluator::time::TimeEvaluator;
    ///
    /// let evaluator = TimeEvaluator::new(
    ///     Some(12),
    ///     None,
    ///     chrono_tz::Europe::Berlin,
    ///     Some(Duration::from_secs(60))
    /// );
    /// ```
    pub fn new(
        hour: Option<u32>,
        minute: Option<u32>,
        timezone: chrono_tz::Tz,
        leeway: Option<Duration>,
    ) -> Self {
        if let Some(hour) = hour {
            assert!(hour < 24, "Hour must be between 0 and 23");
        }

        if let Some(minute) = minute {
            assert!(minute < 60, "Minute must be between 0 and 59");
        }

        Self {
            hour,
            minute,
            timezone,
            leeway: leeway.unwrap_or_default(),
        }
    }
}

impl<Player, Metadata, Trigger> SingleEvaluator<Player, Metadata, Trigger> for TimeEvaluator
where
    Trigger: Copy + PartialEq + Eq + Debug,
{
    fn evaluate(&self, ctx: &AchievementContext<Player, Metadata, Trigger>) -> bool {
        let now = ctx.current_bloop.recorded_at.with_timezone(&self.timezone);

        let target_time = now.with_time(
            NaiveTime::from_hms_opt(
                self.hour.unwrap_or(now.hour()),
                self.minute.unwrap_or(now.minute()),
                0,
            )
            .unwrap(),
        );

        let target_time = match target_time {
            LocalResult::Single(target_time) => target_time,
            LocalResult::Ambiguous(earliest, _) => earliest,
            LocalResult::None => return false,
        };

        now >= target_time && now <= target_time + self.leeway
    }
}
