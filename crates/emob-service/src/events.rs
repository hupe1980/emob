//! The catalogue of every `CloudEvents` `type` emob emits — and only those.
//!
//! One `pub const` per type, and a type is here only because a daemon emits it:
//! the test `every_type_is_emitted` holds each constant to being named in a
//! daemon's source. A new type is added with its emitter, in one change.
//!
//! Every event leaves as a [`crate::cloudevent::CloudEvent`] through a daemon's
//! journalled outbox, signed per Standard Webhooks, to the receivers inside the
//! estate and to every `[[subscribers]]` entry whose `types` admit it
//! ([`crate::estate::Subscriber`]).
//!
//! # Conventions
//!
//! - `de.emob.<entity>.<fact>`, entirely lowercase; a multi-word segment joins
//!   its words with `-`, never `_`. [`segments_are_well_formed`] enforces it.
//! - A namespace belongs to its owner. emob emits only `de.emob.*` and reads no
//!   other system's type; where two systems meet, an adapter renames.
//! - The last segment is a **past participle**: an event is a fact, not a
//!   command.
//! - The `subject` is `<entity>/<id>`: `cdr/<key>`, `invoice/<number>`.
//!
//! | Type | Emitted by | When | `data` |
//! |---|---|---|---|
//! | `de.emob.cdr.issued` | `csmsd` | a session's evidence verified and its record was stored | the record, whom it bills, the signed values, the station (JSON) |
//! | `de.emob.invoice.issued` | `billd` | an invoice was issued | the invoice file, UBL or CII (`application/xml`) |
//!
//! # What a subscription may say
//!
//! A pattern is a concrete type or a `*`-terminated prefix; [`fn@matches`] is
//! the one matcher every subscription uses.

/// The prefix every type in this workspace shares.
pub const PREFIX: &str = "de.emob.";

/// Charge detail records.
pub mod cdr {
    /// A record was issued: the session's signed values verified, the record
    /// was stored, and it is owed to the invoice and the partner.
    pub const ISSUED: &str = "de.emob.cdr.issued";

    /// Private link from a settled CDR to the provider's accepted terms.
    #[derive(Debug, Clone, PartialEq, Eq)]
    #[cfg_attr(
        feature = "serde",
        derive(serde::Deserialize, serde::Serialize),
        serde(deny_unknown_fields)
    )]
    pub struct RetailAuthorization {
        /// Provider-issued agreement handle. A legacy authorization has no binding.
        #[cfg_attr(feature = "serde", serde(default))]
        pub agreement: String,
        /// SHA-256 of the gross retail offer checked at authorization.
        pub reference: String,
    }

    impl RetailAuthorization {
        /// Both handles use the provider's canonical lowercase, 32-byte hex encoding.
        #[must_use]
        pub fn is_well_formed(&self) -> bool {
            [&self.agreement, &self.reference].into_iter().all(|id| {
                id.len() == 64
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
        }
    }
}

/// Invoices.
pub mod invoice {
    /// An invoice was issued; the data is the structured file
    /// `[UStG §14(1)]`.
    pub const ISSUED: &str = "de.emob.invoice.issued";
}

/// Every type in the catalogue.
pub const ALL: &[&str] = &[cdr::ISSUED, invoice::ISSUED];

/// Whether a subscription pattern admits an event type.
///
/// The one matcher every subscription mechanism uses. Two forms and no more —
/// a concrete type, or a `*`-terminated prefix — for the reason
/// [`crate::authority::Capabilities`] gives: a richer grammar makes containment
/// undecidable, and a subscription table nobody can reason about is one nobody
/// can audit.
///
/// The prefix widens at a **segment** boundary, so `de.emob.cdr.*` does not
/// admit `de.emob.cdrs.issued`.
///
/// ```
/// use emob_service::events::{self, matches};
///
/// assert!(matches("de.emob.cdr.*", events::cdr::ISSUED));
/// assert!(matches("*", events::cdr::ISSUED));
/// assert!(!matches("de.emob.cdr.*", events::invoice::ISSUED));
/// ```
#[must_use]
pub fn matches(pattern: &str, event_type: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    pattern.strip_suffix(".*").map_or_else(
        || pattern == event_type,
        |prefix| event_type.starts_with(prefix) && event_type[prefix.len()..].starts_with('.'),
    )
}

/// Whether a type follows the conventions the module documents.
///
/// Exposed so a workspace adding a type gets the same answer the test does,
/// rather than a review comment.
#[must_use]
pub fn segments_are_well_formed(event_type: &str) -> bool {
    event_type.starts_with(PREFIX)
        && event_type
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b == b'.' || b == b'-')
        && !event_type.contains('_')
        && event_type.split('.').all(|segment| {
            !segment.is_empty() && !segment.starts_with('-') && !segment.ends_with('-')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_type_follows_the_convention() {
        for event_type in ALL {
            assert!(
                segments_are_well_formed(event_type),
                "{event_type} does not follow the naming convention"
            );
        }
    }

    #[test]
    fn the_catalogue_holds_each_type_once() {
        // Two constants with one value is two names for one fact, and a
        // subscriber matching on the other one is a bug nothing reports.
        let mut seen: Vec<&str> = ALL.to_vec();
        seen.sort_unstable();
        let count = seen.len();
        seen.dedup();
        assert_eq!(seen.len(), count, "a type appears twice in the catalogue");
    }

    #[test]
    fn a_pattern_widens_at_a_segment_boundary() {
        assert!(matches(cdr::ISSUED, cdr::ISSUED));
        assert!(matches("de.emob.cdr.*", cdr::ISSUED));
        assert!(matches("de.emob.*", cdr::ISSUED));
        assert!(matches("*", cdr::ISSUED));

        assert!(!matches("de.emob.cdr.*", invoice::ISSUED));
        assert!(!matches("de.emob.cdrs.*", cdr::ISSUED));
        // A grant of the children is not a grant of the parent.
        assert!(!matches("de.emob.cdr.issued.*", cdr::ISSUED));
    }

    #[test]
    fn a_malformed_type_is_rejected_by_the_same_function_the_test_uses() {
        assert!(!segments_are_well_formed("de.emob.cdr.Issued"));
        assert!(!segments_are_well_formed("de.emob.cdr.issued_now"));
        assert!(!segments_are_well_formed("cdr.issued"));
        assert!(!segments_are_well_formed("de.emob..issued"));
        assert!(!segments_are_well_formed("de.emob.cdr.-issued"));
    }

    #[test]
    fn every_type_is_a_fact_rather_than_a_command() {
        // A type in the imperative is a command wearing an event's clothes, and
        // a subscriber cannot tell the difference at runtime — so the last word
        // of every type is a past participle.
        //
        // The mechanical proxy is "ends in `ed`", which is right for every
        // regular English participle and wrong for the irregular ones. A
        // workspace adding `sent`, `built` or `found` adds it here and states
        // the reason, rather than weakening the test — which is the direction
        // that keeps the rule enforceable. The list is empty because every name
        // in the catalogue reached a regular participle without contortion, and
        // this test is what made three of them do so.
        const IRREGULAR: &[&str] = &[];

        for event_type in ALL {
            let last = event_type.rsplit('.').next().expect("a segment");
            let verb = last.rsplit('-').next().expect("a word");
            assert!(
                verb.ends_with("ed") || IRREGULAR.contains(&verb),
                "{event_type} does not end in a past participle: `{verb}` names a \
                 thing or a state rather than something that happened"
            );
        }
    }

    /// Every constant is named in a daemon's source, outside this file — the
    /// rule the module documentation states. A type nothing emits is removed,
    /// not kept "for later".
    #[test]
    fn every_type_is_emitted() {
        let services = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../services");
        let mut source = String::new();
        let mut stack = vec![services];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("services/ is readable") {
                let path = entry.expect("an entry").path();
                if path.is_dir() {
                    if !path.ends_with("target") {
                        stack.push(path);
                    }
                } else if path.extension().is_some_and(|e| e == "rs") {
                    source.push_str(&std::fs::read_to_string(&path).expect("a source file"));
                }
            }
        }
        for (name, value) in [
            ("cdr::ISSUED", cdr::ISSUED),
            ("invoice::ISSUED", invoice::ISSUED),
        ] {
            assert!(
                source.contains(&format!("events::{name}")),
                "{value} is in the catalogue and no daemon emits it"
            );
        }
        assert_eq!(
            ALL.len(),
            2,
            "a type added to ALL is added to this test with its emitter"
        );
    }
}
