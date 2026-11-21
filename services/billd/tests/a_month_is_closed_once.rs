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
                        .to_sec1_point(false)
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
    assert!(!recorded.replayed);
    assert!(billd.issued("R-2026-0001").unwrap().booked);

    // 63.333 kWh at 0.49 gross: 12.15 + 8.44 + 5.49 = 26.08 taxable, 4.96 VAT,
    // 31.04 receivable. The books state the same figures the document does.
    let trial = billd.trial_balance().unwrap();
    assert!(trial.is_balanced(), "{trial:?}");

    let vat = billd
        .balance_of("liabilities:vat:DE:19", "EUR")
        .unwrap()
        .expect("the German VAT account was opened by the posting");
    let net = vat.net();
    let amount = net.abs();
    assert!(net.is_negative(), "a credit balance");
    assert_eq!(amount.to_string(), "4.96");

    let receivable = billd
        .balance_of("assets:receivable", "EUR")
        .unwrap()
        .expect("the receivable was opened by the posting");
    let net = receivable.net();
    let amount = net.abs();
    assert!(net.is_positive(), "a debit balance");
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
        .entry(first.id)
        .expect("the entry the journal recorded");
    assert_eq!(entry.parts().postings.len(), 3);
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

    // 63.333 kWh at 0.39 gross: receipts of 11.51 + 8.00 + 5.20 = 24.71,
    // which is the amount due (BT-115) — 24.71 receivable, the replacement
    // *alone*, because the reversal took the original back out.
    let receivable = billd
        .balance_of("assets:receivable", "EUR")
        .unwrap()
        .unwrap();
    let net = receivable.net();
    let amount = net.abs();
    assert!(net.is_positive(), "a debit balance");
    assert_eq!(amount.to_string(), "24.71");
    assert!(billd.trial_balance().unwrap().is_balanced());
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
    assert_eq!(french.net().abs().to_string(), "5.17");
    let german = billd
        .balance_of("liabilities:vat:DE:19", "EUR")
        .unwrap()
        .expect("the network access is owed to the German one");
    assert_eq!(german.net().abs().to_string(), "0.95");
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
    assert_eq!(vat.net().abs().to_string(), "4.96");
    assert!(
        billd
            .balance_of("liabilities:vat:DE:19", "EUR")
            .unwrap()
            .is_none(),
        "the default path is not opened beside the operator's own"
    );
}

/// **A reversal is only a reversal beside the entry it undoes.**
///
/// `postings_for` reverses every side of a *Stornorechnung* — same accounts,
/// same amounts, the other way — which is exactly right against the invoice it
/// cancels and a fabrication on its own. Booked alone it credits a receivable
/// nobody debited and debits revenue nobody earned, and the journal balances:
/// double entry balances whatever it is told, so nothing fails until somebody
/// reconciles against the recipient's own records — which is the failure this
/// service's separation of *issuing* from *posting* exists to prevent (D298).
///
/// Which document cancels which is this service's fact. `emob-billing` builds
/// the cancellation and reverses the postings; neither it nor the journal knows
/// whether the original was ever posted.
#[test]
fn a_cancellation_cannot_reach_the_books_before_the_invoice_it_reverses() {
    let ledger = month("0.49");
    let mut billd = service().posting_to(ChartOfAccounts::new());

    let original = billd
        .issue("2026-06", closing(&ledger, driver()))
        .expect("the first closing")
        .invoice
        .number
        .clone();
    billd
        .accepted(&original, date!(2026 - 07 - 02), None)
        .expect("the recipient took it");

    // Accepted, and deliberately **not** booked.
    let (credit, _replacement) = billd
        .rebill(
            &original,
            date!(2026 - 07 - 10),
            "the meter was re-read",
            closing(&ledger, driver()),
        )
        .expect("a re-rated month cancels before it re-bills");
    billd
        .accepted(&credit, date!(2026 - 07 - 11), None)
        .expect("the recipient took the credit note too");

    let error = billd
        .book(&credit)
        .expect_err("a reversal of an entry that does not exist is not a posting");
    assert!(
        matches!(error, ClosingError::ReversesUnbooked { .. }),
        "{error}"
    );
    assert!(error.to_string().contains(&original), "{error}");
    // …and nothing reached the accounts, which is the whole of the claim: a
    // balance of `None` is a different statement from a balance of zero.
    for path in [
        "assets:receivable",
        "income:energy",
        "liabilities:vat:DE:19",
    ] {
        assert_eq!(
            billd.balance_of(path, "EUR").expect("a readable journal"),
            None,
            "{path}"
        );
    }

    // …and in the order the correction actually has, both halves post and the
    // accounts net to nothing.
    billd.book(&original).expect("the invoice first");
    billd.book(&credit).expect("then the reversal");
    let receivable = billd
        .balance_of("assets:receivable", "EUR")
        .expect("a readable journal")
        .expect("both halves are posted");
    assert_eq!(receivable.debit(), receivable.credit());
}

#[test]
fn a_replacement_cannot_reach_the_books_while_the_invoice_it_replaces_still_stands() {
    // The mirror of the test above, and the half that was missing. A re-billing
    // is two documents; booking only the second leaves the original standing,
    // and the customer is debited twice for one June — 31.04 and 24.70 — in a
    // journal whose every entry balances, because double entry balances
    // whatever it is told (D307).
    let ledger = month("0.49");
    let mut billd = service();
    billd.issue("2026-06", closing(&ledger, driver())).unwrap();
    billd
        .accepted("R-2026-0001", date!(2026 - 07 - 02), None)
        .unwrap();
    billd.book("R-2026-0001").unwrap();

    let rerated = month("0.39");
    let (credit, replacement) = billd
        .rebill(
            "R-2026-0001",
            date!(2026 - 07 - 20),
            "re-rated",
            closing(&rerated, driver()),
        )
        .unwrap();

    billd
        .accepted(&replacement, date!(2026 - 07 - 21), None)
        .unwrap();
    let posted = billd.book(&replacement);
    assert!(
        matches!(
            posted,
            Err(ClosingError::OriginalNotReversed { ref standing, ref credit, .. })
                if standing == "R-2026-0001" && credit == "R-2026-0002"
        ),
        "{posted:?}"
    );

    // …and with the reversal in the books it goes through, and the receivable
    // holds the replacement alone.
    billd
        .accepted(&credit, date!(2026 - 07 - 21), None)
        .unwrap();
    billd.book(&credit).unwrap();
    billd.book(&replacement).unwrap();
    let net = billd
        .balance_of("assets:receivable", "EUR")
        .unwrap()
        .unwrap()
        .net();
    let amount = net.abs();
    assert!(net.is_positive(), "a debit balance");
    assert_eq!(amount.to_string(), "24.71");
}

#[test]
fn a_corrected_period_is_closed_by_the_document_that_closes_it_now() {
    // `AlreadyClosed` names the invoice in force. Left pointing at the
    // original, it named a cancelled document, and an operator following that
    // name would correct a document that had already been corrected (D307).
    let ledger = month("0.49");
    let mut billd = service();
    billd.issue("2026-06", closing(&ledger, driver())).unwrap();
    let rerated = month("0.39");
    let (_, replacement) = billd
        .rebill(
            "R-2026-0001",
            date!(2026 - 07 - 20),
            "re-rated",
            closing(&rerated, driver()),
        )
        .unwrap();

    let again = billd.issue("2026-06", closing(&rerated, driver()));
    assert!(
        matches!(again, Err(ClosingError::AlreadyClosed { ref number, .. }) if *number == replacement),
        "the period is closed by {replacement}, not by the invoice it replaced: {again:?}"
    );
}

// ── The invariants hold whatever order the calls arrive in ──────────────────
//
// Every test above is an example: a sequence somebody thought of, asserted at
// its end. `roamd` learned in D297–D299 that a service's rules are about the
// **pair** — what was true before a call and what may be true after — and that
// no property over a single value can see one. That lesson produced exactly one
// sequence property, in `roamd`, and the three defects below survived in this
// service because it was never carried here (D307).
//
// So this drives the books with a generated sequence of calls — issue, accept,
// reject, book, cancel-and-re-bill, in whatever order a recipient's platform
// and an operator produce between them — and checks the invariants after every
// one of them.

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

    fn below(&mut self, high: usize) -> usize {
        usize::try_from(self.next() % high as u64).unwrap_or(0)
    }
}

/// What the books hold for the customer, in cents, as a signed figure.
fn receivable(billd: &Billd) -> i128 {
    let Some(balance) = billd.balance_of("assets:receivable", "EUR").unwrap() else {
        return 0;
    };
    balance.net().to_minor()
}

/// Stated once rather than at every call site, because the point is that they
/// hold after **every** call and not at the end of a story.
fn books_hold(billd: &Billd) -> Result<(), String> {
    // 1. The journal balances. Weak on its own — double entry balances whatever
    //    it is told — and it is the floor everything else stands on.
    let trial = billd.trial_balance().map_err(|e| e.to_string())?;
    if !trial.is_balanced() {
        return Err(format!("the journal does not balance: {trial:?}"));
    }

    // 2. Nothing is in the books that the recipient did not accept. `book`
    //    refuses one, and a late refusal used to overwrite the field behind it.
    for document in billd.documents() {
        if document.booked && !document.submission.is_accepted() {
            return Err(format!(
                "{} is booked and not accepted: {:?}",
                document.invoice.number, document.submission
            ));
        }
    }

    // 3. **One period is charged once.** The receivable is one document's gross
    //    or nothing — never the sum of an invoice and the invoice that replaced
    //    it, which is what a replacement posted over a standing original comes
    //    to, in a journal whose every entry balances (D307).
    let mut lawful = vec![0_i128];
    for document in billd.documents() {
        if document.invoice.kind.is_credit_note() {
            continue;
        }
        let gross = (document.invoice.payable_total().amount() * dec("100"))
            .round()
            .to_string();
        lawful.push(gross.parse().expect("a whole number of cents"));
    }
    let held = receivable(billd);
    if !lawful.contains(&held) {
        return Err(format!(
            "the receivable holds {held} cents, which is not one document's gross: {lawful:?}"
        ));
    }

    // 4. The period is closed by a document that has not been cancelled — the
    //    name `AlreadyClosed` gives an operator has to be the one in force, and
    //    it went on naming the invoice a correction had already reversed.
    if let Some(closing) = billd.closes("2026-06") {
        let number = &closing.invoice.number;
        if closing.invoice.kind.is_credit_note() {
            return Err(format!("{number} closes the period and is a credit note"));
        }
        if billd
            .documents()
            .any(|d| d.cancels.as_deref() == Some(number.as_str()))
        {
            return Err(format!("{number} closes the period and has been cancelled"));
        }
    }
    Ok(())
}

#[test]
fn the_books_hold_under_any_order_of_calls() {
    let ledger = month("0.49");
    let rerated = month("0.39");

    for seed in 0..128_u64 {
        let mut rng = Rng(seed.wrapping_mul(0x2545_F491_4F6C_DD1D) | 1);
        let mut billd = service();
        billd
            .issue("2026-06", closing(&ledger, driver()))
            .expect("the first closing");
        books_hold(&billd).unwrap_or_else(|broken| panic!("seed {seed}, at issue: {broken}"));

        for step in 0..32 {
            let numbers: Vec<String> = billd
                .documents()
                .map(|d| d.invoice.number.clone())
                .collect();
            let number = numbers[rng.below(numbers.len())].clone();
            match rng.below(5) {
                0 => {
                    let _ = billd.accepted(&number, date!(2026 - 07 - 10), None);
                }
                1 => {
                    let _ = billd.rejected(&number, date!(2026 - 07 - 10), "schema");
                }
                2 | 3 => {
                    let _ = billd.book(&number);
                }
                _ => {
                    let _ = billd.rebill(
                        &number,
                        date!(2026 - 07 - 20),
                        "re-rated",
                        closing(&rerated, driver()),
                    );
                }
            }
            books_hold(&billd).unwrap_or_else(|broken| {
                let story: Vec<String> = billd
                    .documents()
                    .map(|d| {
                        format!(
                            "{} kind={:?} booked={} cancels={:?} replaces={:?} payable={}",
                            d.invoice.number,
                            d.invoice.kind,
                            d.booked,
                            d.cancels,
                            d.replaces,
                            d.invoice.payable_total()
                        )
                    })
                    .collect();
                panic!("seed {seed}, step {step}: {broken}\n{}", story.join("\n"))
            });
        }
    }
}

// ── …and the same four facts, across a restart ──────────────────────────────
//
// Every property above holds inside one process. Each of them is a fact about
// the **past** — this number is spent, this period is closed, this document
// reverses that one, this month is in the books — and a service that held them
// only in memory answered every one of them correctly until it was restarted,
// and then answered them all wrongly at once. `[UStG §14(4) Nr. 4]` is the
// sharp end: a counter that came back at `R-2026-0001` hands a second document
// the number the first one carries, and both documents are individually
// perfect.
//
// `Books` is `Billd` folded from a journal. These run the same decisions, drop
// the process between them, and ask the questions again.

use billd::Books;
use emob_service::Actor;
use emob_service::journal::{File, Journal};

/// Who the decisions below are recorded as: an operator's hand, which is what
/// a closing, a correction or a platform's answer typed in by somebody is.
fn ops() -> Actor {
    Actor::operator("ops").expect("an operator name")
}

/// A journal file of this test's own, removed when it is done.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir()
            .join(format!("emob-billd-{}-{name}", std::process::id()))
            .join("billd.ndjson");
        let _ = std::fs::remove_dir_all(path.parent().expect("a parent"));
        Self(path)
    }

    fn books(&self) -> Books<File> {
        Books::open(
            "emob",
            Numbering::series("R", 2026),
            ChartOfAccounts::new(),
            File::open(&self.0).expect("the journal opens"),
        )
        .expect("the journal folds")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.0.parent().expect("a parent"));
    }
}

#[test]
fn a_number_spent_before_a_restart_is_not_spent_again() {
    let scratch = Scratch::new("numbers");
    let june = month("0.49");

    let first = {
        let mut books = scratch.books();
        books
            .issue(&ops(), "2026-06", closing(&june, driver()))
            .expect("June closes")
    };
    assert_eq!(first, "R-2026-0001");

    // The process is gone. Everything it decided is in one file.
    let mut books = scratch.books();

    // The period is still closed, by the document that closed it.
    let refused = books
        .issue(&ops(), "2026-06", closing(&june, driver()))
        .expect_err("June was closed before the restart");
    assert!(
        matches!(
            refused,
            billd::BooksError::Closing(ref e)
                if matches!(**e, ClosingError::AlreadyClosed { ref number, .. } if number == "R-2026-0001")
        ),
        "{refused:?}"
    );

    // …and the counter resumed where the journal says it was, which is the one
    // thing the statute names.
    let july = month("0.49");
    let next = books
        .issue(&ops(), "2026-07", closing(&july, driver()))
        .expect("July closes");
    assert_eq!(
        next, "R-2026-0002",
        "a counter that restarted at one issues a number a document already carries"
    );
}

#[test]
fn a_correction_survives_a_restart_with_the_reversal_still_first() {
    let scratch = Scratch::new("correction");
    let june = month("0.49");
    let corrected = month("0.39");

    let (original, credit, replacement) = {
        let mut books = scratch.books();
        let original = books
            .issue(&ops(), "2026-06", closing(&june, driver()))
            .expect("June closes");
        let (credit, replacement) = books
            .rebill(
                &ops(),
                &original,
                date!(2026 - 07 - 10),
                "re-rated at the contracted price",
                closing(&corrected, driver()),
            )
            .expect("the month re-bills");
        (original, credit, replacement)
    };

    let books = scratch.books();
    let state = books.state();

    // Both halves came back, and each still knows which half it is.
    assert_eq!(
        state.issued(&credit).and_then(|d| d.cancels.as_deref()),
        Some(original.as_str())
    );
    assert_eq!(
        state
            .issued(&replacement)
            .and_then(|d| d.replaces.as_deref()),
        Some(original.as_str())
    );

    // …and the period is closed by the document that closes it **now**, which
    // is the question D307 exists for and the one a naive fold gets wrong.
    assert_eq!(
        state.closes("2026-06").map(|d| d.invoice.number.as_str()),
        Some(replacement.as_str()),
        "after a restart the period must name the replacement, not the reversed original"
    );
}

#[test]
fn the_books_come_back_balanced_and_a_month_is_not_posted_twice() {
    let scratch = Scratch::new("books");
    let june = month("0.49");

    let number = {
        let mut books = scratch.books();
        let number = books
            .issue(&ops(), "2026-06", closing(&june, driver()))
            .expect("June closes");
        books
            .accepted(
                &ops(),
                &number,
                date!(2026 - 07 - 02),
                Some("BE-1".to_owned()),
            )
            .expect("the recipient took it");
        books.book(&ops(), &number).expect("it reaches the books");
        number
    };

    let books = scratch.books();
    books
        .state()
        .trial_balance()
        .expect("a replayed journal still totals its own postings");

    // The receivable is the month's gross, **once**. A fold that re-posted on
    // replay would double it in a journal every entry of which still balances,
    // which is exactly the failure D298 and D307 are about.
    let receivable = books
        .state()
        .balance_of("assets:receivable", "EUR")
        .expect("the books total")
        .expect("the receivable was opened by the posting");
    let net = receivable.net();
    let amount = net.abs();
    assert!(net.is_positive(), "a debit balance");
    assert_eq!(
        amount.to_string(),
        "31.04",
        "the replay posted the month a second time"
    );
    assert!(
        books.state().issued(&number).is_some_and(|d| d.booked),
        "the replay must remember that this document is in the books"
    );
}

#[test]
fn a_journal_somebody_edited_is_refused_rather_than_folded() {
    let scratch = Scratch::new("tamper");
    let june = month("0.49");
    {
        let mut books = scratch.books();
        books
            .issue(&ops(), "2026-06", closing(&june, driver()))
            .expect("June closes");
    }

    // The amount on the recorded document is changed and the digest is not —
    // which is what an edit made to change an answer looks like.
    let text = std::fs::read_to_string(&scratch.0).expect("read");
    let edited = text.replacen("R-2026-0001", "R-2026-9999", 1);
    assert_ne!(edited, text, "the fixture must actually change the record");
    std::fs::write(&scratch.0, edited).expect("written");

    let refused = Books::open(
        "emob",
        Numbering::series("R", 2026),
        ChartOfAccounts::new(),
        File::open(&scratch.0).expect("opens"),
    )
    .expect_err("a rewritten journal is not a record of what this service decided");
    assert!(refused.to_string().contains("does not chain"), "{refused}");
}

#[test]
fn the_journal_names_what_kind_of_fact_each_record_is() {
    let scratch = Scratch::new("kinds");
    let june = month("0.49");
    let mut books = scratch.books();
    let number = books
        .issue(&ops(), "2026-06", closing(&june, driver()))
        .expect("June closes");
    books
        .accepted(&ops(), &number, date!(2026 - 07 - 02), None)
        .expect("taken");
    books.book(&ops(), &number).expect("booked");

    let kinds: Vec<String> = books
        .journal()
        .records()
        .expect("the chain holds")
        .into_iter()
        .map(|r| r.kind)
        .collect();
    assert_eq!(kinds, ["issued", "submitted", "booked"]);
}

/// 0.2: the calendar closes each partner's month from the records `csmsd`
/// delivered — once, and a closed month is left alone.
mod the_calendar {
    use std::sync::Mutex;

    use billd::Books;
    use billd::daemon::{Estate, close_what_is_due};
    use emob_service::Actor;
    use emob_service::journal::Memory;
    use time::macros::date;

    fn estate() -> Estate {
        emob_service::estate::from_str(include_str!("../../../deploy/estate.example.toml"))
            .expect("the demo estate")
    }

    #[test]
    fn a_partners_month_closes_once_from_what_was_delivered() {
        let estate = estate();
        let partner = estate.partners[0].party.clone();
        let books = Mutex::new(
            Books::open(
                "emob",
                billd::Numbering::series("R", 2026),
                billd::ChartOfAccounts::new(),
                Memory::new(),
            )
            .unwrap(),
        );
        // June's three sessions, delivered as `csmsd` would deliver them.
        for cdr in super::month("0.49").iter() {
            assert!(
                books
                    .lock()
                    .unwrap()
                    .receive(&Actor::service("csmsd"), cdr.clone(), partner.clone(), None)
                    .unwrap()
            );
        }

        // Still June: nothing is due.
        close_what_is_due(&books, &estate, date!(2026 - 06 - 30));
        assert_eq!(books.lock().unwrap().state().documents().count(), 0);

        // July: June closes, with every record on it.
        close_what_is_due(&books, &estate, date!(2026 - 07 - 01));
        let held = books.lock().unwrap();
        let documents: Vec<_> = held.state().documents().collect();
        assert_eq!(documents.len(), 1);
        assert_eq!(documents[0].invoice.records().len(), 3);
        assert_eq!(
            held.state().period_closed_by(&documents[0].invoice.number),
            Some(format!("{partner}/2026-06"))
        );
        drop(held);

        // The next tick in July finds the month closed and leaves it.
        close_what_is_due(&books, &estate, date!(2026 - 07 - 02));
        assert_eq!(books.lock().unwrap().state().documents().count(), 1);
    }
}

/// SR-087: billd's journal is sealed under `BILLD_DATA_KEY`. A received CDR
/// carries what identifies the driver's card, an invoice the debtor's name and
/// IBAN; none of it is on disk in the clear, and a journal written under
/// another key is refused rather than folded.
mod sealed {
    use std::sync::Arc;

    use billd::Books;
    use emob_billing::invoice::PaymentDetails;
    use emob_service::Actor;
    use emob_service::journal::File;
    use emob_service::vault::{DataKey, Sealing};

    use super::{ChartOfAccounts, Numbering, Scratch, cpo, date, dec, driver, month, ops};

    const UID: &str = "04A1B2C3D4E5F6";
    const IBAN: &str = "DE89370400440532013000";
    const RECEIPT: &str = "http://127.0.0.1:9001/r/receipt-token-of-the-driver";

    fn books(scratch: &Scratch, key: &Arc<DataKey>) -> Result<Books<Sealing<File>>, String> {
        Books::open(
            "emob",
            Numbering::series("R", 2026),
            ChartOfAccounts::new(),
            Sealing::new(
                File::open(&scratch.0).expect("the journal opens"),
                Arc::clone(key),
                "billd/journal",
            ),
        )
        .map_err(|error| error.to_string())
    }

    #[test]
    fn sr_087_billds_journal_holds_no_uid_no_debtor_name_and_no_iban() {
        let scratch = Scratch::new("sealed");
        let key = Arc::new(DataKey::from_bytes(&[4; 32]));
        let partner = emob_core::PartyId::new("DE", "MSP").unwrap();
        let june = month("0.49");
        {
            let mut held = books(&scratch, &key).expect("an empty journal folds");
            for cdr in june.iter() {
                let mut cdr = cdr.clone();
                cdr.authorization_reference = Some(UID.to_owned());
                assert!(
                    held.receive(
                        &Actor::service("csmsd"),
                        cdr,
                        partner.clone(),
                        Some(RECEIPT.to_owned())
                    )
                    .unwrap()
                );
            }
            held.issue(&ops(), "2026-06", |number| {
                Ok(emob_billing::InvoiceBuilder::new(
                    number,
                    date!(2026 - 07 - 01),
                    (date!(2026 - 06 - 01), date!(2026 - 06 - 30)),
                    cpo(),
                    driver(),
                )
                .supplied_from("DE", dec("19"))
                .ledger(&june)
                .due_on(date!(2026 - 07 - 15))
                .paid_by(PaymentDetails::DirectDebit {
                    mandate_reference: "MANDATE-1".to_owned(),
                    creditor_identifier: "DE98ZZZ09999999999".to_owned(),
                    debited_iban: IBAN.to_owned(),
                })
                .build()?
                .value)
            })
            .expect("June closes");
        }

        let text = std::fs::read_to_string(&scratch.0).expect("the journal is on disk");
        for secret in [UID, IBAN, "Erika Mustermann", "Nebenweg 7", RECEIPT] {
            assert!(!text.contains(secret), "billd.ndjson holds {secret}");
        }
        // The kinds stay readable: the file says what happened, not to whom.
        assert!(text.contains(r#""received""#) && text.contains(r#""issued""#));

        // Under its own key the books come back whole.
        let back = books(&scratch, &key).expect("the journal folds under its key");
        assert_eq!(back.state().received().count(), 3);
        assert_eq!(back.state().documents().count(), 1);

        // Under another key they do not come back at all.
        let foreign = Arc::new(DataKey::from_bytes(&[5; 32]));
        let refused = books(&scratch, &foreign).expect_err("a foreign key reads nothing");
        assert!(refused.contains("data key"), "{refused}");
    }

    /// The daemon itself: no `BILLD_DATA_KEY`, no billd — naming the variable.
    #[tokio::test]
    async fn sr_087_billd_refuses_to_start_without_its_data_key() {
        let scratch = Scratch::new("no-key");
        let estate: billd::daemon::Estate =
            emob_service::estate::from_str(include_str!("../../../deploy/estate.example.toml"))
                .expect("the demo estate");
        let env: emob_service::estate::Env = Arc::new(|name| match name {
            "EMOB_WEBHOOK_SECRET" => Some("whsec_c2VjcmV0LXNlY3JldC1zZWNyZXQ=".to_owned()),
            "EMOB_OPERATOR_TOKEN" => Some("operator".to_owned()),
            _ => None,
        });
        let refused = billd::daemon::run(billd::daemon::Options {
            http: "127.0.0.1:0".parse().unwrap(),
            journal: scratch.0.clone(),
            estate,
            env,
            shutdown: emob_service::Shutdown::new(),
            identity: emob_service::Identity::new("billd", "test"),
        })
        .await
        .err()
        .expect("billd does not start without a data key");
        assert!(
            format!("{refused:#}").contains(billd::daemon::DATA_KEY),
            "{refused:#}"
        );
    }
}

/// Spec 008: billd's journal signs where it ends, so the one edit the chain
/// cannot see — records cut off the end — stops the daemon (SR-096), and the
/// signed head is an operator's read like every other (SR-103).
mod signed {
    use std::io::{Read as _, Write as _};
    use std::sync::Arc;

    use billd::Books;
    use emob_service::head::{self, Signing, SigningKey};
    use emob_service::journal::File;
    use emob_service::vault::{DataKey, Sealing};

    use super::{ChartOfAccounts, Numbering, Scratch, closing, date, driver, month, ops};

    const SEED: [u8; 32] = [7; 32];

    fn books(scratch: &Scratch) -> Result<Books<billd::daemon::Journal>, String> {
        let sealed = Sealing::new(
            File::open(&scratch.0).expect("the journal opens"),
            Arc::new(DataKey::from_bytes(&[4; 32])),
            "billd/journal",
        );
        let journal = Signing::open(
            sealed,
            SigningKey::from_seed(&SEED, "billd"),
            head::beside(&scratch.0),
        )
        .map_err(|error| error.to_string())?;
        Books::open(
            "emob",
            Numbering::series("R", 2026),
            ChartOfAccounts::new(),
            journal,
        )
        .map_err(|error| error.to_string())
    }

    #[test]
    fn sr_096_a_journal_cut_by_one_record_does_not_start() {
        let scratch = Scratch::new("cut");
        let june = month("0.49");
        {
            let mut books = books(&scratch).expect("an empty journal opens");
            let number = books
                .issue(&ops(), "2026-06", closing(&june, driver()))
                .expect("June closes");
            books
                .accepted(&ops(), &number, date!(2026 - 07 - 02), None)
                .expect("taken");
            books.book(&ops(), &number).expect("booked");
        }
        drop(books(&scratch).expect("a whole journal opens again"));

        // The last record goes; every record left still chains.
        let text = std::fs::read_to_string(&scratch.0).expect("read");
        let kept: Vec<&str> = text.lines().collect();
        assert_eq!(kept.len(), 3);
        std::fs::write(&scratch.0, format!("{}\n", kept[..2].join("\n"))).expect("cut");

        let refused = books(&scratch).expect_err("a cut journal is not the history billd signed");
        assert!(
            refused.contains("holds 2 records") && refused.contains("signed a head at 3"),
            "{refused}"
        );
    }

    fn get(addr: std::net::SocketAddr, path: &str, token: Option<&str>) -> String {
        let mut stream = std::net::TcpStream::connect(addr).expect("connects");
        let auth = token.map_or_else(String::new, |t| format!("authorization: Bearer {t}\r\n"));
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nhost: billd\r\n{auth}connection: close\r\n\r\n"
        )
        .expect("sent");
        let mut answer = String::new();
        stream.read_to_string(&mut answer).expect("answered");
        answer
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sr_103_the_signed_head_is_an_operators_read() {
        let scratch = Scratch::new("head");
        let estate: billd::daemon::Estate =
            emob_service::estate::from_str(include_str!("../../../deploy/estate.example.toml"))
                .expect("the demo estate");
        let data = DataKey::generate();
        let signing = SigningKey::generate();
        let env: emob_service::estate::Env = Arc::new(move |name| match name {
            "EMOB_WEBHOOK_SECRET" => Some("whsec_c2VjcmV0LXNlY3JldC1zZWNyZXQ=".to_owned()),
            "EMOB_OPERATOR_TOKEN" => Some("operator-token-of-the-test".to_owned()),
            billd::daemon::DATA_KEY => Some(data.clone()),
            "BILLD_SIGNING_KEY" => Some(signing.clone()),
            _ => None,
        });
        let running = billd::daemon::run(billd::daemon::Options {
            http: "127.0.0.1:0".parse().unwrap(),
            journal: scratch.0.clone(),
            estate,
            env,
            shutdown: emob_service::Shutdown::new(),
            identity: emob_service::Identity::new("billd", "test"),
        })
        .await
        .expect("billd starts with its keys");
        let addr = running.http;

        let (anonymous, operator) = tokio::task::spawn_blocking(move || {
            (
                get(addr, "/v1/billd/head", None),
                get(addr, "/v1/billd/head", Some("operator-token-of-the-test")),
            )
        })
        .await
        .expect("asked");
        running.kill();

        assert!(anonymous.starts_with("HTTP/1.1 401"), "{anonymous}");
        assert!(operator.starts_with("HTTP/1.1 200"), "{operator}");
        assert!(operator.contains("emob/billd\n0\n"), "{operator}");
    }
}

/// Spec 010: the invoice reaches its recipient — the page, its files and the
/// signed handover to the operator system (SR-116..SR-122).
mod an_invoice_reaches_its_recipient {
    use std::sync::{Arc, Mutex};

    use billd::Books;
    use billd::daemon::{Estate, close_what_is_due};
    use billd::deliver::Dispatcher;
    use billd::invoice_page::{self, Invoices};
    use emob_billing::Syntax;
    use emob_service::Actor;
    use emob_service::estate::{InvoiceContact, InvoiceSpecification, InvoiceSyntax, MandateSeed};
    use emob_service::journal::Memory;
    use emob_service::webhook;
    use time::macros::date;

    const DEBTOR_IBAN: &str = "DE02120300000000202051";
    const SECRET: &str = "whsec_c2VjcmV0LXNlY3JldC1zZWNyZXQ=";

    /// The example estate, with its partner on XRechnung and a mandate.
    fn estate(xrechnung_complete: bool) -> Estate {
        let mut estate: Estate =
            emob_service::estate::from_str(include_str!("../../../deploy/estate.example.toml"))
                .expect("the example estate");
        estate.billing.seller.electronic_address = Some("rechnung@abc.example".to_owned());
        estate.billing.contact = Some(InvoiceContact {
            name: "Rechnungswesen".to_owned(),
            phone: "+49 30 1234567".to_owned(),
            email: "rechnung@abc.example".to_owned(),
        });
        estate.billing.creditor_id = Some("DE98ZZZ09999999999".to_owned());
        let partner = &mut estate.partners[0];
        partner.specification = InvoiceSpecification::Xrechnung;
        partner.billing.electronic_address = Some("ap@msp.example".to_owned());
        partner.mandate = Some(MandateSeed {
            reference: "MSP-2026-01".to_owned(),
            iban: DEBTOR_IBAN.to_owned(),
        });
        if xrechnung_complete {
            partner.buyer_reference = Some("MSP-AP-4711".to_owned());
        }
        estate
    }

    /// June delivered with receipt links, and closed on 1 July.
    fn closed(estate: &Estate) -> Arc<Mutex<Books<Memory>>> {
        closed_for(estate, &[])
    }

    fn closed_for(estate: &Estate, subscribers: &[&str]) -> Arc<Mutex<Books<Memory>>> {
        let books = Arc::new(Mutex::new(
            Books::open(
                "emob",
                billd::Numbering::series("R", 2026),
                billd::ChartOfAccounts::new(),
                Memory::new(),
            )
            .unwrap()
            .delivering_to(subscribers.iter().map(|s| (*s).to_owned()).collect()),
        ));
        let partner = estate.partners[0].party.clone();
        for (n, cdr) in super::month("0.49").iter().enumerate() {
            books
                .lock()
                .unwrap()
                .receive(
                    &Actor::service("csmsd"),
                    cdr.clone(),
                    partner.clone(),
                    Some(format!("https://csms.example/r/receipt-{n}")),
                )
                .unwrap();
        }
        close_what_is_due(&books, estate, date!(2026 - 07 - 01));
        books
    }

    async fn serve(router: axum::Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        format!("http://{addr}")
    }

    fn valid(xml: &str) -> bool {
        let read = en16931_formats::read(xml.as_bytes()).expect("a document en16931 reads");
        en16931::validate(&read.invoice).is_valid()
    }

    #[test]
    fn sr_122_a_month_its_partners_specification_rejects_is_not_issued() {
        // XRechnung needs a buyer reference (BR-DE-15); without one the
        // closing is refused while the document is only a spent number.
        let incomplete = estate(false);
        let books = closed(&incomplete);
        assert_eq!(books.lock().unwrap().state().documents().count(), 0);
        let closing = billd::daemon::closing(
            &incomplete,
            &incomplete.partners[0],
            2026,
            time::Month::June,
            date!(2026 - 07 - 01),
        );
        let refused = books
            .lock()
            .unwrap()
            .close(&Actor::Timer, &closing)
            .expect_err("XRechnung without BT-10");
        assert!(refused.to_string().contains("BR-DE-15"), "{refused}");
        let books = closed(&estate(true));
        let state = books.lock().unwrap();
        let issued = state.state().documents().next().expect("June is closed");
        assert_eq!(issued.specification, emob_billing::Specification::XRechnung);
    }

    #[tokio::test]
    async fn sr_116_sr_117_the_page_shows_the_bill_and_nothing_more() {
        let estate = estate(true);
        let books = closed(&estate);
        let invoices = Arc::new(Invoices::ephemeral(None).unwrap());
        let number = books
            .lock()
            .unwrap()
            .state()
            .documents()
            .next()
            .unwrap()
            .invoice
            .number
            .clone();
        let base = serve(invoice_page::router(
            Arc::clone(&books),
            Arc::clone(&invoices),
        ))
        .await;
        let client = reqwest::Client::new();
        let path = invoices.path(&number);

        let page = client.get(format!("{base}{path}")).send().await.unwrap();
        assert_eq!(page.status(), 200);
        let headers = page.headers().clone();
        assert_eq!(headers["x-robots-tag"], "noindex, nofollow");
        assert_eq!(headers["cache-control"], "no-store");
        assert_eq!(headers["referrer-policy"], "no-referrer");
        let html = page.text().await.unwrap();
        assert!(html.len() < 30_000, "{} bytes", html.len());
        assert!(
            html.contains(&number) && html.contains("fällig am"),
            "German first"
        );
        assert!(html.contains("SEPA-Lastschrift") && html.contains("DE…2051"));
        assert!(!html.contains(DEBTOR_IBAN), "the debtor IBAN is masked");
        for n in 0..3 {
            assert!(html.contains(&format!("https://csms.example/r/receipt-{n}")));
        }
        assert!(html.contains("XRechnung 3.0 · UBL") && !html.contains("<script"));
        let english = client
            .get(format!("{base}{path}?lang=en"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(english.contains("due on"));

        // Both files are XRechnung that en16931 reads back and validates.
        for (file, syntax) in [("ubl.xml", Syntax::Ubl), ("cii.xml", Syntax::Cii)] {
            let answer = client
                .get(format!("{base}{path}/{file}"))
                .send()
                .await
                .unwrap();
            assert_eq!(answer.status(), 200);
            assert!(
                answer.headers()["content-disposition"]
                    .to_str()
                    .unwrap()
                    .contains("xrechnung")
            );
            let xml = answer.text().await.unwrap();
            assert!(valid(&xml), "{file}");
            let state = books.lock().unwrap();
            let issued = state.state().issued(&number).unwrap();
            assert_eq!(xml, invoice_page::file(issued, syntax).unwrap());
        }

        // An unknown or malformed token is the router's own empty 404.
        let unrouted = client.get(format!("{base}/nowhere")).send().await.unwrap();
        for wrong in [
            "/i/AAAAAAAAAAAAAAAAAAAAAA",
            "/i/short",
            "/i/AAAAAAAAAAAAAAAAAAAAAA/ubl.xml",
        ] {
            let answer = client.get(format!("{base}{wrong}")).send().await.unwrap();
            assert_eq!(answer.status(), 404, "{wrong}");
            assert_eq!(answer.headers().len(), unrouted.headers().len(), "{wrong}");
            assert!(answer.text().await.unwrap().is_empty(), "{wrong}");
        }
    }

    #[tokio::test]
    async fn sr_118_a_flood_is_answered_429() {
        let books = closed(&estate(true));
        let invoices = Arc::new(Invoices::ephemeral(None).unwrap());
        let base = serve(invoice_page::router_limited(books, invoices, 2)).await;
        let client = reqwest::Client::new();
        let mut statuses = Vec::new();
        for _ in 0..3 {
            let answer = client
                .get(format!("{base}/i/AAAAAAAAAAAAAAAAAAAAAA"))
                .send()
                .await
                .unwrap();
            statuses.push(answer.status().as_u16());
        }
        assert_eq!(statuses.last(), Some(&429), "{statuses:?}");
    }

    /// A receiver answering from a script, keeping what it was sent.
    #[derive(Clone, Default)]
    struct Receiver {
        answers: Arc<Mutex<Vec<(u16, String)>>>,
        seen: Arc<Mutex<Vec<(String, String)>>>,
    }

    async fn receiver(answers: Vec<(u16, &str)>) -> (String, Receiver) {
        let held = Receiver {
            answers: Arc::new(Mutex::new(
                answers
                    .into_iter()
                    .rev()
                    .map(|(s, b)| (s, b.to_owned()))
                    .collect(),
            )),
            ..Receiver::default()
        };
        let state = held.clone();
        let router = axum::Router::new().route(
            "/in",
            axum::routing::post(
                move |headers: axum::http::HeaderMap, body: axum::body::Bytes| {
                    let state = state.clone();
                    async move {
                        let secret = webhook::Secret::standard(SECRET).unwrap();
                        let id = webhook::receive(
                            &headers,
                            &body,
                            &[secret],
                            time::OffsetDateTime::now_utc(),
                            time::Duration::minutes(5),
                        )
                        .expect("signed with the estate's secret");
                        state
                            .seen
                            .lock()
                            .unwrap()
                            .push((id, String::from_utf8(body.to_vec()).unwrap()));
                        let (status, text) = state
                            .answers
                            .lock()
                            .unwrap()
                            .pop()
                            .unwrap_or((200, String::new()));
                        (axum::http::StatusCode::from_u16(status).unwrap(), text)
                    }
                },
            ),
        );
        (format!("{}/in", serve(router).await), held)
    }

    fn dispatcher(url: &str) -> Dispatcher {
        Dispatcher::new(
            url,
            webhook::Secret::standard(SECRET).unwrap(),
            InvoiceSyntax::Ubl,
            std::time::Duration::from_secs(5),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn sr_119_sr_120_a_retry_then_an_acceptance_books_the_invoice_once() {
        let books = closed(&estate(true));
        let invoices = Invoices::ephemeral(None).unwrap();
        let (url, held) = receiver(vec![
            (503, "starting"),
            (200, r#"{"reference":"PEPPOL-1"}"#),
        ])
        .await;
        let dispatcher = dispatcher(&url);

        assert_eq!(
            dispatcher.drain(&books, &invoices).await,
            0,
            "a 503 records nothing"
        );
        assert_eq!(dispatcher.drain(&books, &invoices).await, 1);
        assert_eq!(
            dispatcher.drain(&books, &invoices).await,
            0,
            "nothing is pending"
        );
        let seen = held.seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 2, "the third drain sent nothing");

        let state = books.lock().unwrap();
        let issued = state.state().documents().next().unwrap();
        assert_eq!(seen[1].0, issued.invoice.number, "webhook-id is the number");
        let event: emob_service::cloudevent::CloudEvent<String> = emob_service::cloudevent::open(
            seen[1].1.as_bytes(),
            emob_service::events::invoice::ISSUED,
        )
        .expect("a de.emob.invoice.issued CloudEvent");
        assert_eq!(
            event.data,
            invoice_page::file(issued, Syntax::Ubl).unwrap(),
            "the file is the data"
        );
        assert_eq!(event.datacontenttype, "application/xml");
        assert_eq!(
            event.subject.as_deref(),
            Some(format!("invoice/{}", issued.invoice.number).as_str())
        );
        assert!(matches!(
            &issued.submission,
            billd::Submission::Accepted { reference: Some(r), .. } if r == "PEPPOL-1"
        ));
        assert!(issued.booked, "accepted, then booked");
    }

    fn subscribers(url: &str) -> billd::deliver::Subscribers {
        billd::deliver::Subscribers::new(
            vec![emob_service::estate::Subscription {
                name: "dwh".to_owned(),
                url: url.to_owned(),
                secret: webhook::Secret::standard(SECRET).unwrap(),
                types: vec!["de.emob.invoice.*".to_owned()],
            }],
            InvoiceSyntax::Ubl,
            std::time::Duration::from_secs(5),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn a_subscriber_receives_each_invoice_event_once_and_decides_nothing() {
        let books = closed_for(&estate(true), &["dwh"]);
        let invoices = Invoices::ephemeral(None).unwrap();
        let (url, held) = receiver(vec![(503, "starting"), (200, "")]).await;
        let subscribers = subscribers(&url);

        assert_eq!(
            subscribers.drain(&books, &invoices).await,
            0,
            "a 503 records nothing"
        );
        assert_eq!(subscribers.drain(&books, &invoices).await, 1);
        assert_eq!(
            subscribers.drain(&books, &invoices).await,
            0,
            "nothing is owed"
        );
        let seen = held.seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 2);

        let state = books.lock().unwrap();
        let issued = state.state().documents().next().unwrap();
        let event: emob_service::cloudevent::CloudEvent<String> = emob_service::cloudevent::open(
            seen[1].1.as_bytes(),
            emob_service::events::invoice::ISSUED,
        )
        .unwrap();
        assert_eq!(event.source, "urn:emob:billd");
        assert_eq!(event.id, issued.invoice.number);
        assert_eq!(issued.answered.get("dwh"), Some(&None), "taken");
        // Spec 012: the published schema describes what was sent.
        let schema: serde_json::Value = serde_json::from_str(include_str!(
            "../../../schemas/events/de.emob.invoice.issued.json"
        ))
        .unwrap();
        let sent: serde_json::Value = serde_json::from_str(&seen[1].1).unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        let errors: Vec<String> = validator
            .iter_errors(&sent)
            .map(|e| e.to_string())
            .collect();
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(
            issued.submission,
            billd::Submission::Issued,
            "only the operator system accepts"
        );
        assert!(!issued.booked);
    }

    #[tokio::test]
    async fn a_subscribers_refusal_is_recorded_once_and_not_retried() {
        let books = closed_for(&estate(true), &["dwh"]);
        let invoices = Invoices::ephemeral(None).unwrap();
        let (url, held) = receiver(vec![(422, "unknown schema")]).await;
        let subscribers = subscribers(&url);
        assert_eq!(subscribers.drain(&books, &invoices).await, 1);
        assert_eq!(subscribers.drain(&books, &invoices).await, 0);
        assert_eq!(held.seen.lock().unwrap().len(), 1);
        let state = books.lock().unwrap();
        let issued = state.state().documents().next().unwrap();
        assert!(
            matches!(issued.answered.get("dwh"), Some(Some(reason)) if reason.contains("unknown schema")),
            "{:?}",
            issued.answered
        );
    }

    #[tokio::test]
    async fn a_document_issued_before_a_subscriber_existed_is_not_owed_to_it() {
        let books = closed(&estate(true));
        let invoices = Invoices::ephemeral(None).unwrap();
        let (url, held) = receiver(vec![]).await;
        assert_eq!(subscribers(&url).drain(&books, &invoices).await, 0);
        assert!(held.seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn sr_120_a_final_refusal_is_recorded_and_not_booked() {
        let books = closed(&estate(true));
        let invoices = Invoices::ephemeral(None).unwrap();
        let (url, _held) = receiver(vec![(422, "BR-DE-15 missing")]).await;
        assert_eq!(dispatcher(&url).drain(&books, &invoices).await, 1);
        let state = books.lock().unwrap();
        let issued = state.state().documents().next().unwrap();
        assert!(matches!(
            &issued.submission,
            billd::Submission::Rejected { reason, .. } if reason.contains("BR-DE-15")
        ));
        assert!(!issued.booked);
    }

    #[test]
    fn sr_121_a_remote_plain_http_target_is_refused() {
        let refused = Dispatcher::new(
            "http://erp.example/in",
            webhook::Secret::standard(SECRET).unwrap(),
            InvoiceSyntax::Ubl,
            std::time::Duration::from_secs(5),
        );
        assert!(refused.is_err());
    }
}
