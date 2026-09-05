//! What a record read back from a store has to have gone through.
//!
//! Every value in this workspace that carries a rule states it in a constructor
//! — `Energy::from_kwh` refuses a negative, `TokenRef::new` refuses anything
//! that is not a keyed digest, `Chargeable::new` refuses overlapping periods —
//! and `serde` was the door none of them was on. A derived `Deserialize`
//! restores the fields and asks the type nothing, which is exactly the path a
//! value takes out of an outbox, an evidence store or a partner's document.
//!
//! This is the composition, not the pieces. Each constructor's own test passed
//! before and after; what did not hold was the seam (rule 5): a charge detail
//! record carrying a period of **−10.000 kWh** deserialised, **conserved**, and
//! validated as **settleable** — import and export netting inside one record,
//! which is the one outcome `Direction` exists as a separate field to prevent
//! (D264).

use emob_cdr::Cdr;

/// A record whose second period runs the meter backwards.
fn record_with(energy: &str) -> String {
    format!(
        r#"{{
      "key": {{"party": "DE*ABC", "id": "c-1"}},
      "reservation": null,
      "session_id": "s-1",
      "evse_id": "DE*AB7*E840*6487",
      "started_at": "2026-01-02T10:00:00+01:00",
      "ended_at": "2026-01-02T10:30:00+01:00",
      "auth_path": "ad_hoc",
      "authorization_reference": null,
      "periods": [
        {{"quarter_hour":"2026-01-02T10:00:00+01:00","start":"2026-01-02T10:00:00+01:00",
         "end":"2026-01-02T10:15:00+01:00","energy":"10.000","activity":"charging",
         "provenance":"measured"}},
        {{"quarter_hour":"2026-01-02T10:15:00+01:00","start":"2026-01-02T10:15:00+01:00",
         "end":"2026-01-02T10:30:00+01:00","energy":"{energy}","activity":"charging",
         "provenance":"measured"}}
      ],
      "total_energy": "20.000",
      "direction": "import",
      "evidence": null,
      "cost": null,
      "supersedes": null
    }}"#
    )
}

#[test]
fn a_record_whose_meter_ran_backwards_does_not_arrive_at_all() {
    let refused = serde_json::from_str::<Cdr>(&record_with("-10.000"));
    let error = refused.expect_err("a negative energy is not a quantity this model has");
    assert!(error.to_string().contains("energy"), "{error}");

    // …and the record it was a corruption of still reads, so the refusal is
    // about the value rather than about the shape.
    let ordinary: Cdr = serde_json::from_str(&record_with("10.000")).expect("an ordinary record");
    assert!(ordinary.conserves());
    assert_eq!(ordinary.total_energy.to_string(), "20.000 kWh");
    assert_eq!(ordinary.key.to_string(), "DE*ABC/c-1");
}

#[test]
fn the_party_that_owns_a_record_is_the_string_it_is_written_as() {
    // `emob_service::authority` compares this value against a scope. A party
    // that arrived in the two-member form could be lower-case, and a lower-case
    // party reaches nothing and matches nothing — without failing.
    let raw = record_with("10.000").replace(r#""DE*ABC""#, r#""deabc""#);
    let cdr: Cdr = serde_json::from_str(&raw).expect("every spelling is one party");
    assert_eq!(cdr.key.party, emob_core::PartyId::new("DE", "ABC").unwrap());
    assert_eq!(cdr.key.to_string(), "DE*ABC/c-1");
}

#[test]
fn a_record_with_no_id_is_not_a_record() {
    // An empty `CdrId` is a ledger key that collides with every other empty one,
    // and `CdrId::new` refuses one.
    let raw = record_with("10.000").replace(r#""id": "c-1""#, r#""id": """#);
    assert!(serde_json::from_str::<Cdr>(&raw).is_err());
}
