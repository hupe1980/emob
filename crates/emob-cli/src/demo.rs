//! `emob demo` — five services, simulated stations and a roaming peer on loopback.
//!
//! One process runs the CSMS, provider, roaming, billing and desk services.
//! It writes a seeded `estate.toml` for **Stadtwerke
//! Musterstadt eMobility** (one site, three charge points on OCPP 1.6, 2.0.1
//! and 2.1, one `emob-sim` meter each, security profile 2 over `just dev-ca`'s
//! throwaway TLS); starts `empd`, `billd`, `roamd`, `csmsd` and `deskd` through
//! the very `daemon::run` each `main` calls (FR-003); registers `ocpi-kit`'s
//! `MockPeer` as the roaming partner; connects three `ocpp-kit` stations over
//! `wss://`; and plays a morning on one simulated clock — seven sessions
//! billed, one signed value tampered with and refused, one blocked card. Then
//! it closes the month with `billd`'s own calendar function and keeps running
//! until Ctrl-C.
//!
//! `--check` plays the same story at full speed and asserts every outcome it
//! printed (SR-078): CI runs it on every push.
//!
//! # Loopback, and nothing that outlives it
//!
//! Every listener binds a loopback address and a non-loopback `--listen` is
//! refused before anything binds (SR-070). Every secret is minted for this run
//! and lives only in this process (SR-071); the one printed is the operator
//! token inside the Heute link (SR-072, SR-074). Journals live in a scratch
//! directory removed on exit (SR-073).

mod estate;
mod inbox;
mod invoice;
mod station;
mod story;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use base64::Engine as _;
use emob_service::estate::Env;
use emob_service::{Identity, Shutdown};

use estate::{CHARGE_POINTS, Ports};
use station::{Started, Wire};
use story::{Clock, Speed};

/// What `emob demo` was asked for.
#[derive(Debug, Clone)]
struct Options {
    seed: u64,
    clock: Option<time::OffsetDateTime>,
    speed: Option<Speed>,
    listen: IpAddr,
    tls: PathBuf,
    links: Option<PathBuf>,
    check: bool,
}

/// A refusal before anything started, with its exit code.
#[derive(Debug)]
struct Refusal(u8, String);

const USAGE: u8 = 64;
const UNAVAILABLE: u8 = 69;

fn parse(args: &[String]) -> Result<Options, Refusal> {
    let mut options = Options {
        seed: 2026,
        clock: None,
        speed: None,
        listen: IpAddr::from([127, 0, 0, 1]),
        tls: PathBuf::from("target/dev-tls"),
        links: None,
        check: false,
    };
    let mut args = args.iter();
    let usage = |what: String| Refusal(USAGE, what);
    while let Some(flag) = args.next() {
        if flag == "--check" {
            options.check = true;
            continue;
        }
        let value = args
            .next()
            .ok_or_else(|| usage(format!("{flag} needs a value")))?;
        match flag.as_str() {
            "--seed" => {
                options.seed = value
                    .parse()
                    .map_err(|_| usage(format!("--seed {value}: a whole number")))?;
            }
            "--clock" => options.clock = Some(instant(value).map_err(usage)?),
            "--speed" => options.speed = Some(speed(value).map_err(usage)?),
            "--listen" => {
                options.listen = value
                    .parse()
                    .map_err(|_| usage(format!("--listen {value}: an IP address")))?;
            }
            "--tls" => options.tls = PathBuf::from(value),
            "--links" => options.links = Some(PathBuf::from(value)),
            other => return Err(usage(format!("{other}: not an option of emob demo"))),
        }
    }
    // SR-070: refused before anything binds.
    if !options.listen.is_loopback() {
        return Err(usage(format!(
            "--listen {}: the demo binds loopback only — it hands out an operator token \
             and station passwords, throwaway ones, and they stay on this machine",
            options.listen
        )));
    }
    Ok(options)
}

/// `2026-06-01T08:00Z`, with or without seconds.
fn instant(text: &str) -> Result<time::OffsetDateTime, String> {
    use time::format_description::well_known::Rfc3339;
    let bad = || format!("--clock {text}: an RFC 3339 instant such as 2026-06-01T08:00Z");
    time::OffsetDateTime::parse(text, &Rfc3339)
        .or_else(|_| {
            let with_seconds = match text.get(..16) {
                Some(head) => format!("{head}:00{}", &text[16..]),
                None => return Err(bad()),
            };
            time::OffsetDateTime::parse(&with_seconds, &Rfc3339).map_err(|_| bad())
        })
        .map_err(|_| bad())
}

fn speed(text: &str) -> Result<Speed, String> {
    if text == "max" {
        return Ok(Speed::Max);
    }
    match text.parse::<u32>() {
        Ok(times @ 1..=10_000) => Ok(Speed::Times(times)),
        _ => Err(format!("--speed {text}: 1 to 10000, or max")),
    }
}

/// SR-075 / SR-076: the dev CA's material is there, and its key is not.
fn tls_material(dir: &Path) -> Result<(), Refusal> {
    for file in ["ca.pem", "server.pem", "server.key"] {
        if !dir.join(file).is_file() {
            return Err(Refusal(
                UNAVAILABLE,
                format!(
                    "{} is missing — the demo's stations speak wss:// (security profile 2); \
                     mint the throwaway certificates with `just dev-ca`",
                    dir.join(file).display()
                ),
            ));
        }
    }
    if dir.join("ca.key").exists() {
        return Err(Refusal(
            UNAVAILABLE,
            format!(
                "{} holds the CA's private key — a CA that can still sign is not a throwaway one; \
                 re-mint with `just dev-ca`",
                dir.join("ca.key").display()
            ),
        ));
    }
    Ok(())
}

// ── secrets (SR-071) ────────────────────────────────────────────────────────

fn random_hex(bytes: usize) -> String {
    let mut buffer = vec![0_u8; bytes];
    getrandom::fill(&mut buffer).expect("the OS random number generator");
    hex::encode(buffer)
}

/// Everything secret this run holds, minted now.
struct Secrets {
    operator: String,
    desk_read: String,
    service: String,
    webhook: String,
    token_a: String,
    token_b: String,
    data_keys: [String; 4],
    signing_keys: [String; 4],
    passwords: [String; 3],
}

impl Secrets {
    fn mint() -> Self {
        let mut webhook = vec![0_u8; 24];
        getrandom::fill(&mut webhook).expect("the OS random number generator");
        Self {
            operator: random_hex(16),
            desk_read: random_hex(16),
            service: random_hex(16),
            webhook: format!(
                "whsec_{}",
                base64::engine::general_purpose::STANDARD.encode(webhook)
            ),
            token_a: random_hex(16),
            token_b: random_hex(16),
            data_keys: std::array::from_fn(|_| emob_service::vault::DataKey::generate()),
            signing_keys: std::array::from_fn(|_| emob_service::head::SigningKey::generate()),
            passwords: std::array::from_fn(|_| random_hex(20)),
        }
    }

    fn env(&self) -> Env {
        let mut values: BTreeMap<&'static str, String> = [
            (estate::env::OPERATOR, self.operator.clone()),
            (estate::env::DESK_READ, self.desk_read.clone()),
            (estate::env::SERVICE, self.service.clone()),
            (estate::env::WEBHOOK, self.webhook.clone()),
            (estate::env::TOKEN_A, self.token_a.clone()),
        ]
        .into_iter()
        .collect();
        for (name, key) in estate::env::DATA_KEYS.iter().zip(&self.data_keys) {
            values.insert(name, key.clone());
        }
        for (name, key) in estate::env::SIGNING_KEYS.iter().zip(&self.signing_keys) {
            values.insert(name, key.clone());
        }
        Arc::new(move |name| values.get(name).cloned())
    }

    /// Every secret but the operator token, which the Heute link carries.
    fn never_printed(&self) -> Vec<&str> {
        let mut all = vec![
            self.desk_read.as_str(),
            self.service.as_str(),
            self.webhook.as_str(),
            self.token_a.as_str(),
            self.token_b.as_str(),
        ];
        all.extend(self.data_keys.iter().map(String::as_str));
        all.extend(self.signing_keys.iter().map(String::as_str));
        all.extend(self.passwords.iter().map(String::as_str));
        all
    }

    /// What no log line may hold (SR-102): every secret, the operator token
    /// too, and the cards' UIDs.
    fn never_logged(&self) -> Vec<&str> {
        let mut all = self.never_printed();
        all.extend([
            self.operator.as_str(),
            estate::GOOD_CARD,
            estate::BLOCKED_CARD,
        ]);
        all
    }
}

// ── the scratch directory (SR-073) ──────────────────────────────────────────

/// A directory only this user can read, removed when dropped — on a normal
/// exit, after Ctrl-C and after a failed check alike.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> std::io::Result<Self> {
        let nanos = time::OffsetDateTime::now_utc().unix_timestamp_nanos();
        let dir = std::env::temp_dir().join(format!("emob-demo-{}-{nanos}", std::process::id()));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(&dir)?;
        Ok(Self(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A file removed when dropped: the links for `just screenshots`.
struct Removed(Option<PathBuf>);

impl Drop for Removed {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}

// ── ports (SR-077) ──────────────────────────────────────────────────────────

/// The ports, each checked free by binding it. Fixed ones are what the README
/// names; `--check` takes whatever the system hands out. The partner's
/// listener is kept: nobody else's code binds it.
fn ports(listen: IpAddr, fixed: bool) -> Result<(Ports, tokio::net::TcpListener), Refusal> {
    let wanted: [(&str, u16); 7] = [
        ("csmsd (OCPP)", 9000),
        ("csmsd", 9001),
        ("roamd", 9583),
        ("billd", 9584),
        ("empd", 9585),
        ("deskd", 9587),
        ("the partner", 9590),
    ];
    let mut held = Vec::new();
    for (daemon, port) in wanted {
        let addr = SocketAddr::new(listen, if fixed { port } else { 0 });
        let listener = std::net::TcpListener::bind(addr).map_err(|error| {
            Refusal(
                UNAVAILABLE,
                format!(
                    "port {port} ({daemon}) on {listen} is in use ({error}) — stop whatever holds it; \
                     `lsof -i :{port}` names it"
                ),
            )
        })?;
        held.push(listener);
    }
    let addr = |i: usize| held[i].local_addr().expect("a bound socket");
    let ports = Ports {
        ocpp: addr(0),
        csmsd: addr(1),
        roamd: addr(2),
        billd: addr(3),
        empd: addr(4),
        deskd: addr(5),
    };
    let partner = held.pop().expect("seven listeners");
    partner
        .set_nonblocking(true)
        .and_then(|()| tokio::net::TcpListener::from_std(partner))
        .map(|listener| (ports, listener))
        .map_err(|error| Refusal(UNAVAILABLE, format!("the partner's port: {error}")))
}

// ── output ──────────────────────────────────────────────────────────────────

/// Everything the demo says goes through here, so `--check` can hold its own
/// output to SR-074.
#[derive(Default)]
struct Out(Mutex<String>);

impl Out {
    fn say(&self, line: impl AsRef<str>) {
        let line = line.as_ref();
        let _ = writeln!(std::io::stdout().lock(), "{line}");
        let mut held = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held.push_str(line);
        held.push('\n');
    }

    fn text(&self) -> String {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

/// One of the three links, aligned: label, address, what it is.
fn link(label: &str, url: &str, what: &str) -> String {
    format!("  {label:<9} {url}  — {what}")
}

fn berlin(at: time::OffsetDateTime) -> String {
    let zone = emob_core::TimeZone::new("Europe/Berlin").expect("a zone the bundle has");
    let local = zone.local(at);
    format!("{:02}:{:02}", local.time.hour(), local.time.minute())
}

// ── the run ─────────────────────────────────────────────────────────────────

/// `emob demo …`.
pub fn main(args: &[String]) -> ExitCode {
    let options = match parse(args) {
        Ok(options) => options,
        Err(Refusal(code, why)) => {
            eprintln!("emob demo: {why}");
            return ExitCode::from(code);
        }
    };
    if let Err(Refusal(code, why)) = tls_material(&options.tls) {
        eprintln!("emob demo: {why}");
        return ExitCode::from(code);
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("emob demo: {error}");
            return ExitCode::from(UNAVAILABLE);
        }
    };
    let code = runtime.block_on(run(options));
    runtime.shutdown_timeout(Duration::from_secs(2));
    code
}

/// The daemons, running.
struct Estate {
    empd: empd::daemon::Running,
    billd: billd::daemon::Running,
    roamd: roamd::daemon::Running,
    csmsd: csmsd::daemon::Running,
    deskd: deskd::Running,
}

impl Estate {
    async fn stop(self) {
        self.deskd.kill().await;
        self.csmsd.kill().await;
        self.roamd.kill().await;
        self.billd.kill().await;
        self.empd.kill().await;
    }
}

async fn run(options: Options) -> ExitCode {
    let began = Instant::now();
    let speed = options.speed.unwrap_or(if options.check {
        Speed::Max
    } else {
        Speed::Times(60)
    });
    let (ports, partner_listener) = match ports(options.listen, !options.check) {
        Ok(found) => found,
        Err(Refusal(code, why)) => {
            eprintln!("emob demo: {why}");
            return ExitCode::from(code);
        }
    };
    let scratch = match Scratch::new() {
        Ok(scratch) => scratch,
        Err(error) => {
            eprintln!("emob demo: a scratch directory: {error}");
            return ExitCode::from(UNAVAILABLE);
        }
    };
    let links = Removed(options.links.clone());
    let out = Out::default();
    let secrets = Secrets::mint();
    // SR-102: under --check every daemon's log line is kept, as the JSON a
    // deployment writes, and searched for anything secret at the end.
    let logs = if options.check {
        emob_service::telemetry::Captured::start("debug")
    } else {
        None
    };
    let result = tokio::select! {
        result = demo(&options, speed, ports, partner_listener, &scratch, &secrets, &out) => result,
        _ = tokio::signal::ctrl_c() => Ok(Ending::Interrupted),
    };
    drop(links);
    drop(scratch);
    match result {
        Ok(Ending::Interrupted) => {
            out.say("\nemob demo stopped — journals and keys removed.");
            ExitCode::SUCCESS
        }
        Ok(Ending::Checked(failures, tokens)) => {
            let mut failures = failures;
            let printed = out.text();
            if let Some(found) = secrets
                .never_printed()
                .into_iter()
                .find(|secret| printed.contains(secret))
            {
                let _ = found;
                failures.push(
                    "SR-074: the output holds a secret other than the operator token".to_owned(),
                );
            }
            if let Some(logs) = &logs {
                let mut never = secrets.never_logged();
                never.extend(tokens.iter().map(String::as_str));
                failures.extend(logs_hold_nothing_secret(&logs.text(), &never));
            }
            if failures.is_empty() {
                let tenths = began.elapsed().as_millis() / 100;
                out.say(format!(
                    "✓ demo check passed in {}.{} s",
                    tenths / 10,
                    tenths % 10
                ));
                ExitCode::SUCCESS
            } else {
                for failure in &failures {
                    eprintln!("✕ {failure}");
                }
                ExitCode::from(1)
            }
        }
        Err(error) => {
            eprintln!("emob demo: {error:#}");
            ExitCode::from(1)
        }
    }
}

enum Ending {
    Interrupted,
    /// The failures, and every receipt token the logs must not hold
    /// (SR-130).
    Checked(Vec<String>, Vec<String>),
}

#[allow(clippy::too_many_lines)]
async fn demo(
    options: &Options,
    speed: Speed,
    ports: Ports,
    partner_listener: tokio::net::TcpListener,
    scratch: &Scratch,
    secrets: &Secrets,
    out: &Out,
) -> Result<Ending> {
    let start = options.clock.unwrap_or_else(|| {
        let now = time::OffsetDateTime::now_utc();
        now.replace_second(0)
            .and_then(|t| t.replace_nanosecond(0))
            .unwrap_or(now)
    });
    let clock = Arc::new(Clock::new(start, speed));
    let tls = std::path::absolute(&options.tls).context("the TLS directory")?;

    // The estate.
    let hashes: [String; 3] = {
        let mut hashes = Vec::new();
        for (point, password) in CHARGE_POINTS.iter().zip(&secrets.passwords) {
            hashes.push(
                csmsd::edge::hash_password(point.version, password).map_err(anyhow::Error::msg)?,
            );
        }
        hashes.try_into().expect("three stations")
    };
    // The first of the month after next: after every session this run makes.
    let price_change = (1..=2)
        .try_fold(start.date(), |day, _| {
            day.replace_day(1)
                .ok()?
                .checked_add(time::Duration::days(32))?
                .replace_day(1)
                .ok()
        })
        .context("the day the demo's price changes")?;
    let text = estate::text(options.seed, &ports, &hashes, &tls, price_change);
    std::fs::write(scratch.0.join("estate.toml"), &text).context("writing the estate")?;
    // Spec 012: the estate the demo runs is one `emob check` passes — the
    // demo is the first estate anybody reads.
    anyhow::ensure!(
        crate::check::passes(&scratch.0.join("estate.toml")),
        "the demo's own estate does not pass `emob check`"
    );
    let env = secrets.env();
    let journal = |name: &str| scratch.0.join(format!("{name}.ndjson"));
    let me = |name: &'static str| Identity::new(name, "demo");

    out.say(format!(
        "emob demo — Stadtwerke Musterstadt eMobility · seed {} · {} · from {} {} Europe/Berlin",
        options.seed,
        match speed {
            Speed::Times(times) => format!("{times}×"),
            Speed::Max => "full speed".to_owned(),
        },
        start.date(),
        berlin(start),
    ));

    let business_clock: emob_service::Clock = {
        let clock = Arc::clone(&clock);
        Arc::new(move || clock.now())
    };
    let empd = empd::daemon::run_with_clock(
        empd::daemon::Options {
            http: ports.empd,
            journal: journal("empd"),
            estate: emob_service::estate::from_str(&text)?,
            env: env.clone(),
            shutdown: Shutdown::new(),
            identity: me("empd"),
        },
        Arc::clone(&business_clock),
    )
    .await
    .context("empd")?;
    let billd = billd::daemon::run(billd::daemon::Options {
        http: ports.billd,
        journal: journal("billd"),
        estate: emob_service::estate::from_str(&text)?,
        env: env.clone(),
        shutdown: Shutdown::new(),
        identity: me("billd"),
    })
    .await
    .context("billd")?;
    let roamd = roamd::daemon::run(roamd::daemon::Options {
        http: ports.roamd,
        journal: journal("roamd"),
        estate: emob_service::estate::from_str(&text)?,
        env: env.clone(),
        shutdown: Shutdown::new(),
        identity: me("roamd"),
    })
    .await
    .context("roamd")?;
    let partner = inbox::start(partner_listener, &secrets.token_b)?;
    inbox::register(
        ports.roamd,
        partner.addr,
        &secrets.token_a,
        &secrets.token_b,
    )
    .await
    .context("the partner registering with roamd")?;
    let csmsd = csmsd::daemon::run(csmsd::daemon::Options {
        ocpp: ports.ocpp,
        http: ports.csmsd,
        journal: journal("csmsd"),
        estate: emob_service::estate::from_str(&text)?,
        env: env.clone(),
        shutdown: Shutdown::new(),
        identity: me("csmsd"),
        // The morning's clock, so a session is counted on the day Heute shows.
        clock: Arc::clone(&business_clock),
    })
    .await
    .context("csmsd")?;
    let deskd = deskd::run(deskd::Options {
        http: ports.deskd,
        estate: emob_service::estate::from_str(&text)?,
        env,
        shutdown: Shutdown::new(),
        identity: me("deskd"),
        clock: business_clock,
        witness: scratch.0.join("witness.ndjson"),
    })
    .await
    .context("deskd")?;
    let estate = Estate {
        empd,
        billd,
        roamd,
        csmsd,
        deskd,
    };
    if !estate.csmsd.readiness.is_ready() {
        anyhow::bail!(
            "csmsd is not ready: {:?}",
            estate.csmsd.readiness.blockers()
        );
    }

    // The stations, over TLS with their passwords.
    let meters = estate::meters(options.seed);
    let mut wires = Vec::new();
    for ((point, password), meter) in CHARGE_POINTS.iter().zip(&secrets.passwords).zip(&meters) {
        wires.push(
            Wire::connect(
                point.identity,
                point.version,
                ports.ocpp,
                &tls.join("ca.pem"),
                password,
                meter.public_key().sec1_bytes(),
            )
            .await?,
        );
    }
    out.say(format!(
        "  ✓ empd billd roamd csmsd deskd up · CP-1 (1.6) CP-2 (2.0.1) CP-3 (2.1) on wss://{} with profile 2 · partner DE*MSP registered",
        ports.ocpp
    ));
    // SR-072 / SR-077: every listener is bound, on loopback.
    out.say(format!(
        "\n{}",
        link(
            "Heute",
            &format!("http://demo:{}@{}/", secrets.operator, ports.deskd),
            "the operator page, signed in"
        )
    ));

    // The morning.
    let mut meters = meters;
    let sessions = story::sessions(options.seed, start, &mut meters);
    let mut transactions: Vec<Option<String>> = vec![None; sessions.len()];
    let mut said_receipt: Option<String> = None;
    let mut refused_receipt: Option<String> = None;
    let mut said_inbox = false;
    let mut said_display = false;
    let mut blocked = Vec::new();
    for (at, s, e) in story::timeline(&sessions) {
        clock.reach(at).await;
        let session = &sessions[s];
        let event = &session.charged.events[e];
        let wire = &mut wires[session.beat.station];
        let point = &CHARGE_POINTS[session.beat.station];
        if event.kind != emob_ocpp::EventKind::Started && transactions[s].is_none() {
            continue; // refused at the start: nothing more is sent
        }
        let id = transactions[s]
            .clone()
            .unwrap_or_else(|| session.label.clone());
        let answer = wire.play(event, session.beat.card, &id).await?;
        match answer {
            Some(Started::Accepted(id)) => transactions[s] = Some(id),
            Some(Started::Refused(status)) => {
                blocked.push((point.identity, status.clone()));
                out.say(format!(
                    "  {}  {} · {:<5}  ⊘ card {}  answered {status} — no transaction opens",
                    berlin(at),
                    point.identity,
                    point.version.to_string(),
                    session.beat.card,
                ));
            }
            None => {}
        }
        if event.kind == emob_ocpp::EventKind::Ended {
            let key = (point.identity.to_owned(), id.clone());
            let line = verdict(&estate.csmsd, &key);
            out.say(format!(
                "  {}  {} · {:<5}  {}",
                berlin(at),
                point.identity,
                point.version.to_string(),
                line.text
            ));
            if session.beat.station == 2 && !said_display {
                out.say(format!(
                    "{:>23}📟 its display shows \"Beleg: <receipt link>\" for 10 minutes",
                    ""
                ));
                said_display = true;
            }
            match line.billed {
                true if said_receipt.is_none() => {
                    if let Some(path) = estate.csmsd.csmsd.receipt_path(&key) {
                        out.say(format!(
                            "\n{}\n",
                            link(
                                "Receipt",
                                &format!("http://{}{path}", ports.csmsd),
                                "what the driver's phone opens"
                            )
                        ));
                        said_receipt = Some(path);
                    }
                }
                false => refused_receipt = estate.csmsd.csmsd.receipt_path(&key),
                _ => {}
            }
        }
        if !said_inbox && !partner.peer.cdrs.is_empty() {
            out.say(format!(
                "\n{}\n",
                link(
                    "Inbox",
                    &format!("http://{}/", partner.addr),
                    "the roaming partner's platform"
                )
            ));
            said_inbox = true;
        }
    }

    // Every record reaches billd and the partner.
    let billable = story::billable();
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline
        && !(received(&estate.billd).len() == billable
            && partner.peer.cdrs.len() == billable
            && consignments(&estate.roamd).iter().all(|accepted| *accepted))
    {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    if !said_inbox && !partner.peer.cdrs.is_empty() {
        out.say(format!(
            "\n{}\n",
            link(
                "Inbox",
                &format!("http://{}/", partner.addr),
                "the roaming partner's platform"
            )
        ));
    }

    // The month's end: billd's own calendar, at the simulated date.
    let last = clock.now();
    let next_month = last
        .date()
        .replace_day(1)
        .ok()
        .and_then(|first| first.checked_add(time::Duration::days(32)))
        .and_then(|later| later.replace_day(1).ok())
        .context("the next month")?;
    billd::daemon::close_what_is_due(&estate.billd.books, &estate.billd.estate, next_month);
    let invoices: Vec<(String, Vec<String>, String)> = estate
        .billd
        .books
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .state()
        .documents()
        .map(|issued| {
            (
                issued.invoice.number.clone(),
                issued
                    .invoice
                    .records()
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
                if issued.invoice.payable_rounding.is_zero() {
                    format!(
                        "{} EUR incl. VAT ({} net)",
                        issued.invoice.payable_total().amount(),
                        issued.invoice.taxable_total().amount()
                    )
                } else {
                    format!(
                        "{} EUR incl. VAT ({} net, rounding {})",
                        issued.invoice.payable_total().amount(),
                        issued.invoice.taxable_total().amount(),
                        issued.invoice.payable_rounding
                    )
                },
            )
        })
        .collect();
    // Spec 023: what the next person needs, on the station they will drive to.
    if let Ok(by) = emob_service::Actor::operator("demo") {
        let _ = estate.csmsd.csmsd.note(
            &by,
            "CP-3",
            "Zufahrt über das Tor am Betriebshof, Schlüssel im Kasten neben Säule 2.",
        );
    }
    for (number, records, total) in &invoices {
        out.say(format!(
            "  ⏩ month end ({next_month}): billd issues invoice {number} to MSP Mobility GmbH — {} lines, {total}",
            records.len()
        ));
        out.say(format!(
            "\n{}\n",
            link(
                "Invoice",
                &format!(
                    "http://{}{}",
                    ports.billd,
                    estate.billd.invoices.path(number)
                ),
                "the bill its receipts are on, with its XRechnung"
            )
        ));
    }
    clock.freeze();

    // The witness reads every journal's signed head (spec 008).
    let rounds = estate.deskd.watch.round().await;
    let held = estate.deskd.watch.view();
    let witnessed: Vec<(String, bool)> = rounds
        .iter()
        .map(|(name, verdict)| {
            (
                (*name).to_owned(),
                verdict.as_ref().is_ok_and(|verdict| !verdict.is_alarm()),
            )
        })
        .collect();
    out.say(format!(
        "  🔏 witness: {}",
        rounds
            .iter()
            .map(|(name, verdict)| match verdict {
                Ok(verdict) if !verdict.is_alarm() => format!(
                    "{name} ✓ {}",
                    held.iter()
                        .find(|seen| seen.daemon == *name)
                        .map_or(0, |seen| seen.position)
                ),
                Ok(verdict) => format!("{name} ✕ {verdict:?}"),
                Err(why) => format!("{name} ? {why}"),
            })
            .collect::<Vec<_>>()
            .join(" · ")
    ));

    let settled: Vec<String> = estate
        .csmsd
        .csmsd
        .with_cdrs(|ledger| ledger.iter().map(|cdr| cdr.key.to_string()).collect());
    if let Some(path) = &options.links {
        let mut text = format!(
            "heute=http://{}/\ntoken={}\ninbox=http://{}/\n",
            ports.deskd, secrets.operator, partner.addr
        );
        if let Some(receipt) = &said_receipt {
            let _ = writeln!(text, "receipt=http://{}{receipt}", ports.csmsd);
        }
        if let Some(refused) = &refused_receipt {
            let _ = writeln!(text, "refused=http://{}{refused}", ports.csmsd);
        }
        // The month's invoice page (spec 010), which `just a11y` checks too.
        if let Some((number, ..)) = invoices.first() {
            let invoice = estate.billd.invoices.path(number);
            let _ = writeln!(text, "invoice=http://{}{invoice}", ports.billd);
        }
        write_private(path, &text)?;
    }

    if options.check {
        let seen = observe(
            &estate,
            &partner,
            &sessions,
            &transactions,
            &blocked,
            &settled,
            &invoices,
        );
        let mut seen = seen;
        seen.witnessed = witnessed;
        // Spec 011: each session's receipt link, and what each display showed.
        let receipts: Vec<(usize, String)> = sessions
            .iter()
            .zip(&transactions)
            .filter_map(|(session, transaction)| {
                let key = (
                    CHARGE_POINTS[session.beat.station].identity.to_owned(),
                    transaction.clone()?,
                );
                Some((session.beat.station, estate.csmsd.csmsd.receipt_path(&key)?))
            })
            .collect();
        let tokens: Vec<String> = receipts
            .iter()
            .filter_map(|(_, path)| path.strip_prefix("/r/").map(str::to_owned))
            .collect();
        let deadline = Instant::now() + Duration::from_secs(10);
        let wanted = receipts.iter().filter(|(station, _)| *station == 2).count();
        while Instant::now() < deadline && wires[2].displayed().len() < wanted {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        seen.displayed = wires
            .iter()
            .enumerate()
            .map(|(index, wire)| {
                let mut expected: Vec<String> = receipts
                    .iter()
                    .filter(|(station, _)| *station == index && index == 2)
                    .map(|(_, path)| format!("Beleg: http://{}{path}", ports.csmsd))
                    .collect();
                expected.sort();
                let mut shown: Vec<String> = wire
                    .displayed()
                    .into_iter()
                    .filter(|message| message.starts_with("Beleg: "))
                    .collect();
                shown.sort();
                (wire.identity.to_owned(), shown, expected)
            })
            .collect();
        // Spec 015: what each station's display was given besides receipts —
        // the price, its Preisstand and, on 2.1, the page's own code.
        seen.prices = wires
            .iter()
            .map(|wire| {
                (
                    wire.identity.to_owned(),
                    wire.displayed()
                        .into_iter()
                        .filter(|message| !message.starts_with("Beleg: "))
                        .collect(),
                )
            })
            .collect();
        seen.unattributed = unattributed(&scratch.0);
        let mut failures = judge(&seen, story::billable());
        // Spec 010: the invoice as its recipient reads it, files validated
        // by their consumer.
        match invoices.first() {
            Some((number, ..)) => {
                let fetched = invoice::fetch(
                    &format!("http://{}", ports.billd),
                    &estate.billd.invoices.path(number),
                )
                .await;
                failures.extend(invoice::judge(&fetched, said_receipt.as_deref()));
            }
            None => failures.push("billd issues the month's invoice".to_owned()),
        }
        out.say(format!("  records  {}", settled.join(" ")));
        for (number, ..) in &invoices {
            out.say(format!("  invoice  {number}"));
        }
        estate.stop().await;
        return Ok(Ending::Checked(failures, tokens));
    }

    out.say("\nThe morning is done; the estate keeps running. Ctrl-C ends the demo.");
    // Heartbeats, so Heute never shows a demo station as silent.
    loop {
        tokio::time::sleep(Duration::from_secs(240)).await;
        for wire in &wires {
            let _ = wire.heartbeat().await;
        }
    }
}

/// One session's verdict, as a line.
struct Line {
    billed: bool,
    text: String,
}

fn verdict(csmsd: &csmsd::daemon::Running, key: &csmsd::Key) -> Line {
    csmsd.csmsd.with_state(|state| {
        let issued = state
            .receipt_of
            .get(key)
            .and_then(|digest| state.receipts.get(digest))
            .and_then(|issued| issued.verdict.clone());
        match issued {
            Some(csmsd::journal::Verdict::Billed(cdr_key)) => {
                let cdr = state.cdrs.iter().find(|cdr| cdr.key == cdr_key);
                let (energy, cost) = cdr.map_or_else(Default::default, |cdr| {
                    (
                        cdr.total_energy.kwh().to_string(),
                        cdr.total_cost()
                            .map(|money| format!("{} EUR", money.amount()))
                            .unwrap_or_default(),
                    )
                });
                Line {
                    billed: true,
                    text: format!("{:<10}{energy} kWh · {cost} · {cdr_key}", "✓ billed"),
                }
            }
            Some(csmsd::journal::Verdict::Refused { reasons, findings }) => {
                let cite = findings
                    .first()
                    .map(|finding| format!(" {}", finding.kind.citation()))
                    .unwrap_or_default();
                Line {
                    billed: false,
                    text: format!(
                        "{:<10}{}{cite}",
                        "✕ refused",
                        reasons.last().map_or("no reason given", String::as_str)
                    ),
                }
            }
            None => Line {
                billed: false,
                text: "? no verdict".to_owned(),
            },
        }
    })
}

fn received(billd: &billd::daemon::Running) -> Vec<String> {
    billd
        .books
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .state()
        .received()
        .map(|received| received.cdr.key.to_string())
        .collect()
}

fn consignments(roamd: &roamd::daemon::Running) -> Vec<bool> {
    roamd
        .node
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .state()
        .consignments()
        .map(|consignment| consignment.delivery.is_accepted())
        .collect()
}

/// What the daemons hold once the story is over — read from their own state,
/// never from what the demo printed.
#[derive(Debug, Clone, Default)]
struct Observed {
    settled: Vec<String>,
    received: Vec<String>,
    partner_cdrs: usize,
    accepted: Vec<bool>,
    /// The tampered session(s): billed?, and the line the demo printed.
    tampered: Vec<(bool, String)>,
    blocked: Vec<String>,
    open: usize,
    invoices: Vec<(String, Vec<String>)>,
    /// What the billed sessions' receipts state, summed — each `Cost::gross`.
    receipts: rust_decimal::Decimal,
    /// BT-115 of each invoice.
    payable: Vec<rust_decimal::Decimal>,
    /// Each journalled daemon the witness read, and whether its head was
    /// witnessed without an alarm (SR-098).
    witnessed: Vec<(String, bool)>,
    /// Journals with a record that names no actor (SC-002).
    unattributed: Vec<String>,
    /// Each station, what its display showed and what it should have
    /// (spec 011): CP-3's receipt links, nothing on CP-1 and CP-2.
    displayed: Vec<(String, Vec<String>, Vec<String>)>,
    /// Each station: what it said at boot, and whether a meter signed a
    /// billed session there (spec 012).
    known: Vec<(String, Option<String>, bool, Option<String>)>,
    /// What each station's display showed that is not a receipt (spec 015).
    prices: Vec<(String, Vec<String>)>,
}

/// SC-002: every record of every journal the demo wrote names who caused it.
fn unattributed(dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    for name in ["csmsd", "billd", "roamd", "empd", "witness"] {
        let text = std::fs::read_to_string(dir.join(format!("{name}.ndjson"))).unwrap_or_default();
        let total = text.lines().count();
        let named = text
            .lines()
            .filter(|line| {
                serde_json::from_str::<serde_json::Value>(line)
                    .ok()
                    .and_then(|record| record["actor"].as_str().map(str::to_owned))
                    .is_some_and(|actor| actor.parse::<emob_service::Actor>().is_ok())
            })
            .count();
        if total == 0 || named != total {
            found.push(format!("{name}: {named} of {total} records name an actor"));
        }
    }
    found
}

/// SR-102: every captured line is JSON, a request line carries its id, and
/// none holds a secret, a token or a card UID — named by kind, never echoed.
fn logs_hold_nothing_secret(logs: &str, never: &[&str]) -> Vec<String> {
    let mut failures = Vec::new();
    if never.iter().any(|secret| logs.contains(secret)) {
        failures.push("SR-102: the logs hold a secret, a token or a card UID".to_owned());
    }
    if logs
        .lines()
        .any(|line| serde_json::from_str::<serde_json::Value>(line).is_err())
    {
        failures.push("SR-102: a log line is not JSON".to_owned());
    }
    let answered: Vec<serde_json::Value> = logs
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|line| line["message"] == "answered")
        .collect();
    if answered.is_empty() || answered.iter().any(|line| !line["span"]["id"].is_string()) {
        failures.push("SR-102: a request was logged without its id".to_owned());
    }
    failures
}

fn observe(
    estate: &Estate,
    partner: &inbox::Partner,
    sessions: &[story::Session],
    transactions: &[Option<String>],
    blocked: &[(&str, String)],
    settled: &[String],
    invoices: &[(String, Vec<String>, String)],
) -> Observed {
    Observed {
        settled: settled.to_vec(),
        received: received(&estate.billd),
        partner_cdrs: partner.peer.cdrs.len(),
        accepted: consignments(&estate.roamd),
        tampered: sessions
            .iter()
            .zip(transactions)
            .filter(|(session, _)| session.beat.fault.is_some())
            .map(|(session, transaction)| {
                let point = &CHARGE_POINTS[session.beat.station];
                let key = (
                    point.identity.to_owned(),
                    transaction.clone().unwrap_or_default(),
                );
                let line = verdict(&estate.csmsd, &key);
                (line.billed, line.text)
            })
            .collect(),
        blocked: blocked.iter().map(|(_, status)| status.clone()).collect(),
        open: estate.csmsd.csmsd.with_state(|state| state.inflight.len()),
        invoices: invoices
            .iter()
            .map(|(number, records, _)| (number.clone(), records.clone()))
            .collect(),
        receipts: estate.csmsd.csmsd.with_cdrs(|ledger| {
            ledger
                .iter()
                .filter_map(|cdr| cdr.cost.as_ref())
                .map(|cost| cost.gross().amount())
                .sum()
        }),
        payable: estate
            .billd
            .books
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .state()
            .documents()
            .map(|issued| issued.invoice.payable_total().amount())
            .collect(),
        witnessed: Vec::new(),
        unattributed: Vec::new(),
        displayed: Vec::new(),
        prices: Vec::new(),
        known: estate::CHARGE_POINTS
            .iter()
            .map(|point| {
                let known = estate.csmsd.csmsd.known(point.identity).unwrap_or_default();
                let said = known.identity.map(|(said, _)| said);
                (
                    point.identity.to_owned(),
                    said.as_ref().and_then(|said| said.firmware.clone()),
                    !known.meters.is_empty(),
                    said.and_then(|said| said.network),
                )
            })
            .collect(),
    }
}

/// SR-078: every printed outcome compared with what the story should have
/// produced; one sentence per difference.
fn judge(seen: &Observed, billable: usize) -> Vec<String> {
    let mut failures = Vec::new();
    let mut expect = |ok: bool, what: String| {
        if !ok {
            failures.push(what);
        }
    };
    let sorted = |list: &[String]| {
        let mut list = list.to_vec();
        list.sort();
        list
    };
    let ours = sorted(&seen.settled);
    expect(
        ours.len() == billable,
        format!("{billable} sessions billed by csmsd, found {}", ours.len()),
    );
    expect(
        sorted(&seen.received) == ours,
        format!(
            "billd received exactly csmsd's records: {:?} vs {ours:?}",
            sorted(&seen.received)
        ),
    );
    expect(
        seen.partner_cdrs == billable,
        format!(
            "the partner holds {billable} CDRs, found {}",
            seen.partner_cdrs
        ),
    );
    expect(
        seen.accepted.len() == billable && seen.accepted.iter().all(|a| *a),
        format!(
            "{billable} consignments accepted by the partner: {:?}",
            seen.accepted
        ),
    );
    expect(
        seen.tampered.len() == 1
            && seen
                .tampered
                .iter()
                .all(|(billed, line)| !billed && line.contains("signature")),
        format!(
            "the tampered session is refused naming the signature: {:?}",
            seen.tampered
        ),
    );
    expect(
        seen.blocked == ["Blocked"],
        format!("one card answered Blocked: {:?}", seen.blocked),
    );
    expect(
        seen.open == 0,
        format!("no transaction left open, found {}", seen.open),
    );
    expect(
        seen.invoices.len() == 1,
        format!("one invoice for the month, found {}", seen.invoices.len()),
    );
    if let Some((number, records)) = seen.invoices.first() {
        expect(
            sorted(records) == ours,
            format!(
                "invoice {number}'s lines are the billed records: {:?}",
                sorted(records)
            ),
        );
    }
    // The driver who adds up the session receipts gets the invoice's amount
    // due: BT-115, with any rounding stated as BT-114.
    expect(
        seen.payable == [seen.receipts],
        format!(
            "the invoice asks for what the receipts said, {} EUR: {:?}",
            seen.receipts, seen.payable
        ),
    );
    expect(
        seen.witnessed.len() == 4 && seen.witnessed.iter().all(|(_, ok)| *ok),
        format!(
            "SR-098: the witness read every journal's signed head: {:?}",
            seen.witnessed
        ),
    );
    expect(
        seen.displayed.len() == 3
            && seen
                .displayed
                .iter()
                .all(|(_, shown, expected)| shown == expected)
            && seen.displayed.iter().any(|(_, shown, _)| !shown.is_empty()),
        format!(
            "spec 011: CP-3 shows each of its receipt links and no other station shows one: {:?}",
            seen.displayed
        ),
    );
    expect(
        seen.known.len() == 3
            && seen
                .known
                .iter()
                .all(|(_, firmware, _, _)| firmware.is_some())
            && seen.known.iter().any(|(_, _, metered, _)| *metered)
            && seen
                .known
                .iter()
                .all(|(_, _, _, network)| network.as_deref().is_some_and(|n| n.len() == 5)),
        format!(
            "spec 012 and spec 022: every station said what it runs and on which network (never its IMSI), and a billed session named its meter: {:?}",
            seen.known
        ),
    );
    expect(
        seen.prices.len() == 3
            && seen.prices[0].1.is_empty()
            && seen.prices[1..]
                .iter()
                .all(|(_, shown)| shown.iter().any(|m| m.contains("pro kWh · Preisstand ")))
            && seen.prices[2].1.iter().any(|m| m.contains("/p/DE*MUS*E")),
        format!(
            "spec 015: the 2.x displays show the price and its Preisstand, CP-3 the price page's code, CP-1 (1.6) nothing: {:?}",
            seen.prices
        ),
    );
    expect(
        seen.unattributed.is_empty(),
        format!(
            "SC-002: every journalled fact names an actor: {:?}",
            seen.unattributed
        ),
    );
    failures
}

fn write_private(path: &Path, text: &str) -> Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options
        .open(path)
        .with_context(|| format!("writing {}", path.display()))?;
    file.write_all(text.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(text: &str) -> Vec<String> {
        text.split_whitespace().map(ToOwned::to_owned).collect()
    }

    fn story_as_told() -> Observed {
        let keys: Vec<String> = (1..=7).map(|n| format!("DE*MUS/CP-1-{n}")).collect();
        Observed {
            settled: keys.clone(),
            received: keys.iter().rev().cloned().collect(),
            partner_cdrs: 7,
            accepted: vec![true; 7],
            tampered: vec![(
                false,
                "✕ refused record 2: signature does not verify".to_owned(),
            )],
            blocked: vec!["Blocked".to_owned()],
            open: 0,
            invoices: vec![("R-2026-0001".to_owned(), keys)],
            receipts: rust_decimal::Decimal::new(8578, 2),
            payable: vec![rust_decimal::Decimal::new(8578, 2)],
            witnessed: ["csmsd", "billd", "roamd", "empd"]
                .map(|name| (name.to_owned(), true))
                .to_vec(),
            unattributed: Vec::new(),
            displayed: vec![
                ("CP-1".to_owned(), Vec::new(), Vec::new()),
                ("CP-2".to_owned(), Vec::new(), Vec::new()),
                (
                    "CP-3".to_owned(),
                    vec!["Beleg: http://127.0.0.1:9001/r/x".to_owned()],
                    vec!["Beleg: http://127.0.0.1:9001/r/x".to_owned()],
                ),
            ],
            prices: vec![
                ("CP-1".to_owned(), Vec::new()),
                (
                    "CP-2".to_owned(),
                    vec![
                        "0,49 € pro kWh · Preisstand 7F3A · http://127.0.0.1:9001/p/DE*MUS*E00002"
                            .to_owned(),
                    ],
                ),
                (
                    "CP-3".to_owned(),
                    vec![
                        "0,49 € pro kWh · Preisstand 7F3A".to_owned(),
                        "http://127.0.0.1:9001/p/DE*MUS*E00003".to_owned(),
                    ],
                ),
            ],
            known: ["CP-1", "CP-2", "CP-3"]
                .iter()
                .map(|id| {
                    (
                        (*id).to_owned(),
                        Some("1.0".to_owned()),
                        true,
                        Some("26201".to_owned()),
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn sr_078_every_outcome_that_differs_fails_the_check_by_name() {
        assert!(
            judge(&story_as_told(), 7).is_empty(),
            "the story as told passes"
        );
        type Break = fn(&mut Observed);
        let wrong: [(&str, Break); 17] = [
            ("billed by csmsd", |o| drop(o.settled.pop())),
            ("billd received", |o| drop(o.received.pop())),
            ("partner holds", |o| o.partner_cdrs = 6),
            ("consignments accepted", |o| o.accepted[3] = false),
            ("tampered session", |o| o.tampered[0].0 = true),
            ("tampered session", |o| {
                o.tampered[0].1 = "✕ refused for no reason".to_owned()
            }),
            ("Blocked", |o| o.blocked[0] = "Invalid".to_owned()),
            ("left open", |o| o.open = 1),
            ("lines are the billed records", |o| {
                drop(o.invoices[0].1.pop())
            }),
            ("what the receipts said", |o| {
                o.payable[0] = rust_decimal::Decimal::new(8575, 2);
            }),
            ("SR-098", |o| o.witnessed[1].1 = false),
            ("SC-002", |o| {
                o.unattributed.push("billd: 3 of 4".to_owned())
            }),
            ("spec 011", |o| o.displayed[2].1.clear()),
            ("spec 011", |o| o.displayed[1].1.push("Beleg: x".to_owned())),
            ("spec 012", |o| o.known[0].1 = None),
            ("spec 022", |o| {
                o.known[1].3 = Some("262011234500002".to_owned())
            }),
            ("spec 015", |o| o.prices[2].1.clear()),
        ];
        for (named, break_it) in wrong {
            let mut seen = story_as_told();
            break_it(&mut seen);
            let failures = judge(&seen, 7);
            assert!(
                failures.iter().any(|failure| failure.contains(named)),
                "{named}: {failures:?}"
            );
        }
    }

    #[test]
    fn sr_102_a_secret_in_the_logs_fails_the_check_without_being_echoed() {
        let clean = r#"{"message":"answered","span":{"id":"ab12","route":"/v1/x"}}"#;
        assert!(logs_hold_nothing_secret(clean, &["s3cret"]).is_empty());
        let leaked = format!("{clean}\n{{\"message\":\"token s3cret\"}}");
        let failures = logs_hold_nothing_secret(&leaked, &["s3cret"]);
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(!failures[0].contains("s3cret"));
        assert!(!logs_hold_nothing_secret("not json", &[]).is_empty());
        let unnamed = r#"{"message":"answered","span":{}}"#;
        assert!(!logs_hold_nothing_secret(unnamed, &[]).is_empty());
    }

    #[test]
    fn sr_070_a_non_loopback_listen_is_refused_before_anything_binds() {
        for address in ["0.0.0.0", "192.168.1.5", "::"] {
            let Err(Refusal(code, why)) = parse(&args(&format!("--listen {address}"))) else {
                panic!("{address} was accepted");
            };
            assert_eq!(code, USAGE);
            assert!(why.contains(address) && why.contains("loopback"), "{why}");
        }
        assert!(parse(&args("--listen ::1")).is_ok());
        assert!(parse(&args("--listen 127.0.0.1")).is_ok());
    }

    #[test]
    fn the_options_take_what_the_contract_names_and_refuse_the_rest() {
        let options = parse(&args(
            "--seed 7 --clock 2026-06-01T08:00Z --speed max --check",
        ))
        .unwrap();
        assert_eq!(options.seed, 7);
        assert_eq!(
            options.clock,
            Some(time::macros::datetime!(2026-06-01 08:00 UTC))
        );
        assert_eq!(options.speed, Some(Speed::Max));
        assert!(options.check);
        assert!(parse(&args("--speed 0")).is_err());
        assert!(parse(&args("--speed fast")).is_err());
        assert!(parse(&args("--clock yesterday")).is_err());
        assert!(parse(&args("--frobnicate 1")).is_err());
    }

    #[test]
    fn sr_071_every_secret_is_fresh_per_run_and_the_estate_holds_none() {
        let (one, two) = (Secrets::mint(), Secrets::mint());
        for secret in one.never_printed() {
            assert!(!two.never_printed().contains(&secret));
        }
        assert_ne!(one.operator, two.operator);
        assert_eq!(one.operator.len(), 32, "SR-072: 128 bits");
        let at = |port| SocketAddr::from(([127, 0, 0, 1], port));
        let ports = Ports {
            ocpp: at(1),
            csmsd: at(2),
            roamd: at(3),
            billd: at(4),
            empd: at(5),
            deskd: at(6),
        };
        let hash = csmsd::edge::hash_password(ocpp_kit::Version::V1_6, &one.passwords[0]).unwrap();
        let text = estate::text(
            2026,
            &ports,
            &[hash.clone(), hash.clone(), hash],
            Path::new("/t"),
            time::macros::date!(2027 - 01 - 01),
        );
        for secret in one
            .never_printed()
            .into_iter()
            .chain([one.operator.as_str()])
        {
            assert!(!text.contains(secret), "the estate file holds a secret");
        }
    }

    #[test]
    fn sr_075_sr_076_tls_material_is_required_and_a_signing_ca_is_refused() {
        let dir = Scratch::new().unwrap();
        let Err(Refusal(code, why)) = tls_material(&dir.0) else {
            panic!("an empty directory was accepted");
        };
        assert_eq!(code, UNAVAILABLE);
        assert!(why.contains("just dev-ca"), "{why}");
        for file in ["ca.pem", "server.pem", "server.key", "ca.key"] {
            std::fs::write(dir.0.join(file), "x").unwrap();
        }
        let Err(Refusal(_, why)) = tls_material(&dir.0) else {
            panic!("a CA key was accepted");
        };
        assert!(why.contains("ca.key"), "{why}");
        std::fs::remove_file(dir.0.join("ca.key")).unwrap();
        assert!(tls_material(&dir.0).is_ok());
    }

    #[test]
    fn sr_073_the_scratch_directory_is_private_and_gone_when_dropped() {
        let scratch = Scratch::new().unwrap();
        let path = scratch.0.clone();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700);
        }
        std::fs::write(path.join("csmsd.ndjson"), "{}").unwrap();
        drop(scratch);
        assert!(!path.exists());
    }

    #[test]
    fn sr_077_a_port_in_use_is_refused_naming_it_and_its_daemon() {
        let Ok(held) = std::net::TcpListener::bind("127.0.0.1:9587") else {
            return; // something on this machine already holds it: same refusal, not ours to test
        };
        let Err(Refusal(code, why)) = ports(IpAddr::from([127, 0, 0, 1]), true) else {
            panic!("a held port was accepted");
        };
        assert_eq!(code, UNAVAILABLE);
        assert!(why.contains("in use") && why.contains("lsof"), "{why}");
        if !why.contains("9000") && !why.contains("9001") {
            assert!(why.contains("9587") && why.contains("deskd"), "{why}");
        }
        drop(held);
    }
}
