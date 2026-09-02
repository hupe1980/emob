//! Who owes the VAT on a charging session, and in which country.
//!
//! # Why an EV platform cannot treat this as a field somebody fills in
//!
//! Every other quantity in this workspace comes out of a meter or a tariff. The
//! VAT category does not: it is a **conclusion** drawn from three facts about
//! the parties and one fact about the supply, and getting it wrong is not a
//! rounding error. A German CPO that puts 19 % on a settlement invoice to a
//! French eMSP has charged tax it may not charge and the partner cannot reclaim;
//! one that omits it where it was due owes it out of its own margin.
//!
//! Platforms model it as a string on the customer record. Here it is a function
//! of the parties, so the same two counterparties always produce the same
//! category and the reason is on the invoice.
//!
//! # What is being supplied
//!
//! Recharging an electric vehicle is a **single composite supply of goods** —
//! the electricity — and not a bundle of services with electricity attached.
//! The Court of Justice settled that in Case C‑282/22 (*Dyrektor Krajowej
//! Informacji Skarbowej*, 20 April 2023): the transfer of electricity is the
//! characteristic element, and the access to the equipment, the technical
//! support and the app around it are ancillary to it. German law reaches the
//! same place from the other side, because electricity is a good for VAT
//! purposes under `[UStG §3]`.
//!
//! That conclusion is what makes the rest of this module apply at all: the place
//! of supply of *electricity* has its own rule, and the place of supply of a
//! *service* does not.
//!
//! # The rule the roaming model runs into
//!
//! `[UStG §3g]` — Article 38 of the VAT Directive — says that a supply of
//! electricity **to a reseller** is made where that reseller is established. An
//! e-mobility provider buying sessions from a charge point operator in order to
//! sell them on to its own drivers is exactly a reseller: it does not consume
//! the electricity, it resells it. So a German CPO settling with a French eMSP
//! is not making a German supply at all — the place of supply is France, German
//! VAT does not arise, and the invoice states the reverse charge with the
//! partner's own VAT identifier on it `[UStG §13b]`.
//!
//! The driver leg is the ordinary one: the eMSP supplies its own customer, and
//! that supply *is* where the electricity is consumed.
//!
//! **This is the case the ad-hoc leg does not share.** A driver paying at the
//! point with a card is not a reseller, so `[UStG §3g]` never engages and the
//! supply is taxed where the charge point stands, whatever passport the driver
//! carries. Two sessions at one post, one minute apart, can therefore carry
//! different VAT — which is why the treatment is decided per invoice from the
//! parties rather than per station from a configuration field.
//!
//! # What this module does not do
//!
//! It does not decide whether a party is a reseller, whether its VAT
//! identifier is valid today, or whether a small-business exemption applies.
//! Those are facts about a counterparty that live in a customer master and are
//! checked against VIES, which is I/O. What it does is take those facts as
//! stated and turn them into the one category code an invoice may carry — and
//! refuse the combinations that have no code, rather than picking the nearest.

use rust_decimal::Decimal;

use crate::error::BillingError;

/// A VAT category from UNCL 5305 — **`en16931`'s own type**, re-exported.
///
/// # Why the codes are not this crate's
///
/// `en16931::codes::VatCategory` carries all ten codes with four predicates
/// generated from the CEN validation artefacts, and two of them decide whether a
/// document is valid at all:
///
/// | | `carries_tax` | `requires_exemption_reason` | `forbids_exemption_reason` | `states_rate` |
/// |---|---|---|---|---|
/// | `S` Standard | ✓ | | ✓ | ✓ |
/// | `AE` Reverse charge | | ✓ | | ✓ |
/// | `O` Outside scope | | ✓ | | **✗** |
///
/// `forbids_exemption_reason` is a different question from "does not require
/// one" — `S` *forbids* a reason. And `states_rate` is false for exactly one
/// category, `O`, which is what `[UStG §3g]` produces for a reseller outside the
/// Union: `BR-O-05` refuses a line carrying BT-152 at all, and a rate of zero is
/// carrying it.
///
/// So the codes and their rules come from the artefacts, and this crate states
/// only what `en16931` cannot: which category two *parties* produce, in
/// [`TaxTreatment::decide`]. The tax law is domain knowledge; the code list is a
/// table (D183).
pub use en16931::codes::VatCategory;

/// Whether this category levies tax on this invoice — `en16931`'s
/// `carries_tax`, under the name the rest of this crate reads it by.
///
/// False for every category but `S`, `L`, `M` and `B`, and that is the point:
/// under a reverse charge the tax exists and is *somebody else's* to declare, so
/// the invoice's own tax amount is zero and its total is its net.
#[must_use]
pub const fn levies_tax(category: VatCategory) -> bool {
    category.carries_tax()
}

/// What a counterparty is, for the purpose of deciding the tax.
///
/// Three facts, and every one of them is somebody else's to establish. This type
/// is where they arrive, already established.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TaxStatus {
    /// The ISO 3166-1 alpha-2 country the party is established in.
    pub country: String,
    /// Its VAT identifier, when it has one.
    ///
    /// `None` for a consumer. Not validated here — a syntactically well-formed
    /// identifier that VIES does not know is worse than none at all, and finding
    /// that out is a network call.
    pub vat_identifier: Option<String>,
    /// Whether this party buys electricity in order to resell it — the test
    /// `[UStG §3g]` turns on.
    ///
    /// True for an e-mobility provider buying sessions through roaming. False
    /// for a driver, and false for a fleet operator whose vehicles consume what
    /// it buys.
    pub reseller: bool,
}

impl TaxStatus {
    /// A driver: established somewhere, no VAT identifier, consumes what they
    /// buy.
    #[must_use]
    pub fn consumer(country: impl Into<String>) -> Self {
        Self {
            country: country.into(),
            vat_identifier: None,
            reseller: false,
        }
    }

    /// A business that consumes what it buys — a fleet, an employer.
    #[must_use]
    pub fn business(country: impl Into<String>, vat_identifier: impl Into<String>) -> Self {
        Self {
            country: country.into(),
            vat_identifier: Some(vat_identifier.into()),
            reseller: false,
        }
    }

    /// An e-mobility provider buying sessions to sell on — the reseller
    /// `[UStG §3g]` names.
    #[must_use]
    pub fn reseller(country: impl Into<String>, vat_identifier: impl Into<String>) -> Self {
        Self {
            country: country.into(),
            vat_identifier: Some(vat_identifier.into()),
            reseller: true,
        }
    }

    /// Whether this party is established inside the European Union.
    ///
    /// A list rather than a lookup, because it decides whether a supply leaves
    /// the Union, and a platform that got that from a runtime table would answer
    /// a two-year-old dispute with today's membership.
    #[must_use]
    pub fn in_the_union(&self) -> bool {
        const MEMBER_STATES: [&str; 27] = [
            "AT", "BE", "BG", "CY", "CZ", "DE", "DK", "EE", "ES", "FI", "FR", "GR", "HR", "HU",
            "IE", "IT", "LT", "LU", "LV", "MT", "NL", "PL", "PT", "RO", "SE", "SI", "SK",
        ];
        MEMBER_STATES.contains(&self.country.to_ascii_uppercase().as_str())
    }
}

/// The standard VAT rates in force, by country, at the moment an invoice is
/// issued.
///
/// # Why a table and not one number
///
/// [`TaxTreatment::decide`] works out **where** a supply is taxed before it
/// works out at what rate, and the two are not always the same country. A German
/// operator selling sessions from charge points in France to a German e-mobility
/// provider is taxed in Germany, because `[UStG §3g]` moves the place of supply
/// to where the *reseller* is established — so the invoice needs the German rate
/// even though every kilowatt-hour on it was drawn in France.
///
/// A single `standard_rate` argument cannot express that: it is the rate of one
/// country, and the branch that moves the place of supply moves it to a
/// different one. Handing that one figure through produces an invoice whose
/// stated place of supply and stated rate disagree — 20 % French VAT under a
/// German place of supply — which is a document that reconciles against nothing
/// and is wrong by the difference between two rates.
///
/// So the caller states the rates it knows, each with its country, and
/// [`TaxTreatment::decide`] looks up the one that belongs to the place of supply
/// it derived. Rates are arguments rather than a lookup for the reason every
/// instant in this workspace is: rates move, and an invoice replayed two years
/// later has to reproduce the rate that was in force rather than today's.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct VatRates(Vec<(String, Decimal)>);

impl VatRates {
    /// No rates at all — enough for an invoice that levies no tax.
    #[must_use]
    pub const fn new() -> Self {
        Self(Vec::new())
    }

    /// The standard rate in force in one country.
    ///
    /// Stating a country twice replaces the earlier figure, so a caller
    /// assembling a table from several sources cannot end up with two answers
    /// for one place of supply.
    #[must_use]
    pub fn at(mut self, country: impl AsRef<str>, rate: Decimal) -> Self {
        let country = country.as_ref().to_ascii_uppercase();
        match self.0.iter_mut().find(|(c, _)| *c == country) {
            Some(entry) => entry.1 = rate,
            None => self.0.push((country, rate)),
        }
        self
    }

    /// The rate for a country, when the caller stated one.
    #[must_use]
    pub fn rate_for(&self, country: &str) -> Option<Decimal> {
        let country = country.to_ascii_uppercase();
        self.0
            .iter()
            .find(|(c, _)| *c == country)
            .map(|(_, rate)| *rate)
    }

    /// The countries a rate was stated for, for a diagnostic that has to say
    /// what the caller did supply.
    fn countries(&self) -> Vec<&str> {
        self.0.iter().map(|(c, _)| c.as_str()).collect()
    }
}

/// The tax treatment of one invoice, and why.
///
/// Carried on the invoice rather than recomputed by each consumer, so the
/// reason travels with the document — which is what `BT-120` asks for and what a
/// tax audit asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TaxTreatment {
    /// The category every line of the invoice carries.
    pub category: VatCategory,
    /// The rate, as a percentage. Zero for every category but
    /// [`VatCategory::Standard`].
    pub rate: Decimal,
    /// Which country's VAT applies — the place of supply.
    pub place_of_supply: String,
    /// The sentence that goes on the invoice, and the reason a reader needs.
    ///
    /// Mandatory in EN 16931 for four of the five categories, and mandatory in
    /// German law for a reverse charge `[UStG §14a]`.
    pub reason: Option<String>,
}

impl TaxTreatment {
    /// Decide the treatment from the two parties, where the electricity was
    /// drawn, and the standard rates in force.
    ///
    /// `point_country` is where the charge point stands, which is where an
    /// ordinary supply of electricity is taxed. `rates` is consulted only for
    /// the country this function concludes the supply is taxed in — which is
    /// `point_country` for every supply but the one `[UStG §3g]` moves. See
    /// [`VatRates`].
    ///
    /// # Errors
    ///
    /// [`BillingError::NoTaxTreatment`] where the combination has no category —
    /// a reverse charge with no VAT identifier on one side of it, above all,
    /// because EN 16931's `BR-AE-*` refuses that invoice and so does the
    /// Finanzamt.
    ///
    /// [`BillingError::NoVatRate`] where the supply is standard-rated and
    /// `rates` states no rate for the place of supply. Refused rather than
    /// defaulted: a rate nobody supplied is not zero, and an invoice that
    /// silently under-declares its own VAT is the failure this module exists to
    /// prevent.
    pub fn decide(
        seller: &TaxStatus,
        buyer: &TaxStatus,
        point_country: &str,
        rates: &VatRates,
    ) -> Result<Self, BillingError> {
        let point = point_country.to_ascii_uppercase();
        let buyer_country = buyer.country.to_ascii_uppercase();
        let seller_country = seller.country.to_ascii_uppercase();

        // `[UStG §3g]`: a supply of electricity to a reseller is made where the
        // reseller is established. This is the roaming leg, and it is the only
        // branch in which the charge point's own country stops deciding.
        if buyer.reseller {
            if buyer_country == seller_country {
                // A domestic reseller. The place of supply moved to a country
                // the seller is already registered in, so nothing is shifted —
                // it is an ordinary domestic supply, taxed at **that** country's
                // rate and not at the rate where the point stands. The two are
                // the same for an operator that only operates at home, and
                // different for one that does not.
                return Ok(Self {
                    rate: standard_rate(rates, &buyer_country, &point)?,
                    category: VatCategory::Standard,
                    place_of_supply: buyer_country,
                    reason: None,
                });
            }
            if !buyer.in_the_union() {
                // The place of supply is outside the Union, so no member
                // state's VAT arises at all: outside scope, not a zero-rated
                // export of goods. See `VatCategory::OutOfScope`.
                return Ok(Self {
                    category: VatCategory::OutOfScope,
                    rate: Decimal::ZERO,
                    place_of_supply: buyer_country.clone(),
                    reason: Some(format!(
                        "Place of supply {buyer_country} under [UStG §3g] (electricity supplied \
                         to a reseller established outside the Union): outside the scope of EU VAT"
                    )),
                });
            }

            // Cross-border inside the Union: the recipient accounts for the tax.
            // Both identifiers are what makes that statement checkable, and
            // EN 16931's BR-AE-2 and BR-AE-3 refuse the document without them —
            // so the refusal happens here, where the reason can be stated, and
            // not as an unexplained validation finding two layers on.
            let (Some(seller_vat), Some(buyer_vat)) =
                (&seller.vat_identifier, &buyer.vat_identifier)
            else {
                return Err(BillingError::NoTaxTreatment {
                    reason: format!(
                        "a supply of electricity to a reseller established in {buyer_country} is \
                         taxed there [UStG §3g] and the recipient accounts for the tax \
                         [UStG §13b], which requires a VAT identifier on both parties: the \
                         seller {} and the buyer {}",
                        described(seller.vat_identifier.as_deref()),
                        described(buyer.vat_identifier.as_deref()),
                    ),
                });
            };
            return Ok(Self {
                category: VatCategory::ReverseCharge,
                rate: Decimal::ZERO,
                place_of_supply: buyer_country.clone(),
                reason: Some(format!(
                    "Reverse charge — place of supply {buyer_country} under [UStG §3g] \
                     (electricity supplied to a reseller); the recipient {buyer_vat} accounts \
                     for the VAT [UStG §13b]. Supplier {seller_vat}"
                )),
            });
        }

        // Everything else is consumed where it was drawn, which for a charging
        // session is where the charge point stands. A driver's passport does not
        // move it, and neither does a fleet's VAT registration: only a reseller
        // moves the place of supply, and this party is not one.
        Ok(Self {
            rate: standard_rate(rates, &point, &point)?,
            category: VatCategory::Standard,
            place_of_supply: point,
            reason: None,
        })
    }

    /// A treatment stated outright, for a caller whose tax engine already
    /// decided.
    ///
    /// The escape hatch, and named so it is visible in a diff: nothing checks
    /// that the category and the rate agree, or that the reason a category
    /// requires is present — [`crate::en16931::to_en16931`] does that
    /// against the standard's own rules, and that is where a stated treatment
    /// gets checked.
    #[must_use]
    pub fn stated(
        category: VatCategory,
        rate: Decimal,
        place_of_supply: impl Into<String>,
        reason: Option<String>,
    ) -> Self {
        Self {
            category,
            rate,
            place_of_supply: place_of_supply.into(),
            reason,
        }
    }
}

/// The standard rate at a place of supply, or a refusal that says what was
/// asked for and what was supplied.
fn standard_rate(
    rates: &VatRates,
    place_of_supply: &str,
    point_country: &str,
) -> Result<Decimal, BillingError> {
    rates
        .rate_for(place_of_supply)
        .ok_or_else(|| BillingError::NoVatRate {
            place_of_supply: place_of_supply.to_owned(),
            point_country: point_country.to_owned(),
            stated_for: rates.countries().join(", "),
        })
}

fn described(identifier: Option<&str>) -> String {
    identifier.map_or_else(|| "has none".to_owned(), |id| format!("has {id}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::prelude::FromStr;

    fn dec(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    fn cpo() -> TaxStatus {
        TaxStatus::business("DE", "DE123456789")
    }

    #[test]
    fn a_driver_is_taxed_where_the_charge_point_stands() {
        // The ordinary ad-hoc leg. The driver's own country never enters into
        // it: electricity is consumed where it is drawn.
        let dutch_driver = TaxStatus::consumer("NL");
        let t = TaxTreatment::decide(
            &cpo(),
            &dutch_driver,
            "DE",
            &VatRates::new().at("DE", dec("19")),
        )
        .unwrap();
        assert_eq!(t.category, VatCategory::Standard);
        assert_eq!(t.rate, dec("19"));
        assert_eq!(t.place_of_supply, "DE");
        assert_eq!(t.reason, None, "a standard rate explains itself");
    }

    #[test]
    fn a_fleet_is_not_a_reseller_however_registered_it_is() {
        // The distinction `[UStG §3g]` turns on is *resale*, not registration.
        // A fleet with a French VAT identifier charging in Germany consumes what
        // it buys, so the supply stays German.
        let fleet = TaxStatus::business("FR", "FR12345678901");
        let t = TaxTreatment::decide(&cpo(), &fleet, "DE", &VatRates::new().at("DE", dec("19")))
            .unwrap();
        assert_eq!(t.category, VatCategory::Standard);
        assert_eq!(t.place_of_supply, "DE");
    }

    #[test]
    fn a_cross_border_reseller_moves_the_place_of_supply_and_the_tax_with_it() {
        // The roaming leg, and the one every platform gets wrong by putting the
        // charge point's rate on it.
        let emsp = TaxStatus::reseller("FR", "FR12345678901");
        let t = TaxTreatment::decide(&cpo(), &emsp, "DE", &VatRates::new().at("DE", dec("19")))
            .unwrap();
        assert_eq!(t.category, VatCategory::ReverseCharge);
        assert_eq!(
            t.rate,
            Decimal::ZERO,
            "the tax is the recipient's to declare"
        );
        assert_eq!(t.place_of_supply, "FR");
        let reason = t.reason.unwrap();
        assert!(reason.contains("Reverse charge"), "{reason}");
        assert!(reason.contains("FR12345678901"), "{reason}");
        assert!(reason.contains("DE123456789"), "{reason}");
    }

    #[test]
    fn a_domestic_reseller_is_still_a_domestic_supply() {
        // §3g moves the place of supply to where the reseller is established,
        // which here is the country the seller is already in. Nothing shifts,
        // and treating it as a reverse charge would drop tax that is due.
        let german_emsp = TaxStatus::reseller("DE", "DE987654321");
        let t = TaxTreatment::decide(
            &cpo(),
            &german_emsp,
            "DE",
            &VatRates::new().at("DE", dec("19")),
        )
        .unwrap();
        assert_eq!(t.category, VatCategory::Standard);
        assert_eq!(t.rate, dec("19"));
        assert_eq!(t.place_of_supply, "DE");
    }

    #[test]
    fn a_reverse_charge_without_both_identifiers_is_refused_here_rather_than_later() {
        // EN 16931's BR-AE-2 and BR-AE-3 refuse this invoice anyway. Refusing it
        // where the rule lives means the message names the missing identifier
        // instead of naming a rule id.
        let anonymous = TaxStatus {
            country: "FR".into(),
            vat_identifier: None,
            reseller: true,
        };
        let err = TaxTreatment::decide(
            &cpo(),
            &anonymous,
            "DE",
            &VatRates::new().at("DE", dec("19")),
        )
        .unwrap_err();
        assert!(err.to_string().contains("has none"), "{err}");
        assert!(err.to_string().contains("[UStG §3g]"), "{err}");
    }

    #[test]
    fn a_reseller_outside_the_union_takes_the_supply_with_it() {
        let swiss = TaxStatus::reseller("CH", "CHE-123.456.789");
        let t = TaxTreatment::decide(&cpo(), &swiss, "DE", &VatRates::new().at("DE", dec("19")))
            .unwrap();
        // Outside scope, not a zero-rated export: the place of supply left the
        // Union, so no member state's VAT arises at all.
        assert_eq!(t.category, VatCategory::OutOfScope);
        assert_eq!(t.category.code(), "O");
        assert_eq!(t.rate, Decimal::ZERO);
        assert_eq!(t.place_of_supply, "CH");
        assert!(t.reason.unwrap().contains("outside the scope"));
    }

    #[test]
    fn membership_is_a_list_because_a_replay_must_not_read_todays_table() {
        assert!(TaxStatus::consumer("DE").in_the_union());
        assert!(TaxStatus::consumer("hr").in_the_union(), "case-insensitive");
        assert!(!TaxStatus::consumer("GB").in_the_union(), "left in 2020");
        assert!(!TaxStatus::consumer("NO").in_the_union());
        assert!(!TaxStatus::consumer("CH").in_the_union());
    }

    #[test]
    fn the_category_predicates_are_en16931s_and_not_a_reading_of_them() {
        // The four this crate reaches for, and the two it used to have. `S`
        // *forbids* an exemption reason where `AE` and `O` require one, which is
        // a distinction the hand-rolled enum could not express — and `O` states
        // no rate at all, which is the one that made a Swiss settlement invalid.
        assert!(VatCategory::Standard.carries_tax());
        assert!(VatCategory::Standard.forbids_exemption_reason());
        assert!(VatCategory::Standard.states_rate());

        for category in [
            VatCategory::ReverseCharge,
            VatCategory::OutOfScope,
            VatCategory::Exempt,
        ] {
            assert!(!category.carries_tax(), "{}", category.code());
            assert!(category.requires_exemption_reason(), "{}", category.code());
        }

        // The only category in the whole of UNCL 5305 that states no rate.
        assert!(!VatCategory::OutOfScope.states_rate());
        for category in VatCategory::ALL {
            assert_eq!(
                category.states_rate(),
                category != VatCategory::OutOfScope,
                "{}",
                category.code()
            );
        }

        assert_eq!(VatCategory::ReverseCharge.code(), "AE");
        assert_eq!(VatCategory::OutOfScope.code(), "O");
    }

    #[test]
    fn the_rate_belongs_to_the_place_of_supply_and_not_to_the_charge_point() {
        // A German operator running points in France, selling to a German
        // e-mobility provider. `[UStG §3g]` taxes that supply in Germany
        // because that is where the reseller is established, so the invoice
        // carries 19 % — even though every kilowatt-hour on it was drawn under
        // a 20 % regime.
        let german_emsp = TaxStatus::reseller("DE", "DE811111111");
        let rates = VatRates::new().at("FR", dec("20")).at("DE", dec("19"));
        let t = TaxTreatment::decide(&cpo(), &german_emsp, "FR", &rates).unwrap();
        assert_eq!(t.place_of_supply, "DE");
        assert_eq!(
            t.rate,
            dec("19"),
            "the rate has to match the place of supply"
        );

        // …and the ad-hoc leg at the same points is taxed where they stand.
        let driver = TaxStatus::consumer("DE");
        let t = TaxTreatment::decide(&cpo(), &driver, "FR", &rates).unwrap();
        assert_eq!(t.place_of_supply, "FR");
        assert_eq!(t.rate, dec("20"));
    }

    #[test]
    fn a_standard_rated_supply_with_no_rate_for_its_place_of_supply_is_refused() {
        // The failure the table exists to make visible: the caller stated the
        // rate where the points stand and the supply is taxed somewhere else.
        let german_emsp = TaxStatus::reseller("DE", "DE811111111");
        let err = TaxTreatment::decide(
            &cpo(),
            &german_emsp,
            "FR",
            &VatRates::new().at("FR", dec("20")),
        )
        .unwrap_err();
        let message = err.to_string();
        assert!(message.contains("taxed in DE"), "{message}");
        assert!(message.contains("stand in FR"), "{message}");
        assert!(message.contains("FR"), "{message}");
    }

    #[test]
    fn stating_a_country_twice_leaves_one_answer() {
        let rates = VatRates::new().at("DE", dec("16")).at("de", dec("19"));
        assert_eq!(rates.rate_for("DE"), Some(dec("19")));
        assert_eq!(rates.rate_for("de"), Some(dec("19")));
        assert_eq!(rates.rate_for("FR"), None);
    }
}
