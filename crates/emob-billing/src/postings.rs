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
/// Debit the receivable with the gross; credit revenue with the taxable amount,
/// split between energy and everything else; credit VAT with the tax.
///
/// # The revenue is the **taxable** amount, not the line total
///
/// A tariff's minimum or maximum is a document level charge or allowance rather
/// than a line (see [`DocumentAdjustment`]), so `BT-106` and `BT-109` are two
/// different figures the moment a bound moves a session. Booking the lines
/// would credit revenue the invoice does not demand: a session capped at ten
/// euros whose lines come to twelve would show twelve of revenue against ten of
/// receivable, and the entry would not balance at all.
///
/// So each adjustment moves the revenue it belongs to, and which role that is
/// follows the largest line by amount **of the record the bound belongs to** —
/// the same choice [`emob_tariff::Adjustment::vat`] makes about the rate, at the
/// same scope: a bound is economically more of whatever *that session* mostly
/// was (D214). With no lines at all — a driver who plugged in, drew nothing and
/// owes a minimum — it is service revenue.
/// A reduction is posted as a **debit** to that role rather than as a negative
/// credit, so a journal reads it the way an accountant would write it.
///
/// # Under a reverse charge there is no tax posting
///
/// …and that is the point of deciding the treatment before the books rather
/// than after. `[UStG §13b]` moves the liability to the recipient, so the
/// supplier's own VAT account never moves — a platform that posts 19 % here and
/// removes it from the invoice has books that disagree with the document it
/// sent.
///
/// [`DocumentAdjustment`]: crate::invoice::DocumentAdjustment
#[must_use]
pub fn postings_for(invoice: &Invoice) -> Postings {
    let currency = invoice.currency;
    let mut postings = Vec::with_capacity(4);

    postings.push(Posting {
        role: Role::Receivable,
        side: Side::Debit,
        amount: invoice.gross_total().amount(),
    });

    // Revenue, split by what was sold. The split is over the *rounded* figures,
    // so the two revenue postings sum to the taxable total exactly and the
    // entry balances without a plug.
    let mut energy: Decimal = revenue(invoice, true);
    let mut service: Decimal = revenue(invoice, false);
    for adjustment in &invoice.adjustments {
        let signed = adjustment.kind.sign() * adjustment.amount;
        // Asked of the record the bound belongs to, not of the document. See
        // `dominant_is_energy`.
        if dominant_is_energy(invoice, &adjustment.cdr) {
            energy += signed;
        } else {
            service += signed;
        }
    }

    for (role, amount) in [
        (Role::EnergyRevenue, energy),
        (Role::ServiceRevenue, service),
    ] {
        if !amount.is_zero() {
            postings.push(Posting {
                role,
                // A bound that took more off a role than its lines earned is a
                // debit to it. Algebraically the same entry; legible in a
                // journal, which a negative credit is not.
                side: if amount.is_sign_negative() {
                    Side::Debit
                } else {
                    Side::Credit
                },
                amount: amount.abs(),
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

    // A credit note reverses the entry. Every side flips and no amount changes,
    // which is the same arithmetic an accountant writes and the reason EN 16931
    // states a credit note's figures as positive: the direction is the document
    // type, here and there (D229). Booking a `Stornorechnung` the same way as
    // the invoice it cancels doubles the revenue and the VAT liability, and the
    // books then disagree with the two documents that were sent.
    if invoice.kind.is_credit_note() {
        for posting in &mut postings {
            posting.side = match posting.side {
                Side::Debit => Side::Credit,
                Side::Credit => Side::Debit,
            };
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

/// One side of the revenue split, over the invoice's own line amounts.
fn revenue(invoice: &Invoice, is_energy: bool) -> Decimal {
    invoice
        .lines
        .iter()
        .filter(|line| (line.dimension == emob_tariff::Dimension::Energy) == is_energy)
        .map(|line| line.net)
        .sum()
}

/// Whether the largest line by amount **of one record** is an energy line —
/// which role that record's document level adjustment belongs to.
///
/// The scope is the record because the bound is: `[OCPI 2.3.0 §Tariff]` states a
/// minimum or maximum on the tariff that priced one session, and
/// `DocumentAdjustment::cdr` names which. Asked of the whole document instead, a
/// month of energy sessions plus one pure-occupancy session that hit a minimum
/// books that minimum to **energy revenue** — while its VAT comes off the
/// occupancy line, because [`emob_tariff::Adjustment::vat`] asks per session
/// (D214).
///
/// `false` where the record has no lines at all — a driver who plugged in, drew
/// nothing and owes a minimum — because there was no energy for the bound to be
/// more of.
fn dominant_is_energy(invoice: &Invoice, cdr: &emob_cdr::CdrKey) -> bool {
    invoice
        .lines
        .iter()
        .filter(|line| line.cdr == *cdr)
        .max_by_key(|line| line.net.abs())
        .is_some_and(|line| line.dimension == emob_tariff::Dimension::Energy)
}

#[cfg(test)]
mod tests {
    use emob_core::Activity;
    use emob_tariff::PriceLimit;

    use super::*;
    use crate::invoice::{Counterparty, DocumentAdjustmentKind, InvoiceBuilder};
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
            reservation: None,
            session_id: "s-1".parse().unwrap(),
            evse_id: "DE*AB7*E840*6487".parse().unwrap(),
            started_at: at(0),
            ended_at: at(60),
            auth_path: AuthPath::AdHoc,
            authorization_reference: None,
            clock: emob_core::ClockResolution::conforming(),
            // The record's own periods are the ones the price was rated from.
            // They used to be a single charging period covering the whole hour
            // while the cost charged half an hour of *occupancy* — which the
            // record accounted none of, and which `emob_cdr::validate` blocks
            // and `InvoiceBuilder` now refuses (D232).
            periods: vec![
                ChargingPeriod {
                    quarter_hour: QuarterHour::containing(at(0)),
                    start: at(0),
                    end: at(30),
                    energy: Energy::from_kwh(dec("29.500")).unwrap(),
                    activity: Activity::Charging,
                    provenance: Provenance::Measured,
                },
                ChargingPeriod {
                    quarter_hour: QuarterHour::containing(at(30)),
                    start: at(30),
                    end: at(60),
                    energy: Energy::ZERO,
                    activity: Activity::Parked,
                    provenance: Provenance::Measured,
                },
            ],
            total_energy: Energy::from_kwh(dec("29.500")).unwrap(),
            direction: Direction::Import,
            // Signed, because `[MessEG §33]` lets a measured value be used in
            // German commercial dealings only where it is traceable to the
            // measurement, and requires an invoice resting on one to be
            // checkable by the person it is addressed to. Every fixture here
            // bills German kilowatt-hours, so every one of them needs a record
            // behind it (D232).
            evidence: Some(emob_cdr::EvidenceRef {
                encoding_method: "OCMF".into(),
                payload_digests: vec![[1u8; 32]],
                identification_strength: emob_core::IdentificationStrength::Trusted,
                energy_billable: true,
                duration_billable: true,
                direction: Some(Direction::Import),
                compensated_loss: None,
                tariff_changes: Vec::new(),
            }),
            cost: Some(Cost {
                tariff_id: "t".parse().unwrap(),
                tariff_fingerprint: tariff.fingerprint(),
                rated,
                reservation: None,
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

    /// A gross energy-only invoice under a tariff the caller shapes — the
    /// bounds are what these tests are about.
    fn bounded(min: Option<Decimal>, max: Option<Decimal>, kwh: &str) -> Invoice {
        use emob_cdr::{Cdr, CdrKey, ChargingPeriod, Cost};
        use emob_core::{Direction, Energy, PartyId, QuarterHour};
        use emob_session::{AuthPath, Provenance};
        use emob_tariff::{
            Chargeable, Dimension, Period, PriceComponent, Tariff, TariffKind, rate,
        };
        use time::macros::datetime;

        let at = |m: i64| datetime!(2026-06-01 10:00 +2) + time::Duration::minutes(m);
        let mut tariff = Tariff::simple(
            "t".parse().unwrap(),
            Currency::EUR,
            TariffKind::AdHoc,
            emob_core::TimeZone::new("Europe/Berlin").unwrap(),
            vec![PriceComponent::new(Dimension::Energy, dec("0.49")).with_vat(dec("19"))],
        );
        // `Tariff::simple` quotes gross prices, so the bounds are gross.
        tariff.min_price = min.map(PriceLimit::gross);
        tariff.max_price = max.map(PriceLimit::gross);

        let energy = Energy::from_kwh(dec(kwh)).unwrap();
        let rated = rate(
            &tariff,
            &Chargeable::new(vec![Period::charging(at(0), at(30), energy)]).unwrap(),
        );
        let cdr = Cdr {
            key: CdrKey {
                party: PartyId::new("DE", "ABC").unwrap(),
                id: "c-b".parse().unwrap(),
            },
            reservation: None,
            session_id: "s-b".parse().unwrap(),
            evse_id: "DE*AB7*E840*6487".parse().unwrap(),
            started_at: at(0),
            ended_at: at(30),
            auth_path: AuthPath::AdHoc,
            authorization_reference: None,
            clock: emob_core::ClockResolution::conforming(),
            periods: vec![ChargingPeriod {
                quarter_hour: QuarterHour::containing(at(0)),
                start: at(0),
                end: at(30),
                energy,
                activity: Activity::Charging,
                provenance: Provenance::Measured,
            }],
            total_energy: energy,
            direction: Direction::Import,
            // Signed, because `[MessEG §33]` lets a measured value be used in
            // German commercial dealings only where it is traceable to the
            // measurement, and requires an invoice resting on one to be
            // checkable by the person it is addressed to. Every fixture here
            // bills German kilowatt-hours, so every one of them needs a record
            // behind it (D232).
            evidence: Some(emob_cdr::EvidenceRef {
                encoding_method: "OCMF".into(),
                payload_digests: vec![[1u8; 32]],
                identification_strength: emob_core::IdentificationStrength::Trusted,
                energy_billable: true,
                duration_billable: true,
                direction: Some(Direction::Import),
                compensated_loss: None,
                tariff_changes: Vec::new(),
            }),
            cost: Some(Cost {
                tariff_id: "t".parse().unwrap(),
                tariff_fingerprint: tariff.fingerprint(),
                rated,
                reservation: None,
            }),
            supersedes: None,
        };

        InvoiceBuilder::new(
            "R-2",
            date!(2026 - 07 - 01),
            (date!(2026 - 06 - 01), date!(2026 - 06 - 30)),
            Counterparty::new(
                "CPO",
                "Musterstadt",
                TaxStatus::business("DE", "DE123456789"),
            ),
            Counterparty::new("Driver", "Beispielstadt", TaxStatus::consumer("DE")),
        )
        .taxed_as(standard())
        .record(&cdr)
        .due_on(date!(2026 - 07 - 15))
        .build()
        .unwrap()
        .value
    }

    #[test]
    fn a_capped_session_still_balances_and_the_cap_comes_off_the_revenue_it_belongs_to() {
        // A cap is a document level allowance rather than a line, so BT-106 and
        // BT-109 are two different figures the moment one moves a session.
        // Booking the lines would credit twelve euros of revenue against ten of
        // receivable, and the entry would not balance at all.
        let invoice = bounded(None, Some(dec("10.00")), "29.500");
        assert_eq!(invoice.line_total().to_string(), "12.15 EUR");
        assert_eq!(invoice.taxable_total().to_string(), "8.40 EUR");
        assert_eq!(invoice.gross_total().to_string(), "10.00 EUR");

        let books = postings_for(&invoice);
        assert!(books.balances(), "{books:?}");
        assert_eq!(books.debits().to_string(), "10.00 EUR");

        // The session was energy, so the cap came off energy revenue.
        let energy = books
            .postings
            .iter()
            .find(|posting| posting.role == Role::EnergyRevenue)
            .expect("an energy session credits energy revenue");
        assert_eq!(energy.side, Side::Credit);
        assert_eq!(energy.amount, dec("8.40"), "12.15 earned, 3.75 capped back");
        assert!(
            !books.roles().contains(&&Role::ServiceRevenue),
            "nothing was sold but energy: {books:?}"
        );
    }

    #[test]
    fn a_minimum_on_a_session_that_delivered_nothing_is_its_own_line() {
        // The load-bearing minimum-charge case: a driver plugged in, drew
        // nothing and owes the minimum. There is no priced dimension for a
        // document level charge to sit beside, and `BR-16` requires an invoice
        // to have at least one line — so the bound is the line, and it is a
        // per-session fee, which the books read as service revenue.
        let invoice = bounded(Some(dec("5.95")), None, "0");
        assert_eq!(invoice.lines.len(), 1, "{:?}", invoice.lines);
        assert!(
            invoice.adjustments.is_empty(),
            "a charge cannot stand alone: {:?}",
            invoice.adjustments
        );
        assert_eq!(invoice.lines[0].unit_code(), "C62");
        assert!(invoice.lines[0].description.contains("minimum charge"));
        assert!(invoice.lines[0].reconciles());
        assert_eq!(invoice.line_total().to_string(), "5.00 EUR");
        assert_eq!(invoice.gross_total().to_string(), "5.95 EUR");
        assert!(invoice.reconciles());

        let books = postings_for(&invoice);
        assert!(books.balances(), "{books:?}");
        let service = books
            .postings
            .iter()
            .find(|posting| posting.role == Role::ServiceRevenue)
            .expect("a per-session fee is service revenue");
        assert_eq!(service.side, Side::Credit);
        assert_eq!(service.amount, dec("5.00"));

        // …and the document the standard judges is valid, which one with no
        // lines at all is not.
        let crossed =
            crate::en16931::to_en16931(&invoice, crate::en16931::Specification::Core).unwrap();
        assert!(
            crossed.value.is_valid(),
            "{:?}",
            crossed.value.reasons().collect::<Vec<_>>()
        );
    }

    /// One record on the invoice, built to order: `kwh` of energy, `minutes` of
    /// occupancy, and the tariff's own bounds.
    #[allow(clippy::too_many_arguments)]
    fn record_for(
        id: &str,
        kwh: &str,
        parked_minutes: i64,
        min: Option<Decimal>,
        max: Option<Decimal>,
    ) -> emob_cdr::Cdr {
        use emob_cdr::{Cdr, CdrKey, ChargingPeriod, Cost};
        use emob_core::{Direction, Energy, PartyId, QuarterHour};
        use emob_session::{AuthPath, Provenance};
        use emob_tariff::{
            Chargeable, Dimension, Period, PriceComponent, Tariff, TariffKind, rate,
        };
        use time::macros::datetime;

        let at = |m: i64| datetime!(2026-06-01 10:00 +2) + time::Duration::minutes(m);
        let mut tariff = Tariff::simple(
            "t".parse().unwrap(),
            Currency::EUR,
            TariffKind::AdHoc,
            emob_core::TimeZone::new("Europe/Berlin").unwrap(),
            vec![
                PriceComponent::new(Dimension::Energy, dec("0.49")).with_vat(dec("19")),
                PriceComponent::new(Dimension::ParkingTime, dec("6.00")).with_vat(dec("19")),
            ],
        );
        // `Tariff::simple` quotes gross prices, so the bounds are gross.
        tariff.min_price = min.map(PriceLimit::gross);
        tariff.max_price = max.map(PriceLimit::gross);

        let energy = Energy::from_kwh(dec(kwh)).unwrap();
        let mut periods = Vec::new();
        let mut cdr_periods = Vec::new();
        let charging_minutes = if energy.is_zero() { 0 } else { 30 };
        if charging_minutes > 0 {
            periods.push(Period::charging(at(0), at(charging_minutes), energy));
            cdr_periods.push(ChargingPeriod {
                quarter_hour: QuarterHour::containing(at(0)),
                start: at(0),
                end: at(charging_minutes),
                energy,
                activity: Activity::Charging,
                provenance: Provenance::Measured,
            });
        }
        let mut cursor = charging_minutes;
        if parked_minutes > 0 {
            let (from, to) = (at(cursor), at(cursor + parked_minutes));
            periods.push(Period::parked(from, to));
            cdr_periods.push(ChargingPeriod {
                quarter_hour: QuarterHour::containing(from),
                start: from,
                end: to,
                energy: Energy::ZERO,
                activity: Activity::Parked,
                provenance: Provenance::Measured,
            });
            cursor += parked_minutes;
        }
        let rated = rate(&tariff, &Chargeable::new(periods).unwrap());

        Cdr {
            key: CdrKey {
                party: PartyId::new("DE", "ABC").unwrap(),
                id: id.parse().unwrap(),
            },
            reservation: None,
            session_id: format!("s-{id}").parse().unwrap(),
            evse_id: "DE*AB7*E840*6487".parse().unwrap(),
            started_at: at(0),
            ended_at: at(cursor),
            auth_path: AuthPath::AdHoc,
            authorization_reference: None,
            clock: emob_core::ClockResolution::conforming(),
            periods: cdr_periods,
            total_energy: energy,
            direction: Direction::Import,
            // Signed, because `[MessEG §33]` lets a measured value be used in
            // German commercial dealings only where it is traceable to the
            // measurement, and requires an invoice resting on one to be
            // checkable by the person it is addressed to. Every fixture here
            // bills German kilowatt-hours, so every one of them needs a record
            // behind it (D232).
            evidence: Some(emob_cdr::EvidenceRef {
                encoding_method: "OCMF".into(),
                payload_digests: vec![[1u8; 32]],
                identification_strength: emob_core::IdentificationStrength::Trusted,
                energy_billable: true,
                duration_billable: true,
                direction: Some(Direction::Import),
                compensated_loss: None,
                tariff_changes: Vec::new(),
            }),
            cost: Some(Cost {
                tariff_id: "t".parse().unwrap(),
                tariff_fingerprint: tariff.fingerprint(),
                rated,
                reservation: None,
            }),
            supersedes: None,
        }
    }

    #[test]
    fn a_bound_is_booked_against_its_own_record_and_not_against_the_month() {
        // Twenty-nine kilowatt-hours in one session, and a second session that
        // delivered nothing and sat there for half an hour under a cap. Energy
        // dominates the *document* by a wide margin; the capped record is pure
        // occupancy.
        //
        // Asked of the document — which this did — the cap comes off **energy
        // revenue**, for a session that delivered no energy. And the same
        // bound's VAT rate came from the occupancy line, because
        // `emob_tariff::Adjustment::vat` asks per session: one question, two
        // scopes, two crates. Every fixture was a single-record invoice, which
        // is the only shape where the two agree (D214).
        let energy_session = record_for("c-e", "29.500", 0, None, None);
        let parked_session = record_for("c-p", "0", 30, None, Some(dec("1.00")));

        let invoice = InvoiceBuilder::new(
            "R-2",
            date!(2026 - 07 - 01),
            (date!(2026 - 06 - 01), date!(2026 - 06 - 30)),
            Counterparty::new(
                "CPO",
                "Musterstadt",
                TaxStatus::business("DE", "DE123456789"),
            ),
            Counterparty::new("Driver", "Beispielstadt", TaxStatus::consumer("DE")),
        )
        .taxed_as(standard())
        .record(&energy_session)
        .record(&parked_session)
        .due_on(date!(2026 - 07 - 15))
        .build()
        .unwrap()
        .value;

        // The cap is a document level allowance on the *occupancy* record.
        let adjustment = invoice
            .adjustments
            .iter()
            .find(|a| a.kind == DocumentAdjustmentKind::Allowance)
            .expect("a cap is an allowance");
        assert_eq!(adjustment.cdr, parked_session.key);

        let books = postings_for(&invoice);
        assert!(books.balances(), "{books:?}");

        // Energy revenue is exactly what the energy session earned — the cap
        // did not reach it.
        let energy = books
            .postings
            .iter()
            .find(|posting| posting.role == Role::EnergyRevenue)
            .expect("an energy session credits energy revenue");
        let energy_line: Decimal = invoice
            .lines
            .iter()
            .filter(|line| line.cdr == energy_session.key)
            .map(|line| line.net)
            .sum();
        assert_eq!(energy.side, Side::Credit);
        assert_eq!(
            energy.amount, energy_line,
            "the cap belongs to the other record: {books:?}"
        );

        // …and the cap came off service revenue, which is what was capped.
        let service = books
            .postings
            .iter()
            .find(|posting| posting.role == Role::ServiceRevenue)
            .expect("the occupancy line is service revenue");
        let service_line: Decimal = invoice
            .lines
            .iter()
            .filter(|line| line.cdr == parked_session.key)
            .map(|line| line.net)
            .sum();
        assert_eq!(service.side, Side::Credit);
        assert_eq!(service.amount, service_line - adjustment.amount);
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
