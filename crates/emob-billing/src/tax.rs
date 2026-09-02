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

/// A VAT category from UNCL 5305, as EN 16931 uses it.
///
/// Five of the ten, which are the five a European charging platform can reach.
/// The rest — the Canary Islands' IGIC, Ceuta and Melilla's IPSI, Italy's split
/// payment, and the zero rate that only a handful of member states operate — are
/// deliberately absent: an enum arm nothing can select is a code path nothing
/// tests, and a platform that grows into one of them adds it with the rule that
/// selects it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum VatCategory {
    /// `S` — standard rated. The tax is charged and shown.
    Standard,
    /// `AE` — reverse charge: the recipient accounts for the tax.
    ///
    /// The rate on the breakdown is zero and both parties' VAT identifiers are
    /// mandatory, which is `BR-AE-*` in EN 16931 and `[UStG §14a]` in German
    /// law saying the same thing twice.
    ReverseCharge,
    /// `K` — VAT-exempt intra-Community supply of goods.
    IntraCommunity,
    /// `G` — free export item, VAT not charged.
    Export,
    /// `E` — exempt with a reason.
    Exempt,
}

impl VatCategory {
    /// The UNCL 5305 code as it is written on an invoice.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Standard => "S",
            Self::ReverseCharge => "AE",
            Self::IntraCommunity => "K",
            Self::Export => "G",
            Self::Exempt => "E",
        }
    }

    /// Whether this category levies tax on this invoice.
    ///
    /// False for all four of the others, and that is the point: under a reverse
    /// charge the tax exists and is *somebody else's* to declare, so the
    /// invoice's own tax amount is zero and its total is its net.
    #[must_use]
    pub const fn levies_tax(self) -> bool {
        matches!(self, Self::Standard)
    }

    /// Whether EN 16931's category rules require an exemption reason (BT-120 or
    /// BT-121) for it.
    #[must_use]
    pub const fn needs_exemption_reason(self) -> bool {
        matches!(
            self,
            Self::ReverseCharge | Self::IntraCommunity | Self::Export | Self::Exempt
        )
    }
}

impl core::fmt::Display for VatCategory {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.code())
    }
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
    /// Decide the treatment from the two parties and the rate that would apply
    /// at the place of supply.
    ///
    /// `standard_rate` is the rate of the country the **charge point** stands
    /// in, because that is where an ordinary supply of electricity is taxed. It
    /// is an argument rather than a table: rates move, a replayed invoice has to
    /// reproduce the rate that was in force, and a lookup would silently
    /// re-rate a two-year-old document.
    ///
    /// # Errors
    ///
    /// [`BillingError::NoTaxTreatment`] where the combination has no category —
    /// a reverse charge with no VAT identifier on one side of it, above all,
    /// because EN 16931's `BR-AE-*` refuses that invoice and so does the
    /// Finanzamt.
    pub fn decide(
        seller: &TaxStatus,
        buyer: &TaxStatus,
        point_country: &str,
        standard_rate: Decimal,
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
                // it is an ordinary domestic supply.
                return Ok(Self {
                    category: VatCategory::Standard,
                    rate: standard_rate,
                    place_of_supply: buyer_country,
                    reason: None,
                });
            }
            if !buyer.in_the_union() {
                return Ok(Self {
                    category: VatCategory::Export,
                    rate: Decimal::ZERO,
                    place_of_supply: buyer_country.clone(),
                    reason: Some(format!(
                        "Place of supply {buyer_country} under [UStG §3g] (electricity supplied \
                         to a reseller); outside the Union, VAT not charged"
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
            category: VatCategory::Standard,
            rate: standard_rate,
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
        let t = TaxTreatment::decide(&cpo(), &dutch_driver, "DE", dec("19")).unwrap();
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
        let t = TaxTreatment::decide(&cpo(), &fleet, "DE", dec("19")).unwrap();
        assert_eq!(t.category, VatCategory::Standard);
        assert_eq!(t.place_of_supply, "DE");
    }

    #[test]
    fn a_cross_border_reseller_moves_the_place_of_supply_and_the_tax_with_it() {
        // The roaming leg, and the one every platform gets wrong by putting the
        // charge point's rate on it.
        let emsp = TaxStatus::reseller("FR", "FR12345678901");
        let t = TaxTreatment::decide(&cpo(), &emsp, "DE", dec("19")).unwrap();
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
        let t = TaxTreatment::decide(&cpo(), &german_emsp, "DE", dec("19")).unwrap();
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
        let err = TaxTreatment::decide(&cpo(), &anonymous, "DE", dec("19")).unwrap_err();
        assert!(err.to_string().contains("has none"), "{err}");
        assert!(err.to_string().contains("[UStG §3g]"), "{err}");
    }

    #[test]
    fn a_reseller_outside_the_union_takes_the_supply_with_it() {
        let swiss = TaxStatus::reseller("CH", "CHE-123.456.789");
        let t = TaxTreatment::decide(&cpo(), &swiss, "DE", dec("19")).unwrap();
        assert_eq!(t.category, VatCategory::Export);
        assert_eq!(t.rate, Decimal::ZERO);
        assert!(t.reason.unwrap().contains("outside the Union"));
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
    fn only_the_standard_rate_levies_tax_and_the_rest_owe_a_reason() {
        assert!(VatCategory::Standard.levies_tax());
        assert!(!VatCategory::Standard.needs_exemption_reason());
        for category in [
            VatCategory::ReverseCharge,
            VatCategory::IntraCommunity,
            VatCategory::Export,
            VatCategory::Exempt,
        ] {
            assert!(!category.levies_tax(), "{category}");
            assert!(category.needs_exemption_reason(), "{category}");
        }
        assert_eq!(VatCategory::ReverseCharge.code(), "AE");
    }
}
