//! **The consignment ledger's invariants hold whatever order the calls arrive
//! in.**
//!
//! # Why a property over *sequences* and not over values
//!
//! Every property in this workspace so far is about a **value** — a price, a
//! record, a document — and can be stated by rating one session or building one
//! invoice. A service is not a value. It holds what *happened*, and its rules
//! are about the pair: what was true before a call and what may be true after.
//! No property over a single value can see one, and none of the nine guards
//! looks at a transition.
//!
//! That is why three defects of the same shape survived every review in three
//! different services on the same day (D297–D299): a delivery that was accepted,
//! an invoice that was posted, a month that was billed — each a fact true at an
//! instant, stored, and read later by something with no way to know the world
//! had moved. `roamd`'s seal was the sharpest: `consign` enforced
//! `[OCPI 2.3.0 §mod_cdrs]` by reading a field that `refused` would overwrite,
//! so the seal was exactly as durable as the field.
//!
//! So this drives the node with a **generated sequence of calls** — consign,
//! accept, refuse, credit, in whatever order a transport and an operator produce
//! between them — and checks the invariants after every one of them. The
//! sequences include the orders nobody writes a test for, which is the whole
//! point: the defect was not that the rule was wrong but that one call could
//! undo another.

use emob_cdr::{CdrBuilder, CdrKey, EvidenceRef};
use emob_core::{Currency, Direction, Energy, PartyId};
use emob_eichrecht::registry::{ComponentRef, RegisteredKey};
use emob_eichrecht::{Evidence, KeyRegistry};
use emob_roam::{Partner, PartnerRegistry, RoamingToken, TokenType};
use emob_session::{
    Authorization, EndReason, MeterReading, MeterSeries, ReadingContext, Session, SessionState,
};
use emob_tariff::{Dimension, PriceComponent, Tariff, TariffKind};
use ocmf::{Curve, PublicKey};
use p256::ecdsa::signature::hazmat::PrehashSigner;
use p256::ecdsa::{DerSignature, SigningKey};
use roamd::{DispatchError, Roamd};
use rust_decimal::Decimal;
use sha2::{Digest, Sha256};
use std::str::FromStr;
use time::macros::datetime;

const METER_SERIAL: &str = "BQ27400330016";

/// SplitMix64 — the workspace takes no `rand`, and a replayable property test
/// wants a seeded sequence anyway.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, high: u64) -> u64 {
        self.next() % high
    }
}

fn dec(s: &str) -> Decimal {
    Decimal::from_str(s).expect("a decimal literal")
}

fn kwh(s: &str) -> Energy {
    Energy::from_kwh(dec(s)).expect("a non-negative energy")
}

fn at(minute: i64) -> time::OffsetDateTime {
    datetime!(2026-06-02 10:00 +2) + time::Duration::minutes(minute)
}

fn signing_key() -> SigningKey {
    SigningKey::from_bytes(&[0x42u8; 32].into()).expect("a valid scalar")
}

fn signed_record(pagination: u64, marker: &str, register: &str, minute: i64) -> String {
    let payload = format!(
        r#"{{"FV":"1.4","GI":"ACME CS-1","GS":"GW-1","PG":"T{pagination}","MV":"Phoenix Contact","MM":"EEM-350-D-MCB","MS":"{METER_SERIAL}","IS":true,"IL":"TRUSTED","IF":["OCPP_AUTH_TLS"],"IT":"CENTRAL","RD":[{{"TM":"2026-06-02T{:02}:{:02}:00,000+0200 S","TX":"{marker}","RV":{register},"RI":"01-00:B2.08.00*FF","RU":"kWh","RT":"AC","EF":"","ST":"G"}}]}}"#,
        10 + (minute / 60),
        minute % 60,
    );
    let digest = Sha256::digest(payload.as_bytes());
    let signature: DerSignature = signing_key().sign_prehash(&digest).expect("a signature");
    format!(
        "OCMF|{payload}|{{\"SD\":\"{}\"}}",
        hex::encode(signature.as_bytes())
    )
}

fn registry() -> KeyRegistry {
    let mut registry = KeyRegistry::new();
    registry
        .insert(
            ComponentRef::Meter {
                serial: METER_SERIAL.into(),
            },
            RegisteredKey::unbounded(
                PublicKey::from_sec1(
                    Curve::Secp256r1,
                    signing_key()
                        .verifying_key()
                        .to_encoded_point(false)
                        .as_bytes(),
                )
                .expect("a well-formed SEC1 point"),
                "type approval 2026-01",
            ),
        )
        .expect("one key per component");
    registry
}

fn session() -> Session {
    let mut session = Session::open(
        "s-roam".parse().expect("a valid session id"),
        "DE*AB7*E840*6487".parse().expect("a valid EVSE id"),
        Authorization {
            path: emob_session::AuthPath::Roaming,
            subject: emob_session::Subject::Contract {
                id: "c-1".parse().expect("a valid contract id"),
                emaid: Some("NL-TNM-C00122045-K".parse().expect("a valid eMAID")),
            },
            token_ref: Some(emob_session::TokenRef::new("a".repeat(64)).expect("a digest")),
            authorization_reference: Some("auth-77".into()),
        },
        at(0),
    );
    session
        .transition_to(SessionState::Charging, at(0))
        .expect("a session charges");
    session
        .attach_series(
            MeterSeries::new(
                Direction::Import,
                vec![
                    MeterReading::new(
                        at(0),
                        kwh("100.000"),
                        Direction::Import,
                        ReadingContext::TransactionBegin,
                    )
                    .signed(),
                    MeterReading::new(
                        at(30),
                        kwh("129.500"),
                        Direction::Import,
                        ReadingContext::TransactionEnd,
                    )
                    .signed(),
                ],
            )
            .expect("an ascending series"),
        )
        .expect("one series per direction");
    session.end(at(30), EndReason::Local).expect("it ends");
    session
}

fn wholesale() -> Tariff {
    Tariff::simple(
        "cpo-ad-hoc".parse().expect("a valid tariff id"),
        Currency::EUR,
        TariffKind::AdHoc,
        emob_core::TimeZone::new("Europe/Berlin").expect("a bundled zone"),
        vec![PriceComponent::new(Dimension::Energy, dec("0.49")).with_vat(dec("19"))],
    )
}

fn evidence() -> Evidence {
    let texts = [
        signed_record(1, "B", "100.000", 0),
        signed_record(2, "E", "129.500", 30),
    ];
    let records: Vec<_> = texts
        .iter()
        .map(|text| ocmf::Record::parse(text).expect("a well-formed record"))
        .collect();
    Evidence::assemble(&records, &registry(), at(0))
}

fn issued(id: &str, supersedes: Option<CdrKey>) -> emob_cdr::Cdr {
    let binding = wholesale();
    let mut builder = CdrBuilder::from_session(&session(), Direction::Import)
        .expect("an ended session")
        .key(
            PartyId::new("DE", "ABC").expect("a valid party"),
            id.parse().expect("a valid CDR id"),
        )
        .evidence(EvidenceRef::from_evidence(&evidence(), "OCMF"))
        .rated_with(&binding);
    if let Some(previous) = supersedes {
        builder = builder.supersedes(previous);
    }
    builder.build().expect("a genuine session builds a record")
}

fn token() -> RoamingToken {
    RoamingToken::new(
        PartyId::new("NL", "TNM").expect("a valid party"),
        "045F2C9A",
        TokenType::Rfid,
        "NL-TNM-C00122045-K".parse().expect("a valid contract id"),
    )
    .expect("a routable token")
}

fn node() -> Roamd {
    Roamd::new(
        PartnerRegistry::new(PartyId::new("DE", "ABC").expect("a valid party")).with(
            Partner::emsp(PartyId::new("NL", "TNM").expect("a valid party"))
                .on_signed_data()
                .settling_within(time::Duration::days(30)),
        ),
    )
}

/// Every rule the ledger states, asked of the whole ledger after every call.
///
/// Stated as one function rather than five assertions at five call sites,
/// because the claim is about the ledger and not about any one operation: a rule
/// that only holds after the calls somebody thought to check it after is a rule
/// that holds by coincidence.
fn invariants_hold(node: &Roamd, keys: &[CdrKey], now: time::OffsetDateTime) -> Result<(), String> {
    let accepted: Vec<&CdrKey> = keys
        .iter()
        .filter(|key| {
            node.consignment(key)
                .is_some_and(|held| held.delivery.is_accepted())
        })
        .collect();

    // 1. `[OCPI 2.3.0 §mod_cdrs]` — a record the eMSP has is sealed. The seal is
    //    the whole point of the ledger, so it is asked of every accepted record
    //    rather than of the one a test just accepted.
    for key in &accepted {
        if node.pending().any(|consignment| &&consignment.key == key) {
            return Err(format!("{key} is accepted and also waiting for a socket"));
        }
        if node.unsettled(now).iter().any(|late| &&late.key == key) {
            return Err(format!("{key} is accepted and also reported unsettled"));
        }
    }

    // 2. …and a pending record is not an accepted one, from the other side, so
    //    the two collections cannot both be right about a record and disagree.
    for consignment in node.pending() {
        if consignment.delivery.is_accepted() {
            return Err(format!(
                "{} is pending and accepted at once",
                consignment.key
            ));
        }
    }

    // 3. Every record `unsettled` names is one somebody addressed, and every one
    //    of them is past the window that partner agreed to rather than a
    //    constant.
    for late in node.unsettled(now) {
        let Some(held) = node.consignment(&late.key) else {
            return Err(format!("{} is unsettled and was never consigned", late.key));
        };
        if held.delivery.is_accepted() {
            return Err(format!("{} is unsettled and accepted", late.key));
        }
        if now - held.ended_at <= late.agreed {
            return Err(format!(
                "{} is inside its window and reported late",
                late.key
            ));
        }
    }
    Ok(())
}

#[test]
fn the_seal_and_the_correction_order_hold_under_any_sequence_of_calls() {
    let token = token();
    // Four records and the replacement each of them may have, which is the
    // smallest set in which a correction has an order at all.
    let originals: Vec<emob_cdr::Cdr> = (0..4).map(|n| issued(&format!("cdr-{n}"), None)).collect();
    let keys: Vec<CdrKey> = originals.iter().map(|cdr| cdr.key.clone()).collect();
    let replacements: Vec<emob_cdr::Cdr> = (0..4)
        .map(|n| issued(&format!("cdr-{n}-R"), Some(keys[n].clone())))
        .collect();
    let credits: Vec<CdrKey> = (0..4)
        .map(|n| CdrKey {
            party: PartyId::new("DE", "ABC").expect("a valid party"),
            id: format!("cdr-{n}-C").parse().expect("a valid CDR id"),
        })
        .collect();
    let all: Vec<CdrKey> = keys
        .iter()
        .chain(replacements.iter().map(|cdr| &cdr.key))
        .chain(credits.iter())
        .cloned()
        .collect();

    let mut rng = Rng(0x005E_A1ED);
    for run in 0..400_u64 {
        let mut node = node();
        // Whether each original has been taken by the partner, tracked here so
        // the property can state the seal without asking the ledger the very
        // question it is checking.
        let mut taken = [false; 4];

        for step in 0..24 {
            let n = usize::try_from(rng.below(4)).unwrap_or(0);
            let now = at(60 * 24 * 40); // past every partner's window
            match rng.below(6) {
                0 => {
                    let outcome = node.consign(&originals[n], &token);
                    if taken[n] {
                        assert!(
                            matches!(outcome, Err(DispatchError::AlreadyAccepted { .. })),
                            "run {run} step {step}: a sealed record was consigned again: \
                             {outcome:?}"
                        );
                    }
                }
                1 => {
                    if node.accepted(&keys[n], at(40), None).is_ok() {
                        taken[n] = true;
                    }
                }
                2 => {
                    let outcome = node.refused(&keys[n], at(41), "the total does not match");
                    assert!(
                        !taken[n] || matches!(outcome, Err(DispatchError::AlreadyAccepted { .. })),
                        "run {run} step {step}: an acceptance was taken back: {outcome:?}"
                    );
                }
                3 => {
                    let _ = node.credit(&keys[n], credits[n].clone(), at(42));
                }
                4 => {
                    let _ = node.accepted(&credits[n], at(43), None);
                }
                _ => {
                    // A replacement may go exactly when nothing is owed back:
                    // the original was never taken, or its reversal has been.
                    // Read before the call, because the call borrows the node.
                    let credit_taken = node
                        .consignment(&credits[n])
                        .is_some_and(|held| held.delivery.is_accepted());
                    let already = node.consignment(&replacements[n].key).is_some();
                    let outcome = node
                        .consign(&replacements[n], &token)
                        .map(|held| held.key.to_string());
                    if taken[n] && !credit_taken {
                        assert!(
                            matches!(outcome, Err(DispatchError::CreditNotAcceptedYet { .. })),
                            "run {run} step {step}: a replacement overtook its own reversal: \
                             {outcome:?}"
                        );
                    } else if !already {
                        assert!(
                            outcome.is_ok(),
                            "run {run} step {step}: nothing was owed back and the replacement \
                             was refused anyway: {outcome:?}"
                        );
                    }
                }
            }

            if let Err(broken) = invariants_hold(&node, &all, now) {
                panic!("run {run} step {step}: {broken}");
            }
        }
    }
}
