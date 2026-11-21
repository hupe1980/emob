//! `emob check-feed <file>` — a DATEX II AFIR file, held to what the
//! Mobilithek takes (spec 016).
//!
//! Any operator's file, not only emob's: XML or JSON is read by `datex2`,
//! which lists every deviation it tolerated, and each publication is then
//! written again through the Mobilithek profile's writer — the one `poid`
//! publishes with — which refuses what the profile's schemas reject and names
//! the member and the rule. What the reader tolerated here, a consumer may
//! drop there.
//!
//! Exit status: `0` the profile takes every publication · `1` it refuses one
//! · `2` unreadable or usage.

use std::io::Write;

/// `emob check-feed <file>`.
pub fn main(args: &[String], out: &mut impl Write, err: &mut impl Write) -> u8 {
    let [path] = args else {
        let _ = writeln!(err, "usage: emob check-feed <file.json|file.xml>");
        return 2;
    };
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            let _ = writeln!(err, "error: {path} could not be read ({error})");
            return 2;
        }
    };
    let json = bytes
        .iter()
        .find(|b| !b.is_ascii_whitespace())
        .is_some_and(|b| *b == b'{' || *b == b'[');
    let read = if json {
        datex2::v3::json::read(&bytes[..])
    } else {
        datex2::v3::read(&bytes[..])
    };
    let document = match read {
        Ok(document) => document,
        Err(error) => {
            let _ = writeln!(err, "error: {path} is not DATEX II v3: {error}");
            return 2;
        }
    };
    let _ = writeln!(
        out,
        "{path}: {} publication(s), {} in {}",
        document.publications.len(),
        if json { "JSON" } else { "XML" },
        if document.soap {
            "a SOAP envelope"
        } else {
            "its envelope"
        },
    );
    for warning in &document.warnings {
        let _ = writeln!(out, "  ⚠ {warning}");
    }
    if document.suppressed_warnings > 0 {
        let _ = writeln!(out, "  ⚠ and {} more", document.suppressed_warnings);
    }
    let mut refused = 0;
    for (index, publication) in document.publications.iter().enumerate() {
        match datex2::v3::json::mobilithek::to_string(publication) {
            Ok(_) => {
                let _ = writeln!(
                    out,
                    "  ✓ publication {}: the Mobilithek profile takes it",
                    index + 1
                );
            }
            Err(error) => {
                refused += 1;
                let _ = writeln!(out, "  ✗ publication {}: {error}", index + 1);
            }
        }
    }
    if refused == 0 && !document.publications.is_empty() {
        let _ = writeln!(
            out,
            "✓ {path}: the Mobilithek profile takes every publication{}",
            if document.warnings.is_empty() {
                String::new()
            } else {
                format!(
                    " — {} tolerated deviation(s) above",
                    document.warnings.len()
                )
            }
        );
        0
    } else {
        let _ = writeln!(
            out,
            "✗ {path}: {refused} publication(s) the Mobilithek would refuse"
        );
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[String]) -> (u8, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = main(args, &mut out, &mut err);
        (
            code,
            String::from_utf8_lossy(&out).into_owned() + &String::from_utf8_lossy(&err),
        )
    }

    #[test]
    fn emobs_own_table_passes_and_garbage_is_named() {
        // The table `poid` writes for the example estate.
        let estate: poid::http::Estate =
            emob_service::estate::from_str(include_str!("../../../deploy/estate.example.toml"))
                .unwrap();
        let now = time::macros::datetime!(2026-12-01 10:00 UTC);
        let rates = poid::rates(&estate, now).unwrap();
        let rate_for = |point: &emob_poi::site::ChargingPoint| rates.get(&point.evse_id).cloned();
        let table = poid::Poid::new(
            emob_poi::datex::Publisher {
                country: "DE".to_owned(),
                national_identifier: "DE-NAP-TEST".to_owned(),
                language: "de".to_owned(),
            },
            emob_poi::site::Facility::new("DE*ABC-TABLE"),
            poid::http::sites(&estate).unwrap(),
            &rate_for,
        )
        .snapshot(now)
        .unwrap()
        .to_json()
        .unwrap();
        let dir = std::env::temp_dir().join(format!("emob-check-feed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let good = dir.join("table.json");
        std::fs::write(&good, table).unwrap();
        let (code, said) = run(&[good.display().to_string()]);
        assert_eq!(code, 0, "{said}");
        assert!(said.contains("takes every publication"), "{said}");

        let bad = dir.join("bad.json");
        std::fs::write(&bad, "{\"not\": \"datex\"}").unwrap();
        let (code, said) = run(&[bad.display().to_string()]);
        assert_eq!(code, 2, "{said}");
        assert_eq!(run(&[]).0, 2);
    }
}
