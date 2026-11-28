use std::fmt::Write as _;

use emob_core::TimeZone;

use super::{
    Billing, Case, Channel, Lang, Outcome, PATH, Page, Response, Review, Staff, Stage, button,
    escape, hidden, invoice, open_case, shell,
};
use crate::investigation::moment;

pub(super) fn title(lang: Lang) -> &'static str {
    lang.pick("Vertragswiderruf", "Agreement withdrawal")
}

impl Channel {
    const fn label(self, lang: Lang) -> &'static str {
        match self {
            Self::Email => lang.pick("E-Mail", "Email"),
            Self::Letter => lang.pick("Brief", "Letter"),
            Self::Phone => lang.pick("Telefon", "Phone"),
            Self::InPerson => lang.pick("Persönlich", "In person"),
            Self::Other => lang.pick("Anderer Eingang", "Other channel"),
        }
    }
}

impl Outcome {
    const fn label(self, lang: Lang) -> &'static str {
        match self {
            Self::Hold => lang.pick("Nachweise anfordern", "Hold for evidence"),
            Self::Approve => lang.pick("Widerruf anerkennen", "Approve withdrawal"),
            Self::Decline => lang.pick("Widerruf ablehnen", "Decline withdrawal"),
        }
    }
    const fn effect(self, lang: Lang) -> &'static str {
        match self {
            Self::Approve => lang.pick("Der Anbieter verweigert die nächste Online-Autorisierung für diesen Vertrag.", "The provider will refuse the next online charging authorization for this agreement."),
            Self::Hold | Self::Decline => lang.pick("Für die nächste Online-Autorisierung gilt der bisherige Vertragsstatus.", "The existing agreement status applies to the next online charging authorization."),
        }
    }
}

impl Billing {
    const fn label(self, lang: Lang) -> &'static str {
        match self {
            Self::Unaffected => lang.pick("Kein Prüfbedarf", "No billing review needed"),
            Self::ReviewRequired => lang.pick(
                "Abrechnung gesondert prüfen",
                "Separate billing review required",
            ),
        }
    }
}

impl Stage {
    const fn label(self, lang: Lang) -> &'static str {
        match self {
            Self::Received => lang.pick("Eingegangen", "Received"),
            Self::Held => lang.pick("Nachweise ausstehend", "Awaiting evidence"),
            Self::Approved => lang.pick("Widerruf anerkannt", "Withdrawal approved"),
            Self::Declined => lang.pick("Widerruf abgelehnt", "Withdrawal declined"),
        }
    }
}

pub(super) fn index(lang: Lang) -> Response {
    let body = format!(
        r#"<section class="card"><h2>{find}</h2>
<form method="post" action="{PATH}/lookup?lang={tag}" autocomplete="off"><p><label>{agreement}<br><input type="text" name="agreement" required minlength="64" maxlength="64" pattern="[0-9a-f]{{64}}" spellcheck="false" aria-describedby="agreement-help"></label><br><small id="agreement-help">{help}</small></p><button class="btn">{open}</button></form></section>"#,
        tag = lang.tag(),
        find = lang.pick("Fall zum Vertrag", "Find a case"),
        agreement = lang.pick("Vertragsreferenz", "Agreement reference"),
        help = lang.pick(
            "Verwenden Sie die Referenz aus dem ursprünglichen Ladevertrag.",
            "Use the reference on the original driver agreement."
        ),
        open = lang.pick("Widerruf öffnen", "Open withdrawal")
    );
    shell(lang, title(lang), &body)
}

pub(super) fn intake(lang: Lang, agreement: &str) -> Response {
    let mut options = String::new();
    for (id, channel) in [
        ("email", Channel::Email),
        ("letter", Channel::Letter),
        ("phone", Channel::Phone),
        ("in_person", Channel::InPerson),
        ("other", Channel::Other),
    ] {
        let _ = write!(
            options,
            r#"<option value="{id}">{}</option>"#,
            channel.label(lang)
        );
    }
    let body=format!(r#"<section class="card"><h2>{record}</h2><p>{message}</p><dl class="kv"><dt>{agreement_label}</dt><dd><code>{agreement}</code></dd></dl>
<form method="post" action="{PATH}/intake?lang={tag}" autocomplete="off">{scope}
<p><label>{reference}<br><input name="request_ref" type="text" required maxlength="96" spellcheck="false" aria-describedby="ref-help"></label><br><small id="ref-help">{ref_help}</small></p>
<p><label>{received}<br><input type="datetime-local" name="received_at" step="1" required></label></p>
<p><label>{channel}<br><select name="channel" required><option value="">{choose}</option>{options}</select></label></p>
<button class="btn">{record}</button></form></section>"#,
        tag=lang.tag(),scope=hidden("agreement",agreement),agreement=escape(agreement),
        agreement_label=lang.pick("Vertragsreferenz","Agreement reference"),
        record=lang.pick("Eingang erfassen","Record request"),
        message=lang.pick("Zu dieser Referenz ist noch kein Widerruf erfasst. Prüfen Sie den Vertrag, bevor Sie den Eingang speichern.","No withdrawal was found under this reference. Check the agreement before recording the request."),
        reference=lang.pick("Eingangsreferenz","Request reference"), ref_help=lang.pick("Kennung der Nachricht oder des Akteneintrags, z. B. POST-42.", "Use the message or case-file reference, such as MAIL-42."),
        received=lang.pick("Tatsächlicher Eingang (UTC)", "Time received (UTC)"), channel=lang.pick("Eingang über","Received via"),
        choose=lang.pick("Eingangskanal wählen","Choose a channel"));
    shell(
        lang,
        title(lang),
        &(body + &agreement_link(agreement, lang)),
    )
}

fn agreement_link(agreement: &str, lang: Lang) -> String {
    button(
        "/contracts/lookup",
        &hidden("agreement", agreement),
        lang.pick(
            "Ursprünglichen Vertrag und Preise öffnen",
            "Open original agreement and prices",
        ),
        lang,
    )
}

pub(super) fn row(rows: &mut String, label: &str, value: &str) {
    let _ = write!(rows, "<dt>{label}</dt><dd>{}</dd>", escape(value));
}

fn time_row(rows: &mut String, label: &str, at: time::OffsetDateTime, zone: &TimeZone, lang: Lang) {
    let _ = write!(rows, "<dt>{label}</dt><dd>{}</dd>", moment(at, zone, lang));
}

pub(super) fn case(page: &Page, case: &Case, staff: &Staff) -> Response {
    let lang = staff.lang;
    let mut rows = String::new();
    row(
        &mut rows,
        lang.pick("Vertragsreferenz", "Agreement reference"),
        &case.agreement,
    );
    row(
        &mut rows,
        lang.pick("Rechnungskonto", "Billing account"),
        &case.account,
    );
    time_row(
        &mut rows,
        lang.pick("Vertrag geschlossen", "Agreement accepted"),
        case.accepted_at,
        &page.zone,
        lang,
    );
    row(
        &mut rows,
        lang.pick("Preisreferenz", "Offer reference"),
        &case.offer_reference,
    );
    row(
        &mut rows,
        lang.pick("Eingangsreferenz", "Request reference"),
        &case.request_ref,
    );
    time_row(
        &mut rows,
        lang.pick("Eingegangen am", "Received at"),
        case.received_at,
        &page.zone,
        lang,
    );
    row(
        &mut rows,
        lang.pick("Eingang über", "Received via"),
        case.channel.label(lang),
    );
    row(
        &mut rows,
        lang.pick("Erfasst von", "Recorded by"),
        &case.recorded_by,
    );
    let mut body = format!(
        r#"<section class="card"><h2>{}</h2><dl class="kv">{rows}</dl>{}</section>"#,
        case.stage.label(lang),
        agreement_link(&case.agreement, lang)
    );
    if let Some(d) = &case.decision {
        let mut result = String::new();
        row(
            &mut result,
            lang.pick("Nachweisreferenz", "Evidence reference"),
            &d.evidence_ref,
        );
        row(
            &mut result,
            lang.pick("Abrechnung", "Billing"),
            d.billing.label(lang),
        );
        row(
            &mut result,
            lang.pick("Entschieden von", "Decided by"),
            &d.actor,
        );
        time_row(
            &mut result,
            lang.pick("Entschieden am", "Decided at"),
            d.at,
            &page.zone,
            lang,
        );
        let _ = write!(
            body,
            r#"<section class="card"><h2>{}</h2><dl class="kv">{result}</dl><p>{}</p></section>"#,
            lang.pick("Letzte Entscheidung", "Latest decision"),
            d.outcome.effect(lang)
        );
    }
    body.push_str(&invoice::find(&case.agreement, &case.account, lang));
    if matches!(case.stage, Stage::Received | Stage::Held) {
        body.push_str(&proposal(&case.agreement, lang));
    }
    shell(lang, title(lang), &body)
}

fn proposal(agreement: &str, lang: Lang) -> String {
    format!(r#"<section class="card"><h2>{decision}</h2><form method="post" action="{PATH}/review?lang={tag}" autocomplete="off">{scope}
<p><label>{evidence}<br><input type="text" name="evidence_ref" required maxlength="96" spellcheck="false" aria-describedby="evidence-help"></label><br><small id="evidence-help">{help}</small></p>
<p><label>{outcome}<br><select name="outcome" required><option value="">{choose}</option><option value="hold">{hold}</option><option value="approve">{approve}</option><option value="decline">{decline}</option></select></label></p>
<p><label>{billing}<br><select name="billing" required><option value="">{choose_billing}</option><option value="unaffected">{no}</option><option value="review_required">{review}</option></select></label></p>
<button class="btn">{submit}</button></form></section>"#,
        tag=lang.tag(),scope=hidden("agreement",agreement),decision=lang.pick("Fall entscheiden","Decide case"),
        evidence=lang.pick("Nachweisreferenz","Evidence reference"),help=lang.pick("Aktenreferenz für die geprüften Bedingungen, Erklärung und Leistungsnachweise.", "Reference the case file containing the terms, declaration and evidence of performance you reviewed."),
        outcome=lang.pick("Entscheidung","Decision"),choose=lang.pick("Entscheidung wählen","Choose a decision"),hold=Outcome::Hold.label(lang),approve=Outcome::Approve.label(lang),decline=Outcome::Decline.label(lang),
        billing=lang.pick("Abrechnung","Billing"),choose_billing=lang.pick("Prüfbedarf wählen","Choose billing disposition"),no=Billing::Unaffected.label(lang),review=Billing::ReviewRequired.label(lang),submit=lang.pick("Entscheidung prüfen","Review decision"))
}

pub(super) fn review(view: &Review, zone: &TimeZone, lang: Lang) -> Response {
    let p = &view.proposal;
    let mut rows = String::new();
    row(
        &mut rows,
        lang.pick("Vertragsreferenz", "Agreement reference"),
        &p.agreement,
    );
    row(
        &mut rows,
        lang.pick("Nachweisreferenz", "Evidence reference"),
        &p.evidence_ref,
    );
    row(
        &mut rows,
        lang.pick("Abrechnung", "Billing"),
        p.billing.label(lang),
    );
    time_row(
        &mut rows,
        lang.pick("Bestätigen bis", "Confirm by"),
        view.expires_at,
        zone,
        lang,
    );
    let body = format!(
        r#"<section class="card"><h2>{outcome}</h2><dl class="kv">{rows}</dl><p><strong>{effect}</strong></p><p>{financial}</p><div class="card-controls">
<form method="post" action="{PATH}/confirm?lang={tag}">{scope}{handle}<button class="btn">{confirm}</button></form>{back}</div></section>"#,
        tag = lang.tag(),
        scope = hidden("agreement", &p.agreement),
        handle = hidden("operation", &view.operation),
        outcome = p.outcome.label(lang),
        effect = p.outcome.effect(lang),
        financial = lang.pick(
            "Für eine Gutschrift ist eine separate Bestätigung in der Abrechnung erforderlich.",
            "Issuing a credit requires a separate billing confirmation."
        ),
        confirm = lang.pick("Entscheidung bestätigen", "Confirm decision"),
        back = open_case(&p.agreement, lang)
    );
    shell(
        lang,
        lang.pick("Widerrufsentscheidung prüfen", "Review withdrawal decision"),
        &body,
    )
}
