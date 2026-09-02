//! The invoice, as bookkeeping.
//!
//! # Why this crate names roles and not accounts
//!
//! An invoice moves three things: a receivable comes into existence, revenue is
//! earned, and — where the supply is taxed — VAT becomes payable to a tax
//! authority. Which *accounts* those are is an operator's decision. SKR03 and
//! SKR04 disagree about the numbers, an operator with an IFRS group reporting
//! line disagrees with both, and a domain crate that shipped a chart of accounts
//! would be telling a finance department how to keep its books.
//!
//! So this produces [`Posting`]s addressed by [`Role`]. The arithmetic — which
//! side, how much, and that the two sides are equal — is here, because that is
//! the part that can be wrong.
//!
//! # Balanced before anything is mapped
//!
//! [`Postings::balances`] holds by construction and is asserted anyway. A ledger
//! would refuse an unbalanced entry, but it would refuse it after the account
//! mapping, where the diagnostic is about an account id and not about an
//! invoice — and the fault, if there ever is one, is this module's.
//!
//! # And the ledger is a service's
//!
//! There is no bridge here into a bookkeeping engine. Posting into a journal
//! needs a **journal** — accounts, a calendar, a policy, a database — none of
//! which can live in a crate that promises to read no clock. `mako` declares
//! `doubleentry` in one manifest, `services/accountingd`, and in no crate;
//! `billd` is where it belongs here.
//!
//! Nor is it only layering: `doubleentry` takes `uuid` with `v7`, and a v7
//! identifier comes from `SystemTime::now()`. `just purity` greps this
//! workspace's source and cannot see into a dependency, so that promise is kept
//! by what the manifests do not declare as much as by what the code does not
//! call.
//!
//! What crosses the seam is [`Postings`]: a currency, a booking date, and a
//! balanced set of role-addressed movements. A service maps [`Role`] onto its
//! own chart, and a role its chart cannot place is a refusal rather than a
//! dropped posting — a dropped posting is an entry that does not balance and a
//! trial balance that is quietly wrong.

use emob_core::{Currency, Money};
use rust_decimal::Decimal;

use crate::invoice::Invoice;
use crate::tax::VatCategory;

/// What an account is *for*, on a charging invoice.
///
/// Four roles, which is the whole of what issuing one moves. A payment, a
/// write-off and a partner settlement move others, and they are not this
/// document's postings.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum Role {
    /// What the buyer now owes. Debited with the gross.
    Receivable,
    /// Revenue from delivered energy.
    EnergyRevenue,
    /// Revenue from time — a charging-time rate, an occupancy fee, a session
    /// fee, a minimum charge.
    ///
    /// Kept apart from energy because the two are different supplies for
    /// accounting even where one invoice carries both, and an operator that
    /// cannot see the split cannot answer what its occupancy fees earned.
    ServiceRevenue,
    /// VAT owed to a tax authority, at one rate.
    ///
    /// Carries the rate, because an operator posts 19 % and 7 % to different
    /// accounts and a role that could not say which would collapse them.
    VatPayable {
        /// The rate, as a percentage.
        rate: Decimal,
    },
}

impl core::fmt::Display for Role {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Receivable => f.write_str("receivable"),
            Self::EnergyRevenue => f.write_str("energy revenue"),
            Self::ServiceRevenue => f.write_str("service revenue"),
            Self::VatPayable { rate } => write!(f, "VAT payable at {rate} %"),
        }
    }
}

/// Which way an amount moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum Side {
    /// Left.
    Debit,
    /// Right.
    Credit,
}

/// One movement.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Posting {
    /// Which account, by what it is for.
    pub role: Role,
    /// Which side.
    pub side: Side,
    /// How much, in the invoice's currency.
    pub amount: Decimal,
}

/// The postings one invoice makes.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Postings {
    /// The invoice number these belong to.
    pub reference: String,
    /// The day they are booked — the invoice's own issue date.
    #[cfg_attr(feature = "serde", serde(with = "emob_core::wire::date"))]
    pub booked_on: time::Date,
    /// The currency.
    pub currency: Currency,
    /// The movements, receivable first.
    pub postings: Vec<Posting>,
}

impl Postings {
    /// The sum of the debits.
    #[must_use]
    pub fn debits(&self) -> Money {
        self.side(Side::Debit)
    }

    /// The sum of the credits.
    #[must_use]
    pub fn credits(&self) -> Money {
        self.side(Side::Credit)
    }

    fn side(&self, side: Side) -> Money {
        Money::new(
            self.postings
                .iter()
                .filter(|posting| posting.side == side)
                .map(|posting| posting.amount)
                .sum(),
            self.currency,
        )
    }

    /// Whether the two sides are equal — the only invariant bookkeeping has.
    #[must_use]
    pub fn balances(&self) -> bool {
        self.debits() == self.credits()
    }

    /// The roles this entry touches, in posting order.
    #[must_use]
    pub fn roles(&self) -> Vec<&Role> {
        self.postings.iter().map(|posting| &posting.role).collect()
    }
}

/// The postings an invoice makes.
///
/// Debit the receivable with the gross; credit revenue with each category's
/// net, split between energy and everything else; credit VAT with the tax.
///
/// # Under a reverse charge there is no tax posting
///
/// …and that is the point of deciding the treatment before the books rather
/// than after. `[UStG §13b]` moves the liability to the recipient, so the
/// supplier's own VAT account never moves — a platform that posts 19 % here and
/// removes it from the invoice has books that disagree with the document it
/// sent.
#[must_use]
pub fn postings_for(invoice: &Invoice) -> Postings {
    let currency = invoice.currency;
    let mut postings = Vec::with_capacity(4);

    postings.push(Posting {
        role: Role::Receivable,
        side: Side::Debit,
        amount: invoice.gross_total().amount(),
    });

    // Revenue, split by what was sold. The split is over the *rounded* line
    // amounts, so the two revenue postings sum to the taxable total exactly and
    // the entry balances without a plug.
    let energy: Decimal = invoice
        .lines
        .iter()
        .filter(|line| line.dimension == emob_tariff::Dimension::Energy)
        .map(|line| line.net)
        .sum();
    let service: Decimal = invoice
        .lines
        .iter()
        .filter(|line| line.dimension != emob_tariff::Dimension::Energy)
        .map(|line| line.net)
        .sum();

    for (role, amount) in [
        (Role::EnergyRevenue, energy),
        (Role::ServiceRevenue, service),
    ] {
        if !amount.is_zero() {
            postings.push(Posting {
                role,
                side: Side::Credit,
                amount,
            });
        }
    }

    for subtotal in &invoice.tax {
        // A VAT liability has a rate by definition, and the one category that
        // states none — `O`, outside the scope — has no liability to post
        // either. The two conditions are the same fact seen twice, and the
        // `let Some` is what makes that structural rather than remembered.
        if let Some(rate) = subtotal.rate
            && subtotal.category == VatCategory::Standard
            && !subtotal.tax.is_zero()
        {
            postings.push(Posting {
                role: Role::VatPayable { rate },
                side: Side::Credit,
                amount: subtotal.tax,
            });
        }
    }

    let out = Postings {
        reference: invoice.number.clone(),
        booked_on: invoice.issued_on,
        currency,
        postings,
    };
    debug_assert!(
        out.balances(),
        "an invoice's postings balance by construction"
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invoice::{Counterparty, InvoiceBuilder};
    use crate::tax::TaxStatus;
    use rust_decimal::prelude::FromStr;
    use time::macros::date;

    fn dec(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    /// An invoice with two lines at 19 %, built the ordinary way so the
    /// postings are exercised against real arithmetic rather than a literal.
    fn invoice(treatment: crate::TaxTreatment) -> Invoice {
        use emob_cdr::{Cdr, CdrKey, ChargingPeriod, Cost};
        use emob_core::{Direction, Energy, PartyId, QuarterHour};
        use emob_session::{AuthPath, Provenance};
        use emob_tariff::{
            Chargeable, Dimension, Period, PriceComponent, Tariff, TariffKind, rate,
        };
        use time::macros::datetime;

        let at = |m: i64| datetime!(2026-06-01 10:00 +2) + time::Duration::minutes(m);
        let tariff = Tariff::simple(
            "t".parse().unwrap(),
            Currency::EUR,
            TariffKind::AdHoc,
            emob_core::TimeZone::new("Europe/Berlin").unwrap(),
            vec![
                PriceComponent::new(Dimension::Energy, dec("0.49")).with_vat(dec("19")),
                PriceComponent::new(Dimension::ParkingTime, dec("6.00")).with_vat(dec("19")),
            ],
        );
        let chargeable = Chargeable::new(vec![
            Period::charging(at(0), at(30), Energy::from_kwh(dec("29.500")).unwrap()),
            Period::parked(at(30), at(60)),
        ])
        .unwrap();
        let rated = rate(&tariff, &chargeable);

        let cdr = Cdr {
            key: CdrKey {
                party: PartyId::new("DE", "ABC").unwrap(),
                id: "c-1".parse().unwrap(),
            },
            session_id: "s-1".parse().unwrap(),
            evse_id: "DE*AB7*E840*6487".parse().unwrap(),
            started_at: at(0),
            ended_at: at(60),
            auth_path: AuthPath::AdHoc,
            periods: vec![ChargingPeriod {
                quarter_hour: QuarterHour::containing(at(0)),
                start: at(0),
                end: at(60),
                energy: Energy::from_kwh(dec("29.500")).unwrap(),
                charging: true,
                provenance: Provenance::Measured,
            }],
            total_energy: Energy::from_kwh(dec("29.500")).unwrap(),
            direction: Direction::Import,
            evidence: None,
            cost: Some(Cost {
                tariff_id: "t".parse().unwrap(),
                tariff_fingerprint: tariff.fingerprint(),
                rated,
            }),
            supersedes: None,
        };

        InvoiceBuilder::new(
            "R-1",
            date!(2026 - 07 - 01),
            (date!(2026 - 06 - 01), date!(2026 - 06 - 30)),
            Counterparty::new(
                "CPO",
                "Musterstadt",
                TaxStatus::business("DE", "DE123456789"),
            ),
            Counterparty::new("Driver", "Beispielstadt", TaxStatus::consumer("DE")),
        )
        .taxed_as(treatment)
        .record(&cdr)
        .due_on(date!(2026 - 07 - 15))
        .build()
        .unwrap()
        .value
    }

    fn standard() -> crate::TaxTreatment {
        crate::TaxTreatment::stated(VatCategory::Standard, dec("19"), "DE", None)
    }

    #[test]
    fn a_standard_rated_invoice_moves_a_receivable_revenue_and_the_vat() {
        let books = postings_for(&invoice(standard()));
        assert!(books.balances());
        // 29.5 × 0.49 = 14.455 gross → 12.15 net; 30 min at 6.00/h = 3.00 gross
        // → 2.52 net. Taxable 14.67, tax 2.79, gross 17.46.
        assert_eq!(books.debits().to_string(), "17.46 EUR");
        assert_eq!(
            books.postings,
            vec![
                Posting {
                    role: Role::Receivable,
                    side: Side::Debit,
                    amount: dec("17.46"),
                },
                Posting {
                    role: Role::EnergyRevenue,
                    side: Side::Credit,
                    amount: dec("12.15"),
                },
                Posting {
                    role: Role::ServiceRevenue,
                    side: Side::Credit,
                    amount: dec("2.52"),
                },
                Posting {
                    role: Role::VatPayable { rate: dec("19") },
                    side: Side::Credit,
                    amount: dec("2.79"),
                },
            ]
        );
    }

    #[test]
    fn a_reverse_charge_moves_no_vat_account_of_ours() {
        // The books have to agree with the document. A platform that posts the
        // rate here and omits it from the invoice has a VAT return that does
        // not reconcile against anything it sent.
        let treatment = crate::TaxTreatment::stated(
            VatCategory::ReverseCharge,
            Decimal::ZERO,
            "FR",
            Some("Reverse charge".into()),
        );
        let books = postings_for(&invoice(treatment));
        assert!(books.balances());
        assert!(
            !books
                .roles()
                .iter()
                .any(|role| matches!(role, Role::VatPayable { .. })),
            "{:?}",
            books.roles()
        );
        assert_eq!(books.debits().to_string(), "14.67 EUR");
    }
}
