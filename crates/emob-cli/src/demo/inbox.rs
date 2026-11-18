//! The roaming partner: `ocpi-kit`'s `MockPeer` standing in for the eMSP
//! `DE*MSP`'s platform, and a page showing what it received.
//!
//! The peer is the kit's; the page is the demo's own window onto the kit's
//! store — what a partner's back office would list as its inbox. It renders
//! the CDRs exactly as they arrived over OCPI, on the workspace's shared shell
//! (`emob_service::ui`), and decides nothing.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use axum::routing::get;
use emob_service::ui::{self, Lang, escape};
use ocpi_kit::VersionNumber;
use ocpi_kit::client::{ClientConfig, OcpiClient};
use ocpi_kit::server::{InMemoryTokenStore, OcpiRouter};
use ocpi_kit::testkit::MockPeer;
use ocpi_kit::transport::CredentialsToken;
use ocpi_kit::types::{CiString, OcpiString, PartyRef, Url as KitUrl};
use serde_json::Value;

/// The partner's platform, listening.
pub struct Partner {
    /// The kit's peer and its stores.
    pub peer: MockPeer,
    /// Where it listens.
    pub addr: SocketAddr,
}

#[derive(serde::Deserialize)]
struct Params {
    lang: Option<String>,
}

/// Start the partner's platform on `listener`: OCPI under `/ocpi`, the inbox
/// at `/`. `token_b` is what it issues to the operator's node.
///
/// # Errors
///
/// The kit refuses the party or the token.
pub fn start(listener: tokio::net::TcpListener, token_b: &str) -> Result<Partner> {
    let addr = listener.local_addr()?;
    let base = KitUrl::new_lenient(format!("http://{addr}/ocpi"));
    let peer = MockPeer::new(
        base.clone(),
        PartyRef::new("DE", "MSP").context("the partner's party")?,
        ocpi_kit::v2_3_0::types::Role::Emsp,
    );
    let tokens = Arc::new(InMemoryTokenStore::new());
    tokens.insert(
        CredentialsToken::new(token_b).context("token B")?,
        ocpi_kit::testkit::registered_peer(
            "DE*MUS",
            vec![PartyRef::new("DE", "MUS").context("the operator's party")?],
        ),
    );
    let app = axum::Router::new()
        .nest(
            "/ocpi",
            peer.mount(OcpiRouter::new(VersionNumber::V2_3_0, base, tokens))
                .build(),
        )
        .merge(
            axum::Router::new()
                .route("/", get(inbox))
                .with_state(peer.clone()),
        )
        .merge(ui::assets());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(Partner { peer, addr })
}

/// The partner registers with roamd with the `Token A` the estate handed it out
/// of band — the OCPI credentials handshake, as a real eMSP runs it.
///
/// # Errors
///
/// roamd refuses the token or the handshake does not complete.
pub async fn register(
    roamd: SocketAddr,
    partner: SocketAddr,
    token_a: &str,
    token_b: &str,
) -> Result<()> {
    let client = OcpiClient::with_config(ClientConfig::for_testing())?;
    let theirs = ocpi_kit::v2_3_0::credentials::Credentials::builder()
        .token(OcpiString::<64>::new(token_b)?)
        .url(KitUrl::new_lenient(format!(
            "http://{partner}/ocpi/versions"
        )))
        .roles(vec![
            ocpi_kit::v2_3_0::credentials::CredentialsRole::builder()
                .role(ocpi_kit::v2_3_0::types::Role::Emsp)
                .business_details(
                    ocpi_kit::v2_3_0::locations::BusinessDetails::builder()
                        .name("MSP Mobility GmbH")
                        .build(),
                )
                .party_id(CiString::new("MSP")?)
                .country_code(CiString::new("DE")?)
                .build(),
        ])
        .build();
    ocpi_kit::client::Registration::new(
        KitUrl::new_lenient(format!("http://{roamd}/ocpi/versions")),
        CredentialsToken::new(token_a)?,
    )
    .discover(client.transport())
    .await
    .context("roamd's versions")?
    .select_best(client.transport())
    .await
    .context("roamd's version details")?
    .register(client.transport(), &theirs)
    .await
    .context("the credentials handshake")?;
    Ok(())
}

async fn inbox(
    State(peer): State<MockPeer>,
    Query(params): Query<Params>,
    headers: HeaderMap,
) -> Response {
    let lang = Lang::negotiate(
        params.lang.as_deref(),
        headers
            .get(axum::http::header::ACCEPT_LANGUAGE)
            .and_then(|value| value.to_str().ok()),
    );
    let cdrs: Vec<Value> = peer
        .cdrs
        .all()
        .iter()
        .filter_map(|cdr| serde_json::to_value(cdr).ok())
        .collect();
    ui::html(render(&cdrs, lang))
}

/// The page, from the CDRs as the partner holds them.
fn render(cdrs: &[Value], lang: Lang) -> String {
    let text = |value: &Value, path: &[&str]| {
        path.iter()
            .try_fold(value, |at, key| at.get(key))
            .map(|leaf| match leaf {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            })
            .unwrap_or_default()
    };
    let mut body = format!(
        "<div class=\"head\"><h1>{h1}</h1><p class=\"when\">MSP Mobility GmbH · DE*MSP · OCPI 2.3.0</p></div>\
         <p class=\"muted\">{lead}</p>",
        h1 = lang.pick("Posteingang", "Inbox"),
        lead = lang.pick(
            "Was die Plattform des Roaming-Partners von Stadtwerke Musterstadt erhalten hat: jeder Ladevorgang als CDR, mit dem signierten Messwert, den der Fahrer selbst prüfen kann.",
            "What the roaming partner's platform received from Stadtwerke Musterstadt: every session as a CDR, with the signed meter value the driver can check.",
        ),
    );
    if cdrs.is_empty() {
        body.push_str(&format!(
            "<p class=\"empty\">{}</p>",
            lang.pick("Noch nichts angekommen.", "Nothing has arrived yet.")
        ));
    } else {
        body.push_str(&format!(
            "<section class=\"group\"><h3>{} <span class=\"count\">{}</span></h3><ol class=\"rows\">",
            lang.pick("Empfangene CDRs", "CDRs received"),
            cdrs.len()
        ));
        // Newest first, by when the session started.
        let mut cdrs: Vec<&Value> = cdrs.iter().collect();
        cdrs.sort_by_key(|cdr| std::cmp::Reverse(text(cdr, &["start_date_time"])));
        for cdr in cdrs {
            let signed = cdr.get("signed_data").is_some_and(|value| !value.is_null());
            let energy = text(cdr, &["total_energy"]);
            let net = text(cdr, &["total_cost", "before_taxes"]);
            let start = text(cdr, &["start_date_time"]);
            body.push_str(&format!(
                "<li class=\"edge-info\"><div><p class=\"what\"><strong>{id}</strong> · {evse}</p>\
                 <p class=\"why num\"><time datetime=\"{start}\">{start_shown}</time> · {energy} kWh · {net} € {net_word}</p></div>\
                 <span class=\"badge {tone}\"><span aria-hidden=\"true\">{icon}</span> {word}</span></li>",
                id = escape(&text(cdr, &["id"])),
                evse = escape(&text(cdr, &["cdr_location", "evse_id"])),
                start = escape(&start),
                start_shown = escape(&format!(
                    "{} UTC",
                    start.replace('T', " ").trim_end_matches('Z').trim_end_matches(":00")
                )),
                energy = escape(&energy),
                net = escape(&net),
                net_word = lang.pick("netto", "net"),
                tone = if signed { "tone-good" } else { "tone-warn" },
                icon = if signed { "✓" } else { "⚠" },
                word = if signed {
                    lang.pick("signiert", "signed")
                } else {
                    lang.pick("ohne Signatur", "unsigned")
                },
            ));
        }
        body.push_str("</ol></section>");
    }
    ui::page(
        &ui::Shell {
            title: lang.pick("Posteingang", "Inbox"),
            place: lang.pick("Roaming-Partner", "Roaming partner"),
            lang,
            reading: false,
            script: None,
        },
        &body,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_a_partner_sent_is_escaped_not_rendered() {
        let cdr = serde_json::json!({ "id": "<script>x</script>", "total_energy": 1 });
        let page = render(&[cdr], Lang::De);
        assert!(!page.contains("<script>x"));
        assert!(page.contains("&lt;script&gt;"));
    }
}
