//! Spec 017: the public verifier's worked example verifies, and its tampered
//! copy does not — checked with the very functions the WebAssembly module
//! exports, on the text the page carries.

use emob_verify::{Verdict, check};

fn attribute(page: &str, name: &str) -> String {
    let start = page.find(&format!("{name}=\"")).expect(name) + name.len() + 2;
    let end = start + page[start..].find('"').expect("closing quote");
    page[start..end]
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

#[test]
fn the_worked_example_verifies_and_one_changed_digit_fails() {
    let page = include_str!("../../../site/content/verify.md");
    let record = attribute(page, "data-example-record");
    let key = attribute(page, "data-example-key");
    assert_eq!(check(&record, &key).verdict, Verdict::Verified);
    // What the page's "tamper" button does.
    let tampered = record.replace("\"RV\":7753", "\"RV\":7754");
    assert_ne!(tampered, record, "the button has something to change");
    assert_eq!(check(&tampered, &key).verdict, Verdict::NotVerified);
    let script = include_str!("../../../site/static/verify/verify.js");
    assert!(
        script.contains(r#"replace('"RV":7753', '"RV":7754')"#),
        "the page tampers as tested"
    );
}
