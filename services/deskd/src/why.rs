//! "Warum?" — each finding of the evidence chain in plain words, and what would
//! have made the session billable.
//!
//! The words are the page's; the decision is the chain's. Which kind a finding
//! is, and the paragraph it rests on, come from `emob-eichrecht`
//! ([`FindingKind`], [`FindingKind::citation`]); this module only says them in
//! German and English. The `match` is exhaustive, so a kind the domain adds is
//! a build failure here rather than a refusal the page cannot explain.

use core::fmt::Write as _;

use emob_eichrecht::{Disqualifies, Finding, FindingKind};
use emob_service::ui::{Lang, escape};

/// What a finding means, and what would have made the session billable.
struct Words {
    what: (&'static str, &'static str),
    fix: (&'static str, &'static str),
}

// A table, one arm per kind: splitting it would only hide which kind is worded where.
#[allow(clippy::too_many_lines)]
const fn words(kind: FindingKind) -> Words {
    match kind {
        FindingKind::NoKey => Words {
            what: (
                "Für den Zähler, der signiert hat, ist zu diesem Zeitpunkt kein öffentlicher Schlüssel hinterlegt — die Werte lassen sich nicht prüfen.",
                "No public key is registered for the meter that signed, at the time it signed — the values cannot be checked.",
            ),
            fix: (
                "der Schlüssel des Zählers mit seinem Gültigkeitszeitraum im Estate eingetragen gewesen wäre. Der Vorgang wird nachträglich abrechenbar, sobald er es ist — an den Daten selbst hat sich nichts geändert.",
                "the meter's key had been in the estate with a window covering the session. The session becomes billable once it is — nothing about the records has to change.",
            ),
        },
        FindingKind::Signature => Words {
            what: (
                "Eine Signatur passt nicht zu den Messwerten: Der Datensatz wurde nach dem Signieren verändert oder nicht von diesem Zähler signiert.",
                "A signature does not match its readings: the record was changed after signing, or was not signed by this meter.",
            ),
            fix: (
                "jeder Datensatz unverändert so angekommen wäre, wie der Zähler ihn signiert hat. Ein veränderter Wert ist nicht reparierbar — die Station gehört geprüft.",
                "every record had arrived exactly as the meter signed it. A changed value cannot be repaired — the station needs checking.",
            ),
        },
        FindingKind::Unreadable => Words {
            what: (
                "Ein Datensatz ist kein lesbares OCMF.",
                "A record is not readable OCMF.",
            ),
            fix: (
                "die Station die signierten Daten vollständig und im OCMF-Format übertragen hätte.",
                "the station had sent the signed data whole and as OCMF.",
            ),
        },
        FindingKind::MeterState => Words {
            what: (
                "Der Zähler meldete einen Zustand oder ein Fehlerkennzeichen, unter dem nicht abgerechnet werden darf.",
                "The meter reported a state or an error flag under which nothing may be billed.",
            ),
            fix: (
                "der Zähler fehlerfrei gemessen hätte. Das ist ein Gerätefehler, kein Streitfall: Hersteller der Station einschalten.",
                "the meter had measured without fault. That is a device fault rather than a dispute: raise it with the station vendor.",
            ),
        },
        FindingKind::Clock => Words {
            what: (
                "Die Uhr hinter den Messwerten war nicht synchronisiert; eine Dauer darf darauf nicht abgerechnet werden.",
                "The clock behind the readings was not synchronised; no duration may be billed on it.",
            ),
            fix: (
                "die Station eine synchronisierte Zeitquelle gehabt hätte — oder am Punkt ein reiner kWh-Tarif gälte.",
                "the station had a synchronised time source — or the point offered a per-kWh tariff only.",
            ),
        },
        FindingKind::Register => Words {
            what: (
                "Abgerechnet würde ein Register, das das Format reserviert und nicht definiert.",
                "The billed register is one the format reserves and does not define.",
            ),
            fix: (
                "die Station das Wirkenergie-Register signiert hätte.",
                "the station had signed the active-energy register.",
            ),
        },
        FindingKind::Unit => Words {
            what: (
                "Die Messwerte sind nicht in einer einzigen Energieeinheit angegeben.",
                "The readings are not in one energy unit.",
            ),
            fix: (
                "Anfang und Ende in derselben Energieeinheit gemessen worden wären.",
                "the begin and end had been measured in the same energy unit.",
            ),
        },
        FindingKind::Directions => Words {
            what: (
                "Energie floss in beide Richtungen, und welche davon dieser Vorgang ist, sagt kein Datensatz.",
                "Energy flowed in both directions, and no record says which of them this session is.",
            ),
            fix: (
                "für den Vorgang feststünde, welche Richtung abgerechnet wird — Bezug und Einspeisung werden nie verrechnet.",
                "it were settled which direction the session bills — import and export never net.",
            ),
        },
        FindingKind::CableLoss => Words {
            what: (
                "Die Kabelverlust-Kompensation lässt sich keinem Messwert zuordnen.",
                "The cable-loss compensation cannot be traced to a reading.",
            ),
            fix: (
                "der Zähler die Kompensation zu Beginn zurückgesetzt und am Energieregister geführt hätte.",
                "the meter had reset the compensation at the begin and kept it on the energy register.",
            ),
        },
        FindingKind::Identification => Words {
            what: (
                "Die Identifizierung des Kunden änderte sich während des Vorgangs oder schlug fehl.",
                "The customer's identification changed during the session or failed.",
            ),
            fix: (
                "der Vorgang von Anfang bis Ende unter einer Identifizierung gelaufen wäre.",
                "the session had run under one identification from begin to end.",
            ),
        },
        FindingKind::Sequence => Words {
            what: (
                "Die Datensätze ergeben keine vollständige, geordnete Folge — einer fehlt, ist doppelt oder steht an falscher Stelle.",
                "The records do not form one complete, ordered sequence — one is missing, doubled or out of place.",
            ),
            fix: (
                "jeder Datensatz genau einmal und in Reihenfolge angekommen wäre. Meist eine Frage der Übertragung, nicht der Messung.",
                "every record had arrived once and in order. Usually a transport question rather than a metrology one.",
            ),
        },
        FindingKind::NoEnergy => Words {
            what: (
                "Es gibt kein verwertbares Paar von Energiewerten — nichts, wovon sich eine Menge abziehen ließe.",
                "There is no usable pair of energy readings — nothing to subtract a quantity from.",
            ),
            fix: (
                "die Station einen signierten Anfangs- und Endwert desselben Registers gesendet hätte.",
                "the station had sent a signed begin and end on one register.",
            ),
        },
    }
}

/// The one sentence a refused session's row leads with: the first finding in
/// plain words, or — for a refusal that did not come from the evidence — the
/// domain's own sentence. With the citation, when there is one.
#[must_use]
pub fn headline(
    lang: Lang,
    findings: &[Finding],
    reasons: &[String],
) -> (String, Option<&'static str>) {
    if let Some(first) = findings.first() {
        let w = words(first.kind);
        return (escape(pick(lang, w.what)), Some(first.kind.citation()));
    }
    (
        reasons.first().map_or_else(
            || {
                escape(lang.pick(
                    "Abgelehnt, ohne genannten Grund.",
                    "Refused, with no reason given.",
                ))
            },
            |reason| escape(reason),
        ),
        None,
    )
}

/// The "Warum?" disclosure for one refused session, as markup.
#[must_use]
pub fn details(lang: Lang, findings: &[Finding], reasons: &[String]) -> String {
    let mut items = String::new();
    for finding in findings {
        let w = words(finding.kind);
        let _ = write!(
            items,
            "<li><p>{what} <span class=\"cite\">{cite}</span></p>\
             <p class=\"detail\">{takes} {detail}</p>\
             <p class=\"fix\"><strong>{if_}</strong> {fix}</p></li>",
            what = escape(pick(lang, w.what)),
            cite = escape(finding.kind.citation()),
            takes = escape(takes(lang, finding.takes)),
            detail = escape(&finding.detail),
            if_ = lang.pick("Abrechenbar, wenn", "Billable, if"),
            fix = escape(pick(lang, w.fix)),
        );
    }
    // A refusal from outside the evidence chain has no typed finding; its
    // sentences are the answer.
    if findings.is_empty() {
        for reason in reasons {
            let _ = write!(items, "<li><p>{}</p></li>", escape(reason));
        }
    }
    format!(
        "<details><summary>{summary}</summary><ol class=\"findings\">{items}</ol></details>",
        summary = lang.pick("Warum?", "Why?"),
    )
}

const fn pick(lang: Lang, pair: (&'static str, &'static str)) -> &'static str {
    lang.pick(pair.0, pair.1)
}

const fn takes(lang: Lang, what: Disqualifies) -> &'static str {
    match what {
        Disqualifies::Energy => lang.pick("Nimmt die Energie.", "Takes the energy."),
        Disqualifies::Duration => lang.pick(
            "Nimmt die Dauer, die Energie bleibt abrechenbar.",
            "Takes the duration; the energy still bills.",
        ),
        Disqualifies::Both => lang.pick("Nimmt Energie und Dauer.", "Takes energy and duration."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KINDS: [FindingKind; 12] = [
        FindingKind::NoKey,
        FindingKind::Signature,
        FindingKind::Unreadable,
        FindingKind::MeterState,
        FindingKind::Clock,
        FindingKind::Register,
        FindingKind::Unit,
        FindingKind::Directions,
        FindingKind::CableLoss,
        FindingKind::Identification,
        FindingKind::Sequence,
        FindingKind::NoEnergy,
    ];

    #[test]
    fn every_kind_is_said_in_both_languages_and_they_differ() {
        for kind in KINDS {
            let w = words(kind);
            for (de, en) in [w.what, w.fix] {
                assert!(!de.is_empty() && !en.is_empty(), "{kind:?}");
                assert_ne!(de, en, "{kind:?} is not translated");
            }
        }
    }

    #[test]
    fn a_tampered_session_says_why_with_the_paragraph_and_the_way_out() {
        let findings = vec![Finding {
            kind: FindingKind::Signature,
            takes: Disqualifies::Both,
            detail: "record 2: the signature does not verify".into(),
        }];
        let (line, cite) = headline(Lang::De, &findings, &[]);
        assert!(line.contains("Signatur"), "{line}");
        assert_eq!(cite, Some("[MessEG §33]"));
        let open = details(Lang::De, &findings, &[]);
        assert!(open.contains("<summary>Warum?</summary>"));
        assert!(open.contains("[MessEG §33]"));
        assert!(open.contains("Abrechenbar, wenn"));
        assert!(
            open.contains("record 2"),
            "the chain's own sentence is kept"
        );
        assert!(details(Lang::En, &findings, &[]).contains("<summary>Why?</summary>"));
    }

    #[test]
    fn a_refusal_from_outside_the_chain_is_its_own_sentence_escaped() {
        let reasons = vec!["tariff <b>unlawful</b> at 150 kW".to_owned()];
        let (line, cite) = headline(Lang::En, &[], &reasons);
        assert_eq!(cite, None);
        assert!(line.contains("&lt;b&gt;"), "{line}");
        assert!(details(Lang::En, &[], &reasons).contains("&lt;b&gt;unlawful"));
    }
}
