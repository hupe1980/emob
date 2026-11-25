//! A refused estate, reported the way `rustc` reports a refused program: what
//! is wrong, where, the line itself with the place marked, and what would be
//! valid (spec 012, design rule 9).
//!
//! ```text
//! error: unknown field `evse_id`
//!   --> estate.toml:16:1
//!    |
//! 16 | evse_id = "DE*ABC*E00001"
//!    | ^^^^^^^
//!    |
//!    = help: did you mean `evse`?
//! ```

use core::fmt::Write as _;

/// Render `message` at `span` of `text`, read from `path`.
#[must_use]
pub fn render(
    path: &str,
    text: &str,
    span: Option<core::ops::Range<usize>>,
    message: &str,
) -> String {
    let (headline, expected) = split(message);
    let mut out = format!("error: {headline}\n");
    if let Some(span) = span.filter(|s| s.start <= text.len()) {
        let line_start = text[..span.start].rfind('\n').map_or(0, |i| i + 1);
        let line_end = text[span.start..]
            .find('\n')
            .map_or(text.len(), |i| span.start + i);
        let line_no = text[..span.start].matches('\n').count() + 1;
        // An inventory diagnostic stays useful with a file and line number.
        // A TOML message or excerpt can contain the entire private card value.
        if private_card_section(&text[..line_end]) {
            return format!(
                "error: invalid card configuration at {path}:{line_no}; check the inventory code and UID fields"
            );
        }
        let column = text[line_start..span.start].chars().count() + 1;
        let width = line_no.to_string().len();
        let pad = " ".repeat(width);
        let source = &text[line_start..line_end];
        let marked = text[span.start..span.end.min(line_end).max(span.start)]
            .chars()
            .count()
            .max(1);
        let _ = writeln!(out, "{pad}--> {path}:{line_no}:{column}");
        let _ = writeln!(out, "{pad} |");
        let _ = writeln!(out, "{line_no} | {source}");
        let _ = writeln!(
            out,
            "{pad} | {}{}",
            " ".repeat(column - 1),
            "^".repeat(marked)
        );
        if let Some(help) = help(&headline, &expected) {
            let _ = writeln!(out, "{pad} |");
            let _ = writeln!(out, "{pad} = help: {help}");
        }
    } else {
        let _ = writeln!(out, "  --> {path}");
        if let Some(help) = help(&headline, &expected) {
            let _ = writeln!(out, "  = help: {help}");
        }
    }
    out
}

fn private_card_section(prefix: &str) -> bool {
    for line in prefix.lines().rev().map(str::trim_start) {
        // Also protect inventory written as an inline or multiline TOML array.
        let key = line
            .split_once('=')
            .map(|(key, _)| key.trim().trim_matches(['\'', '"']));
        if matches!(key, Some("card_stock" | "contracts" | "uid")) {
            return true;
        }
        if line.starts_with('[') {
            let table = line
                .trim_start_matches('[')
                .split([']', '.'])
                .next()
                .unwrap_or_default();
            return matches!(
                table.trim().trim_matches(['\'', '"']),
                "card_stock" | "contracts"
            );
        }
    }
    false
}

/// `serde`'s "unknown field `x`, expected one of `a`, `b`" into the headline
/// and the list.
fn split(message: &str) -> (String, Vec<String>) {
    let message = message.trim();
    for marker in [", expected one of ", ", expected "] {
        if let Some((head, rest)) = message.split_once(marker) {
            let expected = rest
                .split('`')
                .skip(1)
                .step_by(2)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>();
            if !expected.is_empty() {
                return (head.to_owned(), expected);
            }
        }
    }
    (message.to_owned(), Vec::new())
}

fn help(headline: &str, expected: &[String]) -> Option<String> {
    if expected.is_empty() {
        return None;
    }
    let found = headline.split('`').nth(1).unwrap_or_default();
    let close = expected
        .iter()
        .map(|candidate| (distance(found, candidate), candidate))
        .filter(|(d, c)| *d <= c.len().max(found.len()) / 2)
        .min_by_key(|(d, _)| *d);
    Some(match close {
        Some((_, candidate)) => format!("did you mean `{candidate}`?"),
        None => format!(
            "expected one of {}",
            expected
                .iter()
                .map(|e| format!("`{e}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    })
}

/// Edit distance, for "did you mean". Keys are short.
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let current = row[j + 1];
            row[j + 1] = (previous + usize::from(ca != *cb))
                .min(row[j] + 1)
                .min(current + 1);
            previous = current;
        }
    }
    row[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_misspelt_key_is_shown_where_it_is_with_the_key_it_meant() {
        let text = "[[stations]]\nidentity = \"CP-1\"\nevse_id = \"DE*ABC*E00001\"\n";
        let start = text.find("evse_id").unwrap();
        let report = render(
            "estate.toml",
            text,
            Some(start..start + 7),
            "unknown field `evse_id`, expected one of `identity`, `evse`, `ocpp`",
        );
        assert_eq!(
            report,
            "error: unknown field `evse_id`\n --> estate.toml:3:1\n  |\n3 | evse_id = \"DE*ABC*E00001\"\n  | ^^^^^^^\n  |\n  = help: did you mean `evse`?\n"
        );
    }

    #[test]
    fn nothing_close_lists_what_is_accepted() {
        let report = render(
            "e.toml",
            "x = 1\n",
            Some(0..1),
            "unknown field `zzzzzz`, expected `a` or `b`",
        );
        assert!(
            report.contains("= help: expected one of `a`, `b`"),
            "{report}"
        );
    }
}
