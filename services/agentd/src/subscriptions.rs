//! Which event reaches which specialist.
//!
//! # The table is data, and it is checked against the catalogue
//!
//! A subscription written as a string literal at the place that dispatches is a
//! subscription nobody can list. Worse, a typo in one silently matches nothing —
//! and a specialist that never runs looks exactly like a specialist with nothing
//! to say.
//!
//! So the table is one `const`, its patterns go through the same
//! [`emob_service::events::matches`] every other subscription mechanism uses,
//! and a test asserts that **every pattern matches at least one type in the
//! catalogue**. A rename in `emob-service` that orphans a subscription here
//! fails the build rather than quietly stopping a specialist.

use emob_service::events;

/// One specialist, and what wakes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Subscription {
    /// The specialist's name, as its [`agentplane::prelude::SkillDescriptor`]
    /// states it.
    pub specialist: &'static str,
    /// The event types that wake it.
    pub on: &'static [&'static str],
}

/// The table.
///
/// Deliberately small. A specialist that subscribes to everything is a
/// specialist that runs on every event and has an opinion about most of them,
/// which is how an advisory queue stops being read.
pub const TABLE: &[Subscription] = &[
    Subscription {
        specialist: crate::skills::evidence::NAME,
        // A refusal is the event; a key that could not be resolved is the
        // commonest single cause of one, and it arrives separately because the
        // fix is a provisioning run rather than a device.
        on: &[events::evidence::REFUSED, events::evidence::KEY_UNRESOLVED],
    },
    Subscription {
        specialist: crate::skills::tariff::NAME,
        // A version that took effect, and a conformance check that already
        // failed somewhere. The second is not redundant: the check runs per
        // point, and this specialist is what turns "one point objected" into
        // "which points across the estate".
        on: &[
            events::tariff::VERSION_PUBLISHED,
            events::tariff::CONFORMANCE_FAILED,
        ],
    },
];

/// The specialists an event wakes, in table order.
#[must_use]
pub fn specialists_for(event_type: &str) -> Vec<&'static str> {
    TABLE
        .iter()
        .filter(|subscription| {
            subscription
                .on
                .iter()
                .any(|pattern| events::matches(pattern, event_type))
        })
        .map(|subscription| subscription.specialist)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_subscription_matches_something_in_the_catalogue() {
        // A typo silently matches nothing, and a specialist that never runs
        // looks exactly like a specialist with nothing to say.
        for subscription in TABLE {
            for pattern in subscription.on {
                assert!(
                    events::ALL
                        .iter()
                        .any(|event_type| events::matches(pattern, event_type)),
                    "{} subscribes to `{pattern}`, which no event type in the catalogue \
                     matches",
                    subscription.specialist
                );
            }
        }
    }

    #[test]
    fn every_specialist_in_the_table_is_one_the_daemon_registers() {
        // The other direction: a subscription naming a specialist that is not
        // wired is a row that dispatches into nothing.
        let registered = crate::registered_specialists();
        for subscription in TABLE {
            assert!(
                registered.contains(&subscription.specialist),
                "{} is subscribed and not registered",
                subscription.specialist
            );
        }
    }

    #[test]
    fn an_event_reaches_the_specialists_that_asked_for_it_and_no_others() {
        assert_eq!(
            specialists_for(events::evidence::REFUSED),
            vec![crate::skills::evidence::NAME]
        );
        assert_eq!(
            specialists_for(events::tariff::VERSION_PUBLISHED),
            vec![crate::skills::tariff::NAME]
        );
        assert!(
            specialists_for(events::billing::INVOICE_ISSUED).is_empty(),
            "nothing subscribes to it yet, and saying so is the honest answer"
        );
    }
}
