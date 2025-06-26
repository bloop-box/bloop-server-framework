use crate::achievement::AchievementContext;
use crate::evaluator::SingleEvaluator;
use std::collections::HashMap;
use std::fmt::Debug;
use std::time::Duration;

/// Macro to conveniently create a [`HashMap<char, &str>`] mapping letters to client IDs.
///
/// # Examples
///
/// ```
/// use bloop_server_framework::letter_map;
///
/// let letters = letter_map! {
///     'a' => "client1",
///     'b' => "client2",
///     'c' => "client3",
/// };
/// ```
#[macro_export]
macro_rules! letter_map {
    ($($char:literal => $client_id:expr),* $(,)?) => {{
        let mut map = ::std::collections::HashMap::new();
        $(
            map.insert($char, $client_id);
        )*
        map
    }};
}

/// Evaluates whether a player has correctly spelled a given word within a time limit.
///
/// The evaluator uses a mapping of letters to client IDs and expects the player to "bloop"
/// each letter in the word in the correct order, within a time window calculated as
/// `time_per_letter * word.len()`.
#[derive(Debug)]
pub struct SpellingBeeEvaluator {
    client_ids: Vec<String>,
    max_time: Duration,
}

impl SpellingBeeEvaluator {
    /// Creates a new [`SpellingBeeEvaluator`].
    ///
    /// # Examples
    ///
    /// ```
    /// use std::time::Duration;
    /// use bloop_server_framework::evaluator::spelling_bee::SpellingBeeEvaluator;
    /// use bloop_server_framework::letter_map;
    ///
    /// let letters = letter_map! {
    ///     'h' => "clientA",
    ///     'e' => "clientB",
    ///     'l' => "clientC",
    ///     'o' => "clientD",
    /// };
    ///
    /// let evaluator = SpellingBeeEvaluator::new(
    ///     "hello",
    ///     &letters,
    ///     Duration::from_secs(60 * 2),
    /// );
    /// ```
    pub fn new(word: &str, letters: &HashMap<char, &str>, time_per_letter: Duration) -> Self {
        let mut client_ids = Vec::with_capacity(word.len());

        for char in word.chars().rev() {
            let Some(client_id) = letters.get(&char) else {
                panic!("Unregistered letter: {}", char);
            };

            client_ids.push(client_id.to_string());
        }

        let max_time = time_per_letter
            .checked_mul(word.len() as u32)
            .expect("Spelling bee word is too long");

        Self {
            client_ids,
            max_time,
        }
    }
}

impl<Player, Metadata, Trigger> SingleEvaluator<Player, Metadata, Trigger> for SpellingBeeEvaluator
where
    Trigger: Copy + PartialEq + Eq + Debug,
{
    /// Evaluates if the player's recent bloops match the target spelling sequence.
    ///
    /// Checks that the bloops for letters happen in order within the allowed time window.
    fn evaluate(&self, ctx: &AchievementContext<Player, Metadata, Trigger>) -> bool {
        let last_client_ids = ctx
            .global_bloops()
            .filter(ctx.filter_within_window(self.max_time))
            .filter(ctx.filter_current_player())
            .take(self.client_ids.len())
            .map(|bloop| &bloop.client_id);

        last_client_ids.eq(self.client_ids.iter())
    }
}
