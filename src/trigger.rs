use crate::nfc_uid::NfcUid;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fmt::Debug;
use std::time::Duration;

/// Specifies how often or for how long a trigger should remain active.
#[derive(Debug, Copy, Clone, Deserialize)]
pub enum TriggerOccurrence {
    /// The trigger can only be activated once.
    Once,
    /// The trigger can be activated a specified number of times.
    Times(usize),
    /// The trigger remains active for the specified duration.
    Duration(Duration),
}

/// Defines the specification of a trigger, including its activation policy and type.
#[derive(Debug, Copy, Clone, Deserialize)]
pub struct TriggerSpec<T: Copy + PartialEq + Eq + Debug> {
    /// Whether the trigger is global (affects all clients) or local (per client).
    global: bool,
    /// How often or for how long this trigger can be active.
    occurrence: TriggerOccurrence,
    /// The trigger identifier of type `T`.
    trigger: T,
}

/// Represents an active trigger instance, tracking its usage and activation time.
#[derive(Debug)]
struct ActiveTrigger<T: Copy + PartialEq + Eq + Debug> {
    /// The specification of the trigger being tracked.
    spec: TriggerSpec<T>,
    /// The time when the trigger was activated.
    triggered_at: DateTime<Utc>,
    /// The number of times this trigger has been used.
    usages: usize,
}

impl<T: Copy + PartialEq + Eq + Debug> ActiveTrigger<T> {
    /// Creates a new active trigger instance for a given spec and client ID.
    ///
    /// The trigger is considered activated at the current time.
    fn new(spec: TriggerSpec<T>) -> Self {
        Self {
            spec,
            triggered_at: Utc::now(),
            usages: 0,
        }
    }

    /// Checks whether the trigger is active with respect to the provided trigger value and
    /// reference time.
    fn check(&mut self, trigger: T, reference_time: DateTime<Utc>) -> (bool, bool) {
        if trigger != self.spec.trigger {
            // The trigger value doesn't match this active trigger's spec.
            return (false, true);
        }

        match self.spec.occurrence {
            TriggerOccurrence::Once => (true, false),
            TriggerOccurrence::Times(times) => {
                let active = self.usages <= times;
                self.usages += 1;
                (active, self.usages <= times)
            }
            TriggerOccurrence::Duration(duration) => {
                let still_active = self.triggered_at + duration <= reference_time;
                (still_active, still_active)
            }
        }
    }
}

/// Registry that manages trigger specifications and tracks active triggers.
#[derive(Debug)]
pub struct TriggerRegistry<T: Copy + PartialEq + Eq + Debug> {
    trigger_specs: HashMap<NfcUid, TriggerSpec<T>>,
    active_local_triggers: HashMap<String, ActiveTrigger<T>>,
    active_global_triggers: Option<ActiveTrigger<T>>,
}

impl<T: Copy + PartialEq + Eq + Debug> TriggerRegistry<T> {
    /// Creates a new [TriggerRegistry] from a set of trigger specifications.
    pub fn new(specs: HashMap<NfcUid, TriggerSpec<T>>) -> Self {
        Self {
            trigger_specs: specs,
            active_local_triggers: HashMap::new(),
            active_global_triggers: None,
        }
    }

    /// Checks if there is an active trigger for the given trigger and client at the specified time.
    ///
    /// This will update usage counts and remove triggers that should no longer be retained.
    pub(crate) fn check_active_trigger(
        &mut self,
        trigger: T,
        client_id: &str,
        reference_time: DateTime<Utc>,
    ) -> bool {
        if let Entry::Occupied(mut active_trigger) =
            self.active_local_triggers.entry(client_id.to_string())
        {
            let (active, retain) = active_trigger.get_mut().check(trigger, reference_time);

            if !retain {
                active_trigger.remove();
            }

            return active;
        }

        self.active_global_triggers
            .take()
            .is_some_and(|mut active_trigger| {
                let (active, retain) = active_trigger.check(trigger, reference_time);

                if retain {
                    self.active_global_triggers = Some(active_trigger);
                }

                active
            })
    }

    /// Attempts to activate a trigger based on the provided NFC UID and client ID.
    ///
    /// If the NFC UID is associated with a trigger spec, an active trigger is created and stored.
    pub(crate) fn try_activate_trigger(&mut self, nfc_uid: NfcUid, client_id: &str) -> bool {
        let Some(spec) = self.trigger_specs.get(&nfc_uid) else {
            return false;
        };

        let active_trigger = ActiveTrigger::new(*spec);

        if spec.global {
            self.active_global_triggers = Some(active_trigger);
        } else {
            self.active_local_triggers
                .insert(client_id.to_string(), active_trigger);
        }

        true
    }
}
