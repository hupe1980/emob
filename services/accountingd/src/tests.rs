use super::*;
use crate::ledger::{Book, CONFLICT};
use crate::model::{Allocation, Document, Fact};
use emob_service::Actor;
use emob_service::journal::Memory;
use serde_json::{Value, json};

fn issuer() -> PartyId {
    "DE*MSP".parse().unwrap()
}

fn book() -> Book<Memory> {
    Book::open(
        issuer(),
        Memory::new(),
        Arc::new(DataKey::from_bytes(&[2; 32])),
        Readiness::new().expecting("ledger"),
    )
    .unwrap()
}

fn actor() -> Actor {
    Actor::operator("finance").unwrap()
}

fn doc(b: &mut Book<Memory>, number: &str, predecessor: Option<&str>, kind: &str, amount: i64) {
    let doc:Document=serde_json::from_value(json!({"position":b.state.cursor()+1,"issuer":issuer(),"number":number,"account":"PAYER-A","currency":"EUR","kind":kind,"amount_minor":amount,"predecessor":predecessor})).unwrap();
    b.commit(&Actor::service("billd"), &Fact::Document { document: doc })
        .unwrap();
}
fn entry(reference: &str, amount: i64) -> Value {
    json!({"reference":reference,"status":"booked","direction":"credit","amount_minor":amount})
}

fn import(b: &mut Book<Memory>, account: &str, entries: &[Value]) -> Value {
    let s=serde_json::from_value(json!({"version":1,"account":account,"currency":"EUR","source":"LOCAL-BANK","entries":entries})).unwrap();
    serde_json::to_value(b.import(&actor(), s).unwrap()).unwrap()
}

fn allocate(b: &mut Book<Memory>, number: &str, reference: &str, amount: i64) -> Allocation {
    let selection:Allocation=serde_json::from_value(json!({"account":"PAYER-A","currency":"EUR","source":"LOCAL-BANK","reference":reference,"number":number,"amount_minor":amount})).unwrap();
    let review = b.review(&actor(), &selection).unwrap();
    b.confirm(&actor(), selection.clone(), &review.review)
        .unwrap();
    assert_eq!(
        b.confirm(&actor(), selection.clone(), &review.review)
            .unwrap()
            .open_after_minor,
        review.open_after_minor
    );
    selection
}

// receivables-allocation/SR-352
#[test]
fn an_external_reference_books_once_across_payers_and_conflicts_stay_in_review() {
    let mut b = book();
    let row = entry("STMT-01", 6000);
    let result = import(&mut b, "PAYER-A", &[row.clone(), row.clone()]);
    assert_eq!(result["booked"], 1);
    assert_eq!(result["duplicates"], 1);
    assert_eq!(
        import(&mut b, "PAYER-A", std::slice::from_ref(&row))["duplicates"],
        1
    );
    for _ in 0..2 {
        let changed = import(&mut b, "PAYER-B", std::slice::from_ref(&row));
        assert_eq!(changed["held"][0]["reason"], "reference_conflict");
        assert_eq!(changed["booked"], 0);
    }
    let mut pending = entry("PENDING", 500);
    pending["status"] = json!("pending");
    let mut reversal = entry("REVERSE", 150);
    reversal["reversal"] = json!(true);
    let mut missing = entry("X", 100);
    missing["reference"] = Value::Null;
    let rows = [
        pending,
        reversal,
        missing,
        entry("AMBIGUOUS", 100),
        entry("AMBIGUOUS", 150),
    ];
    let held = import(&mut b, "PAYER-A", &rows);
    assert_eq!(held["booked"], 0);
    assert_eq!(held["held"].as_array().unwrap().len(), 5);
    let balance = b.state.balance("PAYER-A", emob_core::Currency::EUR, 0);
    assert_eq!(balance["unapplied_minor"], 6000);
    assert_eq!(balance["held"].as_array().unwrap().len(), 5);
    assert_eq!(b.state.proof()["held"], 6);
}

// receivables-allocation/SR-353
#[test]
fn partial_cash_and_credits_follow_the_document_chain_in_exact_minor_units() {
    let mut b = book();
    doc(&mut b, "A", None, "invoice", 10000);
    import(
        &mut b,
        "PAYER-A",
        &[entry("PAY", 6000), entry("NEXT", 5000)],
    );
    allocate(&mut b, "A", "PAY", 6000);
    let s = b.state.balance("PAYER-A", emob_core::Currency::EUR, 0);
    assert_eq!(s["net_due_minor"], 4000);
    allocate(&mut b, "A", "NEXT", 4000);
    let s = b.state.balance("PAYER-A", emob_core::Currency::EUR, 0);
    assert_eq!(s["unapplied_minor"], 1000);
    doc(&mut b, "C", Some("A"), "credit", 10000);
    doc(&mut b, "B", Some("A"), "replacement", 8000);
    let s = b.state.balance("PAYER-A", emob_core::Currency::EUR, 0);
    assert_eq!(s["net_due_minor"], -2000);
    assert_eq!(s["credit_minor"], 2000);
    assert_eq!(s["unapplied_minor"], 1000);
    assert!(
        s["documents"]
            .as_array()
            .unwrap()
            .iter()
            .all(|d| d["open_minor"] == 0)
    );
    doc(&mut b, "D", Some("B"), "credit", 8000);
    doc(&mut b, "E", Some("B"), "replacement", 11000);
    let s = b.state.balance("PAYER-A", emob_core::Currency::EUR, 0);
    assert_eq!(s["credit_minor"], 0);
    assert_eq!(s["net_due_minor"], 1000);
    allocate(&mut b, "E", "NEXT", 1000);
    assert_eq!(
        b.state.balance("PAYER-A", emob_core::Currency::EUR, 0)["net_due_minor"],
        0
    );
}

// receivables-allocation/SR-353
#[test]
fn a_named_review_expires_on_balance_change_and_cannot_cross_scope() {
    let mut b = book();
    doc(&mut b, "A", None, "invoice", 10000);
    import(
        &mut b,
        "PAYER-A",
        &[entry("PAY", 6000), entry("NEXT", 5000)],
    );
    let req:Allocation=serde_json::from_value(json!({"account":"PAYER-A","currency":"EUR","source":"LOCAL-BANK","reference":"PAY","number":"A","amount_minor":3000})).unwrap();
    let preview = b.review(&actor(), &req).unwrap().review;
    assert_eq!(
        b.confirm(&Actor::operator("other").unwrap(), req.clone(), &preview)
            .err(),
        Some(CONFLICT)
    );
    allocate(&mut b, "A", "NEXT", 1000);
    assert_eq!(
        b.confirm(&actor(), req.clone(), &preview).err(),
        Some(CONFLICT)
    );
    let mut wrong = req.clone();
    wrong.currency = emob_core::Currency::new("CHF").unwrap();
    assert_eq!(b.review(&actor(), &wrong).err(), Some(CONFLICT));
    let mut wrong = req;
    wrong.account = "PAYER-B".into();
    assert_eq!(b.review(&actor(), &wrong).err(), Some(CONFLICT));
}

// receivables-allocation/SR-352
#[test]
fn an_out_of_range_statement_is_rejected_before_any_cash_is_booked() {
    let mut b = book();
    let batch=serde_json::from_value(json!({"version":1,"account":"PAYER-A","currency":"EUR","source":"LOCAL-BANK","entries":[entry("A",i64::MAX),entry("B",1)]})).unwrap();
    assert_eq!(b.import(&actor(), batch).err(), Some(CONFLICT));
    assert_eq!(b.state.proof()["payments"], 0);
}

#[test]
fn successive_line_credits_reduce_only_the_open_item_and_preserve_paid_cash() {
    let mut b = book();
    doc(&mut b, "A", None, "invoice", 10000);
    import(&mut b, "PAYER-A", &[entry("PAY", 9500)]);
    allocate(&mut b, "A", "PAY", 8000);
    doc(&mut b, "C1", Some("A"), "line_credit", 1500);
    let balance = |b: &Book<Memory>| b.state.balance("PAYER-A", emob_core::Currency::EUR, 0);
    let first = balance(&b);
    assert_eq!(first["net_due_minor"], 500);
    assert_eq!(first["documents"][0]["open_minor"], 500);
    doc(&mut b, "C2", Some("A"), "line_credit", 1000);
    let now = balance(&b);
    assert_eq!(now["net_due_minor"], -500);
    assert_eq!(now["credit_minor"], 500);
    assert_eq!(now["unapplied_minor"], 1500);
    assert_eq!(now["allocations"], first["allocations"]);
    for (kind, amount, parent) in [
        ("line_credit", 7501, "A"),
        ("credit", 10000, "A"),
        ("replacement", 10000, "A"),
        ("line_credit", 1, "C1"),
    ] {
        let document = serde_json::from_value(json!({"position":4,"issuer":issuer(),"number":"INVALID","account":"PAYER-A","currency":"EUR","kind":kind,"amount_minor":amount,"predecessor":parent})).unwrap();
        assert_eq!(
            b.commit(&Actor::service("billd"), &Fact::Document { document }),
            Err(CONFLICT)
        );
    }
    assert_eq!(balance(&b), now);
}

// customer-credit-offset/SR-030
#[test]
fn credit_offsets_bind_both_documents_and_the_writer_across_replay() {
    use crate::model::Offset;
    let mut b = book();
    doc(&mut b, "A", None, "invoice", 10000);
    import(&mut b, "PAYER-A", &[entry("PAY", 10000)]);
    allocate(&mut b, "A", "PAY", 10000);
    doc(&mut b, "C", Some("A"), "line_credit", 3000);
    doc(&mut b, "B", None, "invoice", 8000);
    let req = Offset {
        source_number: "A".into(),
        target_number: "B".into(),
        account: "PAYER-A".into(),
        currency: emob_core::Currency::EUR,
        amount_minor: 2000,
    };
    let balance = |b: &Book<Memory>| b.state.balance("PAYER-A", emob_core::Currency::EUR, 0);
    let before = balance(&b);
    let other = Actor::operator("other").unwrap();
    let rival = b.offset_review(&other, &req).unwrap();
    let reviewed = b.offset_review(&actor(), &req).unwrap();
    assert_eq!(
        (
            reviewed.source_before_minor,
            reviewed.source_after_minor,
            reviewed.target_before_minor,
            reviewed.target_after_minor
        ),
        (-3000, -1000, 8000, 6000)
    );
    assert_eq!(
        b.confirm_offset(&other, &req, &reviewed.review).err(),
        Some(CONFLICT)
    );
    let receipt = b.confirm_offset(&actor(), &req, &reviewed.review).unwrap();
    assert_eq!(receipt, reviewed);
    assert_eq!(
        b.confirm_offset(&other, &req, &rival.review).err(),
        Some(CONFLICT)
    );
    let after = balance(&b);
    assert_eq!(after["net_due_minor"], 5000);
    assert_eq!(after["credit_minor"], 1000);
    assert_eq!(after["payments"], before["payments"]);
    assert_eq!(after["allocations"], before["allocations"]);
    assert_eq!(after["offsets"].as_array().unwrap().len(), 1);
    let journal = b.into_journal();
    let mut replay = Book::open(
        issuer(),
        journal,
        Arc::new(DataKey::from_bytes(&[2; 32])),
        Readiness::new().expecting("ledger"),
    )
    .unwrap();
    assert_eq!(balance(&replay), after);
    assert_eq!(
        replay
            .confirm_offset(&actor(), &req, &receipt.review)
            .unwrap(),
        receipt
    );
    assert_eq!(
        replay.confirm_offset(&other, &req, &rival.review).err(),
        Some(CONFLICT)
    );
    doc(&mut replay, "BC", Some("B"), "credit", 8000);
    doc(&mut replay, "B2", Some("B"), "replacement", 9000);
    assert_eq!(balance(&replay)["net_due_minor"], 6000);
    let evidence = replay
        .state
        .allocation_evidence("PAYER-A", req.currency, "B2", 0)
        .unwrap();
    assert_eq!(evidence["open_minor"], 7000);
    assert_eq!(evidence["offsets"][0]["target_number"], "B");
}

#[test]
fn a_stale_credit_review_stays_stale_when_the_amount_returns_to_its_old_value() {
    use crate::model::Offset;
    let mut b = book();
    doc(&mut b, "A", None, "invoice", 100);
    import(&mut b, "PAYER-A", &[entry("PAY", 100)]);
    allocate(&mut b, "A", "PAY", 100);
    doc(&mut b, "AC", Some("A"), "line_credit", 40);
    doc(&mut b, "B", None, "invoice", 50);
    doc(&mut b, "C", None, "invoice", 50);
    let selection = Offset {
        source_number: "A".into(),
        target_number: "B".into(),
        account: "PAYER-A".into(),
        currency: emob_core::Currency::EUR,
        amount_minor: 10,
    };
    let old = b.offset_review(&actor(), &selection).unwrap();
    let other = Offset {
        target_number: "C".into(),
        ..selection.clone()
    };
    let quote = b.offset_review(&actor(), &other).unwrap();
    b.confirm_offset(&actor(), &other, &quote.review).unwrap();
    doc(&mut b, "AC2", Some("A"), "line_credit", 10);
    let fresh = b.offset_review(&actor(), &selection).unwrap();
    assert_eq!(fresh.source_before_minor, old.source_before_minor);
    assert_eq!(fresh.target_before_minor, old.target_before_minor);
    assert_ne!(fresh.review, old.review);
    assert_eq!(
        b.confirm_offset(&actor(), &selection, &old.review).err(),
        Some(CONFLICT)
    );
    for invalid in [
        Offset {
            account: "PAYER-B".into(),
            ..selection.clone()
        },
        Offset {
            currency: emob_core::Currency::new("CHF").unwrap(),
            ..selection.clone()
        },
        Offset {
            target_number: "A".into(),
            ..selection.clone()
        },
        Offset {
            source_number: "AC".into(),
            ..selection.clone()
        },
        Offset {
            amount_minor: i64::MAX,
            ..selection.clone()
        },
    ] {
        assert_eq!(b.offset_review(&actor(), &invalid).err(), Some(CONFLICT));
    }
    let wrong = serde_json::from_value(json!({"position":b.state.cursor()+1,"issuer":"DE*XXX","number":"FOREIGN","account":"PAYER-A","currency":"EUR","kind":"invoice","amount_minor":100,"predecessor":null})).unwrap();
    assert_eq!(
        b.commit(
            &Actor::service("billd"),
            &Fact::Document { document: wrong }
        ),
        Err(CONFLICT)
    );
    assert_eq!(crate::ledger::signed_due(i64::MAX, 0, 0, 1), Err(CONFLICT));
    assert_eq!(crate::ledger::signed_due(0, i64::MAX, 2, 0), Err(CONFLICT));

    // An unacknowledged journal append must leave both sides untouched.
    let journal = b.into_journal();
    let mut refused = Book::open(
        issuer(),
        RejectOffset(journal),
        Arc::new(DataKey::from_bytes(&[2; 32])),
        Readiness::new().expecting("ledger"),
    )
    .unwrap();
    let before = refused.state.proof();
    assert_eq!(
        refused
            .confirm_offset(&actor(), &selection, &fresh.review)
            .err(),
        Some(crate::ledger::OFFLINE)
    );
    assert_eq!(refused.state.proof(), before);
}

struct RejectOffset(Memory);
impl emob_service::journal::Journal for RejectOffset {
    fn append(
        &self,
        kind: &str,
        actor: &Actor,
        body: &str,
    ) -> Result<emob_service::journal::Record, emob_service::journal::JournalError> {
        if kind == "offset" {
            return Err(emob_service::journal::JournalError::Io {
                path: "test-journal".into(),
                doing: "appended",
                source: std::io::Error::other("injected storage failure"),
            });
        }
        self.0.append(kind, actor, body)
    }
    fn records(
        &self,
    ) -> Result<Vec<emob_service::journal::Record>, emob_service::journal::JournalError> {
        self.0.records()
    }
}
