use crate::achievement::AchievementContext;
use crate::evaluator::{EvalResult, Evaluator};
use cached::proc_macro::cached;
use num_integer::Roots;
use std::fmt;
use std::fmt::Debug;
use std::time::Duration;

/// Trait to provide a registration number for a player.
pub trait RegistrationNumberProvider {
    fn registration_number(&self) -> usize;
}

/// Checks if a number is prime.
pub fn is_prime(number: usize) -> bool {
    internal_is_prime(number)
}

#[cached(size = 10_000)]
#[inline]
fn internal_is_prime(number: usize) -> bool {
    if number == 2 || number == 3 {
        return true;
    }

    if number <= 1 || number.is_multiple_of(2) || number.is_multiple_of(3) {
        return false;
    }

    let mut i = 5;

    while i * i <= number {
        if number.is_multiple_of(i) || number.is_multiple_of(i + 2) {
            return false;
        }

        i += 6;
    }

    true
}

/// Checks if a number is a perfect square.
pub fn is_perfect_square(number: usize) -> bool {
    internal_is_perfect_square(number)
}

#[cached(size = 10_000)]
#[inline]
fn internal_is_perfect_square(number: usize) -> bool {
    number.sqrt().pow(2) == number
}

/// Checks if a number is a Fibonacci number.
pub fn is_fibonacci(number: usize) -> bool {
    internal_is_fibonacci(number)
}

#[cached(size = 10_000)]
#[inline]
fn internal_is_fibonacci(number: usize) -> bool {
    is_perfect_square(5 * number * number + 4) || is_perfect_square(5 * number * number - 4)
}

/// Evaluates if recent bloops meet a numeric property predicate.
///
/// Checks if at least `min_required` recent bloops within `max_window` satisfy the given predicate
/// on player registration numbers.
pub struct NumberPredicateEvaluator<F>
where
    F: Fn(usize) -> bool + Send + Sync + 'static,
{
    /// Closure or function to test a player's registration number.
    predicate: F,

    /// Minimum number of bloops that must pass the test.
    min_required: usize,

    /// Time window in which to evaluate recent bloops, counting backward from the current one.
    max_window: Duration,
}

impl<F> NumberPredicateEvaluator<F>
where
    F: Fn(usize) -> bool + Send + Sync + 'static,
{
    /// Creates a new [`NumberPredicateEvaluator`] with the given predicate, count, and window.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::time::Duration;
    /// use bloop_server_framework::evaluator::registration_number::{
    ///     is_prime,
    ///     NumberPredicateEvaluator,
    /// };
    ///
    /// let evaluator = NumberPredicateEvaluator::new(is_prime, 3, Duration::from_secs(60));
    /// ```
    pub fn new(predicate: F, min_required: usize, max_window: Duration) -> Self {
        Self {
            predicate,
            min_required,
            max_window,
        }
    }
}

impl<Player, Metadata, Trigger, F> Evaluator<Player, Metadata, Trigger>
    for NumberPredicateEvaluator<F>
where
    Player: RegistrationNumberProvider,
    F: Fn(usize) -> bool + Send + Sync + 'static,
{
    fn evaluate(
        &self,
        ctx: &AchievementContext<Player, Metadata, Trigger>,
    ) -> impl Into<EvalResult> {
        let bloops = ctx
            .client_bloops()
            .filter(ctx.filter_within_window(self.max_window))
            .take(self.min_required)
            .collect::<Vec<_>>();

        bloops
            .iter()
            .all(|bloop| (self.predicate)(bloop.player().registration_number()))
            && bloops.len() >= self.min_required
    }
}

impl<F> Debug for NumberPredicateEvaluator<F>
where
    F: Fn(usize) -> bool + Send + Sync + 'static,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NumberPredicateEvaluator")
            .field("predicate", &"<closure>")
            .field("min_required", &self.min_required)
            .field("max_window", &self.max_window)
            .finish()
    }
}

pub fn digit_sum(number: usize) -> usize {
    let mut sum = 0;
    let mut value = number;

    while value > 0 {
        sum += value % 10;
        value /= 10;
    }

    sum
}

/// Evaluates whether a number projection of recent players matches that of the
/// current player.
///
/// The [`ProjectionMatchEvaluator`] takes a projection function that
/// transforms a player's registration number into a comparable value (e.g.,
/// digit sum, modulo, etc.). It then checks whether at least `min_required`
/// recent players (within a given time window) have the *same* projected value
/// as the current player.
///
/// This is useful for recognizing players with "numerical affinity", such as
/// those with the same digit sum, same last digit, or belonging to the same
/// modulo class.
///
/// # Notes
///
/// - The projection function must return a value that implements [`PartialEq`].
/// - Only bloops from the same client (i.e., `client_bloops()`) are considered.
/// - The comparison starts from the most recent bloop and checks up to
///   `min_required`.
/// - The current bloop is used as the reference point for the projected value.
pub struct ProjectionMatchEvaluator<F, V>
where
    F: Fn(usize) -> V + Send + Sync + 'static,
    V: PartialEq,
{
    /// Closure or function to project a player's registration number.
    projector: F,

    /// Minimum number of bloops that must pass the test.
    min_required: usize,

    /// Time window in which to evaluate recent bloops, counting backward from the
    /// current one.
    max_window: Duration,
}

impl<F, V> ProjectionMatchEvaluator<F, V>
where
    F: Fn(usize) -> V + Send + Sync + 'static,
    V: PartialEq,
{
    /// Creates a new [`ProjectionMatchEvaluator`] with the given projector, count,
    /// and window.
    ///
    /// # Examples
    ///
    /// Basic usage with a digit sum comparison:
    ///
    /// ```
    /// use std::time::Duration;
    /// use bloop_server_framework::evaluator::registration_number::{
    ///     digit_sum,
    ///     ProjectionMatchEvaluator,
    /// };
    ///
    /// let evaluator = ProjectionMatchEvaluator::new(
    ///     digit_sum,
    ///     3,
    ///     Duration::from_secs(60),
    /// );
    /// ```
    ///
    /// Custom projection based on the last digit:
    ///
    /// ```
    /// use std::time::Duration;
    /// use bloop_server_framework::evaluator::{
    ///     registration_number::ProjectionMatchEvaluator
    /// };
    ///
    /// let evaluator = ProjectionMatchEvaluator::new(
    ///     |n| n % 10,
    ///     2,
    ///     Duration::from_secs(30),
    /// );
    /// ```
    pub fn new(projector: F, min_required: usize, max_window: Duration) -> Self {
        Self {
            projector,
            min_required,
            max_window,
        }
    }
}

impl<F, V> Debug for ProjectionMatchEvaluator<F, V>
where
    F: Fn(usize) -> V + Send + Sync + 'static,
    V: PartialEq,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProjectionMatchEvaluator")
            .field("projector", &"<closure>")
            .field("min_required", &self.min_required)
            .field("max_window", &self.max_window)
            .finish()
    }
}

impl<Player, Metadata, Trigger, F, V> Evaluator<Player, Metadata, Trigger>
    for ProjectionMatchEvaluator<F, V>
where
    Player: RegistrationNumberProvider,
    F: Fn(usize) -> V + Send + Sync + 'static,
    V: PartialEq,
{
    fn evaluate(
        &self,
        ctx: &AchievementContext<Player, Metadata, Trigger>,
    ) -> impl Into<EvalResult> {
        let reference = (self.projector)(ctx.current_bloop.player().registration_number());

        let bloops = ctx
            .client_bloops()
            .filter(ctx.filter_within_window(self.max_window))
            .take(self.min_required)
            .collect::<Vec<_>>();

        bloops
            .iter()
            .all(|bloop| (self.projector)(bloop.player().registration_number()) == reference)
            && bloops.len() >= self.min_required
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bloop::Bloop;
    use crate::evaluator::{EvalResult, Evaluator};
    use crate::test_utils::{MockPlayer, TestCtxBuilder};
    use chrono::{DateTime, Utc};
    use std::time::{Duration, SystemTime};

    fn make_bloop(number: usize, seconds_ago: u64) -> Bloop<MockPlayer> {
        let now = SystemTime::now() - Duration::from_secs(seconds_ago);
        let timestamp: DateTime<Utc> = now.into();

        let (player, _) = MockPlayer::builder().registration_number(number).build();
        Bloop::new(player, "test", timestamp)
    }

    #[test]
    fn predicate_eval_returns_true_when_enough_bloops_match_predicate() {
        let evaluator = NumberPredicateEvaluator::new(|n| n % 2 == 0, 3, Duration::from_secs(60));

        let current = make_bloop(8, 0);
        let past = vec![
            make_bloop(2, 10),
            make_bloop(4, 20),
            make_bloop(6, 30),
            make_bloop(3, 40),
        ];

        let mut builder = TestCtxBuilder::new(current).bloops(past);
        assert_eq!(
            evaluator.evaluate(&builder.build()).into(),
            EvalResult::AwardSelf
        );
    }

    #[test]
    fn predicate_eval_returns_false_when_not_enough_bloops_match_predicate() {
        let evaluator = NumberPredicateEvaluator::new(|n| n % 2 == 0, 3, Duration::from_secs(60));

        let current = make_bloop(1, 0);
        let past = vec![make_bloop(2, 10), make_bloop(3, 20), make_bloop(5, 30)];

        let mut builder = TestCtxBuilder::new(current).bloops(past);
        assert_eq!(
            evaluator.evaluate(&builder.build()).into(),
            EvalResult::NoAward
        );
    }

    #[test]
    fn predicate_eval_ignores_bloops_outside_time_window() {
        let evaluator = NumberPredicateEvaluator::new(|n| n % 2 == 0, 2, Duration::from_secs(20));

        let current = make_bloop(8, 0);
        let past = vec![make_bloop(2, 10), make_bloop(4, 25), make_bloop(6, 30)];

        let mut builder = TestCtxBuilder::new(current).bloops(past);
        assert_eq!(
            evaluator.evaluate(&builder.build()).into(),
            EvalResult::NoAward
        );
    }

    #[test]
    fn predicate_eval_takes_only_min_required_recent_matches() {
        let evaluator = NumberPredicateEvaluator::new(|n| n < 10, 2, Duration::from_secs(60));

        let current = make_bloop(1, 0);
        let past = vec![make_bloop(9, 10), make_bloop(8, 15), make_bloop(100, 20)];

        let mut builder = TestCtxBuilder::new(current).bloops(past);
        assert_eq!(
            evaluator.evaluate(&builder.build()).into(),
            EvalResult::AwardSelf
        );
    }

    #[test]
    fn projection_eval_returns_true_when_enough_bloops_match_projection() {
        let evaluator = ProjectionMatchEvaluator::new(|n| n % 2, 3, Duration::from_secs(60));

        let current = make_bloop(4, 0);
        let past = vec![
            make_bloop(2, 10),
            make_bloop(6, 20),
            make_bloop(8, 30),
            make_bloop(3, 40),
        ];

        let mut builder = TestCtxBuilder::new(current).bloops(past);
        assert_eq!(
            evaluator.evaluate(&builder.build()).into(),
            EvalResult::AwardSelf
        );
    }

    #[test]
    fn projection_eval_returns_false_when_not_enough_bloops_match_projection() {
        let evaluator = ProjectionMatchEvaluator::new(|n| n % 2, 4, Duration::from_secs(60));

        let current = make_bloop(4, 0);
        let past = vec![make_bloop(2, 10), make_bloop(6, 20), make_bloop(3, 30)];

        let mut builder = TestCtxBuilder::new(current).bloops(past);
        assert_eq!(
            evaluator.evaluate(&builder.build()).into(),
            EvalResult::NoAward
        );
    }

    #[test]
    fn projection_eval_ignores_bloops_outside_time_window() {
        let evaluator = ProjectionMatchEvaluator::new(|n| n % 2, 2, Duration::from_secs(20));

        let current = make_bloop(4, 0);
        let past = vec![make_bloop(2, 10), make_bloop(6, 25), make_bloop(8, 30)];

        let mut builder = TestCtxBuilder::new(current).bloops(past);
        assert_eq!(
            evaluator.evaluate(&builder.build()).into(),
            EvalResult::NoAward
        );
    }

    #[test]
    fn projection_eval_takes_only_min_required_recent_matches() {
        let evaluator = ProjectionMatchEvaluator::new(|n| n, 2, Duration::from_secs(60));

        let current = make_bloop(4, 0);
        let past = vec![make_bloop(4, 10), make_bloop(4, 15), make_bloop(4, 20)];

        let mut builder = TestCtxBuilder::new(current).bloops(past);
        assert_eq!(
            evaluator.evaluate(&builder.build()).into(),
            EvalResult::AwardSelf
        );
    }

    #[test]
    fn returns_true_for_perfect_squares() {
        for n in [
            0, 1, 4, 9, 16, 25, 36, 49, 64, 81, 100, 121, 144, 1024, 4096, 10000,
        ] {
            assert!(
                internal_is_perfect_square_no_cache(n),
                "{n} should be a perfect square"
            );
        }
    }

    #[test]
    fn returns_false_for_non_squares() {
        for n in [
            2, 3, 5, 6, 8, 10, 26, 50, 63, 65, 99, 101, 123, 10001, 12345,
        ] {
            assert!(
                !internal_is_perfect_square_no_cache(n),
                "{n} should not be a perfect square"
            );
        }
    }

    #[test]
    fn returns_true_for_primes() {
        for n in [
            2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37, 41, 43, 47, 101, 997,
        ] {
            assert!(internal_is_prime_no_cache(n), "{n} should be prime");
        }
    }

    #[test]
    fn returns_false_for_non_primes() {
        for n in [
            0, 1, 4, 6, 8, 9, 10, 12, 15, 21, 25, 27, 100, 1024, 4096, 10000,
        ] {
            assert!(!internal_is_prime_no_cache(n), "{n} should not be prime");
        }
    }

    #[test]
    fn returns_true_for_fibonacci_numbers() {
        for n in [
            0, 1, 2, 3, 5, 8, 13, 21, 34, 55, 89, 144, 233, 377, 610, 987, 1597,
        ] {
            assert!(
                internal_is_fibonacci_no_cache(n),
                "{n} should be a Fibonacci number"
            );
        }
    }

    #[test]
    fn returns_false_for_non_fibonacci_numbers() {
        for n in [
            4, 6, 7, 9, 10, 11, 12, 14, 15, 22, 100, 200, 1000, 1234, 2024,
        ] {
            assert!(
                !internal_is_fibonacci_no_cache(n),
                "{n} should not be a Fibonacci number"
            );
        }
    }
}
