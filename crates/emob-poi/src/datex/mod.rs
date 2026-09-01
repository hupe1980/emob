//! The DATEX II AFIR Recharging profile, version 01-00-00.
//!
//! `[AFIR Art. 20]` obliges an operator of publicly accessible recharging
//! points to make static and dynamic data available through the national access
//! point, free of charge and without discrimination. From **14 April 2026** the
//! German access point — the Mobilithek — requires that data in this profile
//! `[DATEX-II-Profil]`, which is DATEX II version 3 with AFIR extensions and is
//! proposed as a future CEN standard.
//!
//! Two publications, and they are not symmetrical:
//!
//! | | [`table`] | [`status`] |
//! |---|---|---|
//! | Carries | the infrastructure and its prices | references and live state |
//! | Changes when | somebody sends an engineer | somebody plugs in |
//! | AFIR | Art. 20(2)(a)–(b) | Art. 20(2)(c) |
//! | Wrapper | a bare `payload` | a `messageContainer` with exchange metadata |
//!
//! # Why the types are typed
//!
//! The profile has several hundred optional attributes and this module writes
//! the subset AFIR actually obliges. It would have been shorter to build a
//! `serde_json::Value` — and a mistyped key would then be a runtime bug found
//! by whoever consumed the feed. Named structs make the key names a compile
//! error instead, which is worth the lines given how few consumers ever report
//! a malformed feed rather than quietly skipping it.

pub mod status;
pub mod table;
pub mod wire;

pub use status::{PointUpdate, PriceUpdate, StatusPublication};
pub use table::{InformationStatus, Publisher, TablePublication};

use emob_core::{AdHocPayment, V2gCommunication};

/// How `[AFIR Art. 5(1)]`'s payment instruments are spelled in the profile.
///
/// `[DATEX-II-Profil]`'s `AuthenticationAndIdentificationEnum` is a list of
/// twenty; the mapping that matters is the one that keeps a driver's actual
/// options truthful:
///
/// - a card reader is `creditCard`, `debitCard` and `nfc` — the article names a
///   payment card reader **or** a contactless device, and a terminal that reads
///   cards contactlessly is all three;
/// - a QR code is `website`, because that is what it opens. It is deliberately
///   **not** `apps`: `[AFIR Art. 5(1)(c)]` requires payment without an app, and
///   publishing `apps` for a QR code claims the opposite of the duty it is
///   meant to satisfy.
#[must_use]
pub fn authentication_methods(payment: AdHocPayment) -> Vec<&'static str> {
    match payment {
        AdHocPayment::None => Vec::new(),
        AdHocPayment::QrCode => vec!["website"],
        AdHocPayment::CardReader => vec!["creditCard", "debitCard", "nfc"],
    }
}

/// The `VehicleToGridCommunicationTypeEnum` literal for a point's generations.
///
/// The trap this function exists for: the profile defines its `iso15118`
/// literal as "Communication according to **ISO15118-20**"
/// `[DATEX-II-Profil Tab. A.130]` — the 2022 generation specifically, not the
/// 2016 one everybody calls ISO 15118. A point that speaks only ISO 15118-2 and
/// publishes `iso15118` is claiming readiness for exactly the duty AFIR and
/// `[DA-656 Anh. 2.1.3]` phase in from 2027, and a market that reads the feed
/// would count it as compliant.
///
/// So `iso15118` is written only for a point that really does -20. A -2-only
/// point is `other`, which is the truth: the enumeration has no literal for it.
#[must_use]
pub fn v2g_literal(v2g: V2gCommunication) -> &'static str {
    if v2g.iso15118_20 {
        "iso15118"
    } else if v2g.iso15118_2 || v2g.din70121 {
        // Neither has a literal. DIN SPEC 70121 is high-level communication
        // over PLC and most of the pre-2020 European DC fleet speaks it, so
        // `none` would be false about it in exactly the way `iso15118` would be
        // false about a -2 point.
        "other"
    } else {
        // PWM is signalling, not a vehicle-to-grid exchange. The enumeration's
        // own definition of `none` — "No communication between vehicle and the
        // grid" — is what a basic-signalling point does.
        "none"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_point_that_speaks_only_the_2016_generation_does_not_claim_the_2027_one() {
        // The profile's `iso15118` means -20. Publishing it for a -2 point
        // claims compliance with the duty that is being phased in.
        let old = V2gCommunication {
            pwm: true,
            din70121: false,
            iso15118_2: true,
            iso15118_20: false,
        };
        assert_eq!(v2g_literal(old), "other");
        assert_eq!(
            v2g_literal(V2gCommunication::both_generations()),
            "iso15118"
        );
        assert_eq!(v2g_literal(V2gCommunication::pwm_only()), "none");

        // …and `none` says "No communication between vehicle and the grid",
        // which is false about a DIN SPEC 70121 charger: it talks to the car
        // over PLC, it just does not do it in a document AFIR names.
        assert_eq!(v2g_literal(V2gCommunication::din_only()), "other");
    }

    #[test]
    fn a_qr_code_is_a_website_and_never_an_app() {
        // `[AFIR Art. 5(1)(c)]` is satisfied by a device allowing secure
        // payment *without* an app. Publishing `apps` would advertise the one
        // thing the paragraph rules out.
        assert_eq!(authentication_methods(AdHocPayment::QrCode), ["website"]);
        assert!(authentication_methods(AdHocPayment::None).is_empty());
        assert!(
            authentication_methods(AdHocPayment::CardReader).contains(&"nfc"),
            "a contactless reader is a contactless reader"
        );
    }
}
