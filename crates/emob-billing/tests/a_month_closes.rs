//! A month of charging, closed: rated records → invoice → EN 16931 → SEPA →
//! balanced books.
//!
//! The demo the milestone is phrased as, and the property it turns on is not
//! that the code runs. It is that **one number survives four documents**: the
//! kilowatt-hours a tariff priced are the kilowatt-hours the invoice bills, the
//! invoice's own subtotals reproduce its lines, the collection draws the
//! invoice's total to the cent, and the books balance.
//!
//! Every fixture here is signed for real — the OCMF records are produced with a
//! private key at test time and verified through the same path a station would
//! go through — so the chain is exercised from the meter, not from a constant.

use emob_billing::{
    Counterparty, InvoiceBuilder, TaxStatus, VatCategory, en16931, payment, postings,
};
use emob_cdr::{CdrBuilder, CdrLedger, EvidenceRef};
use emob_core::{Currency, Direction, Energy, PartyId};
use emob_eichrecht::registry::{ComponentRef, RegisteredKey};
use emob_eichrecht::{Evidence, KeyRegistry};
use emob_session::{
    Authorization, EndReason, MeterReading, MeterSeries, ReadingContext, Session, SessionState,
};
use emob_tariff::{Dimension, PriceComponent, Tariff, TariffKind, TaxIncluded};
use ocmf::Curve;
use ocmf::PublicKey;
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

/// One session on day `n`, half an hour long, delivering `energy` kWh.
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

/// A gross ad-hoc tariff at 19 %, the ordinary German shape.
fn tariff() -> Tariff {
    Tariff::simple(
        "ad-hoc-2026".parse().unwrap(),
        Currency::EUR,
        TariffKind::AdHoc,
        emob_core::TimeZone::new("Europe/Berlin").unwrap(),
        vec![PriceComponent::new(Dimension::Energy, dec("0.49")).with_vat(dec("19"))],
    )
}

/// Three days of charging, in a ledger.
fn month() -> CdrLedger {
    let mut ledger = CdrLedger::new();
    let party = PartyId::new("DE", "ABC").unwrap();
    let tariff = tariff();

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
    // BT-30. The only way `BR-CO-26` can identify a seller on the one document
    // that may carry no VAT identifier — see the outside-scope test.
    .registered_as("HRB 12345", None)
    // BR-DE-2 and BR-DE-5..7: a German public buyer wants somebody to ask.
    .contactable("Rechnungswesen", "+49 555 123456", "rechnung@example.org")
}

#[test]
fn a_driver_month_closes_from_the_meter_to_the_books() {
    let ledger = month();
    let driver = Counterparty::new(
        "Erika Mustermann",
        "Beispielstadt",
        TaxStatus::consumer("DE"),
    )
    .at("Nebenweg 7", "54321");

    let crossing = InvoiceBuilder::new(
        "R-2026-0001",
        date!(2026 - 07 - 01),
        (date!(2026 - 06 - 01), date!(2026 - 06 - 30)),
        cpo(),
        driver,
    )
    .supplied_from("DE", dec("19"))
    .ledger(&ledger)
    .due_on(date!(2026 - 07 - 15))
    .build()
    .unwrap();
    let residual_note = crossing
        .reasons()
        .any(|reason| reason.contains("/totals/taxable"));
    let invoice = crossing.value;

    // ── The energy survives the whole chain ─────────────────────────────────
    // 29.500 + 20.500 + 13.333 kWh, priced at 0.49 gross, three lines.
    assert_eq!(invoice.lines.len(), 3, "{:?}", invoice.lines);
    let billed: Decimal = invoice.lines.iter().map(|line| line.quantity).sum();
    assert_eq!(billed, dec("63.333"), "the kilowatt-hours the meter signed");
    for line in &invoice.lines {
        assert_eq!(line.unit_code(), "KWH");
        // BT-146 is the item price **excluding VAT**, so a tariff quoting 0.49
        // gross at 19 % states 0.49 ÷ 1.19 here — and the line reproduces its
        // own amount from it, which is what `PEPPOL-EN16931-R120` asks.
        assert_eq!(line.unit_price, dec("0.49") / dec("1.19"));
        assert!(line.reconciles());
    }

    // ── …and the document adds up ───────────────────────────────────────────
    // Each line's net is its gross over 1.19, rounded to the cent:
    //   29.500 × 0.49 = 14.45500 gross → 12.15 net
    //   20.500 × 0.49 = 10.04500 gross →  8.44 net
    //   13.333 × 0.49 =  6.53317 gross →  5.49 net
    assert_eq!(
        invoice.lines.iter().map(|l| l.net).collect::<Vec<_>>(),
        vec![dec("12.15"), dec("8.44"), dec("5.49")]
    );
    assert_eq!(invoice.taxable_total().to_string(), "26.08 EUR");
    assert_eq!(invoice.tax_total().to_string(), "4.96 EUR");
    assert_eq!(invoice.gross_total().to_string(), "31.04 EUR");
    assert!(invoice.reconciles(), "the subtotals reproduce the lines");

    // ── The rounding is stated, not absorbed ────────────────────────────────
    // The lines came to 26.0800840336… exactly; the document says 26.08. That
    // is the whole of what it approximated, and it says so.
    assert!(!invoice.rounding_residual().is_zero());
    assert!(
        residual_note,
        "the document has to say what it approximated"
    );
    assert_eq!(invoice.records().len(), 3);

    // ── The European e-invoice, and the verdict on it ───────────────────────
    let crossed = en16931::to_en16931(&invoice, en16931::Specification::Core).unwrap();
    assert!(
        crossed.value.is_valid(),
        "{:?}",
        crossed.value.reasons().collect::<Vec<_>>()
    );
    assert_eq!(crossed.value.invoice.lines.len(), 3);
    assert_eq!(
        crossed.value.invoice.totals.gross_total.to_string(),
        "31.04",
        "the standard's own totals are this invoice's totals"
    );
    assert_eq!(crossed.value.invoice.vat_breakdown.len(), 1);
    assert_eq!(
        crossed.value.invoice.vat_breakdown[0].category.as_str(),
        "S"
    );

    // ── The collection draws exactly what the invoice asks ──────────────────
    let creditor = payment::Creditor {
        name: "Stadtwerke Musterstadt GmbH".into(),
        iban: sepa::validate_iban("DE89370400440532013000").unwrap(),
        bic: None,
        creditor_id: sepa::validate_creditor_id("DE98ZZZ09999999999").unwrap(),
    };
    let mandate = payment::Mandate {
        reference: "MND-2026-0001".into(),
        signed_on: sepa::IsoDate::new(2026, 1, 15).unwrap(),
        debtor_name: "Erika Mustermann".into(),
        debtor_iban: sepa::validate_iban("DE02120300000000202051").unwrap(),
    };
    let collection = payment::instruct(
        &invoice,
        &creditor,
        &mandate,
        sepa::IsoDate::new(2026, 7, 15).unwrap(),
        "2026-07-01T09:00:00".parse().unwrap(),
    )
    .unwrap();
    assert_eq!(collection.amount_minor, 3104, "31.04 EUR, to the cent");
    assert!(collection.xml.contains("MND-2026-0001"));
    assert!(collection.xml.contains("R-2026-0001"));

    // …and it reads no clock, so two runs of one billing job are one file.
    let again = payment::instruct(
        &invoice,
        &creditor,
        &mandate,
        sepa::IsoDate::new(2026, 7, 15).unwrap(),
        "2026-07-01T09:00:00".parse().unwrap(),
    )
    .unwrap();
    assert_eq!(collection.xml, again.xml, "a replay is the same bytes");

    // ── …and the books balance before an account is named ───────────────────
    let books = postings::postings_for(&invoice);
    assert!(books.balances());
    assert_eq!(books.debits().to_string(), "31.04 EUR");
    assert_eq!(
        books.roles(),
        vec![
            &postings::Role::Receivable,
            &postings::Role::EnergyRevenue,
            &postings::Role::VatPayable { rate: dec("19") },
        ],
        "a driver invoice moves a receivable, revenue and the VAT it owes"
    );
}

#[test]
fn a_roaming_settlement_is_a_reverse_charge_and_the_books_show_no_vat() {
    // The same three sessions, sold to an e-mobility provider in France rather
    // than to a driver. `[UStG §3g]` moves the place of supply to where the
    // reseller is established, so German VAT does not arise — and the books must
    // agree with the document, which is the half every platform gets wrong.
    let ledger = month();
    let emsp = Counterparty::new(
        "Mobilité SAS",
        "Lyon",
        TaxStatus::reseller("FR", "FR12345678901"),
    )
    .at("2 rue de la Charge", "69001");

    let invoice = InvoiceBuilder::new(
        "R-2026-0002",
        date!(2026 - 07 - 01),
        (date!(2026 - 06 - 01), date!(2026 - 06 - 30)),
        cpo(),
        emsp,
    )
    .supplied_from("DE", dec("19"))
    .ledger(&ledger)
    .payment_terms("30 days net")
    .build()
    .unwrap()
    .value;

    assert_eq!(invoice.treatment.category, VatCategory::ReverseCharge);
    assert_eq!(invoice.treatment.place_of_supply, "FR");
    assert_eq!(invoice.tax_total().to_string(), "0.00 EUR");
    // The taxable amount is the same 26.08: what moved is who declares the tax.
    assert_eq!(invoice.taxable_total().to_string(), "26.08 EUR");
    assert_eq!(invoice.gross_total().to_string(), "26.08 EUR");

    let crossed = en16931::to_en16931(&invoice, en16931::Specification::Core).unwrap();
    assert!(
        crossed.value.is_valid(),
        "{:?}",
        crossed.value.reasons().collect::<Vec<_>>()
    );
    // BR-AE-* wants the reason and both identifiers, and they are there because
    // `TaxTreatment` refused to produce the category without them.
    let breakdown = &crossed.value.invoice.vat_breakdown[0];
    assert_eq!(breakdown.category.as_str(), "AE");
    assert!(
        breakdown
            .exemption_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("[UStG §3g]")),
        "{breakdown:?}"
    );

    // No VAT posting: the liability is the recipient's.
    let books = postings::postings_for(&invoice);
    assert!(books.balances());
    assert_eq!(
        books.roles(),
        vec![&postings::Role::Receivable, &postings::Role::EnergyRevenue],
        "a reverse charge moves no VAT account of ours"
    );
}

#[test]
fn a_corrected_session_is_billed_once() {
    // The failure `CdrLedger::live` exists to prevent, met one layer up: a
    // correction is a *new* record, so a ledger holding a session and its
    // correction holds both, and an invoice run that sums everything bills that
    // session twice.
    let mut ledger = month();
    let party = PartyId::new("DE", "ABC").unwrap();
    let (session, evidence) = session(0, "s-1", "100.000", "129.500");
    let correction = CdrBuilder::from_session(&session, Direction::Import)
        .unwrap()
        .key(party, "cdr-s-1-corrected".parse().unwrap())
        .evidence(EvidenceRef::from_evidence(&evidence, "OCMF"))
        .rated_with(&tariff())
        .supersedes(emob_cdr::CdrKey {
            party: PartyId::new("DE", "ABC").unwrap(),
            id: "cdr-s-1".parse().unwrap(),
        })
        .build()
        .unwrap();
    assert!(ledger.accept(correction).is_stored());
    assert_eq!(ledger.len(), 4, "the original is still held");

    let invoice = InvoiceBuilder::new(
        "R-2026-0003",
        date!(2026 - 07 - 01),
        (date!(2026 - 06 - 01), date!(2026 - 06 - 30)),
        cpo(),
        Counterparty::new(
            "Erika Mustermann",
            "Beispielstadt",
            TaxStatus::consumer("DE"),
        ),
    )
    .supplied_from("DE", dec("19"))
    .ledger(&ledger)
    .due_on(date!(2026 - 07 - 15))
    .build()
    .unwrap()
    .value;

    assert_eq!(invoice.lines.len(), 3, "three sessions, not four");
    assert_eq!(invoice.gross_total().to_string(), "31.04 EUR");

    // …and a caller that assembled the list by hand gets the same answer.
    let all: Vec<_> = ledger.iter().collect();
    let mut builder = InvoiceBuilder::new(
        "R-2026-0004",
        date!(2026 - 07 - 01),
        (date!(2026 - 06 - 01), date!(2026 - 06 - 30)),
        cpo(),
        Counterparty::new(
            "Erika Mustermann",
            "Beispielstadt",
            TaxStatus::consumer("DE"),
        ),
    );
    for cdr in all {
        builder = builder.record(cdr);
    }
    let err = builder.build().unwrap_err();
    assert!(
        matches!(err, emob_billing::BillingError::SupersededRecord { .. }),
        "{err}"
    );
}

#[test]
fn an_unrated_record_has_no_invoice_line() {
    // A CDR that was never priced has no amount, and inventing one here would
    // put a second pricing engine in the workspace.
    let party = PartyId::new("DE", "ABC").unwrap();
    let (session, evidence) = session(0, "s-1", "100.000", "129.500");
    let unrated = CdrBuilder::from_session(&session, Direction::Import)
        .unwrap()
        .key(party, "cdr-unrated".parse().unwrap())
        .evidence(EvidenceRef::from_evidence(&evidence, "OCMF"))
        .build()
        .unwrap();

    let err = InvoiceBuilder::new(
        "R-2026-0005",
        date!(2026 - 07 - 01),
        (date!(2026 - 06 - 01), date!(2026 - 06 - 30)),
        cpo(),
        Counterparty::new(
            "Erika Mustermann",
            "Beispielstadt",
            TaxStatus::consumer("DE"),
        ),
    )
    .supplied_from("DE", dec("19"))
    .record(&unrated)
    .build()
    .unwrap_err();
    assert!(
        matches!(err, emob_billing::BillingError::NotRated { .. }),
        "{err}"
    );
}

#[test]
fn a_net_tariff_needs_no_conversion_and_says_nothing_about_rounding() {
    // The other basis. A tariff quoted net states the taxable amount directly,
    // so the line amounts are exact and the document approximates nothing —
    // which is worth asserting, because a residual that is *always* reported is
    // a note nobody reads.
    let mut net = tariff();
    net.tax_included = TaxIncluded::No;
    net.elements[0].components[0].price = dec("0.40");

    let mut ledger = CdrLedger::new();
    let party = PartyId::new("DE", "ABC").unwrap();
    let (session, evidence) = session(0, "s-1", "100.000", "129.500");
    let cdr = CdrBuilder::from_session(&session, Direction::Import)
        .unwrap()
        .key(party, "cdr-net".parse().unwrap())
        .evidence(EvidenceRef::from_evidence(&evidence, "OCMF"))
        .rated_with(&net)
        .build()
        .unwrap();
    assert!(ledger.accept(cdr).is_stored());

    let crossing = InvoiceBuilder::new(
        "R-2026-0006",
        date!(2026 - 07 - 01),
        (date!(2026 - 06 - 01), date!(2026 - 06 - 30)),
        cpo(),
        Counterparty::new(
            "Erika Mustermann",
            "Beispielstadt",
            TaxStatus::consumer("DE"),
        ),
    )
    .supplied_from("DE", dec("19"))
    .ledger(&ledger)
    .due_on(date!(2026 - 07 - 15))
    .build()
    .unwrap();

    // 29.5 × 0.40 = 11.80 exactly, so nothing was rounded and nothing is said.
    assert_eq!(crossing.value.taxable_total().to_string(), "11.80 EUR");
    assert!(crossing.value.rounding_residual().is_zero());
    assert!(
        crossing.is_lossless(),
        "an exact document has no account to give: {:?}",
        crossing.notes()
    );
    assert_eq!(crossing.value.tax_total().to_string(), "2.24 EUR");
    assert_eq!(crossing.value.gross_total().to_string(), "14.04 EUR");
}

#[test]
fn an_invoice_that_owes_something_has_to_say_when() {
    // `BR-CO-25`, asked where the answer lives. Left to the validator this is a
    // rule id against a finished document; asked here it names the two methods
    // that fix it.
    let ledger = month();
    let err = InvoiceBuilder::new(
        "R-2026-0007",
        date!(2026 - 07 - 01),
        (date!(2026 - 06 - 01), date!(2026 - 06 - 30)),
        cpo(),
        Counterparty::new(
            "Erika Mustermann",
            "Beispielstadt",
            TaxStatus::consumer("DE"),
        ),
    )
    .supplied_from("DE", dec("19"))
    .ledger(&ledger)
    .build()
    .unwrap_err();
    assert!(
        matches!(err, emob_billing::BillingError::NoDueDate { .. }),
        "{err}"
    );
    assert!(err.to_string().contains("BR-CO-25"), "{err}");
}

#[test]
fn the_german_public_buyers_document_is_produced_or_refused_with_its_reasons() {
    // XRechnung 3.0 asks for more than the CEN core does — a buyer reference, a
    // seller contact, an electronic address on both parties — and a document
    // that does not carry them is one the German reference validator rejects.
    // `Validated<XRechnung>` cannot be constructed from it, so the XML is not
    // produced at all and the findings name the terms.
    let ledger = month();
    let bare = InvoiceBuilder::new(
        "R-2026-0008",
        date!(2026 - 07 - 01),
        (date!(2026 - 06 - 01), date!(2026 - 06 - 30)),
        cpo(),
        Counterparty::new(
            "Erika Mustermann",
            "Beispielstadt",
            TaxStatus::consumer("DE"),
        ),
    )
    .supplied_from("DE", dec("19"))
    .ledger(&ledger)
    .due_on(date!(2026 - 07 - 15))
    .build()
    .unwrap()
    .value;

    match en16931::write(
        &bare,
        en16931::Specification::XRechnung,
        en16931::Syntax::Ubl,
    ) {
        Err(emob_billing::BillingError::NotCollectable { reason }) => {
            assert!(reason.contains("XRechnung 3.0"), "{reason}");
            assert!(reason.contains("BR-DE-"), "{reason}");
        }
        Err(other) => panic!("{other}"),
        Ok(_) => panic!("a document missing BR-DE terms must not be produced"),
    }

    // The CEN core is what a roaming partner asks for, and that one is valid.
    let crossed = en16931::to_en16931(&bare, en16931::Specification::Core).unwrap();
    assert!(
        crossed.value.is_valid(),
        "{:?}",
        crossed.value.reasons().collect::<Vec<_>>()
    );

    // …and with the terms the German profile asks for, the document is
    // produced. This is the one that goes to a Rechnungseingangsplattform.
    let complete = InvoiceBuilder::new(
        "R-2026-0009",
        date!(2026 - 07 - 01),
        (date!(2026 - 06 - 01), date!(2026 - 06 - 30)),
        cpo(),
        Counterparty::new(
            "Stadt Beispielstadt",
            "Beispielstadt",
            TaxStatus::consumer("DE"),
        )
        .at("Rathausplatz 1", "54321")
        .reachable_at("991-01234-56", "0204"),
    )
    .supplied_from("DE", dec("19"))
    .ledger(&ledger)
    .due_on(date!(2026 - 07 - 15))
    .buyer_reference("04011000-12345-67")
    .paid_by(emob_billing::invoice::PaymentDetails::CreditTransfer {
        iban: "DE89370400440532013000".into(),
        holder: Some("Stadtwerke Musterstadt GmbH".into()),
    })
    .build()
    .unwrap()
    .value;

    let xml = en16931::write(
        &complete,
        en16931::Specification::XRechnung,
        en16931::Syntax::Ubl,
    )
    .unwrap();
    assert!(xml.value.contains("xrechnung_3.0"), "the profile it claims");
    assert!(xml.value.contains("04011000-12345-67"), "the Leitweg-ID");
    assert!(
        xml.value.contains("31.04"),
        "the total the records produced"
    );
    assert!(
        xml.value.contains("DE89370400440532013000"),
        "and how to pay it"
    );

    // …and the same invoice in the other syntax CEN/TS 16931-2 makes
    // mandatory. One semantic document, two spellings: a recipient's access
    // point takes one, the other or either, and which is a fact about the
    // recipient. Every ZUGFeRD payload is one of these.
    let cii = en16931::write(
        &complete,
        en16931::Specification::XRechnung,
        en16931::Syntax::Cii,
    )
    .unwrap();
    assert!(
        cii.value.contains("CrossIndustryInvoice"),
        "the CII root element"
    );
    assert!(cii.value.contains("xrechnung_3.0"), "the profile it claims");
    assert!(cii.value.contains("04011000-12345-67"), "the Leitweg-ID");
    assert!(cii.value.contains("31.04"), "and the same total");
    assert_ne!(
        cii.value, xml.value,
        "two syntaxes are two documents, not one"
    );

    // The `[UStG §14]` case the German profile would refuse: an ordinary
    // business customer with no Leitweg-ID, on the specification the Directive
    // actually names. `XRechnung`'s `BR-DE-*` are a **public-sector** usage
    // specification, and writing every invoice against them would refuse lawful
    // B2B documents for want of a routing identifier the recipient does not
    // issue.
    let b2b = en16931::write(&bare, en16931::Specification::Core, en16931::Syntax::Cii).unwrap();
    assert!(b2b.value.contains("CrossIndustryInvoice"));
    assert!(
        b2b.value.contains("urn:cen.eu:en16931:2017<"),
        "BT-24 is the core, not a CIUS: {}",
        &b2b.value[..400.min(b2b.value.len())]
    );
    assert!(
        b2b.reasons().any(|note| note.contains("CII")),
        "the crossing names the syntax the caller chose: {:?}",
        b2b.reasons().collect::<Vec<_>>()
    );
}

#[test]
fn a_two_rate_invoice_states_both_taxable_amounts_and_the_standard_accepts_it() {
    // Electricity and a service fee can sit in different VAT categories, and
    // EN 16931 wants the taxable amount **per rate** — `BR-S-08` sums the lines
    // of each rate and `BR-S-09` computes its tax. An invoice that taxed both at
    // one rate would over-declare on one of them, and the standard's own rules
    // are what say so.
    let party = PartyId::new("DE", "ABC").unwrap();
    let mixed = Tariff::simple(
        "mixed".parse().unwrap(),
        Currency::EUR,
        TariffKind::AdHoc,
        emob_core::TimeZone::new("Europe/Berlin").unwrap(),
        vec![
            PriceComponent::new(Dimension::Energy, dec("0.49")).with_vat(dec("19")),
            PriceComponent::new(Dimension::Flat, dec("1.07")).with_vat(dec("7")),
        ],
    );

    let mut ledger = CdrLedger::new();
    let (session, evidence) = session(0, "s-1", "100.000", "129.500");
    let cdr = CdrBuilder::from_session(&session, Direction::Import)
        .unwrap()
        .key(party, "cdr-mixed".parse().unwrap())
        .evidence(EvidenceRef::from_evidence(&evidence, "OCMF"))
        .rated_with(&mixed)
        .build()
        .unwrap();
    assert!(ledger.accept(cdr).is_stored());

    let invoice = InvoiceBuilder::new(
        "R-2026-0010",
        date!(2026 - 07 - 01),
        (date!(2026 - 06 - 01), date!(2026 - 06 - 30)),
        cpo(),
        Counterparty::new(
            "Erika Mustermann",
            "Beispielstadt",
            TaxStatus::consumer("DE"),
        ),
    )
    .supplied_from("DE", dec("19"))
    .ledger(&ledger)
    .due_on(date!(2026 - 07 - 15))
    .build()
    .unwrap()
    .value;

    // 29.5 × 0.49 = 14.455 gross at 19 % → 12.15 net; the fee is 1.07 gross at
    // 7 % → 1.00 net. Two rates, two taxable amounts.
    assert_eq!(
        invoice.lines.iter().map(|l| l.vat_rate).collect::<Vec<_>>(),
        vec![Some(dec("19")), Some(dec("7"))]
    );
    assert_eq!(invoice.tax.len(), 2, "{:?}", invoice.tax);
    assert_eq!(invoice.tax[0].rate, Some(dec("7")));
    assert_eq!(invoice.tax[0].tax, dec("0.07"));
    assert_eq!(invoice.tax[1].rate, Some(dec("19")));
    assert_eq!(invoice.tax[1].tax, dec("2.31"));
    assert_eq!(invoice.gross_total().to_string(), "15.53 EUR");
    assert!(invoice.reconciles());

    // …and the standard accepts it, which is the check that matters: the
    // per-category arithmetic rules are exactly the ones a single-rate invoice
    // would have failed.
    let crossed = en16931::to_en16931(&invoice, en16931::Specification::Core).unwrap();
    assert!(
        crossed.value.is_valid(),
        "{:?}",
        crossed.value.reasons().collect::<Vec<_>>()
    );
    assert_eq!(crossed.value.invoice.vat_breakdown.len(), 2);

    // The books follow the document: one VAT posting per rate, because an
    // operator posts 19 % and 7 % to different accounts.
    let books = postings::postings_for(&invoice);
    assert!(books.balances());
    assert_eq!(
        books
            .roles()
            .iter()
            .filter(|role| matches!(role, postings::Role::VatPayable { .. }))
            .count(),
        2
    );
}

#[test]
fn a_settlement_outside_the_union_is_outside_the_scope_and_states_neither_a_rate_nor_an_identifier()
{
    // `[UStG §3g]` moves the place of supply to where the reseller is
    // established, and a Swiss e-mobility provider takes it out of the Union
    // altogether — so no member state's VAT arises and the category is `O`,
    // outside scope, rather than the `G` that describes goods leaving the
    // customs territory zero-rated.
    //
    // The document that follows is the point. `O` is the only category in
    // UNCL 5305 that states **no rate** (`BR-O-05`) and allows **no VAT
    // identifier** on either party (`BR-O-02`) — and once the seller's is gone,
    // `BR-CO-26` still wants the buyer to be able to identify its supplier, so
    // the legal registration BT-30 stops being optional. All three are visible
    // only by running the standard's own rules over the finished document
    // (D183).
    let ledger = month();
    let swiss = Counterparty::new(
        "Helvetia Mobility AG",
        "Zug",
        TaxStatus::reseller("CH", "CHE-123.456.789"),
    )
    .at("Bahnhofstrasse 1", "6300");

    let invoice = InvoiceBuilder::new(
        "R-2026-0003",
        date!(2026 - 07 - 01),
        (date!(2026 - 06 - 01), date!(2026 - 06 - 30)),
        cpo(),
        swiss,
    )
    .supplied_from("DE", dec("19"))
    .ledger(&ledger)
    .payment_terms("30 days net")
    .build()
    .unwrap()
    .value;

    assert_eq!(invoice.treatment.category, VatCategory::OutOfScope);
    assert_eq!(invoice.treatment.category.code(), "O");
    assert_eq!(invoice.treatment.place_of_supply, "CH");
    assert_eq!(invoice.tax_total().to_string(), "0.00 EUR");
    assert_eq!(invoice.gross_total().to_string(), "26.08 EUR");

    // No rate anywhere — on the lines or on the breakdown. A rate of zero is a
    // rate, and `BR-O-05` refuses the field rather than the value.
    assert!(invoice.lines.iter().all(|line| line.vat_rate.is_none()));
    assert!(invoice.tax.iter().all(|subtotal| subtotal.rate.is_none()));

    let crossed = en16931::to_en16931(&invoice, en16931::Specification::Core).unwrap();
    assert!(
        crossed.value.is_valid(),
        "{:?}",
        crossed.value.reasons().collect::<Vec<_>>()
    );

    // Neither party's VAT identifier reaches the document, and the seller is
    // identified by its registration instead.
    let document = &crossed.value.invoice;
    assert_eq!(document.seller.vat_identifier, None);
    assert_eq!(document.buyer.vat_identifier, None);
    assert!(document.seller.legal_registration.is_some());
    assert!(document.vat_breakdown[0].rate.is_none());

    // And the books carry no VAT posting, because there is no liability.
    let books = postings::postings_for(&invoice);
    assert!(books.balances());
    assert!(
        !books
            .roles()
            .iter()
            .any(|role| matches!(role, postings::Role::VatPayable { .. }))
    );
}
