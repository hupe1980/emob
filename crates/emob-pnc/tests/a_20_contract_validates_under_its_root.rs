//! ISO 15118-20 from 01.01.2027 (spec 010, User Story 1).
//!
//! `[DA-656 Anh. 2.1.2]` makes ISO 15118-20 mandatory on new public points, and
//! a point offering Plug & Charge supports -2 **and** -20 — so one platform
//! judges both generations, under three European roots, in one process. The
//! fixtures are OpenSSL's (`scripts/make-test-pki.sh`), one secp521r1 root per
//! ecosystem, every contract carrying the same EMAID: what decides is the
//! root, never the identifier.

use emob_pnc::fixtures::{self, iso20};
use emob_pnc::{
    Generation, PncError, Pool, PoolId, PoolRegistry, RevocationList, TrustAnchor, Withdrawals,
    verify_contract,
};
use iso15118::pnc::pki::ChainError;
use time::macros::datetime;

fn at() -> time::OffsetDateTime {
    datetime!(2027-01-04 09:00 UTC)
}

const ECOSYSTEMS: [(&str, &str, &str, &str); 3] = [
    (
        "hubject-test",
        iso20::HUBJECT_ROOT,
        iso20::HUBJECT_MO_SUB_CA,
        iso20::HUBJECT_CONTRACT,
    ),
    (
        "gireve-test",
        iso20::GIREVE_ROOT,
        iso20::GIREVE_MO_SUB_CA,
        iso20::GIREVE_CONTRACT,
    ),
    (
        "charin-test",
        iso20::CHARIN_ROOT,
        iso20::CHARIN_MO_SUB_CA,
        iso20::CHARIN_CONTRACT,
    ),
];

/// A platform connected to the three ecosystems, a -2 pool and an Ed448 one.
fn platform() -> PoolRegistry {
    let mut registry = PoolRegistry::new();
    for (name, root, ..) in ECOSYSTEMS {
        registry = registry.with(
            name,
            Pool::new().with(TrustAnchor::unbounded(
                fixtures::der(root),
                format!("{name}, minted by make-test-pki.sh"),
            )),
        );
    }
    registry
        .with(
            "iso2-test",
            Pool::new().with(TrustAnchor::unbounded(
                fixtures::der(fixtures::ROOT),
                "the -2 test root",
            )),
        )
        .with(
            "ed448-test",
            Pool::new().with(TrustAnchor::unbounded(
                fixtures::der(iso20::ED448_ROOT),
                "the Ed448 test root",
            )),
        )
}

/// Every certificate on the path standing good — revocation is not the
/// question here.
fn all_good(chains: &[&[Vec<u8>]]) -> RevocationList {
    let mut list = RevocationList::new();
    for chain in chains {
        for der in chain.iter().skip(1).chain(chain.iter().take(1)) {
            let certificate = iso15118::pnc::pki::Certificate::parse(der)
                .expect("a certificate this test minted");
            list = list.with(
                certificate.issuer.encoded,
                Withdrawals::none_at(datetime!(2027-01-04 08:00 UTC)),
            );
        }
    }
    list
}

fn refs(chain: &[Vec<u8>]) -> Vec<&[u8]> {
    chain.iter().map(Vec::as_slice).collect()
}

fn chain(contract: &str, sub_ca: &str) -> Vec<Vec<u8>> {
    vec![fixtures::der(contract), fixtures::der(sub_ca)]
}

#[test]
fn sr_123_a_20_contract_validates_in_its_own_pool_and_nowhere_else() {
    let platform = platform();
    let chains: Vec<Vec<Vec<u8>>> = ECOSYSTEMS
        .iter()
        .map(|(_, _, sub, contract)| chain(contract, sub))
        .collect();
    let status = all_good(&chains.iter().map(Vec::as_slice).collect::<Vec<_>>());

    for (minted, (home, ..)) in chains.iter().zip(ECOSYSTEMS) {
        for (pool, ..) in ECOSYSTEMS {
            let outcome = verify_contract(
                &platform,
                &PoolId::new(pool),
                Generation::Iso20,
                &refs(minted),
                &status,
                at(),
                None,
            );
            if pool == home {
                let contract = outcome.expect("its own root");
                assert_eq!(contract.emaid.canonical(), "DE8AACA2B3C4D5L");
                assert_eq!(contract.pool, PoolId::new(home));
                assert_eq!(contract.generation, Generation::Iso20);
                let root = home.trim_end_matches("-test");
                assert_eq!(contract.anchor, format!("{root} V2G Root CA 20"));
            } else {
                let refused = outcome.expect_err("another ecosystem's root");
                assert!(
                    matches!(
                        refused,
                        PncError::Chain {
                            refusal: ChainError::NoTrustAnchor,
                            ..
                        }
                    ),
                    "{home} in {pool}: {refused}"
                );
            }
        }
    }
}

#[test]
fn sr_124_a_2_chain_judged_under_20_rules_is_refused_and_still_validates_under_2() {
    let platform = platform();
    let minted = vec![
        fixtures::der(fixtures::CONTRACT),
        fixtures::der(fixtures::MO_SUB_CA_2),
        fixtures::der(fixtures::MO_SUB_CA_1),
    ];
    let status = all_good(&[&minted]);
    let pool = PoolId::new("iso2-test");

    // The downgrade: secp256r1 presented to a session that negotiated -20.
    let refused = verify_contract(
        &platform,
        &pool,
        Generation::Iso20,
        &refs(&minted),
        &status,
        at(),
        None,
    )
    .expect_err("ISO 15118-20 has no secp256r1");
    assert!(
        matches!(
            refused,
            PncError::Chain {
                refusal: ChainError::AlgorithmNotAllowed { .. },
                ..
            }
        ),
        "{refused}"
    );

    // …and -2 keeps working under -2.
    let contract = verify_contract(
        &platform,
        &pool,
        Generation::Iso2,
        &refs(&minted),
        &status,
        at(),
        None,
    )
    .expect("a -2 contract under -2 rules");
    assert_eq!(contract.generation, Generation::Iso2);
}

#[test]
fn sr_124_a_20_chain_judged_under_2_rules_is_refused_by_name() {
    // The other direction: a secp521r1 chain read as -2 would escape -20's own
    // rules — the size bound, the nesting, Annex B's extensions.
    let platform = platform();
    let minted = chain(iso20::HUBJECT_CONTRACT, iso20::HUBJECT_MO_SUB_CA);
    let status = all_good(&[&minted]);
    let refused = verify_contract(
        &platform,
        &PoolId::new("hubject-test"),
        Generation::Iso2,
        &refs(&minted),
        &status,
        at(),
        None,
    )
    .expect_err("-2 has secp256r1 only");
    assert!(
        matches!(
            &refused,
            PncError::UnsupportedAlgorithm {
                generation: Generation::Iso2,
                curve,
                ..
            } if curve == "P521"
        ),
        "{refused}"
    );
}

#[test]
fn sr_125_an_ed448_chain_is_refused_by_name_not_skipped() {
    let platform = platform();
    let minted = chain(iso20::ED448_CONTRACT, iso20::ED448_MO_SUB_CA);
    let status = all_good(&[&minted]);
    for generation in [Generation::Iso20, Generation::Iso2] {
        let refused = verify_contract(
            &platform,
            &PoolId::new("ed448-test"),
            generation,
            &refs(&minted),
            &status,
            at(),
            None,
        )
        .expect_err("no Ed448 verifier in this build");
        assert!(
            matches!(
                &refused,
                PncError::UnsupportedAlgorithm { curve, .. } if curve == "Ed448"
            ),
            "{refused}"
        );
        assert!(refused.to_string().contains("Ed448"), "{refused}");
    }
}
