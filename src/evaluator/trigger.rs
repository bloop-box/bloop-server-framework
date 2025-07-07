use crate::achievement::AchievementContext;
use crate::evaluator::{EvalResult, Evaluator};
use std::fmt::Debug;

/// Evaluates whether a specific trigger is active for the current player.
///
/// This evaluator checks if the given `Trigger` is present in the context's
/// [`TriggerRegistry`]. It can be used to award achievements to players via
/// special trigger NFC tags.
#[derive(Debug)]
pub struct TriggerEvaluator<Trigger: PartialEq + Copy> {
    trigger: Trigger,
}

impl<Trigger: PartialEq + Copy> TriggerEvaluator<Trigger> {
    /// Creates a new `TriggerEvaluator` for the specified trigger.
    ///
    /// # Examples
    ///
    /// ```
    /// use bloop_server_framework::evaluator::trigger::TriggerEvaluator;
    ///
    /// #[derive(Copy, Clone, PartialEq)]
    /// enum MyTrigger {
    ///     CompletedTutorial,
    /// }
    ///
    /// let evaluator = TriggerEvaluator::new(MyTrigger::CompletedTutorial);
    /// ```
    pub fn new(trigger: Trigger) -> Self {
        Self { trigger }
    }
}

impl<Player, Metadata, Trigger> Evaluator<Player, Metadata, Trigger> for TriggerEvaluator<Trigger>
where
    Trigger: PartialEq + Copy + Send + Sync,
{
    fn evaluate(
        &self,
        ctx: &AchievementContext<Player, Metadata, Trigger>,
    ) -> impl Into<EvalResult> {
        ctx.has_trigger(self.trigger)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bloop::Bloop;
    use crate::evaluator::{EvalResult, Evaluator};
    use crate::nfc_uid::NfcUid;
    use crate::test_utils::{MockPlayer, TestCtxBuilder};
    use crate::trigger::{TriggerOccurrence, TriggerRegistry, TriggerSpec};
    use chrono::Utc;
    use std::collections::HashMap;

    #[derive(Debug, Copy, Clone, PartialEq, Eq)]
    enum Trigger {
        Foo,
        Bar,
    }

    #[test]
    fn returns_true_when_trigger_is_present() {
        let (player, _) = MockPlayer::builder().build();
        let bloop = Bloop::new(player.clone(), "client1", Utc::now());
        let mut triggers = HashMap::new();
        triggers.insert(
            NfcUid::default(),
            TriggerSpec {
                trigger: Trigger::Foo,
                global: false,
                occurrence: TriggerOccurrence::Once,
            },
        );
        let mut trigger_registry = TriggerRegistry::new(triggers);
        trigger_registry.try_activate_trigger(NfcUid::default(), "client1");

        let mut builder = TestCtxBuilder::new(bloop).trigger_registry(trigger_registry);

        let evaluator = TriggerEvaluator::new(Trigger::Foo);
        assert_eq!(
            evaluator.evaluate(&builder.build()).into(),
            EvalResult::AwardSelf
        );
    }

    #[test]
    fn returns_false_when_trigger_is_missing() {
        let (player, _) = MockPlayer::builder().build();
        let bloop = Bloop::new(player.clone(), "client1", Utc::now());

        let mut builder =
            TestCtxBuilder::new(bloop).trigger_registry(TriggerRegistry::new(HashMap::new()));

        let evaluator = TriggerEvaluator::new(Trigger::Bar);
        assert_eq!(
            evaluator.evaluate(&builder.build()).into(),
            EvalResult::NoAward
        );
    }
}
