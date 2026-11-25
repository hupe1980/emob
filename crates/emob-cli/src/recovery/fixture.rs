//! Build a cold cut from synthetic commands in a single writer process. Each
//! journal is closed before capture. No HTTP listeners, timers or senders run.
use anyhow::{Context, Result, ensure};
use emob_service::Actor;
use emob_service::head::{self, Signing, SigningKey};
use emob_service::journal::{File, Journal, Kinded};
use emob_service::vault::{DataKey, Sealing};
use emob_service::witness::Witness;
use serde::Serialize;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use super::replay::Expected;
use super::{Entry, Manifest, SERVICES, VERSION, digest};

const EXAMPLE: &str = concat!(
    include_str!("../../../../deploy/estate.example.toml"),
    "\n[[subscribers]]\nname = \"pilot-archive\"\nurl = \"https://archive.invalid/pilot\"\nsecret_env = \"PILOT_ARCHIVE_SECRET\"\ntypes = [\"de.emob.invoice.issued\"]\n",
);

fn directory(path: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(path)
}

fn append<J: Journal, E: Kinded + Serialize>(journal: &J, event: &E) -> Result<()> {
    journal.append(event.kind(), &Actor::Estate, &serde_json::to_string(event)?)?;
    Ok(())
}

struct Writer<'a> {
    cold: &'a Path,
    keys: BTreeMap<String, String>,
    signers: BTreeMap<String, String>,
}
impl Writer<'_> {
    fn journal(&mut self, name: &str) -> Result<Signing<File>> {
        let path = self.cold.join(format!("{name}.ndjson"));
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).map_err(|_| anyhow::anyhow!("random unavailable"))?;
        let key = SigningKey::from_seed(&seed, name);
        self.signers.insert(name.into(), key.verifier().to_string());
        Ok(Signing::open(File::open(&path)?, key, head::beside(&path))?)
    }
    fn key(&mut self, name: &str) -> Result<DataKey> {
        use base64::Engine as _;
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|_| anyhow::anyhow!("random unavailable"))?;
        self.keys.insert(
            name.into(),
            base64::engine::general_purpose::STANDARD.encode(bytes),
        );
        Ok(DataKey::from_bytes(&bytes))
    }
}

pub(super) fn create(root: &Path) -> Result<()> {
    use csmsd::journal::{Delivery, Target};
    use emob_sim::{Fault, FaultPlan, Rate, ReferenceDay};
    use time::macros::datetime;
    // An existing or incomplete attempt must be preserved. Never merge outputs.
    directory(root)?;
    let (cold, evidence, keys) = (root.join("copy"), root.join("evidence"), root.join("keys"));
    for path in [&cold, &evidence, &keys] {
        directory(path)?;
    }
    let mut writer = Writer {
        cold: &cold,
        keys: BTreeMap::new(),
        signers: BTreeMap::new(),
    };
    let at = datetime!(2026-11-24 23:00 UTC);
    let day = || {
        ReferenceDay::builder()
            .stations(2)
            .sessions_per_station(1)
            .seed(2701)
            .day_start(datetime!(2026-11-22 0:00 UTC))
    };
    let cdr = day()
        .faults(FaultPlan::none())
        .build()
        .run()
        .settled
        .into_iter()
        .next()
        .context("settled case")?;
    let refusal = day()
        .faults(FaultPlan::none().with(Fault::TamperedValue, Rate::ALWAYS))
        .build()
        .run()
        .refused
        .into_iter()
        .nth(1)
        .context("refused case")?;
    let id = cdr.key.to_string();
    let price = cdr.total_cost().context("rated case")?;
    let estate: emob_service::estate::Estate = emob_service::estate::from_str(EXAMPLE)?;
    let bill: billd::daemon::Estate = emob_service::estate::from_str(EXAMPLE)?;
    let partner = bill.partners.first().context("fixture partner")?;
    let body = emob_service::cloudevent::CloudEvent::new(
        &id,
        "csmsd",
        emob_service::events::cdr::ISSUED,
        serde_json::json!({"cdr":cdr,"token":{"issuer":partner.party}}),
    )
    .about(format!("cdr/{id}"));
    {
        let j = Sealing::new(
            writer.journal("csmsd")?,
            Arc::new(writer.key("csmsd")?),
            "csmsd/journal",
        );
        append(
            &j,
            &csmsd::journal::Event::Settled {
                station: "pilot-1".into(),
                transaction: cdr.session_id.to_string(),
                cdr: Box::new(cdr.clone()),
                outcome: csmsd::Outcome::Settled {
                    key: id.clone(),
                    energy: cdr.total_energy.to_string(),
                    retries: 0,
                    gap: vec![],
                },
                deliveries: vec![Delivery {
                    id: id.clone(),
                    to: Target::Billd,
                    body: serde_json::to_string(&body)?,
                }],
                at,
            },
        )?;
        append(
            &j,
            &csmsd::journal::Event::Noted {
                closes: None,
                outcome: csmsd::Outcome::Refused {
                    identity: "pilot-2".into(),
                    transaction: refusal.session_id.to_string(),
                    reasons: refusal.reasons.clone(),
                    findings: vec![],
                },
                at,
            },
        )?;
    }
    let (invoice, invoice_total) = {
        let j = Sealing::new(
            writer.journal("billd")?,
            Arc::new(writer.key("billd")?),
            "billd/journal",
        );
        let books = billd::Books::open(
            "emob",
            billd::Numbering::series(&bill.billing.series, at.year()),
            billd::ChartOfAccounts::new(),
            j,
        )?;
        let mut books = books.delivering_to(vec!["pilot-archive".to_owned()]);
        ensure!(
            books.receive(
                &Actor::service("csmsd"),
                cdr.clone(),
                partner.party.clone(),
                None
            )?,
            "first receipt"
        );
        let closing = billd::daemon::closing(
            &bill,
            partner,
            cdr.ended_at.year(),
            cdr.ended_at.month(),
            at.date(),
        );
        let number = books.close(&Actor::Timer, &closing)?.context("invoice")?;
        let amount = books
            .state()
            .issued(&number)
            .context("invoice at the cut")?
            .invoice
            .payable_total();
        (number, amount.amount().to_string())
    };
    {
        let j = Sealing::new(
            writer.journal("roamd")?,
            Arc::new(writer.key("roamd")?),
            "roamd/journal",
        );
        let consignment = roamd::Consignment {
            key: cdr.key.clone(),
            reach: emob_roam::Reach::Direct(partner.party.clone()),
            wire: emob_roam::Wire::Ocpi,
            ended_at: cdr.ended_at,
            delivery: roamd::Delivery::Pending,
            supersedes: None,
            credits: None,
        };
        append(
            &j,
            &roamd::node::Event::Consigned {
                consignment: Box::new(consignment),
                document: None,
            },
        )?;
        append(
            &j,
            &roamd::node::Event::Delivered {
                key: cdr.key.clone(),
                delivery: roamd::Delivery::Accepted { at, location: None },
            },
        )?;
    }
    {
        let provider = empd::Empd::new(estate.parties.provider.clone(), writer.key("empd")?);
        let mut book = empd::book::Book::open(provider, writer.journal("empd")?)?;
        empd::seed::apply(&mut book, &estate.contracts)?;
    }
    let publication = BTreeMap::from([("pilot-EVSE".to_owned(), "1".to_owned())]);
    let rate_ids = vec!["pilot-rate".to_owned()];
    {
        let j = writer.journal("poid")?;
        append(
            &j,
            &poid::Event::Accepted {
                at,
                published_at: at,
                published: serde_json::from_value(
                    serde_json::json!({"facilities":publication,"rates":rate_ids}),
                )?,
            },
        )?;
    }
    // All five writers have closed and no code below can append to the source.
    // Record the expectations separately, from the exercise inputs and invoice
    // as observed by its issuer, before running any recovery code.
    let contract = estate.contracts.first().context("fixture contract")?;
    let expected = Expected {
        settled: id,
        energy_kwh: cdr.total_energy.kwh().to_string(),
        price: price.amount().to_string(),
        currency: price.currency().as_str().into(),
        refused_station: "pilot-2".into(),
        refused_tx: refusal.session_id.to_string(),
        reasons: refusal.reasons,
        invoice,
        invoice_total,
        partner: partner.party.to_string(),
        contract: contract.id.to_string(),
        allowed_uid: contract
            .tokens
            .iter()
            .find(|x| !x.blocked)
            .context("allowed card")?
            .uid
            .clone(),
        blocked_uid: contract
            .tokens
            .iter()
            .find(|x| x.blocked)
            .context("blocked card")?
            .uid
            .clone(),
        publication,
        rate_ids,
        owed: BTreeMap::from([
            ("billd".into(), 1),
            ("roamd".into(), 0),
            ("subscriber".into(), 0),
            ("invoice_receiver".into(), 0),
            ("invoice_subscriber".into(), 1),
            ("roaming_peer".into(), 0),
        ]),
    };
    let oracle = serde_json::to_vec_pretty(&expected)?;
    std::fs::write(evidence.join("outcomes.json"), &oracle)?;
    std::fs::write(cold.join("estate.toml"), EXAMPLE)?;
    std::fs::write(keys.join("data.json"), serde_json::to_vec(&writer.keys)?)?;
    let captured = time::OffsetDateTime::now_utc();
    let witness_path = evidence.join("witness.ndjson");
    let witness = Witness::open(&witness_path, &writer.signers)?;
    let mut services = BTreeMap::new();
    for name in SERVICES {
        let jp = cold.join(format!("{name}.ndjson"));
        let note = std::fs::read_to_string(head::beside(&jp))?;
        let journal = std::fs::read(&jp)?;
        let signer = writer.signers.get(name).context("fixture signer")?;
        witness.observe(
            name,
            &note,
            Some(signer),
            captured,
            &Actor::operator("pilot")?,
        )?;
        services.insert(
            name.into(),
            Entry {
                bytes: journal.len() as u64,
                journal_sha256: digest(&journal),
                head_sha256: digest(note.as_bytes()),
                signer: signer.clone(),
                key_sha256: writer.keys.get(name).map(|v| digest(v.as_bytes())),
            },
        );
    }
    let cut = Manifest {
        version: VERSION,
        synthetic: true,
        build_sha256: super::build_digest()
            .map_err(|_| anyhow::anyhow!("build digest unavailable"))?,
        config_sha256: digest(EXAMPLE.as_bytes()),
        witness_sha256: digest(&std::fs::read(witness_path)?),
        outcomes_sha256: digest(&oracle),
        captured,
        assessed: at,
        services,
    };
    std::fs::write(evidence.join("cut.json"), serde_json::to_vec_pretty(&cut)?)?;
    Ok(())
}

pub(crate) fn main(args: &[String], out: &mut impl Write) -> u8 {
    let [root] = args else {
        let _ = writeln!(out, "usage: emob recovery-fixture NEW_DIRECTORY");
        return 64;
    };
    match create(Path::new(root)) {
        Ok(()) => {
            let _ = writeln!(
                out,
                "Synthetic cut created. Keep copy, independent evidence and keyring in separate custody."
            );
            0
        }
        Err(_) => {
            let _ = writeln!(
                out,
                "Could not create a complete synthetic cut in a new directory."
            );
            2
        }
    }
}
