use crate::achievement::AchievementContext;
use crate::evaluator::SingleEvaluator;
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

    if number <= 1 || number % 2 == 0 || number % 3 == 0 {
        return false;
    }

    let mut i = 5;

    while i * i <= number {
        if number % i == 0 || number % (i + 2) == 0 {
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
pub struct NumberPropertyEvaluator<F>
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

impl<F> NumberPropertyEvaluator<F>
where
    F: Fn(usize) -> bool + Send + Sync + 'static,
{
    /// Creates a new [`NumberPropertyEvaluator`] with the given predicate, count, and window.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::time::Duration;
    /// use bloop_server_framework::evaluator::registration_number::{is_prime, NumberPropertyEvaluator};
    ///
    /// let evaluator = NumberPropertyEvaluator::new(is_prime, 3, Duration::from_secs(60));
    /// ```
    pub fn new(predicate: F, min_required: usize, max_window: Duration) -> Self {
        Self {
            predicate,
            min_required,
            max_window,
        }
    }
}

impl<Player, Metadata, Trigger, F> SingleEvaluator<Player, Metadata, Trigger>
    for NumberPropertyEvaluator<F>
where
    Player: RegistrationNumberProvider,
    Trigger: Copy + PartialEq + Eq + Debug,
    F: Fn(usize) -> bool + Send + Sync + 'static,
{
    fn evaluate(&self, ctx: &AchievementContext<Player, Metadata, Trigger>) -> bool {
        ctx.client_bloops()
            .filter(ctx.filter_within_window(self.max_window))
            .take(self.min_required)
            .all(|bloop| (self.predicate)(bloop.player().registration_number()))
    }
}

impl<F> Debug for NumberPropertyEvaluator<F>
where
    F: Fn(usize) -> bool + Send + Sync + 'static,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NumberPropertyEvaluator")
            .field("predicate", &"<closure>")
            .field("min_required", &self.min_required)
            .field("max_window", &self.max_window)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use crate::evaluator::registration_number::{
        internal_is_fibonacci_no_cache, internal_is_prime_no_cache,
    };

    #[test]
    fn fibonacci_test_works_correctly() {
        assert!(internal_is_fibonacci_no_cache(1));
        assert!(internal_is_fibonacci_no_cache(2));
        assert!(internal_is_fibonacci_no_cache(3));
        assert!(!internal_is_fibonacci_no_cache(4));
        assert!(internal_is_fibonacci_no_cache(5));
        assert!(!internal_is_fibonacci_no_cache(6));
        assert!(!internal_is_fibonacci_no_cache(7));
        assert!(internal_is_fibonacci_no_cache(8));
        assert!(!internal_is_fibonacci_no_cache(9));
        assert!(!internal_is_fibonacci_no_cache(10));
        assert!(!internal_is_fibonacci_no_cache(11));
        assert!(!internal_is_fibonacci_no_cache(12));
        assert!(internal_is_fibonacci_no_cache(13));
    }

    #[test]
    fn prime_test_works_correctly() {
        assert!(!internal_is_prime_no_cache(1));
        assert!(internal_is_prime_no_cache(2));
        assert!(internal_is_prime_no_cache(3));
        assert!(!internal_is_prime_no_cache(4));
        assert!(internal_is_prime_no_cache(5));
        assert!(!internal_is_prime_no_cache(6));
        assert!(internal_is_prime_no_cache(7));
        assert!(!internal_is_prime_no_cache(8));
        assert!(!internal_is_prime_no_cache(9));
        assert!(!internal_is_prime_no_cache(10));
        assert!(internal_is_prime_no_cache(11));
        assert!(!internal_is_prime_no_cache(12));
        assert!(internal_is_prime_no_cache(13));
    }
}
