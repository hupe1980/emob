//! Every quantity door, driven with a figure a **partner** chose.
//!
//! `Decimal` panics on overflow rather than saturating, and every figure here is
//! multiplied afterwards — by a price, by a VAT factor, by a thousand to reach
//! watt-hours — or summed, one partner record at a time. Both operands of most
//! of those arrive from outside: the quantity on a charge detail record, the
//! price on the tariff beside it, the stated totals an invoice reconciliation
//! adds up.
//!
//! So the rule is the one this workspace already keeps about signs and about
//! scale: **a figure is checked where it is built**, not where it is used,
//! because the place it is used has nowhere to put the answer (D305).

use emob_core::{Currency, Energy, Money, QuantityError};
use rust_decimal::Decimal;

/// What a partner can put in a JSON number, and what it costs to believe it.
fn a_number_a_partner_chose() -> Decimal {
    Decimal::MAX / Decimal::from(10)
}

#[test]
fn an_energy_past_what_a_meter_states_never_becomes_an_energy() {
    let absurd = a_number_a_partner_chose();
    for refused in [Energy::from_kwh(absurd), Energy::from_wh(absurd)] {
        let error = refused.expect_err("a partner does not get to choose this");
        assert!(
            matches!(error, QuantityError::Unbillable { what: "energy", .. }),
            "{error:?}"
        );
        // The refusal names the bound, because "invalid" is not an answer
        // anybody can act on.
        assert!(error.to_string().contains("1000000000000"), "{error}");
    }

    // …and the wire is the door that was walked past once already (D264): a
    // derived `Deserialize` is a constructor nobody wrote.
    assert!(serde_json::from_str::<Energy>(&absurd.to_string()).is_err());

    // What a machine could actually deliver is still an energy. The bound is a
    // line past which a number stopped being a measurement, not a limit
    // anything real approaches: thirty years of a 3.75 MW charger, flat out.
    let lifetime = Decimal::from(30 * 365 * 24) * Decimal::from(3750);
    assert!(Energy::from_kwh(lifetime).is_ok());
    assert!(Energy::from_kwh(Energy::MAX_KWH).is_ok());
}

/// `checked_add` and `checked_sub` were named for a check they did not do: they
/// compared the currencies and then ran a `Decimal` operator that panics. The
/// name is the claim, and this is the caller that reaches it — an invoice
/// reconciliation adds one partner-stated total per record it names.
#[test]
fn a_money_total_that_no_decimal_holds_is_an_error_rather_than_a_panic() {
    let big = Money::new(a_number_a_partner_chose(), Currency::EUR);

    // Seven of these is past `Decimal::MAX`, and an invoice may name any number
    // of records.
    let mut running = big;
    let mut refusal = None;
    for _ in 0..20 {
        match running + big {
            Ok(next) => running = next,
            Err(error) => {
                refusal = Some(error);
                break;
            }
        }
    }
    let error = refusal.expect("summing a partner's totals cannot run forever");
    assert!(
        matches!(
            error,
            QuantityError::Unbillable {
                what: "a sum of money",
                ..
            }
        ),
        "{error:?}"
    );

    // The other direction, which is how a difference against an invoice is
    // taken: `Reconciled::difference_from` subtracts the records' total from
    // what the partner invoiced, and a credit note makes the second negative.
    let invoiced = Money::new(Decimal::MAX, Currency::EUR);
    let credited = Money::new(-Decimal::ONE, Currency::EUR);
    let error = (invoiced - credited).expect_err("a difference this wide is not money");
    assert!(
        matches!(
            error,
            QuantityError::Unbillable {
                what: "a difference of money",
                ..
            }
        ),
        "{error:?}"
    );

    // And the currency check still comes first, because two currencies are a
    // different mistake from one figure being absurd.
    let dollars = Money::new(Decimal::ONE, Currency::new("USD").expect("a code"));
    assert!(matches!(
        (big + dollars).expect_err("euros and dollars"),
        QuantityError::CurrencyMismatch { .. }
    ));
}
