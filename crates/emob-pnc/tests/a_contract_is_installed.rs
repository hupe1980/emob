//! M6c, for the half that decides *whose* contract this is.
//!
//! > A contract certificate is installed against a vehicle, and a session is
//! > authorised by it.
//!
//! Every assertion here is about a fact that spans two artefacts, and is
//! therefore invisible to a crate that validates one chain at a time:
//!
//! * a chain that validates **beautifully** against the wrong ecosystem's root
//!   is a driver this platform has no relationship with `[V2G2-925]`;
//! * a certificate nobody can vouch for is not one to read as good, and *"leaf
//!   and chain"* is what `[OPCP §Provisioning Certificate Pool]` asks about;
//! * the identifier a session is attributed to comes **off the certificate**
//!   `[V2G2-108]`, so a caller cannot name a different one;
//! * and one vehicle holds several contracts, which is the shape a map keyed by
//!   PCID answers with whichever arrived last.
//!
//! The DER is OpenSSL's — see [`emob_pnc::fixtures`] — and the last test in the
//! file runs Hubject's own published test PKI where the working copy has it.

use emob_pnc::fixtures;
use emob_pnc::{
    Contract, ContractBook, Generation, Pcid, PncError, Pool, PoolId, PoolRegistry, RevocationList,
    Standing, TrustAnchor, Withdrawals, verify_contract, verify_provisioning,
};
use iso15118::pnc::pki::Certificate;
use time::macros::datetime;

/// Inside every fixture's window: the roots run to 2060 and the leaves to 2050.
fn at() -> time::OffsetDateTime {
    datetime!(2030-06-01 12:00 UTC)
}

fn der(constant: &str) -> Vec<u8> {
    fixtures::der(constant)
}

fn refs(chain: &[Vec<u8>]) -> Vec<&[u8]> {
    chain.iter().map(Vec::as_slice).collect()
}

/// The contract chain as `CertificateChainType` carries it: leaf first.
fn contract_chain() -> Vec<Vec<u8>> {
    vec![
        der(fixtures::CONTRACT),
        der(fixtures::MO_SUB_CA_2),
        der(fixtures::MO_SUB_CA_1),
    ]
}

/// The vehicle's, the same way.
fn provisioning_chain() -> Vec<Vec<u8>> {
    vec![
        der(fixtures::PROVISIONING),
        der(fixtures::OEM_SUB_CA_2),
        der(fixtures::OEM_SUB_CA_1),
    ]
}

/// A contract minted in a **different** ecosystem, whose chain is impeccable.
fn foreign_chain() -> Vec<Vec<u8>> {
    vec![
        der(fixtures::FOREIGN_CONTRACT),
        der(fixtures::OTHER_MO_SUB_CA_1),
    ]
}

/// This platform, connected to two ecosystems from the first line (R10).
fn platform() -> PoolRegistry {
    PoolRegistry::new()
        .with(
            "hubject-ccp",
            Pool::new().with(TrustAnchor::unbounded(
                der(fixtures::ROOT),
                "the Root Certificate Pool, over OFTP2, 2030-01",
            )),
        )
        .with(
            "another-scheme",
            Pool::new().with(TrustAnchor::unbounded(
                der(fixtures::OTHER_ROOT),
                "that scheme's own root distribution, 2030-01",
            )),
        )
}

/// A revocation status this side holds for every issuer in the fixtures, listing
/// nothing — which is what a working responder connection looks like.
fn nothing_withdrawn(as_of: time::OffsetDateTime) -> RevocationList {
    let mut list = RevocationList::new();
    for pem in [
        fixtures::ROOT,
        fixtures::MO_SUB_CA_1,
        fixtures::MO_SUB_CA_2,
        fixtures::OEM_SUB_CA_1,
        fixtures::OEM_SUB_CA_2,
        fixtures::OTHER_ROOT,
        fixtures::OTHER_MO_SUB_CA_1,
    ] {
        let bytes = der(pem);
        let certificate = Certificate::parse(&bytes).expect("a certificate this crate minted");
        list = list.with(
            certificate.subject.encoded.to_vec(),
            Withdrawals::none_at(as_of),
        );
    }
    list
}

#[test]
fn a_contract_is_installed_against_a_vehicle_and_both_halves_are_checked() {
    let platform = platform();
    let pool = PoolId::new("hubject-ccp");
    let status = nothing_withdrawn(at());

    // The vehicle first: `[OPCP §Provisioning Certificate Pool]`'s own order —
    // *"Before creating the Contract Data package, the MO has to request the
    // current Provisioning Certificate by the Provisioning Certificate ID"*.
    let chain = provisioning_chain();
    let vehicle = verify_provisioning(
        &platform,
        &pool,
        Generation::Iso2,
        &refs(&chain),
        &status,
        at(),
    )
    .expect("a chain to the root this pool publishes");
    assert_eq!(vehicle.pcid.as_str(), "WVW00000000000001");
    assert_eq!(vehicle.pcid.wmi(), "WVW");

    // …and the account may only publish for manufacturers it is enrolled for,
    // which is the pool's *first* control step and not its last.
    assert!(emob_pnc::may_publish_for(&vehicle.pcid, &["WVW"]).is_ok());
    assert!(emob_pnc::may_publish_for(&vehicle.pcid, &["HUB"]).is_err());

    // Then the contract. The identifier comes **off the certificate**: nothing
    // in this call names an eMAID, because a check fed by a caller is not one.
    let chain = contract_chain();
    let contract = verify_contract(
        &platform,
        &pool,
        Generation::Iso2,
        &refs(&chain),
        &status,
        at(),
        None,
    )
    .expect("a chain to the root this pool publishes");
    assert_eq!(contract.emaid.canonical(), "DE8AACA2B3C4D5L");
    assert_eq!(contract.anchor, "V2G Root CA");
    assert_eq!(contract.valid_until, datetime!(2050-01-01 00:00 UTC));

    // …and the two halves are linked by two verifications rather than by a
    // string the caller supplied.
    let mut book = ContractBook::new();
    assert!(
        book.install(Contract::installed(&contract, &vehicle))
            .is_none()
    );
    let held = book
        .get(&pool, &contract.emaid)
        .expect("the contract this platform now holds");
    assert_eq!(held.vehicle.as_ref(), Some(&vehicle.pcid));
    assert_eq!(book.for_vehicle(&vehicle.pcid).count(), 1);
}

#[test]
fn a_contract_from_another_ecosystem_validates_and_is_still_refused() {
    // The whole of R10, and the reason this crate has no global trust store.
    // Every signature on this chain checks. Every Annex F profile holds. It
    // reaches a V2G root — *a* V2G root — and the driver has no relationship
    // with this platform `[V2G2-925]`.
    let platform = platform();
    let status = nothing_withdrawn(at());
    let chain = foreign_chain();

    // In its own ecosystem it is a contract, and this asserts that the chain is
    // genuinely sound — without it the refusal below would prove nothing.
    let ours = verify_contract(
        &platform,
        &PoolId::new("another-scheme"),
        Generation::Iso2,
        &refs(&chain),
        &status,
        at(),
        None,
    )
    .expect("impeccable, in the ecosystem that minted it");
    assert_eq!(ours.emaid.canonical(), "DE8BBCA2B3C4D5M");
    assert_eq!(ours.anchor, "Other V2G Root CA");

    // …and in this one it reaches nothing.
    let refused = verify_contract(
        &platform,
        &PoolId::new("hubject-ccp"),
        Generation::Iso2,
        &refs(&chain),
        &status,
        at(),
        None,
    )
    .expect_err("that ecosystem's root is not this pool's");
    assert!(matches!(refused, PncError::Chain { .. }), "{refused}");
}

#[test]
fn a_certificate_nobody_can_vouch_for_is_not_read_as_good() {
    // `iso15118::pnc::pki::validate` is handed no evidence, so revocation is
    // this crate's question to answer. The default `RevocationList` holds nothing, so every answer is
    // `Unknown` — and `Unknown` is not `Good`, which is the whole reason there
    // are four standings and not a `bool`.
    let platform = platform();
    let chain = contract_chain();
    let refused = verify_contract(
        &platform,
        &PoolId::new("hubject-ccp"),
        Generation::Iso2,
        &refs(&chain),
        &RevocationList::new(),
        at(),
        None,
    )
    .expect_err("no responder has ever been connected");
    let PncError::Revoked { ref standing, .. } = refused else {
        panic!("{refused:?}");
    };
    assert_eq!(**standing, Standing::Unknown);
    assert!(refused.to_string().contains("[OPCP"), "{refused}");
}

#[test]
fn a_withdrawn_sub_ca_takes_the_contracts_under_it_and_not_only_its_own_leaf() {
    // `[OPCP §Provisioning Certificate Pool]`'s third control step is *"each
    // delivered certificate (leaf and chain)"*. A check that asked only about
    // the contract certificate would pass this: the leaf is not listed, and the
    // sub-CA that signed it was withdrawn this morning.
    let platform = platform();
    let pool = PoolId::new("hubject-ccp");
    let chain = contract_chain();

    let sub2 = der(fixtures::MO_SUB_CA_2);
    let sub2 = Certificate::parse(&sub2).expect("a certificate this crate minted");
    let status = nothing_withdrawn(at()).with(
        sub2.issuer.encoded.to_vec(),
        Withdrawals::none_at(at()).and(sub2.serial.to_vec(), Some("keyCompromise".to_owned())),
    );

    let refused = verify_contract(
        &platform,
        &pool,
        Generation::Iso2,
        &refs(&chain),
        &status,
        at(),
        None,
    )
    .expect_err("the sub-CA is withdrawn");
    let PncError::Revoked {
        ref subject,
        ref standing,
    } = refused
    else {
        panic!("{refused:?}");
    };
    assert_eq!(subject, "MO Sub CA 2");
    assert!(
        matches!(**standing, Standing::Revoked { .. }),
        "{standing:?}"
    );
    assert!(refused.to_string().contains("keyCompromise"), "{refused}");
}

#[test]
fn a_status_older_than_the_operator_vouches_for_is_not_evidence_of_good_standing() {
    // The failure mode of published data, one PKI along: the file parses, every
    // signature checks, and the answer is about a world that has moved. A list
    // from March consulted in September is not evidence that a contract
    // cancelled in June is still good.
    let platform = platform();
    let pool = PoolId::new("hubject-ccp");
    let chain = contract_chain();

    let stale =
        nothing_withdrawn(at() - time::Duration::days(180)).fresh_for(time::Duration::hours(24));
    let refused = verify_contract(
        &platform,
        &pool,
        Generation::Iso2,
        &refs(&chain),
        &stale,
        at(),
        None,
    )
    .expect_err("the only status held is six months old");
    let PncError::Revoked { ref standing, .. } = refused else {
        panic!("{refused:?}");
    };
    assert!(matches!(**standing, Standing::Stale { .. }), "{standing:?}");

    // …and the same list is evidence again where the operator vouches for a
    // window that covers it. The number is the operator's, not this crate's.
    let generous =
        nothing_withdrawn(at() - time::Duration::days(180)).fresh_for(time::Duration::days(365));
    assert!(
        verify_contract(
            &platform,
            &pool,
            Generation::Iso2,
            &refs(&chain),
            &generous,
            at(),
            None
        )
        .is_ok()
    );
}

#[test]
fn a_contract_that_belongs_to_another_provider_is_refused_by_name() {
    // `[OPCP §Contract Certificate Pool]` keeps each MO's contracts separated.
    // The identifier's own provider section says which MO a contract belongs
    // to, so an installation that names the provider it asked about has the
    // comparison made once rather than at every call site.
    let platform = platform();
    let pool = PoolId::new("hubject-ccp");
    let chain = contract_chain();
    let status = nothing_withdrawn(at());

    assert!(
        verify_contract(
            &platform,
            &pool,
            Generation::Iso2,
            &refs(&chain),
            &status,
            at(),
            Some("8AA")
        )
        .is_ok()
    );

    let refused = verify_contract(
        &platform,
        &pool,
        Generation::Iso2,
        &refs(&chain),
        &status,
        at(),
        Some("8ZZ"),
    )
    .expect_err("a valid contract, of somebody else's");
    assert!(
        matches!(&refused, PncError::NotThisProvider { provider, .. } if provider == "8AA"),
        "{refused}"
    );
}

#[test]
fn a_certificate_outside_its_own_validity_is_refused_and_the_instant_is_an_argument() {
    // Every instant in this workspace is an argument, and here it is the whole
    // demonstration: the same bytes are a contract in 2030 and are not one in
    // 2019, and a replay two years from now asks the question exactly as it was
    // asked today.
    let platform = platform();
    let pool = PoolId::new("hubject-ccp");
    let chain = contract_chain();
    let before = datetime!(2019-06-01 12:00 UTC);

    assert!(
        verify_contract(
            &platform,
            &pool,
            Generation::Iso2,
            &refs(&chain),
            &nothing_withdrawn(before),
            before,
            None
        )
        .is_err(),
        "nothing here was valid in 2019"
    );
}

#[test]
fn one_vehicle_holds_several_contracts_and_retiring_it_takes_all_of_them() {
    let platform = platform();
    let status = nothing_withdrawn(at());

    let provisioning = provisioning_chain();
    let vehicle = verify_provisioning(
        &platform,
        &PoolId::new("hubject-ccp"),
        Generation::Iso2,
        &refs(&provisioning),
        &status,
        at(),
    )
    .expect("the vehicle");

    let ours = contract_chain();
    let theirs = foreign_chain();
    let one = verify_contract(
        &platform,
        &PoolId::new("hubject-ccp"),
        Generation::Iso2,
        &refs(&ours),
        &status,
        at(),
        None,
    )
    .expect("a contract in one ecosystem");
    let two = verify_contract(
        &platform,
        &PoolId::new("another-scheme"),
        Generation::Iso2,
        &refs(&theirs),
        &status,
        at(),
        None,
    )
    .expect("a contract in the other");

    // `[OPCP §Contract Certificate Pool]`: *"The CCP also enables Multiple
    // contracts storing for each PCID."* Two providers, two ecosystems, one car.
    let mut book = ContractBook::new();
    book.install(Contract::installed(&one, &vehicle));
    book.install(Contract::installed(&two, &vehicle));
    assert_eq!(book.for_vehicle(&vehicle.pcid).count(), 2);

    // …and the one cascade the specification describes: deleting a provisioning
    // certificate *"triggers a deletion of all existing Contract Certificates
    // linked to this Provisioning Certificate"*.
    assert_eq!(book.retire_vehicle(&vehicle.pcid), 2);
    assert_eq!(book.contracts().count(), 0);
}

/// The leg CI cannot repeat, run where the working copy has the archive.
///
/// `concepts/reference/` is gitignored — it holds third-party publications, indexed with
/// retrieval URLs so it can be rebuilt — so this skips **by name** rather than
/// silently, which is the same discipline `check-concepts` keeps about
/// `concepts/` (D228). A test that quietly passes when its input is missing
/// reads exactly like one that ran.
#[test]
fn hubjects_own_published_vehicle_certificate_validates_against_its_own_root() {
    let archive = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../concepts/reference/roaming/openplugncharge-master.zip"
    );
    if !std::path::Path::new(archive).exists() {
        eprintln!(
            "skipped by name: {archive} is not in this working copy. It is Hubject's own open \
             Plug & Charge test PKI — a real V2G Root, real OEM sub-CAs and real provisioning \
             certificates — and concepts/REFERENCES.md carries the URL it is retrieved from. The \
             fixtures this file otherwise runs are OpenSSL's, which is a weaker claim and the \
             accurate one to make without it."
        );
        return;
    }

    let read = |name: &str| -> Vec<u8> {
        let pem = std::process::Command::new("unzip")
            .args(["-p", archive, name])
            .output()
            .expect("unzip")
            .stdout;
        let body: String = String::from_utf8_lossy(&pem)
            .lines()
            .filter(|line| !line.starts_with("-----"))
            .collect();
        fixtures::der(&body)
    };

    let root = read("openPlugnCharge-master/Certificates/V2G Root Certificate/V2G Root.pem");
    let oem1 = read("openPlugnCharge-master/Certificates/OEM certificates/OEM Sub CA 1.pem");
    let oem2 = read("openPlugnCharge-master/Certificates/OEM certificates/OEM Sub CA 2.pem");
    let leaf = read(
        "openPlugnCharge-master/Certificates/OEM certificates/OEM Provisioning \
         Certificates/hubjectopenpnc0008.pem",
    );

    let platform = PoolRegistry::new().with(
        "hubject-open-pnc",
        Pool::new().with(TrustAnchor::unbounded(
            root.clone(),
            "Hubject's published open Plug & Charge test environment",
        )),
    );
    let pool = PoolId::new("hubject-open-pnc");

    // Inside every one of those certificates' windows — the sub-CAs run to
    // December 2022 and the leaf to November 2022. That the instant is an
    // argument is what makes a published test PKI usable years after it
    // expired, and it is the same property a disputed session rests on.
    let at = datetime!(2020-06-01 12:00 UTC);

    let mut status = RevocationList::new();
    for der in [&root, &oem1, &oem2] {
        let certificate = Certificate::parse(der).expect("Hubject's own DER");
        status = status.with(
            certificate.subject.encoded.to_vec(),
            Withdrawals::none_at(at),
        );
    }

    let chain = vec![leaf, oem2, oem1];
    let vehicle = verify_provisioning(
        &platform,
        &pool,
        Generation::Iso2,
        &refs(&chain),
        &status,
        at,
    )
    .expect("Hubject's own chain, against Hubject's own root");
    assert_eq!(vehicle.pcid.as_str(), "HUBJECTOPENPNC0008");
    assert_eq!(vehicle.pcid.wmi(), "HUB");
    assert_eq!(vehicle.anchor, "V2G Root");

    // …and the PCID out of a real certificate is the one the ecosystem's own
    // documentation describes: 18 alphanumeric characters, WMI first.
    let parsed: Pcid = "hubjectopenpnc0008".parse().expect("the same identifier");
    assert_eq!(vehicle.pcid, parsed);
}
