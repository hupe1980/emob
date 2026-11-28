//! Seed a customer's own charging history through the provider and invoice APIs.
use anyhow::{Context as _, Result, ensure};
use reqwest::Client;
use serde_json::{Value, json};

use super::estate::Ports;

/// The second successful charge on CP-1 in the demo morning.
pub(super) const PRIVATE_SESSION: usize = 4;
pub(super) const PRIVATE_CARD: &str = "04AB0000000006";
pub(super) const FLEET_CARDS: [&str; 2] = ["04AB0000000007", "04AB0000000008"];

/// Keep both fleet sessions on one calendar invoice even with a custom start at month end.
pub(super) fn fleet_sessions(sessions: &[super::story::Session]) -> [usize; 2] {
    let month = |index: usize| {
        sessions[index]
            .charged
            .ended_at
            .to_offset(time::UtcOffset::UTC)
            .month()
    };
    if month(5) == month(8) {
        [5, 8]
    } else if month(5) == month(7) {
        [5, 7]
    } else {
        [7, 8]
    }
}

async fn post(client: &Client, url: &str, operator: &str, body: &Value) -> Result<Value> {
    Ok(client
        .post(url)
        .bearer_auth(operator)
        .json(body)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}

pub(super) async fn enrol(
    ports: &Ports,
    writer: &str,
    stock: &str,
    point: &str,
    account: &str,
) -> Result<String> {
    let client = Client::builder().no_proxy().build()?;
    let base = format!("http://{}/v1/empd", ports.empd);
    let body = json!({"stock":stock,"point":point,"plan":"standard","account":account});
    let review = post(&client, &format!("{base}/contracts/preview"), writer, &body).await?;
    let issued = post(
        &client,
        &format!("{base}/contracts/confirm"),
        writer,
        &json!({"operation":review["operation"]}),
    )
    .await?;
    issued["agreement"]
        .as_str()
        .map(str::to_owned)
        .context("demo agreement")
}

pub(super) async fn grant_private(ports: &Ports, writer: &str, agreement: &str) -> Result<()> {
    let client = Client::builder().no_proxy().build()?;
    let base = format!("http://{}/v1/empd", ports.empd);
    let mut access = post(
        &client,
        &format!("{base}/customer/access"),
        writer,
        &json!({"subject":"mika"}),
    )
    .await?;
    for (kind, reference) in [("agreement", agreement), ("account", "PAYER-PRIVATE")] {
        access = post(&client, &format!("{base}/customer/grants"), writer, &json!({"subject":"mika","kind":kind,"reference":reference,"granted":true,"revision":access["revision"]})).await?;
    }
    Ok(())
}

pub(super) async fn issue(
    ports: &Ports,
    writer: &str,
    account: &str,
    agreements: &[&str],
    ended: time::OffsetDateTime,
) -> Result<String> {
    let client = Client::builder().no_proxy().build()?;
    let base = format!("http://{}/v1/billd/retail", ports.billd);
    let ended = ended.to_offset(time::UtcOffset::UTC);
    let mut query = json!({"account":account,"period":format!("{:04}-{:02}",ended.year(),u8::from(ended.month()))});
    let review = post(&client, &format!("{base}/review"), writer, &query).await?;
    ensure!(review["state"] == "ready", "demo customer invoice was held");
    let lines = review["lines"]
        .as_array()
        .context("missing demo invoice lines")?;
    ensure!(
        agreements
            .iter()
            .all(|id| lines.iter().any(|l| l["agreement"].as_str() == Some(id))),
        "demo {account} invoice for {} lacks an enrolled driver's charge",
        query["period"]
    );
    query["review"] = review["review"].clone();
    let issued = post(&client, &format!("{base}/close"), writer, &query).await?;
    issued["number"]
        .as_str()
        .map(str::to_owned)
        .context("demo customer invoice was not issued")
}

/// Supply a local payment so the finance desk can review an actual allocation.
pub(super) async fn fund(ports: &Ports, operator: &str) -> Result<()> {
    let client = Client::builder().no_proxy().build()?;
    let url = format!("http://{}/v1/accountingd/import", ports.accountingd);
    let statement = json!({
        "version": 1, "source": "LOCAL-BANK", "account": "PAYER-PRIVATE", "currency": "EUR",
        "entries": [{"reference": "DEMO-PAYMENT", "status": "booked", "direction": "credit", "amount_minor": 500}]
    });
    let result = post(&client, &url, operator, &statement).await?;
    ensure!(result["booked"] == 1, "demo receipt of payment was held");
    Ok(())
}
