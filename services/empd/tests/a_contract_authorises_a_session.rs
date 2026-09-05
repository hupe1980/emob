//! The provider's four decisions, each one a fact that spans two systems.
//!
//! Nothing here re-tests a document. `emob-roam` already knows what a token
//! looks like on the wire and `emob-tariff` already knows what a price is worth;
//! every assertion below is about something neither of them can see — a mapping
//! held behind a key, a contract's window against a clock, a ledger of what is
//! not yet invoiced, and a price list read as a whole rather than one quote at a
//! time.

use emob_core::{ContractId, Currency, Money, PartyId};
use emob_roam::TokenType;
use emob_tariff::{Dimension, PriceComponent, Tariff, TariffKind};
use empd::{Allowed, ChargedBy, Contract, Empd, Fee, Markup, ProviderError, Whitelist};
use rust_decimal::Decimal;
use std::str::FromStr;
use time::macros::{date, datetime};

fn dec(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

fn me() -> PartyId {
    PartyId::new("DE", "MSP").unwrap()
}

fn operator() -> PartyId {
    PartyId::new("DE", "ABC").unwrap()
}

fn abroad() -> PartyId {
    PartyId::new("FR", "XYZ").unwrap()
}

/// The ISO 15118-1 reference vector, check digit and all.
fn contract() -> ContractId {
    "NL-TNM-000122045-U".parse().unwrap()
}

fn provider() -> Empd {
    Empd::new(me(), *b"a key nobody outside this service holds")
        .with(Contract::new(contract(), date!(2026 - 01 - 01)))
}

fn tariff() -> Tariff {
    Tariff::simple(
        "ad-hoc-2026".parse().unwrap(),
        Currency::EUR,
        TariffKind::AdHoc,
        emob_core::TimeZone::new("Europe/Berlin").unwrap(),
        vec![
            PriceComponent::new(Dimension::Energy, dec("0.49")),
            PriceComponent::new(Dimension::Flat, dec("0.35")),
        ],
    )
}

#[test]
fn the_uid_is_held_here_and_reaches_only_the_record_that_is_leaving() {
    // The gap `emob_roam::token` documents in as many words: OCPI requires a
    // uid on every outgoing CDR, `emob_session::Authorization` refuses to store
    // one, and the resolution was a service with a key rather than a weakened
    // type on either side.
    let mut provider = provider();
    let token = provider
        .issue_token(
            &contract(),
            "04A1B2C3D4E5F6",
            TokenType::Rfid,
            Whitelist::Allowed,
        )
        .unwrap();

    // What a session may carry is a keyed digest, and it is not the uid.
    assert_eq!(token.as_str().len(), 64);
    assert!(!token.as_str().contains("04A1B2C3D4E5F6"));

    // …and the uid appears exactly at the edge that has to send it.
    let presented = provider.present(&token).unwrap();
    assert_eq!(presented.uid, "04A1B2C3D4E5F6");
    assert_eq!(presented.issuer, me());
    assert_eq!(presented.contract_id, contract());

    // The digest is keyed, so a second provider holding the same card cannot
    // recompute this one's references — which is the whole privacy property.
    let mut other = Empd::new(PartyId::new("NL", "TNM").unwrap(), *b"a different key")
        .with(Contract::new(contract(), date!(2026 - 01 - 01)));
    let elsewhere = other
        .issue_token(
            &contract(),
            "04A1B2C3D4E5F6",
            TokenType::Rfid,
            Whitelist::Allowed,
        )
        .unwrap();
    assert_ne!(token.as_str(), elsewhere.as_str());
}

#[test]
fn a_mistyped_contract_identifier_is_refused_at_the_counter() {
    // `emob-roam` verifies the check digit at the crossing, *"because here is
    // the last place anyone looks"*. This is one place earlier, and it is the
    // difference between refusing a card and discovering three weeks later that
    // a month of sessions was billed to somebody else's contract.
    let mut provider = Empd::new(me(), *b"key").with(Contract::new(
        "NL-TNM-000122045-X".parse().unwrap(),
        date!(2026 - 01 - 01),
    ));
    let refused = provider.issue_token(
        &"NL-TNM-000122045-X".parse().unwrap(),
        "04A1B2C3D4E5F6",
        TokenType::Rfid,
        Whitelist::Allowed,
    );
    assert!(
        matches!(refused, Err(ProviderError::Contract(_))),
        "{refused:?}"
    );
}

#[test]
fn a_contracts_window_is_a_clock_question_and_the_answer_is_expired() {
    let mut provider = Empd::new(me(), *b"key")
        .with(Contract::new(contract(), date!(2026 - 01 - 01)).until(date!(2026 - 06 - 30)));
    let token = provider
        .issue_token(&contract(), "04A1", TokenType::AppUser, Whitelist::Allowed)
        .unwrap();

    assert_eq!(
        provider.authorize(&token, date!(2026 - 06 - 30)).unwrap(),
        Allowed::Allowed,
        "a contract that runs *until* the last of the month covers that day"
    );
    assert_eq!(
        provider.authorize(&token, date!(2026 - 07 - 01)).unwrap(),
        Allowed::Expired
    );
    assert_eq!(
        provider.authorize(&token, date!(2025 - 12 - 31)).unwrap(),
        Allowed::Expired
    );

    // A blocked token stops the driver whatever else is true, which is why it
    // is asked first.
    provider.block(&token).unwrap();
    assert_eq!(
        provider.authorize(&token, date!(2026 - 06 - 01)).unwrap(),
        Allowed::Blocked
    );
}

#[test]
fn no_credit_is_a_question_about_sessions_nobody_has_invoiced() {
    // The answer that needs a ledger. A document cannot give it: the sessions
    // it is about are precisely the ones no document has been written for.
    let mut provider = Empd::new(me(), *b"key").with(
        Contract::new(contract(), date!(2026 - 01 - 01))
            .limited_to(Money::new(dec("50.00"), Currency::EUR)),
    );
    let token = provider
        .issue_token(&contract(), "04A1", TokenType::AppUser, Whitelist::Allowed)
        .unwrap();

    provider
        .ran_up(&contract(), Money::new(dec("31.04"), Currency::EUR))
        .unwrap();
    assert_eq!(
        provider.authorize(&token, date!(2026 - 06 - 01)).unwrap(),
        Allowed::Allowed
    );

    provider
        .ran_up(&contract(), Money::new(dec("24.70"), Currency::EUR))
        .unwrap();
    assert_eq!(
        provider.unbilled(&contract()).unwrap().to_string(),
        "55.74 EUR"
    );
    assert_eq!(
        provider.authorize(&token, date!(2026 - 06 - 01)).unwrap(),
        Allowed::NoCredit
    );

    // …and a month that has been invoiced is a driver who may charge again.
    provider.invoiced(&contract());
    assert!(provider.unbilled(&contract()).is_none());
    assert_eq!(
        provider.authorize(&token, date!(2026 - 06 - 01)).unwrap(),
        Allowed::Allowed
    );
}

#[test]
fn the_whitelist_is_a_two_sided_rule_and_both_sides_are_refusals() {
    let mut provider = provider();
    let never = provider
        .issue_token(&contract(), "04A1", TokenType::Rfid, Whitelist::Never)
        .unwrap();
    let always = provider
        .issue_token(&contract(), "04B2", TokenType::Rfid, Whitelist::Always)
        .unwrap();

    // NEVER means always ask. A session started from a list was not authorised
    // by this provider, and its CDR arrives with nobody to bill.
    let started = provider.started_from_list(&never, date!(2026 - 06 - 01));
    assert!(
        matches!(started, Err(ProviderError::StartedFromListWhenNever { .. })),
        "{started:?}"
    );
    assert_eq!(
        provider.authorize(&never, date!(2026 - 06 - 01)).unwrap(),
        Allowed::Allowed
    );

    // ALWAYS means never ask. An operator that asks is one whose list is stale —
    // and the sessions it is *not* asking about are being started from it,
    // including for tokens this provider has since blocked. That is the half a
    // platform answering `ALLOWED` to everything would never notice.
    let asked = provider.authorize(&always, date!(2026 - 06 - 01));
    assert!(
        matches!(asked, Err(ProviderError::AskedAboutAlways { .. })),
        "{asked:?}"
    );
    assert_eq!(
        provider
            .started_from_list(&always, date!(2026 - 06 - 01))
            .unwrap(),
        Allowed::Allowed
    );
    assert_eq!(Whitelist::Always.as_str(), "ALWAYS");
    assert_eq!(Allowed::NoCredit.as_str(), "NO_CREDIT");
    assert!(Allowed::Allowed.is_allowed());
}

#[test]
fn a_quote_names_the_providers_own_charges_and_passes_the_operators_price_through() {
    // `[AFIR Art. 5(5)]`: *"all price information specific to that recharging
    // session … clearly distinguishing all price components, including
    // applicable e-roaming costs and other fees or charges applied by the
    // mobility service provider"*.
    let provider = provider().charging(
        &operator(),
        Markup::per_kwh(dec("0.05"))
            .and_per_session(dec("0.25"))
            .and_e_roaming(dec("0.10")),
    );
    let at = datetime!(2026-06-01 10:00 +2);
    let quote = provider.quote(&operator(), &tariff(), at).unwrap();

    // The operator's components come first, in `[AFIR Art. 5(4)]`'s order, and
    // they are the operator's own figures rather than this provider's account of
    // them. That is the fold the paragraph exists to forbid: a provider that
    // adds five cents to the kilowatt-hour price and shows one number has named
    // every component and told the driver nothing.
    assert!(quote.passes_the_operators_price_through(&tariff(), at));
    let theirs: Vec<_> = quote.charged_by(ChargedBy::Operator).collect();
    assert_eq!(theirs.len(), 2);
    assert_eq!(theirs[0].price, dec("0.49"));
    assert_eq!(theirs[0].unit, "kWh");
    assert_eq!(theirs[1].price, dec("0.35"));

    // …and the e-roaming cost is its own component, because the article names it
    // separately from "other fees or charges".
    let roaming: Vec<_> = quote.charged_by(ChargedBy::ProviderERoaming).collect();
    assert_eq!(roaming.len(), 1);
    assert_eq!(roaming[0].price, dec("0.10"));
    assert_eq!(quote.charged_by(ChargedBy::Provider).count(), 2);
    assert_eq!(quote.currency, Currency::EUR);
    assert_eq!(quote.operator, operator());

    // A partner with no price list is a refusal rather than a free session: a
    // driver shown a quote is entitled to hold this provider to it.
    let unknown = provider.quote(&abroad(), &tariff(), at);
    assert!(
        matches!(unknown, Err(ProviderError::NoMarkup { .. })),
        "{unknown:?}"
    );
}

#[test]
fn a_price_list_with_no_country_in_it_cannot_surcharge_the_border() {
    // `[AFIR Art. 5(5)]` does not ask this to be reasonable and transparent; it
    // forbids it: *"Mobility service providers shall not apply any extra charges
    // for cross-border e-roaming."*
    //
    // `emob_core::ProviderProfile` takes four booleans, and a boolean somebody
    // ticked is a claim. Three of these are facts about this service's own data;
    // the fourth is a fact about the **type**, because `Markup` has no country
    // in it and a price list that varies with where the point stands is not a
    // thing this provider can express.
    let provider = provider()
        .charging(&operator(), Markup::per_kwh(dec("0.05")))
        .charging(&abroad(), Markup::per_kwh(dec("0.05")))
        // …and the one duty on this profile that is a document rather than a
        // price list: `[MessEG §33(2)]` wants a confirmation from the operator
        // of every meter whose values are billed on, and *who* those are is
        // this service's own markup list rather than a claim.
        .confirmed_by(&operator())
        .confirmed_by(&abroad());

    let profile = provider.provider_profile();
    assert_eq!(profile.party, me());
    assert!(profile.discloses_all_price_components);
    assert!(profile.discloses_e_roaming_costs);
    assert!(!profile.surcharges_cross_border_roaming);

    let report = emob_core::obligation::assess_provider(&profile, date!(2026 - 06 - 01));
    assert_eq!(
        report.breaches().count(),
        0,
        "{:?}",
        report.breaches().collect::<Vec<_>>()
    );

    // Adding a peer without its confirmation moves the answer by itself: eight
    // out of nine is a breach on the ninth.
    let ninth = provider.charging(
        &PartyId::new("NL", "QQQ").expect("a party id"),
        Markup::per_kwh(dec("0.05")),
    );
    assert!(
        !ninth.provider_profile().holds_meter_operator_confirmation,
        "a peer billed on with no [MessEG §33(2)] confirmation"
    );

    // A provider that has stated no price list at all discloses nothing, and the
    // calendar says so rather than assuming the best.
    let silent = Empd::new(me(), *b"key").provider_profile();
    let report = emob_core::obligation::assess_provider(&silent, date!(2026 - 06 - 01));
    assert!(
        report
            .breaches()
            .any(|finding| finding.obligation.citation == "[AFIR Art. 5(5)]")
    );
}

#[test]
fn the_fee_is_owed_by_a_contract_that_never_charged() {
    // C-60/23's own reasoning, and the line no ledger of records can produce:
    // the fee is charged *"regardless of whether the user actually purchased
    // electricity during the relevant period"*. A month's invoice is assembled
    // from CDRs, and a contract with no sessions produces none — so a platform
    // that derived the document from the ledger alone would never bill it.
    let quiet = contract();
    let ended = "NL-TNM-000122046-Z".parse::<ContractId>().unwrap();
    let future = "NL-TNM-000122047-W".parse::<ContractId>().unwrap();

    let provider = Empd::new(me(), *b"key")
        .with(
            Contract::new(quiet.clone(), date!(2026 - 01 - 01))
                .charging(Fee::monthly("network access", dec("4.99"))),
        )
        // Ended inside the period: the access it charges for existed for part of
        // it, so it is billed. Whether the amount is pro-rated is a commercial
        // decision, and it is the contract's own figure rather than this
        // service's arithmetic.
        .with(
            Contract::new(ended.clone(), date!(2025 - 01 - 01))
                .until(date!(2026 - 06 - 15))
                .charging(Fee::monthly("network access", dec("4.99"))),
        )
        // Starts after the period ends: not billed.
        .with(
            Contract::new(future, date!(2026 - 08 - 01))
                .charging(Fee::monthly("network access", dec("4.99"))),
        );

    let fees = provider.fees_for(date!(2026 - 06 - 01), date!(2026 - 06 - 30));
    let ids: Vec<_> = fees.iter().map(|(id, _)| id.clone()).collect();
    assert_eq!(ids, vec![quiet, ended]);
    assert_eq!(fees[0].1.net, dec("4.99"));
    assert_eq!(fees[0].1.description, "network access");

    // A contract with no fee produces no line, which is the ordinary pay-as-you-
    // go driver rather than a zero somebody has to read past.
    let bare = Empd::new(me(), *b"key").with(Contract::new(contract(), date!(2026 - 01 - 01)));
    assert!(
        bare.fees_for(date!(2026 - 06 - 01), date!(2026 - 06 - 30))
            .is_empty()
    );
    assert_eq!(bare.contracts().count(), 1);
    assert!(bare.contract(&contract()).is_some());
    assert_eq!(bare.party(), &me());
    assert!(Markup::none().is_free());
}
