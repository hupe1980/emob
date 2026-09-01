//! Where this crate and a protocol kit describe the same thing, they have to
//! describe it the same way — and that is a test, not a hope.
//!
//! `emob-core` deliberately carries its own identifiers and its own vocabulary:
//! a platform speaking several wires needs one type its handlers are written
//! against, and every crate that decides money here builds with no protocol
//! implementation anywhere in its tree. The cost of that decision is drift, and
//! this is where it is paid.
//!
//! The kits are **dev-dependencies**. Nothing a downstream links carries them,
//! so the purity argument holds; the agreement is still checked on every `cargo
//! test`, which is the point.

use emob_core::station::V2gCommunication;

/// The naming hazard this test exists for.
///
/// `[DATEX-II-Profil Tab. A.130]` spells its literal `iso15118`, with no
/// generation, and defines it as ISO 15118-**20**. It has no literal for -2 at
/// all. So a CPO whose points speak the 2016 generation and who maps "we do
/// ISO 15118" onto that literal publishes, in the official record, a claim of
/// compliance with a duty that phases in on 01.01.2027 and that its estate does
/// not meet `[DA-656 Anh. 2.1.2–2.1.3]` — and no schema validator objects.
///
/// The `iso15118` crate owns the unambiguous spelling. Ours has to be the same
/// spelling or the ambiguity comes back at the seam between us.
#[test]
fn the_generation_names_are_the_ones_the_protocol_crate_owns() {
    use iso15118::Protocol;

    let din = V2gCommunication::din_only();
    assert_eq!(
        din.protocol_names().collect::<Vec<_>>(),
        vec![Protocol::Din70121.as_str()]
    );

    let both = V2gCommunication::both_generations();
    assert_eq!(
        both.protocol_names().collect::<Vec<_>>(),
        vec![Protocol::Iso2.as_str(), Protocol::Iso20.as_str()],
        "in generation order, because that is the order the duties phase in"
    );

    // A point that speaks nothing high-level names nothing. PWM is IEC 61851
    // signalling and has no place in this list.
    assert_eq!(V2gCommunication::pwm_only().protocol_names().count(), 0);
}

/// Every name this crate can emit is one that crate can read back.
///
/// A `Display` with no `FromStr` on the far side is a value that leaves and
/// does not come home — and a protocol name written into a CDR six weeks ago
/// has to become a `Protocol` again when the dispute arrives.
#[test]
fn every_name_round_trips_through_the_protocol_crate() {
    use iso15118::Protocol;

    let everything = V2gCommunication {
        pwm: true,
        din70121: true,
        iso15118_2: true,
        iso15118_20: true,
    };

    for name in everything.protocol_names() {
        let protocol: Protocol = name
            .parse()
            .unwrap_or_else(|_| panic!("`{name}` is not a name the protocol crate reads"));
        assert_eq!(protocol.as_str(), name);
    }
}

/// The bare literal is refused, by them, with an error that says what is
/// missing.
///
/// This is the check that makes the whole arrangement worth having. The
/// dangerous string is not one either side emits — it is the one a *partner's*
/// onboarding form, or the DATEX profile itself, hands us. Parsing it as -20
/// because it happens to mean -20 in one vocabulary is exactly how a -2 fleet
/// ends up claiming a 2027 duty.
#[test]
fn a_generation_omitted_is_refused_rather_than_guessed() {
    use iso15118::Protocol;

    let refused = "iso15118".parse::<Protocol>();
    assert!(
        refused.is_err(),
        "a bare `iso15118` names no generation, and guessing one is the failure \
         [DA-656 Anh. 2.1.2–2.1.3] turns on"
    );

    // …and neither side is silently permissive about the other's spelling.
    assert!("iso15118-2".parse::<Protocol>().is_ok());
    assert!("iso15118-20".parse::<Protocol>().is_ok());
}
