//! The half of a closing no document can decide: **when** it happens, **what
//! number** it carries, **which** document supersedes which, and **which
//! account** each posting lands in.
//!
//! `emob-billing` is tested on what a document says. Nothing here re-tests that.
//! Every assertion below is about a fact that spans two closings or two systems,
//! and is therefore invisible to a crate that sees one document at a time:
//!
//! * a month closed twice is one invoice or a correction, and the difference is
//!   whether money moves twice;
//! * `[UStG §14(4) Nr. 4]` spends a number **once**, whatever happens next;
//! * a document the recipient refused is not one to book;
//! * and a VAT liability has a creditor, so 19 % owed in Germany and 20 % owed
//!   in France are two accounts and two filings (D270).
//!
//! The month itself is signed for real: OCMF records produced with a private key
//! at test time, verified through the path a station would go through, rated by
//! a tariff, assembled by `emob-billing`. The chain reaches the books from the
//! meter rather than from a constant.

use billd::{Billd, ChartOfAccounts, ClosingError, Numbering, Submission};
use emob_billing::invoice::Subscription;
use emob_billing::{Counterparty, InvoiceBuilder, TaxStatus, postings};
use emob_cdr::{CdrBuilder, CdrLedger, EvidenceRef};
use emob_core::{Currency, Direction, Energy, PartyId};
use emob_eichrecht::registry::{ComponentRef, RegisteredKey};
use emob_eichrecht::{Evidence, KeyRegistry};
use emob_session::{
    Authorization, EndReason, MeterReading, MeterSeries, ReadingContext, Session, SessionState,
};
use emob_tariff::{Dimension, PriceComponent, Tariff, TariffKind};
use ocmf::{Curve, PublicKey};
use p256::ecdsa::signature::hazmat::PrehashSigner;
use p256::ecdsa::{DerSignature, SigningKey};
use rust_decimal::Decimal;
use sha2::{Digest, Sha256};
use std::str::FromStr;
use time::macros::{date, datetime};

const METER_SERIAL: &str = "BQ27400330016";

fn dec(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

fn kwh(s: &str) -> Energy {
    Energy::from_kwh(dec(s)).unwrap()
}

fn day(n: i64) -> time::OffsetDateTime {
    datetime!(2026-06-01 10:00 +2) + time::Duration::days(n)
}

fn signing_key() -> SigningKey {
    SigningKey::from_bytes(&[0x42u8; 32].into()).unwrap()
}

/// A real OCMF record, signed with the key the registry knows.
fn signed_record(
    session: u64,
    pagination: u64,
    marker: &str,
    register: &str,
    at: time::OffsetDateTime,
) -> String {
    let payload = format!(
        r#"{{"FV":"1.4","GI":"ACME CS-1","GS":"GW-1","PG":"T{pagination}","MV":"Phoenix Contact","MM":"EEM-350-D-MCB","MS":"{METER_SERIAL}","IS":true,"IL":"TRUSTED","IF":["OCPP_AUTH_TLS"],"IT":"CENTRAL","ID":"s{session}","RD":[{{"TM":"{}-{:02}-{:02}T{:02}:{:02}:00,000+0200 S","TX":"{marker}","RV":{register},"RI":"01-00:B2.08.00*FF","RU":"kWh","RT":"AC","EF":"","ST":"G"}}]}}"#,
        at.year(),
        u8::from(at.month()),
        at.day(),
        at.hour(),
        at.minute(),
    );
    let digest = Sha256::digest(payload.as_bytes());
    let sig: DerSignature = signing_key().sign_prehash(&digest).unwrap();
    format!(
        "OCMF|{payload}|{{\"SD\":\"{}\"}}",
        hex::encode(sig.as_bytes())
    )
}

fn registry() -> KeyRegistry {
    let mut registry = KeyRegistry::new();
    registry
        .insert(
            ComponentRef::GatewayAndMeter {
                gateway: "GW-1".into(),
                meter: METER_SERIAL.into(),
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
        .unwrap();
    registry
}

/// One session on day `n`, half an hour long.
fn session(n: i64, id: &str, from: &str, to: &str) -> (Session, Evidence) {
    let start = day(n);
    let end = start + time::Duration::minutes(30);
    let counter = u64::try_from(n).unwrap() * 2;

    let raw = [
        signed_record(counter, counter + 1, "B", from, start),
        signed_record(counter, counter + 2, "E", to, end),
    ];
    let records: Vec<_> = raw
        .iter()
        .map(|r| ocmf::Record::parse(r).unwrap())
        .collect();
    let evidence = Evidence::assemble(&records, &registry(), end);

    let mut session = Session::open(
        id.parse().unwrap(),
        "DE*AB7*E840*6487".parse().unwrap(),
        Authorization::ad_hoc(),
        start,
    );
    session
        .transition_to(SessionState::Charging, start)
        .unwrap();
    session
        .attach_series(
            MeterSeries::new(
                Direction::Import,
                vec![
                    MeterReading::new(
                        start,
                        kwh(from),
                        Direction::Import,
                        ReadingContext::TransactionBegin,
                    )
                    .signed(),
                    MeterReading::new(
                        end,
                        kwh(to),
                        Direction::Import,
                        ReadingContext::TransactionEnd,
                    )
                    .signed(),
                ],
            )
            .unwrap(),
        )
        .unwrap();
    session.end(end, EndReason::Local).unwrap();
    (session, evidence)
}

/// A tariff that states no VAT of its own, so the place of supply governs.
fn tariff(price: &str) -> Tariff {
    Tariff::simple(
        "ad-hoc-2026".parse().unwrap(),
        Currency::EUR,
        TariffKind::AdHoc,
        emob_core::TimeZone::new("Europe/Berlin").unwrap(),
        vec![PriceComponent::new(Dimension::Energy, dec(price))],
    )
}

/// Three days of charging, in a ledger.
fn month(price: &str) -> CdrLedger {
    let mut ledger = CdrLedger::new();
    let party = PartyId::new("DE", "ABC").unwrap();
    let tariff = tariff(price);

    for (n, id, from, to) in [
        (0_i64, "s-1", "100.000", "129.500"),
        (1, "s-2", "129.500", "150.000"),
        (2, "s-3", "150.000", "163.333"),
    ] {
        let (session, evidence) = session(n, id, from, to);
        let cdr = CdrBuilder::from_session(&session, Direction::Import)
            .unwrap()
            .key(party.clone(), format!("cdr-{id}").parse().unwrap())
            .evidence(EvidenceRef::from_evidence(&evidence, "OCMF"))
            .rated_with(&tariff)
            .build()
            .unwrap();
        assert!(ledger.accept(cdr).is_stored());
    }
    ledger
}

fn cpo() -> Counterparty {
    Counterparty::new(
        "Stadtwerke Musterstadt GmbH",
        "Musterstadt",
        TaxStatus::business("DE", "DE123456789"),
    )
    .at("Hauptstraße 1", "12345")
    .reachable_at("de-cpo@example.org", "EM")
    .registered_as("HRB 12345", None)
    .contactable("Rechnungswesen", "+49 555 123456", "rechnung@example.org")
}

fn driver() -> Counterparty {
    Counterparty::new(
        "Erika Mustermann",
        "Beispielstadt",
        TaxStatus::consumer("DE"),
    )
    .at("Nebenweg 7", "54321")
}

/// The closing the caller hands over: a document `emob-billing` assembled, with
/// the number this service issued.
fn closing(
    ledger: &CdrLedger,
    buyer: Counterparty,
) -> impl FnOnce(String) -> Result<emob_billing::invoice::Invoice, emob_billing::BillingError> + '_
{
    move |number| {
        Ok(InvoiceBuilder::new(
            number,
            date!(2026 - 07 - 01),
            (date!(2026 - 06 - 01), date!(2026 - 06 - 30)),
            cpo(),
            buyer,
        )
        .supplied_from("DE", dec("19"))
        .ledger(ledger)
        .due_on(date!(2026 - 07 - 15))
        .build()?
        .value)
    }
}

fn service() -> Billd {
    Billd::new("emob", Numbering::series("R", 2026))
}

#[test]
fn a_month_closes_once_and_the_second_closing_is_refused_by_name() {
    let ledger = month("0.49");
    let mut billd = service();

    let number = billd
        .issue("2026-06", closing(&ledger, driver()))
        .unwrap()
        .invoice
        .number
        .clone();
    assert_eq!(number, "R-2026-0001");

    // The failure this service exists to prevent. A month re-closed against the
    // same records is not a second month, and a platform that answered by
    // issuing a second invoice would collect June twice.
    let again = billd.issue("2026-06", closing(&ledger, driver()));
    assert!(
        matches!(
            again,
            Err(ClosingError::AlreadyClosed { ref period, ref number })
                if period == "2026-06" && number == "R-2026-0001"
        ),
        "{again:?}"
    );

    // …and the refusal did not spend a number: a different month gets 0002.
    let july = billd
        .issue("2026-07", closing(&ledger, driver()))
        .unwrap()
        .invoice
        .number
        .clone();
    assert_eq!(july, "R-2026-0002");
}

#[test]
fn a_document_the_recipient_refused_is_not_one_to_book() {
    let ledger = month("0.49");
    let mut billd = service();
    billd.issue("2026-06", closing(&ledger, driver())).unwrap();

    // Issued, and therefore *not* in the books. The whole reason posting is a
    // separate act: a platform that books on issue holds a trial balance the
    // recipient's own records disagree with, and nothing fails until somebody
    // reconciles.
    let unbooked = billd.book("R-2026-0001");
    assert!(
        matches!(unbooked, Err(ClosingError::NotAccepted { ref number }) if number == "R-2026-0001"),
        "{unbooked:?}"
    );

    billd
        .rejected("R-2026-0001", date!(2026 - 07 - 02), "Leitweg-ID unknown")
        .unwrap();
    assert!(billd.book("R-2026-0001").is_err());
    assert!(matches!(
        billd.issued("R-2026-0001").unwrap().submission,
        Submission::Rejected { .. }
    ));

    // The number stays spent. `[UStG §14(4) Nr. 4]` issues one **einmalig**, and
    // a counter that rewound on a rejection hands the corrected document a
    // number the refused one already carried — which is exactly the collision
    // the statute names.
    let corrected = billd
        .issue("2026-06-resubmitted", closing(&ledger, driver()))
        .unwrap();
    assert_eq!(corrected.invoice.number, "R-2026-0002");
}

#[test]
fn an_accepted_month_reaches_the_books_and_they_balance() {
    let ledger = month("0.49");
    let mut billd = service();
    billd.issue("2026-06", closing(&ledger, driver())).unwrap();
    billd
        .accepted(
            "R-2026-0001",
            date!(2026 - 07 - 02),
            Some("ZRE-2026-88123".to_owned()),
        )
        .unwrap();

    let recorded = billd.book("R-2026-0001").unwrap();
    assert!(recorded.is_new);
    assert!(billd.issued("R-2026-0001").unwrap().booked);

    // 63.333 kWh at 0.49 gross: 12.15 + 8.44 + 5.49 = 26.08 taxable, 4.96 VAT,
    // 31.04 receivable. The books state the same figures the document does.
    let trial = billd.trial_balance().unwrap();
    let totals = trial
        .totals(doubleentry::Currency::EUR, doubleentry::Layer::Settled)
        .unwrap();
    assert!(totals.is_balanced(), "{totals:?}");

    let vat = billd
        .balance_of("liabilities:vat:DE:19", "EUR")
        .unwrap()
        .expect("the German VAT account was opened by the posting");
    let (direction, amount) = vat.net().unwrap();
    assert_eq!(direction, doubleentry::Direction::Credit);
    assert_eq!(amount.to_string(), "4.96");

    let receivable = billd
        .balance_of("assets:receivable", "EUR")
        .unwrap()
        .expect("the receivable was opened by the posting");
    let (direction, amount) = receivable.net().unwrap();
    assert_eq!(direction, doubleentry::Direction::Debit);
    assert_eq!(amount.to_string(), "31.04");

    // An operator that books twice is repeating themselves, and is told so.
    assert!(matches!(
        billd.book("R-2026-0001"),
        Err(ClosingError::AlreadyBooked { .. })
    ));

    // A *process* that crashed between the journal's write and the flag is a
    // different case: the entry is keyed on the invoice number, so the replay
    // finds what it already wrote instead of posting June a second time.
    let mut replayed = service();
    replayed
        .issue("2026-06", closing(&ledger, driver()))
        .unwrap();
    replayed
        .accepted("R-2026-0001", date!(2026 - 07 - 02), None)
        .unwrap();
    let first = replayed.book("R-2026-0001").unwrap();
    let entry = replayed
        .journal()
        .get(first.id)
        .expect("the entry the journal sealed");
    assert_eq!(entry.postings().len(), 3);
}

#[test]
fn a_re_rated_month_cancels_before_it_re_bills_and_the_books_net_to_the_replacement() {
    let ledger = month("0.49");
    let mut billd = service();
    billd.issue("2026-06", closing(&ledger, driver())).unwrap();
    billd
        .accepted("R-2026-0001", date!(2026 - 07 - 02), None)
        .unwrap();
    billd.book("R-2026-0001").unwrap();

    // The tariff was wrong: the same three sessions at 0.39 net.
    let rerated = month("0.39");
    let (credit, replacement) = billd
        .rebill(
            "R-2026-0001",
            date!(2026 - 07 - 20),
            "re-rated: the June tariff was published at 0.39 and billed at 0.49",
            closing(&rerated, driver()),
        )
        .unwrap();
    assert_eq!(credit, "R-2026-0002");
    assert_eq!(replacement, "R-2026-0003");

    // The order is `[OCPI 2.3.0 §mod_cdrs]`'s and EN 16931's alike: the reversal
    // is its own numbered document, it names what it cancels, and it states
    // positive figures because the direction is in BT-3.
    let storno = &billd.issued(&credit).unwrap().invoice;
    assert!(storno.kind.is_credit_note());
    assert_eq!(storno.cancels.as_ref().unwrap().number, "R-2026-0001");
    assert_eq!(storno.gross_total().to_string(), "31.04 EUR");
    assert_eq!(
        billd.issued(&credit).unwrap().cancels.as_deref(),
        Some("R-2026-0001")
    );

    // …and the reason is on the document the recipient reads, not in a log here.
    assert!(
        storno
            .notes
            .iter()
            .any(|note| { note.cdr.is_none() && note.text.contains("published at 0.39") }),
        "{:?}",
        storno.notes
    );

    // Cancelling it again is refused: a second reversal against one invoice is
    // two credit notes for one demand, and the recipient books both.
    let twice = billd.rebill(
        "R-2026-0001",
        date!(2026 - 07 - 21),
        "again",
        closing(&rerated, driver()),
    );
    assert!(
        matches!(twice, Err(ClosingError::AlreadyCancelled { ref by, .. }) if by == "R-2026-0002"),
        "{twice:?}"
    );

    // Both documents are accepted and booked, in that order.
    for number in [&credit, &replacement] {
        billd.accepted(number, date!(2026 - 07 - 21), None).unwrap();
        billd.book(number).unwrap();
    }

    // 63.333 kWh at 0.39 gross: 9.67 + 6.72 + 4.37 = 20.76 taxable, 3.94 VAT,
    // 24.70 receivable — and the receivable holds the replacement *alone*,
    // because the reversal took the original back out.
    let receivable = billd
        .balance_of("assets:receivable", "EUR")
        .unwrap()
        .unwrap();
    let (direction, amount) = receivable.net().unwrap();
    assert_eq!(direction, doubleentry::Direction::Debit);
    assert_eq!(amount.to_string(), "24.70");
    assert!(
        billd
            .trial_balance()
            .unwrap()
            .totals(doubleentry::Currency::EUR, doubleentry::Layer::Settled)
            .unwrap()
            .is_balanced()
    );
}

#[test]
fn a_correction_against_a_document_this_service_never_issued_is_refused() {
    let ledger = month("0.49");
    let mut billd = service();
    let missing = billd.rebill(
        "R-2025-0099",
        date!(2026 - 07 - 20),
        "re-rated",
        closing(&ledger, driver()),
    );
    assert!(
        matches!(missing, Err(ClosingError::NotIssued { ref number }) if number == "R-2025-0099"),
        "{missing:?}"
    );
}

#[test]
fn a_vat_liability_has_a_creditor_so_two_countries_are_two_accounts() {
    // C-60/23: the subscription is a **separate and independent** supply of
    // services, so a document can owe tax in two places at once — the
    // electricity where the point stands, the network access where the
    // customer's provider is. A single `liabilities:vat` account would net two
    // filings into one figure that is right for neither authority (D270).
    let ledger = month("0.49");
    let mut billd = service();

    // A German provider billing a Dutch driver who charged in France. The
    // electricity is taxed where it was drawn `[UStG §3g]`; the fee where the
    // supplier sits `[UStG §3a(1)]`, because a service to a private person does
    // not follow its customer. Two supplies, two authorities, one document.
    let dutch_driver = Counterparty::new("Jan de Vries", "Amsterdam", TaxStatus::consumer("NL"))
        .at("Prinsengracht 1", "1015");

    billd
        .issue("2026-06-FR", move |number| {
            Ok(InvoiceBuilder::new(
                number,
                date!(2026 - 07 - 01),
                (date!(2026 - 06 - 01), date!(2026 - 06 - 30)),
                cpo(),
                dutch_driver,
            )
            .supplied_from("FR", dec("20"))
            .vat_rate_in("DE", dec("19"))
            .ledger(&ledger)
            .subscription(Subscription::new(
                "network access, June 2026",
                dec("4.99"),
                date!(2026 - 06 - 01),
                date!(2026 - 06 - 30),
            ))
            .due_on(date!(2026 - 07 - 15))
            .build()?
            .value)
        })
        .unwrap();
    billd
        .accepted("R-2026-0001", date!(2026 - 07 - 02), None)
        .unwrap();
    billd.book("R-2026-0001").unwrap();

    let invoice = &billd.issued("R-2026-0001").unwrap().invoice;
    let roles = postings::postings_for(invoice);
    assert!(
        roles.roles().contains(&&postings::Role::ServiceRevenue),
        "the subscription is revenue in its own right: {:?}",
        roles.roles()
    );

    // Two accounts, because they are two filings. A single `liabilities:vat`
    // would net a French return into a German one and be right for neither.
    let french = billd
        .balance_of("liabilities:vat:FR:20", "EUR")
        .unwrap()
        .expect("the electricity is owed to the French authority");
    assert_eq!(french.net().unwrap().1.to_string(), "5.17");
    let german = billd
        .balance_of("liabilities:vat:DE:19", "EUR")
        .unwrap()
        .expect("the network access is owed to the German one");
    assert_eq!(german.net().unwrap().1.to_string(), "0.95");
}

#[test]
fn an_operators_own_chart_of_accounts_is_the_one_that_is_posted_to() {
    // The default paths say what an account is *for*; a deployment running
    // SKR 03 says what it is *called*. The role is the stable name in between,
    // which is why `emob-billing` addresses a posting by role and stops there.
    let ledger = month("0.49");
    let chart = ChartOfAccounts::new()
        .mapping(&postings::Role::Receivable, "1400")
        .mapping(&postings::Role::EnergyRevenue, "8400")
        .mapping(
            &postings::Role::VatPayable {
                rate: dec("19"),
                place_of_supply: "DE".to_owned(),
            },
            "1776",
        );

    let mut billd =
        Billd::new("emob", Numbering::series("R", 2026).resuming_after(6)).posting_to(chart);
    let issued = billd.issue("2026-06", closing(&ledger, driver())).unwrap();
    // …and a series resumed from the store carries on rather than colliding.
    assert_eq!(issued.invoice.number, "R-2026-0007");

    billd
        .accepted("R-2026-0007", date!(2026 - 07 - 02), None)
        .unwrap();
    billd.book("R-2026-0007").unwrap();

    let vat = billd.balance_of("1776", "EUR").unwrap().unwrap();
    assert_eq!(vat.net().unwrap().1.to_string(), "4.96");
    assert!(
        billd
            .balance_of("liabilities:vat:DE:19", "EUR")
            .unwrap()
            .is_none(),
        "the default path is not opened beside the operator's own"
    );
}
