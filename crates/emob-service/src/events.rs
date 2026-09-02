//! The compile-time catalogue of every `CloudEvents` `type` this workspace emits.
//!
//! One `pub const` per event type. Emitters and subscribers reference these
//! rather than inline string literals, so a rename is a one-line change and
//! drift between a producer and a consumer is a compile error rather than a
//! subscription that silently matches nothing.
//!
//! # Conventions
//!
//! - Every type starts with `de.emob.` and is entirely lowercase — `CloudEvents`
//!   §3.1 recommends lowercase reverse-DNS types.
//! - Segments are separated by `.`; a multi-word segment joins its words with
//!   `-`, never `_`. [`segments_are_well_formed`] enforces both.
//! - The last segment is a **past participle**. An event is a fact that has
//!   happened; a type in the imperative is a command wearing an event's
//!   clothes, and a subscriber cannot tell the difference at runtime.
//!
//! # Why this is here and not in a crate of its own
//!
//! `mako-events` is a separate crate because every crate in that workspace
//! might emit, and a shared module would make the service framework a
//! dependency magnet. Here the emitters are **daemons only** — the domain
//! crates emit nothing, by the same purity rule that keeps them free of a
//! clock — and every daemon already depends on this crate. A second crate
//! would buy nothing and cost a manifest.
//!
//! # What a subscription may say
//!
//! A pattern is a concrete type or a `*`-terminated prefix, and [`fn@matches`] is
//! the one matcher every subscription mechanism uses. Glob patterns are not in
//! the catalogue — only concrete types are — because a pattern is a *reader's*
//! choice and a type is a *writer's* promise.

/// The prefix every type in this workspace shares.
pub const PREFIX: &str = "de.emob.";

/// The charging session, as the CSMS sees it.
pub mod session {
    /// A station opened a transaction.
    pub const STARTED: &str = "de.emob.session.started";
    /// …and closed it.
    pub const ENDED: &str = "de.emob.session.ended";
    /// The vehicle stopped drawing while remaining connected — the interval
    /// `[AFIR Art. 5(4)]` prices per minute.
    pub const SUSPENDED: &str = "de.emob.session.suspended";
    /// An authorisation was asked for and refused.
    pub const AUTHORIZATION_REFUSED: &str = "de.emob.session.authorization-refused";
}

/// The evidence chain.
pub mod evidence {
    /// A set of signed records verified end to end.
    pub const VERIFIED: &str = "de.emob.evidence.verified";
    /// …or did not, with the findings that stopped it.
    ///
    /// The event an operator queue is built on: a session whose energy cannot
    /// be billed is one somebody has to act on the same day.
    pub const REFUSED: &str = "de.emob.evidence.refused";
    /// A record named a signing component the registry could not resolve a
    /// key for.
    pub const KEY_UNRESOLVED: &str = "de.emob.evidence.key-unresolved";
    /// A transparency file was produced for a customer's own verifier
    /// `[MessEG §33]`.
    pub const TRANSPARENCY_EXPORTED: &str = "de.emob.evidence.transparency-exported";
}

/// The charge detail record.
pub mod cdr {
    /// A record was built, priced and accepted.
    pub const ISSUED: &str = "de.emob.cdr.issued";
    /// A record arrived from a partner and passed its pre-flight.
    pub const RECEIVED: &str = "de.emob.cdr.received";
    /// …or did not.
    pub const REJECTED: &str = "de.emob.cdr.rejected";
    /// A record supersedes an earlier one.
    pub const CORRECTED: &str = "de.emob.cdr.corrected";
    /// A partner restated a settled number under an id already held.
    ///
    /// Never an upsert. This is the event a human answers.
    pub const CONFLICTED: &str = "de.emob.cdr.conflicted";
}

/// Tariffs and the price the driver is shown.
pub mod tariff {
    /// A new version took effect.
    pub const VERSION_PUBLISHED: &str = "de.emob.tariff.version-published";
    /// A version was installed on a charge point over OCPP 2.1.
    pub const INSTALLED: &str = "de.emob.tariff.installed";
    /// A tariff failed its `[AFIR Art. 5(4)]` shape check at the power it is
    /// offered at.
    pub const CONFORMANCE_FAILED: &str = "de.emob.tariff.conformance-failed";
}

/// Locations and the national access point.
pub mod location {
    /// The inventory changed.
    pub const UPDATED: &str = "de.emob.location.updated";
    /// A point's availability changed.
    pub const STATUS_CHANGED: &str = "de.emob.location.status-changed";
    /// A snapshot reached the national access point `[AFIR Art. 20(2)]`.
    pub const FEED_PUBLISHED: &str = "de.emob.location.feed-published";
}

/// Roaming.
pub mod roaming {
    /// A record left for a partner.
    pub const CDR_PUSHED: &str = "de.emob.roaming.cdr-pushed";
    /// A crossing could not carry something exactly, and said what.
    pub const CROSSING_NOTED: &str = "de.emob.roaming.crossing-noted";
    /// A partner's credentials were exchanged.
    pub const PARTNER_REGISTERED: &str = "de.emob.roaming.partner-registered";
}

/// Invoicing and settlement.
pub mod billing {
    /// An invoice was issued.
    pub const INVOICE_ISSUED: &str = "de.emob.billing.invoice-issued";
    /// …and a collection instruction was written for it.
    pub const COLLECTION_INSTRUCTED: &str = "de.emob.billing.collection-instructed";
    /// A collection came back unpaid.
    pub const COLLECTION_RETURNED: &str = "de.emob.billing.collection-returned";
    /// A document did not satisfy the profile it was addressed to.
    pub const DOCUMENT_REJECTED: &str = "de.emob.billing.document-rejected";
}

/// The obligation calendar.
pub mod compliance {
    /// A duty came into force for a subject the operator holds.
    pub const DUTY_COMMENCED: &str = "de.emob.compliance.duty-commenced";
    /// An assessment found a subject in breach.
    pub const BREACH_DETECTED: &str = "de.emob.compliance.breach-detected";
    /// A notification window under `[LSV26 §4]` opened, and closes on a
    /// deadline.
    pub const NOTICE_WINDOW_OPENED: &str = "de.emob.compliance.notice-window-opened";
}

/// The station, as an operational object.
pub mod station {
    /// A station connected and completed its boot notification.
    pub const BOOTED: &str = "de.emob.station.booted";
    /// A station stopped answering.
    pub const DISCONNECTED: &str = "de.emob.station.disconnected";
    /// A station sent a security notification
    /// `[OCPP 2.0.1 Part 2, SecurityEventNotification]`.
    pub const SECURITY_NOTIFIED: &str = "de.emob.station.security-notified";
}

/// Every type in the catalogue, for the tests and for an operator listing what
/// a deployment can emit.
///
/// A `const` array rather than a function, so a subscription table can be
/// checked against it at compile time.
pub const ALL: &[&str] = &[
    session::STARTED,
    session::ENDED,
    session::SUSPENDED,
    session::AUTHORIZATION_REFUSED,
    evidence::VERIFIED,
    evidence::REFUSED,
    evidence::KEY_UNRESOLVED,
    evidence::TRANSPARENCY_EXPORTED,
    cdr::ISSUED,
    cdr::RECEIVED,
    cdr::REJECTED,
    cdr::CORRECTED,
    cdr::CONFLICTED,
    tariff::VERSION_PUBLISHED,
    tariff::INSTALLED,
    tariff::CONFORMANCE_FAILED,
    location::UPDATED,
    location::STATUS_CHANGED,
    location::FEED_PUBLISHED,
    roaming::CDR_PUSHED,
    roaming::CROSSING_NOTED,
    roaming::PARTNER_REGISTERED,
    billing::INVOICE_ISSUED,
    billing::COLLECTION_INSTRUCTED,
    billing::COLLECTION_RETURNED,
    billing::DOCUMENT_REJECTED,
    compliance::DUTY_COMMENCED,
    compliance::BREACH_DETECTED,
    compliance::NOTICE_WINDOW_OPENED,
    station::BOOTED,
    station::DISCONNECTED,
    station::SECURITY_NOTIFIED,
];

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
/// assert!(!matches("de.emob.cdr.*", events::tariff::VERSION_PUBLISHED));
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

        assert!(!matches("de.emob.cdr.*", tariff::VERSION_PUBLISHED));
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
}
