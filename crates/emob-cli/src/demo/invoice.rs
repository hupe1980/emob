//! The demo's invoice, checked the way its recipient would read it (spec 010).
//!
//! `--check` fetches `/i/{token}` and both files from the running billd, reads
//! each file back with `en16931-formats` and validates it with `en16931` — the
//! consumer of the format, not the writer — and asserts the page links the
//! receipt the demo printed.

/// What the recipient sees, fetched.
pub struct Fetched {
    /// The page: status and body.
    pub page: (u16, String),
    /// Each file: its name, and its body where it answered 200.
    pub files: Vec<(&'static str, Option<String>)>,
}

/// Fetch the page at `base + path` and its two files.
pub async fn fetch(base: &str, path: &str) -> Fetched {
    let client = reqwest::Client::new();
    let get = |url: String| {
        let client = client.clone();
        async move {
            match client.get(url).send().await {
                Ok(answer) => (
                    answer.status().as_u16(),
                    answer.text().await.unwrap_or_default(),
                ),
                Err(error) => (0, error.to_string()),
            }
        }
    };
    let page = get(format!("{base}{path}")).await;
    let mut files = Vec::new();
    for name in ["ubl.xml", "cii.xml"] {
        let (status, body) = get(format!("{base}{path}/{name}")).await;
        files.push((name, (status == 200).then_some(body)));
    }
    Fetched { page, files }
}

/// One sentence per way the invoice falls short; empty when it does not.
pub fn judge(fetched: &Fetched, receipt_path: Option<&str>) -> Vec<String> {
    let mut failures = Vec::new();
    let (status, html) = &fetched.page;
    if *status != 200 {
        failures.push(format!("the invoice page answers 200, found {status}"));
    }
    if html.len() >= 30_000 {
        failures.push(format!(
            "the invoice page is under 30 kB, found {} bytes",
            html.len()
        ));
    }
    if let Some(receipt) = receipt_path
        && !html.contains(receipt)
    {
        failures.push(format!(
            "the invoice links the receipt the demo showed ({receipt})"
        ));
    }
    for (name, body) in &fetched.files {
        let Some(xml) = body else {
            failures.push(format!("the invoice's {name} downloads"));
            continue;
        };
        match en16931_formats::read(xml.as_bytes()) {
            Ok(read) => {
                let report = en16931::validate(&read.invoice);
                if !report.is_valid() {
                    failures.push(format!(
                        "the invoice's {name} is valid EN 16931: {}",
                        report
                            .fatal()
                            .map(|f| f.rule.to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
            }
            Err(error) => failures.push(format!("the invoice's {name} reads back: {error}")),
        }
    }
    failures
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_invoice_that_falls_short_fails_the_check_by_name() {
        let fetched = Fetched {
            page: (404, String::new()),
            files: vec![("ubl.xml", None), ("cii.xml", Some("<nope/>".to_owned()))],
        };
        let failures = judge(&fetched, Some("/r/abc"));
        assert_eq!(failures.len(), 4, "{failures:?}");
        assert!(failures.iter().any(|f| f.contains("/r/abc")));
        assert!(failures.iter().any(|f| f.contains("ubl.xml downloads")));
        assert!(failures.iter().any(|f| f.contains("cii.xml reads back")));
    }
}
