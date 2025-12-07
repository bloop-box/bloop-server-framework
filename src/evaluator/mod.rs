use crate::achievement::AchievementContext;
use uuid::Uuid;

pub(crate) mod boxed;
pub mod min_bloops;
pub mod registration_number;
pub mod spelling_bee;
pub mod time;
pub mod trigger;

/// Result of evaluating whether an achievement should be awarded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvalResult {
    /// Award the achievement to the evaluating player only.
    AwardSelf,
    /// Award the achievement to multiple players, identified by UUIDs.
    AwardMultiple(Vec<Uuid>),
    /// Do not award the achievement.
    NoAward,
}

impl From<bool> for EvalResult {
    fn from(value: bool) -> Self {
        if value {
            EvalResult::AwardSelf
        } else {
            EvalResult::NoAward
        }
    }
}

impl From<Option<Uuid>> for EvalResult {
    fn from(value: Option<Uuid>) -> Self {
        value.map_or(EvalResult::NoAward, |value| {
            EvalResult::AwardMultiple(vec![value])
        })
    }
}

impl From<Vec<Uuid>> for EvalResult {
    fn from(value: Vec<Uuid>) -> Self {
        Self::AwardMultiple(value)
    }
}

impl From<Option<Vec<Uuid>>> for EvalResult {
    fn from(value: Option<Vec<Uuid>>) -> Self {
        value.map_or(EvalResult::NoAward, |value| {
            EvalResult::AwardMultiple(value)
        })
    }
}

/// Trait for statically typed achievement evaluators.
///
/// This is the primary abstraction for writing custom logic to evaluate whether
/// an achievement should be awarded.
pub trait Evaluator<Player, State, Trigger> {
    /// Evaluate the achievement for the given context.
    fn evaluate(&self, ctx: &AchievementContext<Player, State, Trigger>) -> impl Into<EvalResult>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn converts_bool_to_eval_result() {
        assert_eq!(EvalResult::from(true), EvalResult::AwardSelf);
        assert_eq!(EvalResult::from(false), EvalResult::NoAward);
    }

    #[test]
    fn converts_option_uuid_to_eval_result() {
        let uuid = Uuid::new_v4();
        assert_eq!(
            EvalResult::from(Some(uuid)),
            EvalResult::AwardMultiple(vec![uuid])
        );

        assert_eq!(EvalResult::from(None::<Uuid>), EvalResult::NoAward);
    }

    #[test]
    fn converts_vec_uuid_to_eval_result() {
        let uuid1 = Uuid::new_v4();
        let uuid2 = Uuid::new_v4();
        let uuids = vec![uuid1, uuid2];

        assert_eq!(
            EvalResult::from(uuids.clone()),
            EvalResult::AwardMultiple(uuids)
        );
    }

    #[test]
    fn converts_option_vec_uuid_to_eval_result() {
        let uuid = Uuid::new_v4();

        assert_eq!(
            EvalResult::from(Some(vec![uuid])),
            EvalResult::AwardMultiple(vec![uuid])
        );

        assert_eq!(
            EvalResult::from(Some(vec![])),
            EvalResult::AwardMultiple(vec![])
        );

        assert_eq!(EvalResult::from(None::<Vec<Uuid>>), EvalResult::NoAward);
    }
}
