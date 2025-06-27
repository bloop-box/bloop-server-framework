use crate::achievement::AchievementContext;
use crate::evaluator::SingleEvaluator;
use std::collections::HashMap;
use std::fmt::Debug;
use std::time::Duration;
use thiserror::Error;

/// Macro to conveniently create a [`HashMap<char, &str>`] mapping characters to
/// client IDs.
///
/// # Examples
///
/// ```
/// use bloop_server_framework::char_map;
///
/// let letters = char_map! {
///     'a' => "client1",
///     'b' => "client2",
///     'c' => "client3",
/// };
/// ```
#[macro_export]
macro_rules! char_map {
    ($($char:literal => $client_id:expr),* $(,)?) => {{
        let mut map = ::std::collections::HashMap::new();
        $(
            map.insert($char, $client_id);
        )*
        map
    }};
}

/// Evaluates whether a player has correctly spelled a given word within a time
/// limit.
///
/// The evaluator uses a mapping of characters to client IDs and expects the
/// player to "bloop" each letter in the word in the correct order, within a
/// time window calculated as `time_per_character * word.len()`.
#[derive(Debug)]
pub struct SpellingBeeEvaluator {
    client_ids: Vec<String>,
    max_time: Duration,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("the character '{0}' does not exist in character map")]
    UnregisteredCharacter(char),
    #[error("the provided word must be at least one character")]
    WordTooShort,
    #[error("the provided word is too long")]
    WordTooLong,
}

type Result<T> = std::result::Result<T, Error>;

impl SpellingBeeEvaluator {
    /// Creates a new [`SpellingBeeEvaluator`].
    ///
    /// # Examples
    ///
    /// ```
    /// use std::time::Duration;
    /// use bloop_server_framework::evaluator::spelling_bee::SpellingBeeEvaluator;
    /// use bloop_server_framework::char_map;
    ///
    /// let characters = char_map! {
    ///     'h' => "clientA",
    ///     'e' => "clientB",
    ///     'l' => "clientC",
    ///     'o' => "clientD",
    /// };
    ///
    /// let evaluator = SpellingBeeEvaluator::new(
    ///     "hello",
    ///     &characters,
    ///     Duration::from_secs(60 * 2),
    /// );
    /// ```
    pub fn new(
        word: &str,
        characters: &HashMap<char, &str>,
        time_per_character: Duration,
    ) -> Result<Self> {
        let mut client_ids = Vec::with_capacity(word.len());

        if word.is_empty() {
            return Err(Error::WordTooShort);
        }

        if word.len() > 100 {
            return Err(Error::WordTooLong);
        }

        let max_time = time_per_character
            .checked_mul(word.len() as u32)
            .ok_or(Error::WordTooLong)?;

        for char in word.chars().rev() {
            let Some(client_id) = characters.get(&char) else {
                return Err(Error::UnregisteredCharacter(char));
            };

            client_ids.push(client_id.to_string());
        }

        Ok(Self {
            client_ids,
            max_time,
        })
    }
}

impl<Player, Metadata, Trigger> SingleEvaluator<Player, Metadata, Trigger> for SpellingBeeEvaluator
where
    Trigger: Copy + PartialEq + Eq + Debug,
{
    /// Evaluates if the player's recent bloops match the target spelling sequence.
    ///
    /// Checks that the bloops for letters happen in order within the allowed time
    /// window.
    fn evaluate(&self, ctx: &AchievementContext<Player, Metadata, Trigger>) -> bool {
        if ctx.current_bloop.client_id != *self.client_ids.first().unwrap() {
            return false;
        }

        let last_client_ids = ctx
            .global_bloops()
            .filter(ctx.filter_within_window(self.max_time))
            .filter(ctx.filter_current_player())
            .take(self.client_ids.len())
            .map(|bloop| &bloop.client_id);

        last_client_ids.eq(self.client_ids.iter().skip(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bloop::Bloop;
    use crate::char_map;
    use crate::evaluator::test_utils::{MockPlayer, TestCtxBuilder};
    use chrono::Utc;
    use std::sync::{Arc, RwLock};
    use std::time::Duration;

    fn make_bloop(
        player: &Arc<RwLock<MockPlayer>>,
        client_id: &str,
        offset_secs: u64,
    ) -> Bloop<MockPlayer> {
        let recorded_at = Utc::now() - Duration::from_secs(offset_secs);
        Bloop::new(player.clone(), client_id.to_string(), recorded_at)
    }

    #[test]
    fn matches_correct_sequence() {
        let word = "hey";
        let client_map = char_map! {
            'h' => "client1",
            'e' => "client2",
            'y' => "client3"
        };

        let (player, _) = MockPlayer::builder().build();
        let bloops = vec![
            make_bloop(&player, "client1", 20),
            make_bloop(&player, "client2", 10),
        ];
        let mut builder = TestCtxBuilder::new(make_bloop(&player, "client3", 0)).bloops(bloops);

        let evaluator =
            SpellingBeeEvaluator::new(word, &client_map, Duration::from_secs(15)).unwrap();
        assert!(evaluator.evaluate(&builder.build()));
    }

    #[test]
    fn fails_on_wrong_order() {
        let word = "hey";
        let client_map = char_map! {
            'h' => "client1",
            'e' => "client2",
            'y' => "client3"
        };

        let (player, _) = MockPlayer::builder().build();
        let bloops = vec![
            make_bloop(&player, "client1", 20),
            make_bloop(&player, "client3", 10),
        ];

        let mut builder = TestCtxBuilder::new(make_bloop(&player, "client2", 0)).bloops(bloops);
        let ctx = builder.build();

        let evaluator =
            SpellingBeeEvaluator::new(word, &client_map, Duration::from_secs(15)).unwrap();
        assert!(!evaluator.evaluate(&ctx));
    }

    #[test]
    fn fails_if_current_id_mismatch() {
        let word = "abc";
        let client_map = char_map! {
            'a' => "client1",
            'b' => "client2",
            'c' => "client3"
        };

        let (player, _) = MockPlayer::builder().build();
        let bloops = vec![
            make_bloop(&player, "client1", 20),
            make_bloop(&player, "client2", 10),
        ];

        let mut builder =
            TestCtxBuilder::new(make_bloop(&player, "wrong_client", 0)).bloops(bloops);
        let ctx = builder.build();

        let evaluator =
            SpellingBeeEvaluator::new(word, &client_map, Duration::from_secs(15)).unwrap();
        assert!(!evaluator.evaluate(&ctx));
    }

    #[test]
    fn fails_if_too_slow() {
        let word = "abc";
        let client_map = char_map! {
            'a' => "client1",
            'b' => "client2",
            'c' => "client3"
        };

        let (player, _) = MockPlayer::builder().build();
        let player = player.clone();
        let bloops = vec![
            make_bloop(&player, "client1", 100),
            make_bloop(&player, "client2", 50),
        ];

        let mut builder = TestCtxBuilder::new(make_bloop(&player, "client3", 0)).bloops(bloops);
        let ctx = builder.build();

        let evaluator =
            SpellingBeeEvaluator::new(word, &client_map, Duration::from_secs(10)).unwrap();
        assert!(!evaluator.evaluate(&ctx));
    }

    #[test]
    fn build_fails_if_unregistered_char() {
        let client_map = char_map! {
            'a' => "client1",
            'b' => "client2"
        };

        let result = SpellingBeeEvaluator::new("abc", &client_map, Duration::from_secs(5));
        assert!(matches!(result, Err(Error::UnregisteredCharacter('c'))));
    }

    #[test]
    fn build_fails_if_word_too_short() {
        let client_map = HashMap::new();

        let result = SpellingBeeEvaluator::new("", &client_map, Duration::from_secs(1));
        assert!(matches!(result, Err(Error::WordTooShort)));
    }

    #[test]
    fn build_fails_if_word_too_long() {
        let client_map = ('a'..='z').map(|c| (c, "client")).collect();
        let long_word: String = std::iter::repeat('a').take(1_000).collect();

        let result = SpellingBeeEvaluator::new(&long_word, &client_map, Duration::from_secs(1));
        assert!(matches!(result, Err(Error::WordTooLong)));
    }
}
