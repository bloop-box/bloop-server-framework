use std::fmt::Debug;
use crate::achievement::AchievementContext;
use crate::evaluator::Evaluator;
use crate::evaluator::streak::StreakEvaluatorBuilder;
use cached::proc_macro::cached;
use num_integer::Roots;
use std::sync::Arc;
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

/// Builds a [`crate::evaluator::streak::StreakEvaluator`] that matches
/// registration numbers against a predicated.
pub fn build_predicate_evaluator<Player, State, Trigger, F>(
    predicate: F,
    min_required: usize,
    max_window: Duration,
) -> impl Evaluator<Player, State, Trigger> + Debug
where
    Player: RegistrationNumberProvider + Send + Sync + 'static,
    State: 'static,
    Trigger: 'static,
    F: Fn(usize) -> bool + Send + Sync + 'static,
{
    StreakEvaluatorBuilder::new()
        .min_required(min_required)
        .max_window(max_window)
        .build(move |player: &Player| predicate(player.registration_number()))
}

/// Builds a [`crate::evaluator::streak::StreakEvaluator`] that matches
/// registration number projections against the project of the current player.
pub fn build_projection_match_evaluator<Player, State, Trigger, F, V>(
    projector: F,
    min_required: usize,
    max_window: Duration,
) -> impl Evaluator<Player, State, Trigger> + Debug
where
    Player: RegistrationNumberProvider,
    F: Fn(usize) -> V + Send + Sync + 'static,
    V: PartialEq + Clone + Send + Sync + 'static,
{
    let projector = Arc::new(projector);

    StreakEvaluatorBuilder::new()
        .min_required(min_required)
        .max_window(max_window)
        .build_with_derived_ctx(
            {
                let projector = projector.clone();
                move |ctx: &AchievementContext<Player, State, Trigger>| {
                    let number = ctx.current_bloop.player().registration_number();
                    projector(number)
                }
            },
            move |player: &Player, reference: &V| {
                projector(player.registration_number()) == *reference
            },
        )
}

/// Calculates the sum of the digits of a number.
pub fn digit_sum(number: usize) -> usize {
    let mut sum = 0;
    let mut value = number;

    while value > 0 {
        sum += value % 10;
        value /= 10;
    }

    sum
}

#[cfg(test)]
mod tests {
    use super::*;

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
