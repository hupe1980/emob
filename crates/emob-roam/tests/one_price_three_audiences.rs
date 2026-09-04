//! One tariff, three audiences, one price.
//!
//! A CPO states its ad-hoc price to three parties, and each of the three is a
//! duty with its own citation:
//!
//! | Audience | Wire | Duty |
//! |---|---|---|
//! | the driver at the point | OCPP 2.1 `SetDefaultTariff` | `[AFIR Art. 5(4)]` — "known to end users **before they initiate**" |
//! | the roaming partner | OCPI 2.3.0 `Tariff` | the price a partner re-rates and settles against |
//! | the national access point | DATEX II Recharging | `[AFIR Art. 20(2)(c)]` — published free of charge through the NAP |
//!
//! Almost every stack in this market computes that number three times, in
//! three systems, and reconciles none of them against the invoice. The failure
//! is asymmetric and it is the one `[PAngV]` and `[AFIR Art. 5(2)]` are about:
//! the screen is read by the driver who pays, the feed by route planners, and
//! the invoice by nobody until it is disputed.
//!
//! Here the three crossings read one [`emob_tariff::Tariff`] — the same value
//! [`emob_tariff::rate`] charges with — so there is no second computation to
//! drift from. This test is that claim, run.

use emob_core::Currency;
use emob_poi::rate;
use emob_roam::ocpi::tariff::to_ocpi;
use emob_tariff::{Dimension, PriceComponent, Tariff, TariffKind, TaxIncluded, describe};
use rust_decimal::Decimal;
use std::str::FromStr;
use time::macros::datetime;

fn dec(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

fn at() -> time::OffsetDateTime {
    datetime!(2026-04-14 10:00 +2)
}

/// A lawful German fast-charger ad-hoc tariff: a price per kWh, and the one
/// addition `[AFIR Art. 5(4)]` permits above 50 kW — an occupancy fee, quoted
/// at an hourly rate that has an exact price per minute.
fn ad_hoc() -> Tariff {
    let mut tariff = Tariff::simple(
        "ad-hoc-dc".parse().unwrap(),
        Currency::EUR,
        TariffKind::AdHoc,
        emob_core::TimeZone::new("Europe/Berlin").unwrap(),
        vec![
            PriceComponent::new(Dimension::Energy, dec("0.59")),
            PriceComponent::new(Dimension::ParkingTime, dec("6.00")),
        ],
    );
    // Net, because OCPP 2.1 and the OCPI `PriceLimit` both quote before tax and
    // this test is about the price rather than about the tax basis.
    tariff.tax_included = TaxIncluded::No;
    tariff.elements[0].components[0].vat = Some(dec("19"));
    tariff.elements[0].components[1].vat = Some(dec("19"));
    tariff
}

#[test]
fn the_price_per_kwh_is_one_decimal_on_all_three_wires() {
    let tariff = ad_hoc();
    let expected = dec("0.59");

    // 1 — the charge point's own display, via OCPP 2.1's Tariff and Cost block.
    let ocpp = emob_ocpp::to_ocpp(&tariff, at()).expect("a lawful tariff crosses onto OCPP 2.1");
    let on_the_station = &ocpp.value.energy.as_ref().unwrap().prices[0].price_kwh;
    assert_eq!(on_the_station.to_string(), "0.59");
    assert!(ocpp.is_lossless(), "{:?}", ocpp.notes());

    // 2 — the roaming partner, via OCPI 2.3.0.
    let party = emob_core::PartyId::new("DE", "ABC").unwrap();
    let ocpi = to_ocpi(&tariff, &party, at()).expect("and onto OCPI");
    let at_the_partner = &ocpi.value.elements[0].price_components[0];
    assert_eq!(at_the_partner.price.get(), expected);

    // 3 — the national access point, via the DATEX II Recharging profile.
    let (published, notes) = rate::publish(&tariff, "rate-1");
    let in_the_feed = published
        .prices
        .iter()
        .find(|price| price.price_type == rate::PriceType::PricePerKwh)
        .expect("the profile has a price type for a price per kWh");
    assert_eq!(in_the_feed.value, expected);

    // The profile has no occupancy price type at all, which is the one gap it
    // reports rather than papers over — and it is reported, not silent.
    assert!(
        notes
            .iter()
            .any(|note| note.to_string().contains("parking")
                || note.to_string().contains("occupancy")),
        "{notes:?}"
    );

    // …and the number the *driver* is shown is the same one again, in the unit
    // the article states it in.
    let shown = describe(&tariff, at());
    assert_eq!(shown.per_kwh(), Some(expected));
    assert_eq!(
        shown.occupancy_per_minute(),
        Some(dec("0.10")),
        "6.00 an hour is 0.10 a minute, exactly"
    );
}

#[test]
fn the_occupancy_fee_is_the_same_minute_on_the_station_as_on_the_screen() {
    // `[AFIR Art. 5(4)]` states the occupancy fee per **minute**. OCPI carries
    // it per hour, the DATEX II profile has no type for it at all, and OCPP 2.1
    // carries `priceMinute` — the only one of the three in the article's own
    // unit. All three have to agree about the money, and the two that speak
    // different units have to agree after the conversion rather than before.
    let tariff = ad_hoc();

    let ocpp = emob_ocpp::to_ocpp(&tariff, at()).unwrap();
    let per_minute = &ocpp.value.idle_time.as_ref().unwrap().prices[0].price_minute;
    assert_eq!(per_minute.to_string(), "0.10");

    let party = emob_core::PartyId::new("DE", "ABC").unwrap();
    let ocpi = to_ocpi(&tariff, &party, at()).unwrap();
    let per_hour = ocpi.value.elements[0]
        .price_components
        .iter()
        .find(|component| {
            component.component_type == ocpi_kit::v2_3_0::tariffs::TariffDimensionType::ParkingTime
        })
        .expect("the occupancy fee crosses onto OCPI");
    assert_eq!(per_hour.price.get(), dec("6.00"));

    // Sixty times the per-minute figure is the per-hour one, exactly. That is
    // the whole content of the AFIR shape check for a time price, met across
    // two wires rather than inside one crate.
    assert_eq!(
        Decimal::from_str(&per_minute.to_string()).unwrap() * Decimal::from(60),
        per_hour.price.get()
    );
}

#[test]
fn a_tariff_no_station_can_state_exactly_is_refused_before_it_reaches_one() {
    // The same tariff with an ordinary €2.50-an-hour occupancy fee. It is
    // lawful-looking, it crosses onto OCPI without complaint — OCPI's unit is
    // the hour — and it cannot be stated on a charge point at all, because
    // 2.50 / 60 does not terminate and OCPP 2.1's field is per minute.
    //
    // `emob-tariff` already calls that an `[AFIR Art. 5(4)]` breach: a price a
    // driver cannot be shown exactly is not one "known to end users before they
    // initiate". This is the same finding, arriving as a wire refusal.
    let mut tariff = ad_hoc();
    tariff.elements[0].components[1].price = dec("2.50");

    let party = emob_core::PartyId::new("DE", "ABC").unwrap();
    assert!(
        to_ocpi(&tariff, &party, at()).is_ok(),
        "OCPI carries an hourly price and has no objection"
    );

    let objections = emob_tariff::check_afir(&tariff, dec("150"));
    assert!(!objections.is_lawful());

    let err = emob_ocpp::to_ocpp(&tariff, at()).unwrap_err();
    assert!(
        err.to_string().contains("divisible by three"),
        "the wire and the conformance check give the same remedy: {err}"
    );
}

#[test]
fn the_station_selects_the_tier_the_invoice_will_be_built_from() {
    // OCPI picks the first *element* with a component for the dimension whose
    // restrictions match; OCPP picks the first *price* in the dimension's own
    // list whose conditions match. Projecting the element list per dimension in
    // order makes those the same choice — so a tiered tariff installed on a
    // station charges the tier the CDR is rated at, by construction.
    let tariff = Tariff {
        elements: vec![
            emob_tariff::TariffElement {
                components: vec![PriceComponent::new(Dimension::Energy, dec("0.39"))],
                restrictions: emob_tariff::Restrictions {
                    max_kwh: Some(dec("10")),
                    ..emob_tariff::Restrictions::default()
                },
            },
            emob_tariff::TariffElement::unrestricted(vec![PriceComponent::new(
                Dimension::Energy,
                dec("0.59"),
            )]),
        ],
        tax_included: TaxIncluded::No,
        ..ad_hoc()
    };

    let ocpp = emob_ocpp::to_ocpp(&tariff, at()).unwrap();
    let prices = &ocpp.value.energy.as_ref().unwrap().prices;
    let party = emob_core::PartyId::new("DE", "ABC").unwrap();
    let ocpi = to_ocpi(&tariff, &party, at()).unwrap();

    // Same order, same prices, same thresholds — in each wire's own unit.
    assert_eq!(prices.len(), ocpi.value.elements.len());
    assert_eq!(prices[0].price_kwh.to_string(), "0.39");
    assert_eq!(
        ocpi.value.elements[0].price_components[0].price.get(),
        dec("0.39")
    );
    assert_eq!(
        prices[0]
            .conditions
            .as_ref()
            .unwrap()
            .max_energy
            .unwrap()
            .to_string(),
        "10000",
        "OCPP counts energy in watt-hours"
    );
    assert_eq!(
        ocpi.value.elements[0]
            .restrictions
            .as_ref()
            .unwrap()
            .max_kwh
            .unwrap()
            .get(),
        dec("10"),
        "and OCPI in kilowatt-hours"
    );
}

/// SplitMix64 — the workspace takes no `rand`, and a seeded sequence is what a
/// replayable property test wants anyway.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn between(&mut self, low: u64, high: u64) -> u64 {
        low + self.next() % (high - low + 1)
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.between(1, 100) <= percent
    }
}

/// A lawful German fast-charger ad-hoc tariff, generated: a price per kWh under
/// up to two energy tiers, sometimes an occupancy fee at an hourly rate that has
/// an exact price per minute, sometimes a VAT rate, in either tax basis.
fn generated(rng: &mut Rng) -> Tariff {
    let vat = rng.chance(70).then(|| dec("19"));
    let priced = |dimension: Dimension, price: Decimal| {
        let component = PriceComponent::new(dimension, price);
        match vat {
            Some(rate) => component.with_vat(rate),
            None => component,
        }
    };
    let cents = |rng: &mut Rng| Decimal::new(i64::try_from(rng.between(20, 99)).unwrap_or(59), 2);

    let mut elements: Vec<emob_tariff::TariffElement> = Vec::new();
    if rng.chance(50) {
        let price = cents(rng);
        let boundary = Decimal::from(rng.between(5, 40));
        elements.push(emob_tariff::TariffElement {
            components: vec![priced(Dimension::Energy, price)],
            restrictions: emob_tariff::Restrictions {
                max_kwh: Some(boundary),
                ..emob_tariff::Restrictions::default()
            },
        });
    }

    let price = cents(rng);
    let mut last = vec![priced(Dimension::Energy, price)];
    if rng.chance(60) {
        // An hourly rate divisible by three, so it has an exact price per
        // minute — the shape `[AFIR Art. 5(4)]` asks for and OCPP 2.1 can state.
        let per_hour = Decimal::new(i64::try_from(rng.between(1, 12) * 30).unwrap_or(600), 2);
        last.push(priced(Dimension::ParkingTime, per_hour));
    }
    elements.push(emob_tariff::TariffElement::unrestricted(last));

    Tariff {
        elements,
        tax_included: if rng.chance(50) {
            TaxIncluded::No
        } else {
            TaxIncluded::Yes
        },
        ..ad_hoc()
    }
}

/// Every energy price in a tariff, in element order — what each of the three
/// wires has to carry, in the order it has to carry them.
fn energy_prices(tariff: &Tariff) -> Vec<Decimal> {
    tariff
        .elements
        .iter()
        .filter_map(|element| element.component(Dimension::Energy))
        .map(|component| component.price)
        .collect()
}

#[test]
fn every_lawful_ad_hoc_tariff_reads_one_price_on_all_three_wires() {
    // The claim above, over five hundred generated tariffs rather than one. A
    // crossing that rounds, reorders or drops a tier is a station quoting a
    // price the invoice does not charge — `[PAngV]` and `[AFIR Art. 5(2)]` are
    // about exactly that, and it is the failure no single example finds because
    // every example is the shape somebody already thought of.
    //
    // **One price, not one decimal**, and the difference is the tax basis. OCPI
    // 2.3.0 carries the figure verbatim and states the basis on the Tariff
    // object beside it; the DATEX II profile carries it with its own tax flag;
    // OCPP 2.1 quotes **net** with a `taxRates` list. So the invariant is that
    // each wire states the price in the basis it declares, and the station's net
    // figure grosses back up to the number the other two publish — or the
    // crossing says by how much it could not.
    let mut rng = Rng(0x0FE_0000_0000_0001);
    let party = emob_core::PartyId::new("DE", "ABC").unwrap();
    let mut gross_tariffs = 0usize;
    let (mut exact, mut noted) = (0usize, 0usize);

    for case in 0..500 {
        let tariff = generated(&mut rng);
        let quoted = energy_prices(&tariff);
        let rate = tariff.elements[0].components[0].vat;

        // The generator only builds shapes the article permits at 150 kW, and
        // the conformance check is what says so rather than the generator.
        assert!(
            emob_tariff::check_afir(&tariff, dec("150")).is_lawful(),
            "case {case}: the generator built an unlawful tariff: {:?}",
            emob_tariff::check_afir(&tariff, dec("150"))
                .reasons()
                .collect::<Vec<_>>()
        );

        // 1 — the roaming partner, over OCPI 2.3.0: the figure verbatim, with
        //     the basis stated on the Tariff object.
        let ocpi = to_ocpi(&tariff, &party, at()).expect("a lawful tariff crosses onto OCPI");
        let at_the_partner: Vec<Decimal> = ocpi
            .value
            .elements
            .iter()
            .flat_map(|element| &element.price_components)
            .filter(|component| {
                component.component_type == ocpi_kit::v2_3_0::tariffs::TariffDimensionType::Energy
            })
            .map(|component| component.price.get())
            .collect();
        assert_eq!(at_the_partner, quoted, "case {case}: OCPI 2.3.0");
        assert_eq!(
            ocpi.value.tax_included,
            emob_roam::ocpi::tariff::tax_included(tariff.tax_included),
            "case {case}: the basis the figure is stated in"
        );

        // 2 — the national access point, over DATEX II: the same figure, with
        //     the profile's own tax flag.
        let (published, _) = rate::publish(&tariff, "rate-1");
        let in_the_feed: Vec<Decimal> = published
            .prices
            .iter()
            .filter(|price| price.price_type == rate::PriceType::PricePerKwh)
            .map(|price| price.value)
            .collect();
        assert_eq!(in_the_feed, quoted, "case {case}: DATEX II");

        // 3 — the driver at the point, over OCPP 2.1: **net**, and the gross it
        //     grosses back up to is the number the other two publish.
        let ocpp = emob_ocpp::to_ocpp(&tariff, at())
            .unwrap_or_else(|error| panic!("case {case}: a lawful tariff was refused: {error}"));
        let on_the_station: Vec<Decimal> = ocpp
            .value
            .energy
            .as_ref()
            .expect("every generated tariff prices energy")
            .prices
            .iter()
            // `ocpp-kit` carries its own exact decimal, so the comparison goes
            // through the digits both types agree on rather than through a
            // conversion either could round.
            .map(|price| dec(&price.price_kwh.to_string()))
            .collect();

        let factor = match (tariff.tax_included, rate) {
            (TaxIncluded::Yes, Some(rate)) => Decimal::ONE + rate / Decimal::from(100),
            _ => Decimal::ONE,
        };
        if factor != Decimal::ONE {
            gross_tariffs += 1;
        }
        for (net, gross) in on_the_station.iter().zip(&quoted) {
            let regrossed = net * factor;
            if regrossed != *gross {
                noted += 1;
            } else {
                exact += 1;
            }
            assert!(
                regrossed == *gross || !ocpp.is_lossless(),
                "case {case}: the station quotes {net} net, which grosses to {regrossed} \
                 against the {gross} the feed publishes, and nothing said so"
            );
        }

        // …and the driver's own screen reads the tariff's own figure.
        assert_eq!(
            describe(&tariff, at()).per_kwh(),
            Some(quoted[0]),
            "case {case}: the price the driver reads first is the first tier"
        );

        // The occupancy fee, where there is one: per minute at the station, per
        // hour at the partner, and sixty of the first is the second — in the
        // station's own basis, so the conversion is the only difference.
        if let Some(per_hour) = tariff
            .elements
            .iter()
            .filter_map(|element| element.component(Dimension::ParkingTime))
            .map(|component| component.price)
            .next()
        {
            let per_minute = dec(&ocpp
                .value
                .idle_time
                .as_ref()
                .expect("an occupancy fee crosses onto OCPP 2.1")
                .prices[0]
                .price_minute
                .to_string());
            let regrossed = per_minute * Decimal::from(60) * factor;
            assert!(
                regrossed == per_hour || !ocpp.is_lossless(),
                "case {case}: the minute and the hour disagree ({regrossed} against {per_hour})"
            );
            assert_eq!(
                describe(&tariff, at()).occupancy_per_minute(),
                Some(per_hour / Decimal::from(60)),
                "case {case}: the screen quotes the tariff's own basis"
            );
        }
    }

    assert!(
        gross_tariffs > 100,
        "only {gross_tariffs} of 500 tariffs were quoted gross, and that is the case the \
         station's net figure exists for"
    );
    // Both halves of the invariant are exercised, and the second is not rare:
    // an ordinary €0.57 at 19 % has **no** exact net, so a third of the prices
    // here cannot be stated on the wire to the last digit. The residual is a
    // tenth of an attoeuro per kilowatt-hour and it is reported by JSON Pointer
    // rather than absorbed, which is the whole difference between this and a
    // station quietly quoting a price the feed does not publish.
    assert!(exact > 0 && noted > 0, "exact {exact}, noted {noted}");
}
