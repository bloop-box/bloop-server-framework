use crate::achievement::AchievementContext;
use crate::evaluator::{EvalResult, Evaluator};
use chrono::{LocalResult, NaiveTime, Timelike};
use std::fmt::Debug;
use std::time::Duration;
use thiserror::Error;

/// Evaluates whether the current bloop's timestamp falls within a specified
/// time window.
///
/// The `TimeEvaluator` checks if the `recorded_at` time of a bloop, converted
/// to a configured timezone, matches the given hour and/or minute, allowing for
/// an optional leeway duration.
///
/// # Behavior
///
/// - If `hour` is `Some`, it checks if the time's hour equals.
/// - If `minute` is `Some`, it checks if the time's minute equals.
/// - If either `hour` or `minute` is `None`, that component is ignored
///   (i.e., any hour or minute matches).
/// - The time window starts at the exact configured hour/minute (or current
///   hour/minute if `None`),
///   and extends forward by the `leeway` duration.
/// - Handles daylight saving time ambiguities by selecting the earliest
///   matching local time.
/// - If the configured time does not exist on the day (e.g., during a DST gap),
///   evaluation returns false.
#[derive(Debug)]
pub struct TimeEvaluator {
    hour: Option<u32>,
    minute: Option<u32>,
    timezone: chrono_tz::Tz,
    leeway: Duration,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("hour must be between 0 and 23")]
    InvalidHour(u32),
    #[error("minute must be between 0 and 59")]
    InvalidMinute(u32),
}

type Result<T> = std::result::Result<T, Error>;

impl TimeEvaluator {
    /// Creates a new `TimeEvaluator`.
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
    /// ).unwrap();
    /// ```
    pub fn new(
        hour: Option<u32>,
        minute: Option<u32>,
        timezone: chrono_tz::Tz,
        leeway: Option<Duration>,
    ) -> Result<Self> {
        if let Some(hour) = hour
            && hour > 23
        {
            return Err(Error::InvalidHour(hour));
        }

        if let Some(minute) = minute
            && minute > 59
        {
            return Err(Error::InvalidMinute(minute));
        }

        Ok(Self {
            hour,
            minute,
            timezone,
            leeway: leeway.unwrap_or_default(),
        })
    }
}

impl<Player, Metadata, Trigger> Evaluator<Player, Metadata, Trigger> for TimeEvaluator {
    fn evaluate(
        &self,
        ctx: &AchievementContext<Player, Metadata, Trigger>,
    ) -> impl Into<EvalResult> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bloop::Bloop;
    use crate::evaluator::{EvalResult, Evaluator};
    use crate::test_utils::{MockPlayer, TestCtxBuilder};
    use chrono::{DateTime, TimeZone, Utc};
    use chrono_tz::Europe::Berlin;
    use std::time::Duration;

    fn make_ctx_builder_for_time(dt: DateTime<Utc>) -> TestCtxBuilder<MockPlayer, (), ()> {
        let (player, _) = MockPlayer::builder().build();
        let bloop = Bloop::new(player.clone(), "client1", dt);
        TestCtxBuilder::new(bloop)
    }

    #[test]
    fn matches_exact_hour_minute() {
        let time = Berlin
            .with_ymd_and_hms(2024, 10, 1, 14, 30, 0)
            .unwrap()
            .with_timezone(&Utc);
        let mut builder = make_ctx_builder_for_time(time);
        let evaluator = TimeEvaluator::new(Some(14), Some(30), Berlin, None).unwrap();
        assert_eq!(
            evaluator.evaluate(&builder.build()).into(),
            EvalResult::AwardSelf
        );
    }

    #[test]
    fn within_leeway() {
        let time = Berlin
            .with_ymd_and_hms(2024, 10, 1, 14, 30, 30)
            .unwrap()
            .with_timezone(&Utc);
        let mut builder = make_ctx_builder_for_time(time);
        let evaluator =
            TimeEvaluator::new(Some(14), Some(30), Berlin, Some(Duration::from_secs(60))).unwrap();
        assert_eq!(
            evaluator.evaluate(&builder.build()).into(),
            EvalResult::AwardSelf
        );
    }

    #[test]
    fn outside_leeway_fails() {
        let time = Berlin
            .with_ymd_and_hms(2024, 10, 1, 14, 32, 0)
            .unwrap()
            .with_timezone(&Utc);
        let mut builder = make_ctx_builder_for_time(time);
        let evaluator =
            TimeEvaluator::new(Some(14), Some(30), Berlin, Some(Duration::from_secs(60))).unwrap();
        assert_eq!(
            evaluator.evaluate(&builder.build()).into(),
            EvalResult::NoAward
        );
    }

    #[test]
    fn with_only_hour() {
        let time = Berlin
            .with_ymd_and_hms(2024, 10, 1, 9, 15, 0)
            .unwrap()
            .with_timezone(&Utc);
        let mut builder = make_ctx_builder_for_time(time);
        let evaluator =
            TimeEvaluator::new(Some(9), None, Berlin, Some(Duration::from_secs(1800))).unwrap();
        assert_eq!(
            evaluator.evaluate(&builder.build()).into(),
            EvalResult::AwardSelf
        );
    }

    #[test]
    fn with_only_minute() {
        let time = Berlin
            .with_ymd_and_hms(2024, 10, 1, 22, 45, 0)
            .unwrap()
            .with_timezone(&Utc);
        let mut builder = make_ctx_builder_for_time(time);
        let evaluator =
            TimeEvaluator::new(None, Some(45), Berlin, Some(Duration::from_secs(60))).unwrap();
        assert_eq!(
            evaluator.evaluate(&builder.build()).into(),
            EvalResult::AwardSelf
        );
    }

    #[test]
    fn dst_gap_returns_false() {
        let time = Utc.with_ymd_and_hms(2024, 10, 27, 1, 30, 0).unwrap();
        let mut builder = make_ctx_builder_for_time(time);
        let evaluator =
            TimeEvaluator::new(Some(2), Some(30), Berlin, Some(Duration::from_secs(60))).unwrap();
        assert_eq!(
            evaluator.evaluate(&builder.build()).into(),
            EvalResult::NoAward
        );
    }

    #[test]
    fn handles_ambiguous_time() {
        let time = Utc.with_ymd_and_hms(2024, 10, 27, 0, 30, 0).unwrap();
        let mut builder = make_ctx_builder_for_time(time);
        let evaluator =
            TimeEvaluator::new(Some(2), Some(30), Berlin, Some(Duration::from_secs(60))).unwrap();
        assert_eq!(
            evaluator.evaluate(&builder.build()).into(),
            EvalResult::AwardSelf
        );
    }
}
