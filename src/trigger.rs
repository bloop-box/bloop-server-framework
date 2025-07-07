//! This module provides functionality to define, manage, and track triggers,
//! which represent event-based activations identified by NFC tags (`NfcUid`).
//!
//! Triggers can be configured with different usage policies, such as single
//! use, limited number of uses, or duration-based activation. The module
//! supports both local (per client) and global triggers.
//!
//! # Key types
//!
//! - [`TriggerOccurrence`]: Specifies how often or for how long a trigger
//!   remains active.
//! - [`TriggerSpec`]: Defines the properties of a trigger including its type
//!   and occurrence.
//! - [`ActiveTrigger`]: Tracks usage and activation time for an active trigger
//!   instance.
//! - [`TriggerRegistry`]: Holds trigger specifications and active triggers,
//!   providing methods to activate and check triggers per client.
//!
//! # Usage
//!
//! To use triggers, initialize a `TriggerRegistry` with your trigger
//! specifications, then activate triggers upon NFC scans (`NfcUid`) and check
//! their active status for clients.
//!
//! The registry automatically manages usage counts and expiration of active
//! triggers.

use crate::nfc_uid::NfcUid;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::time::Duration;

/// Specifies how often or for how long a trigger should remain active.
#[derive(Debug, Copy, Clone, Deserialize)]
pub enum TriggerOccurrence {
    /// The trigger can only be used once.
    Once,
    /// The trigger can be used a specified number of times.
    Times(usize),
    /// The trigger remains active for the specified duration.
    Duration(Duration),
}

impl Default for TriggerOccurrence {
    fn default() -> Self {
        Self::Once
    }
}

/// Defines the specification of a trigger, including its activation policy and type.
#[derive(Debug, Copy, Clone, Deserialize)]
pub struct TriggerSpec<T> {
    /// Whether the trigger is global (affects all clients) or local (per client).
    #[serde(default)]
    pub global: bool,
    /// How often or for how long this trigger can be active.
    #[serde(default)]
    pub occurrence: TriggerOccurrence,
    /// The trigger identifier of type `T`.
    pub trigger: T,
}

/// Represents an active trigger instance, tracking its usage and activation time.
#[derive(Debug)]
struct ActiveTrigger<T> {
    /// The specification of the trigger being tracked.
    spec: TriggerSpec<T>,
    /// The time when the trigger was activated.
    triggered_at: DateTime<Utc>,
    /// The number of times this trigger has been used.
    usages: usize,
}

impl<T> ActiveTrigger<T> {
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
}

impl<T: PartialEq> ActiveTrigger<T> {
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
                let active = self.usages < times;
                self.usages += 1;
                (active, self.usages < times)
            }
            TriggerOccurrence::Duration(duration) => {
                let still_active = self.triggered_at + duration >= reference_time;
                (still_active, still_active)
            }
        }
    }
}

/// Registry that manages trigger specifications and tracks active triggers.
#[derive(Debug)]
pub struct TriggerRegistry<T> {
    trigger_specs: HashMap<NfcUid, TriggerSpec<T>>,
    active_local_triggers: HashMap<String, ActiveTrigger<T>>,
    active_global_trigger: Option<ActiveTrigger<T>>,
}

impl<T> From<HashMap<NfcUid, TriggerSpec<T>>> for TriggerRegistry<T> {
    fn from(specs: HashMap<NfcUid, TriggerSpec<T>>) -> Self {
        Self::new(specs)
    }
}

impl<T> TriggerRegistry<T> {
    /// Creates a new [TriggerRegistry] from a set of trigger specifications.
    pub fn new(specs: HashMap<NfcUid, TriggerSpec<T>>) -> Self {
        Self {
            trigger_specs: specs,
            active_local_triggers: HashMap::new(),
            active_global_trigger: None,
        }
    }
}

impl<T: PartialEq> TriggerRegistry<T> {
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

        self.active_global_trigger
            .take()
            .is_some_and(|mut active_trigger| {
                let (active, retain) = active_trigger.check(trigger, reference_time);

                if retain {
                    self.active_global_trigger = Some(active_trigger);
                }

                active
            })
    }
}

impl<T: Copy> TriggerRegistry<T> {
    /// Attempts to activate a trigger based on the provided NFC UID and client ID.
    ///
    /// If the NFC UID is associated with a trigger spec, an active trigger is created and stored.
    pub(crate) fn try_activate_trigger(&mut self, nfc_uid: NfcUid, client_id: &str) -> bool {
        let Some(spec) = self.trigger_specs.get(&nfc_uid) else {
            return false;
        };

        let active_trigger = ActiveTrigger::new(*spec);

        if spec.global {
            self.active_global_trigger = Some(active_trigger);
        } else {
            self.active_local_triggers
                .insert(client_id.to_string(), active_trigger);
        }

        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::time::Duration;

    #[test]
    fn activate_and_check_once_trigger() {
        let uid = NfcUid::default();
        let mut registry: TriggerRegistry<u8> = HashMap::from_iter([(
            uid,
            TriggerSpec {
                global: false,
                occurrence: TriggerOccurrence::Once,
                trigger: 42,
            },
        )])
        .into();

        assert!(registry.try_activate_trigger(uid, "client1"));

        let now = Utc::now();
        assert!(registry.check_active_trigger(42, "client1", now));
        assert!(!registry.check_active_trigger(42, "client1", now));
    }

    #[test]
    fn activate_and_check_times_trigger() {
        let uid = NfcUid::default();
        let mut registry: TriggerRegistry<u8> = HashMap::from_iter([(
            uid,
            TriggerSpec {
                global: false,
                occurrence: TriggerOccurrence::Times(2),
                trigger: 42,
            },
        )])
        .into();

        assert!(registry.try_activate_trigger(uid, "client2"));

        let now = Utc::now();
        assert!(registry.check_active_trigger(42, "client2", now));
        assert!(registry.check_active_trigger(42, "client2", now));
        assert!(!registry.check_active_trigger(42, "client2", now));
    }

    #[test]
    fn activate_and_check_duration_trigger() {
        let uid = NfcUid::default();
        let mut registry: TriggerRegistry<u8> = HashMap::from_iter([(
            uid,
            TriggerSpec {
                global: false,
                occurrence: TriggerOccurrence::Duration(Duration::from_secs(50)),
                trigger: 42,
            },
        )])
        .into();

        assert!(registry.try_activate_trigger(uid, "client3"));

        let now = Utc::now();
        assert!(registry.check_active_trigger(42, "client3", now));

        let later = now + Duration::from_secs(30);
        assert!(registry.check_active_trigger(42, "client3", later));

        let expired = now + Duration::from_secs(70);
        assert!(!registry.check_active_trigger(42, "client3", expired));
    }

    #[test]
    fn global_trigger_works_from_any_client() {
        let uid = NfcUid::default();
        let mut registry: TriggerRegistry<u8> = HashMap::from_iter([(
            uid,
            TriggerSpec {
                global: true,
                occurrence: TriggerOccurrence::Once,
                trigger: 42,
            },
        )])
        .into();

        assert!(registry.try_activate_trigger(uid, "any_client"));

        let now = Utc::now();
        assert!(registry.check_active_trigger(42, "client1", now));
        assert!(!registry.check_active_trigger(42, "client2", now));
    }
}
