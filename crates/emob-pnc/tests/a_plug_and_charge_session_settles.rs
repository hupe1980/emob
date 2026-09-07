//! **M6c: a Plug & Charge session, end to end.**
//!
//! > A vehicle presents its contract, and the kilowatt-hours it draws become a
//! > settled record.
//!
//! Everything in this file is real except the car. The certificates are DER
//! **OpenSSL** encoded, validated against ISO 15118's own Annex F profiles by
//! `iso15118::pnc::pki`; the meter records are OCMF a private key actually
//! signed, verified by `ocmf` against a registry entry a provisioning run would
//! have written; the transaction is assembled through the same OCPP seam every
//! session in this workspace goes through; and the money is
//! `emob_tariff::rate`'s.
//!
//! # The one thing this proves that a chain validation cannot
//!
//! A contract certificate establishes **who**. A signed meter record
//! establishes **how much**, and — the part that matters here — it states in
//! its own signed bytes *how strongly the driver was identified*
//! `[OCMF Tab. 11]`. Those are two independent claims about one session, made
//! by two parties, and the whole reason `emob-core` owns
//! `IdentificationStrength` is that they can then be compared.
//!
//! So the assertion at the end is not that a certificate parsed. It is that a
//! session authorised by a contract certificate carries a record whose own
//! signature says `SECURE`, that `SECURE` is what
//! `AuthPath::PlugAndCharge` is allowed to claim, and that the eMAID in the
//! signed payload is the one that came **off the certificate** — never a value
//! a caller put there.

use emob_cdr::{CdrBuilder, EvidenceRef};
use emob_core::{Direction, PartyId, V2gProtocol};
use emob_eichrecht::{Evidence, KeyRegistry};
use emob_ocpp::Transaction;
use emob_pnc::fixtures;
use emob_pnc::{
    Contract, ContractBook, Pool, PoolId, PoolRegistry, RevocationList, TrustAnchor, Withdrawals,
    verify_contract, verify_provisioning,
};
use emob_session::{AuthPath, Authorization};
use emob_sim::{Identification, Rng, SessionPlan, VirtualStation};
use emob_tariff::{Dimension, PriceComponent, Tariff, TariffKind};
use iso15118::pnc::pki::Certificate;
use rust_decimal::Decimal;
use std::str::FromStr;
use time::macros::datetime;

/// Inside every fixture's validity window.
fn at() -> time::OffsetDateTime {
    datetime!(2030-06-01 12:00 UTC)
}

fn der(pem: &str) -> Vec<u8> {
    fixtures::der(pem)
}

fn refs(chain: &[Vec<u8>]) -> Vec<&[u8]> {
    chain.iter().map(Vec::as_slice).collect()
}

fn platform() -> PoolRegistry {
    PoolRegistry::new().with(
        "hubject-ccp",
        Pool::new().with(TrustAnchor::unbounded(
            der(fixtures::ROOT),
            "the Root Certificate Pool, over OFTP2",
        )),
    )
}

fn nothing_withdrawn() -> RevocationList {
    let mut list = RevocationList::new();
    for pem in [
        fixtures::ROOT,
        fixtures::MO_SUB_CA_1,
        fixtures::MO_SUB_CA_2,
        fixtures::OEM_SUB_CA_1,
        fixtures::OEM_SUB_CA_2,
    ] {
        let bytes = der(pem);
        let certificate = Certificate::parse(&bytes).expect("DER this crate minted");
        list = list.with(
            certificate.subject.encoded.to_vec(),
            Withdrawals::none_at(at()),
        );
    }
    list
}

fn tariff() -> Tariff {
    Tariff::simple(
        "pnc-contract".parse().expect("a valid tariff id"),
        emob_core::Currency::EUR,
        TariffKind::Contract,
        emob_core::TimeZone::new("Europe/Berlin").expect("an IANA zone"),
        vec![
            PriceComponent::new(
                Dimension::Energy,
                Decimal::from_str("0.42").expect("a price"),
            )
            .with_vat(Decimal::from(19)),
        ],
    )
}

#[test]
fn a_vehicles_contract_authorises_a_session_and_its_kilowatt_hours_settle() {
    let platform = platform();
    let pool = PoolId::new("hubject-ccp");
    let status = nothing_withdrawn();

    // ── 1. The vehicle, out of the Provisioning Certificate Pool ────────────
    let provisioning = vec![
        der(fixtures::PROVISIONING),
        der(fixtures::OEM_SUB_CA_2),
        der(fixtures::OEM_SUB_CA_1),
    ];
    let vehicle = verify_provisioning(&platform, &pool, &refs(&provisioning), &status, at())
        .expect("a chain to the root this pool publishes");

    // ── 2. The contract the Mobility Operator wrote against it ──────────────
    //
    // Nothing in this call names an eMAID. `[V2G2-108]` puts it in the leaf's
    // Common Name and that is where it is read: a platform that took it as an
    // argument would attribute the session to whichever contract the caller
    // named, and the certificate would be decoration.
    let contract_chain = vec![
        der(fixtures::CONTRACT),
        der(fixtures::MO_SUB_CA_2),
        der(fixtures::MO_SUB_CA_1),
    ];
    let verified = verify_contract(
        &platform,
        &pool,
        &refs(&contract_chain),
        &status,
        at(),
        Some("8AA"),
    )
    .expect("a contract of the provider this installation is for");
    assert_eq!(verified.emaid.canonical(), "DE8AACA2B3C4D5L");

    let mut book = ContractBook::new();
    let held = Contract::installed(&verified, &vehicle);
    assert!(
        held.is_valid_at(at()),
        "a contract is installed inside its own validity or not at all"
    );
    book.install(held);

    // ── 3. The authorisation the session runs under ─────────────────────────
    let authorization = Authorization::plug_and_charge(
        "DE-8AA-C000001".parse().expect("a valid contract id"),
        verified.emaid.clone(),
    );
    assert_eq!(authorization.path, AuthPath::PlugAndCharge);

    // ── 4. A station that signs the assignment, rather than being told it ───
    //
    // The eMAID goes into the payload the private key covers. An identifier a
    // backend typed beside a session is a claim; one inside the signed bytes is
    // evidence, and it is the half a dispute two years later can be answered
    // from.
    let mut rng = Rng::new(6);
    let mut station = VirtualStation::new(0, &mut rng).identifying(Identification::PlugAndCharge {
        emaid: verified.emaid.canonical(),
    });
    let mut registry = KeyRegistry::new();
    registry
        .insert(station.component(), station.registered_key())
        .expect("one station, one key");

    let plan = SessionPlan::draw(datetime!(2030-06-01 08:00 +2), &mut rng);
    let charged = station.charge(&plan, &[]);

    // ── 5. Through the OCPP seam, with that authorisation ───────────────────
    let transaction = Transaction {
        id: "pnc-0001".parse().expect("a valid session id"),
        evse_id: station.evse_id.clone(),
        authorization,
        events: charged.events.clone(),
    };
    let assembled = transaction
        .assemble(Direction::Import)
        .expect("a session the station actually signed");
    let session = assembled.session.negotiated(V2gProtocol::Iso15118_2);
    assert!(
        !session.contradicts_its_own_protocol(),
        "Plug & Charge over ISO 15118-2 is the pairing that exists"
    );

    // The two facts the roadmap kept apart until there was somewhere to put the
    // second one. `V2gCommunication` is what the **charger implements**, which
    // is what every `[DA-656 Anh. 2.1.x]` duty is judged on; `Session::negotiated`
    // is what two parties agreed on the wire, once. A point that implements
    // both generations and negotiated -2 with a 2018 car is compliant and had a
    // -2 session, and a record carrying only the first cannot say so.
    let implements = emob_core::station::V2gCommunication::both_generations();
    assert!(
        implements
            .protocols()
            .any(|protocol| Some(protocol) == session.negotiated),
        "a session cannot have negotiated a generation its charger does not implement"
    );

    let borrowed: Vec<ocmf::Record<'_>> = assembled
        .records
        .iter()
        .filter_map(|r| r.record().ok())
        .collect();
    let evidence = Evidence::assemble(&borrowed, &registry, charged.started_at);
    assert!(
        evidence.problems.is_empty(),
        "the station signed for real: {:?}",
        evidence.reasons().collect::<Vec<_>>()
    );

    // ── 6. …and the record settles ──────────────────────────────────────────
    let offered = tariff();
    let cdr = CdrBuilder::from_session(&session, Direction::Import)
        .expect("a session with a meter series")
        .key(
            PartyId::new("DE", "SIM").expect("a valid party id"),
            "pnc-0001".parse().expect("a valid CDR id"),
        )
        .evidence(EvidenceRef::from_evidence(&evidence, "OCMF"))
        .rated_with(&offered)
        .build()
        .expect("a genuine session settles");

    assert!(emob_cdr::validate(&cdr).is_settleable());
    assert!(cdr.total_cost().is_some(), "and it carries its own price");

    // The claim the whole file exists for. The record's **own signature** says
    // the assignment was `SECURE` — `[OCMF Tab. 11]`'s example for a contract
    // certificate the signature component verified — and that is exactly what
    // `AuthPath::PlugAndCharge` is allowed to claim. Neither side was told the
    // other's answer.
    assert_eq!(
        cdr.evidence
            .as_ref()
            .expect("the record is backed by evidence")
            .identification_strength,
        emob_core::IdentificationStrength::Secure
    );
    assert_eq!(
        AuthPath::PlugAndCharge.strongest_plausible_level(),
        emob_core::IdentificationStrength::Secure,
        "the ceiling the path allows, and the record does not exceed it"
    );

    // …and the vehicle behind it is still one lookup away.
    assert_eq!(book.for_vehicle(&vehicle.pcid).count(), 1);
}

#[test]
fn a_din_session_cannot_claim_a_contract_certificate_authorised_it() {
    // DIN SPEC 70121 is external identification only — a card or an app — so a
    // session recorded as `din70121` and authorised by a contract certificate
    // is two stories about one event. Each half is an ordinary value; only the
    // pair is a contradiction, which is why the check is on the session and not
    // on either field.
    let mut rng = Rng::new(7);
    let mut station = VirtualStation::new(0, &mut rng);
    let plan = SessionPlan::draw(datetime!(2030-06-01 08:00 +2), &mut rng);
    let charged = station.charge(&plan, &[]);

    let transaction = Transaction {
        id: "din-0001".parse().expect("a valid session id"),
        evse_id: station.evse_id.clone(),
        authorization: Authorization::plug_and_charge(
            "DE-8AA-C000001".parse().expect("a valid contract id"),
            emob_core::Emaid::parse("DE8AACA2B3C4D5L").expect("a valid EMAID"),
        ),
        events: charged.events.clone(),
    };
    let assembled = transaction
        .assemble(Direction::Import)
        .expect("a session the station signed");

    let honest = assembled
        .session
        .clone()
        .negotiated(V2gProtocol::Iso15118_20);
    assert!(!honest.contradicts_its_own_protocol());

    let impossible = assembled.session.negotiated(V2gProtocol::Din70121);
    assert!(
        impossible.contradicts_its_own_protocol(),
        "DIN SPEC 70121 carries no contract certificate"
    );
}
