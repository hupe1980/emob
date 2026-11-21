//! `emob qr <estate.toml> <evse>` — the price page's code, for print (spec 015).
//!
//! An SVG of the QR code for `<receipts.public_url>/p/<evse>`, with the address
//! in plain text beneath it, because a code a person cannot read is the one a
//! sticker can replace. Black on white, a four-module quiet zone, and a
//! `viewBox` so it prints at any size. The Preisstand is not on it: it changes
//! with the price, and a sticker does not.
//!
//! Exit status: `0` written · `1` the estate or the EVSE id is refused · `2`
//! usage.

use std::fmt::Write as _;
use std::io::Write;

use emob_service::estate::Estate;

/// The quiet zone the standard asks for, in modules.
const QUIET: usize = 4;

/// `emob qr <estate.toml> <evse>`.
pub fn main(args: &[String], out: &mut impl Write, err: &mut impl Write) -> u8 {
    let [path, evse] = args else {
        let _ = writeln!(
            err,
            "usage: emob qr <estate.toml> <evse> (writes SVG to stdout)"
        );
        return 2;
    };
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => {
            let _ = writeln!(err, "error: {path} could not be read ({error})");
            return 1;
        }
    };
    let estate: Estate = match emob_service::estate::from_str_at(&text, path) {
        Ok(estate) => estate,
        Err(error) => {
            let _ = writeln!(err, "{error}");
            return 1;
        }
    };
    let Some(receipts) = estate.receipts else {
        let _ = writeln!(
            err,
            "error: {path} has no [receipts] public_url — the price page is served beside the receipts"
        );
        return 1;
    };
    let Ok(evse) = evse.parse::<emob_core::EvseId>() else {
        let _ = writeln!(err, "error: {evse:?} is not an EVSE id like DE*MUS*E00001");
        return 1;
    };
    let url = format!("{}/p/{evse}", receipts.public_url.trim_end_matches('/'));
    match svg(&url) {
        Some(svg) => {
            let _ = out.write_all(svg.as_bytes());
            0
        }
        None => {
            let _ = writeln!(err, "error: {url} is too long for a QR code");
            1
        }
    }
}

/// The code for `url` and the address beneath it, as SVG.
#[must_use]
pub fn svg(url: &str) -> Option<String> {
    let code = qrcode::QrCode::new(url.as_bytes()).ok()?;
    let width = code.width();
    let size = width + 2 * QUIET;
    // Room beneath for one line of text at a size a phone camera ignores.
    let text_height = 6;
    let mut path = String::new();
    for (index, colour) in code.to_colors().iter().enumerate() {
        if *colour == qrcode::Color::Dark {
            let (x, y) = (index % width + QUIET, index / width + QUIET);
            let _ = write!(path, "M{x} {y}h1v1h-1z");
        }
    }
    let escaped = url
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    Some(format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {size} {height}\" shape-rendering=\"crispEdges\">\
         <title>Preis vor dem Laden: {escaped}</title>\
         <rect width=\"{size}\" height=\"{height}\" fill=\"#fff\"/>\
         <path d=\"{path}\" fill=\"#000\"/>\
         <text x=\"{middle}\" y=\"{baseline}\" font-family=\"sans-serif\" font-size=\"2\" text-anchor=\"middle\" fill=\"#000\">{escaped}</text></svg>\n",
        height = size + text_height,
        middle = size / 2,
        baseline = size + 2,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_code_carries_the_address_and_prints_it_beneath() {
        let svg = svg("https://charge.example/p/DE*MUS*E00001").unwrap();
        assert!(svg.starts_with("<svg"));
        assert!(svg.contains(">https://charge.example/p/DE*MUS*E00001</text>"));
        assert!(svg.contains("fill=\"#000\""));
        assert!(!svg.contains("<script"));
    }

    #[test]
    fn usage_and_a_bad_evse_are_refused() {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(main(&[], &mut out, &mut err), 2);
        let estate = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../deploy/estate.example.toml"
        );
        let args = [estate.to_owned(), "not an evse".to_owned()];
        assert_eq!(main(&args, &mut out, &mut err), 1);
        let args = [estate.to_owned(), "DE*ABC*E00001".to_owned()];
        assert_eq!(
            main(&args, &mut out, &mut err),
            0,
            "{}",
            String::from_utf8_lossy(&err)
        );
        assert!(String::from_utf8_lossy(&out).contains("/p/DE*ABC*E00001"));
    }
}
