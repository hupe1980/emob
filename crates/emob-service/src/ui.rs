//! The pages a daemon serves: one look, one shell, one set of headers.
//!
//! Pages are server-rendered HTML with their assets compiled in — no npm, no
//! build step, no external URL. A page renders a document a daemon already
//! holds and computes nothing that decides: every figure on it comes from the
//! function that billed, published or refused. Script, where a page has any,
//! only enhances; every page works without it.
//!
//! What lives here and nowhere else:
//!
//! - [`STYLESHEET`], served at [`STYLESHEET_PATH`] by [`assets`] — one accent,
//!   status as word + icon + tint, light/dark/high-contrast, 320 px reflow;
//! - [`page`], the document shell (skip link, header, `lang`, viewport);
//! - [`hardened`], the headers every page carries (a `default-src 'self'`
//!   content security policy, no framing, no sniffing, no referrer);
//! - [`escape`], the only way text reaches markup;
//! - [`Lang`], German first with English typed against it, and the way a date
//!   and a decimal are written in each;
//! - [`operator_page`], the door an operator's browser comes through — HTTP
//!   Basic over the estate's operator tokens, so a page needs no login form,
//!   no session store and no script;
//! - [`script`], a page's own same-origin script, served with the same headers.

use axum::Router;
use axum::http::{HeaderValue, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;

/// The shared stylesheet.
pub const STYLESHEET: &str = include_str!("ui.css");

/// Where [`assets`] serves the stylesheet.
pub const STYLESHEET_PATH: &str = "/ui/emob.css";

/// The content security policy every page carries. Nothing loads from another
/// origin, nothing frames the page, and a page that needs WebAssembly says so
/// with [`CSP_WITH_WASM`].
pub const CSP: &str = "default-src 'self'; img-src 'self' data:; style-src 'self'; \
                       script-src 'self'; connect-src 'self'; frame-ancestors 'none'; \
                       base-uri 'none'; form-action 'self'";

/// [`CSP`] plus the one permission WebAssembly compilation needs.
pub const CSP_WITH_WASM: &str = "default-src 'self'; img-src 'self' data:; style-src 'self'; \
                                 script-src 'self' 'wasm-unsafe-eval'; connect-src 'self'; \
                                 frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

/// The language a page is rendered in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lang {
    /// German — the source language, because the rules are German.
    #[default]
    De,
    /// English.
    En,
}

impl Lang {
    /// `?lang=en|de` first, then the first supported language in
    /// `Accept-Language`, then German.
    #[must_use]
    pub fn negotiate(query_lang: Option<&str>, accept_language: Option<&str>) -> Self {
        if let Some(lang) = query_lang.and_then(Self::parse) {
            return lang;
        }
        accept_language
            .into_iter()
            .flat_map(|value| value.split(','))
            .filter_map(|part| part.split(';').next())
            .find_map(|tag| Self::parse(tag.trim()))
            .unwrap_or_default()
    }

    fn parse(tag: &str) -> Option<Self> {
        let primary = tag.split('-').next()?.to_ascii_lowercase();
        match primary.as_str() {
            "de" => Some(Self::De),
            "en" => Some(Self::En),
            _ => None,
        }
    }

    /// The BCP 47 tag for the `lang` attribute.
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::De => "de",
            Self::En => "en",
        }
    }

    /// Pick the text for this language.
    #[must_use]
    pub const fn pick(self, de: &'static str, en: &'static str) -> &'static str {
        match self {
            Self::De => de,
            Self::En => en,
        }
    }

    /// A calendar date as a reader of this language writes it: `01.01.2027`,
    /// `1 Jan 2027`.
    #[must_use]
    pub fn date(self, date: time::Date) -> String {
        match self {
            Self::De => format!(
                "{:02}.{:02}.{}",
                date.day(),
                u8::from(date.month()),
                date.year()
            ),
            Self::En => {
                const MONTHS: [&str; 12] = [
                    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov",
                    "Dec",
                ];
                let month = MONTHS[usize::from(u8::from(date.month())) - 1];
                format!("{} {month} {}", date.day(), date.year())
            }
        }
    }

    /// A decimal as a reader of this language writes it — a comma in German.
    /// The digits are the decimal's own; nothing is rounded here.
    #[must_use]
    pub fn decimal(self, value: rust_decimal::Decimal) -> String {
        let text = value.to_string();
        match self {
            Self::De => text.replace('.', ","),
            Self::En => text,
        }
    }
}

/// Escape text for HTML element content and quoted attribute values.
#[must_use]
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// What a page says about itself in its shell.
#[derive(Debug, Clone, Copy)]
pub struct Shell<'a> {
    /// The document title (escaped here).
    pub title: &'a str,
    /// Where in the estate this page is, shown beside the brand (escaped here).
    pub place: &'a str,
    /// The page's language.
    pub lang: Lang,
    /// A narrow reading column rather than the full width.
    pub reading: bool,
    /// A same-origin script to load, if the page has one.
    pub script: Option<&'a str>,
}

/// The document around a page's body. `body` is trusted markup the caller
/// built with [`escape`]; nothing else is interpolated unescaped.
#[must_use]
pub fn page(shell: &Shell<'_>, body: &str) -> String {
    page_with_navigation(shell, body, "")
}

/// Render trusted navigation markup in the header, outside the skip-to-content target.
#[must_use]
pub fn page_with_navigation(shell: &Shell<'_>, body: &str, navigation: &str) -> String {
    let script = shell.script.map_or_else(String::new, |src| {
        format!("<script type=\"module\" src=\"{}\"></script>", escape(src))
    });
    format!(
        "<!doctype html><html lang=\"{lang}\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <meta name=\"color-scheme\" content=\"light dark\">\
         <meta name=\"robots\" content=\"noindex\">\
         <title>{title} · emob</title><link rel=\"stylesheet\" href=\"{css}\">{script}</head>\
         <body><a class=\"skip\" href=\"#main\">{skip}</a>\
         <header class=\"bar\"><span class=\"brand\">emob</span><span class=\"where\">{place}</span>{navigation}</header>\
         <main id=\"main\"{class}>{body}</main></body></html>",
        lang = shell.lang.tag(),
        title = escape(shell.title),
        css = STYLESHEET_PATH,
        skip = shell.lang.pick("Zum Inhalt", "Skip to content"),
        place = escape(shell.place),
        class = if shell.reading {
            " class=\"reading\""
        } else {
            ""
        },
    )
}

/// A rendered page with the hardening headers applied.
#[must_use]
pub fn html(markup: String) -> Response {
    hardened(Html(markup).into_response(), CSP)
}

/// Apply the headers every page carries, with the given policy.
#[must_use]
pub fn hardened(mut response: Response, csp: &'static str) -> Response {
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(csp),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// A page's own script at `path`, with the headers every page carries.
///
/// Same origin by construction — the source is compiled in — so the policy's
/// `script-src 'self'` admits it and nothing else.
pub fn script(path: &'static str, source: &'static str) -> Router {
    Router::new().route(
        path,
        get(move || async move {
            let mut response = hardened(source.into_response(), CSP);
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/javascript; charset=utf-8"),
            );
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=3600"),
            );
            response
        }),
    )
}

/// Put pages behind the estate's operator tokens, for a browser.
///
/// A browser does not send `Authorization: Bearer`; it sends HTTP Basic, and it
/// asks for it natively when answered `401` with a challenge — so the page
/// needs no login form, no cookie and no script. The **password** is checked
/// as an operator token by [`crate::authority::Operators`] (constant time; an
/// empty set admits nobody); the user name is ignored. The `401` carries the
/// page headers too, so even the refusal cannot be framed or cached.
///
/// Basic sends the token on every request, which is why a page daemon binds
/// loopback unless it is told otherwise and belongs behind TLS when it is.
pub fn operator_page(routes: Router, operators: crate::authority::Operators) -> Router {
    routes.layer(axum::middleware::from_fn(
        move |request: axum::extract::Request, next: axum::middleware::Next| {
            let operators = operators.clone();
            async move {
                let password = request
                    .headers()
                    .get(header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    .and_then(basic_password);
                let admitted = password
                    .as_deref()
                    .and_then(|password| operators.identify(password));
                if let Some((actor, access)) = admitted {
                    let mut request = request;
                    crate::serve::acting(&mut request, actor.clone());
                    request.extensions_mut().insert(access);
                    next.run(request).await
                } else {
                    let mut refused =
                        hardened(axum::http::StatusCode::UNAUTHORIZED.into_response(), CSP);
                    refused.headers_mut().insert(
                        header::WWW_AUTHENTICATE,
                        HeaderValue::from_static("Basic realm=\"emob\", charset=\"UTF-8\""),
                    );
                    refused
                }
            }
        },
    ))
}

/// The password of an `Authorization: Basic` value.
fn basic_password(value: &str) -> Option<String> {
    use base64::Engine as _;
    let encoded = value.strip_prefix("Basic ")?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .ok()?;
    let text = String::from_utf8(decoded).ok()?;
    let (_, password) = text.split_once(':')?;
    Some(password.to_owned())
}

/// The shared assets: the stylesheet at [`STYLESHEET_PATH`].
pub fn assets() -> Router {
    Router::new().route(
        STYLESHEET_PATH,
        get(|| async {
            let mut response = hardened(STYLESHEET.into_response(), CSP);
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/css; charset=utf-8"),
            );
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=3600"),
            );
            response
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_cannot_become_markup() {
        assert_eq!(
            escape("<script>alert('x')</script> & \"q\""),
            "&lt;script&gt;alert(&#39;x&#39;)&lt;/script&gt; &amp; &quot;q&quot;"
        );
    }

    #[test]
    fn the_shell_escapes_what_it_is_given_and_names_no_other_origin() {
        let markup = page(
            &Shell {
                title: "<x>",
                place: "Heute & morgen",
                lang: Lang::En,
                reading: true,
                script: None,
            },
            "<p>body</p>",
        );
        assert!(markup.contains("<title>&lt;x&gt; · emob</title>"));
        assert!(markup.contains("Heute &amp; morgen"));
        assert!(markup.contains("lang=\"en\""));
        assert!(!markup.contains("http://") && !markup.contains("https://"));
        assert!(!STYLESHEET.contains("http://") && !STYLESHEET.contains("https://"));
        assert!(!STYLESHEET.contains("@import"));
    }

    #[test]
    fn german_unless_asked_otherwise() {
        assert_eq!(Lang::negotiate(None, None), Lang::De);
        assert_eq!(Lang::negotiate(Some("en"), Some("de-DE")), Lang::En);
        assert_eq!(Lang::negotiate(None, Some("fr-FR, en-GB;q=0.8")), Lang::En);
        assert_eq!(Lang::negotiate(Some("xx"), Some("fr")), Lang::De);
    }

    #[test]
    fn dates_and_decimals_are_written_the_reader_s_way() {
        let date = time::macros::date!(2027 - 01 - 01);
        assert_eq!(Lang::De.date(date), "01.01.2027");
        assert_eq!(Lang::En.date(date), "1 Jan 2027");
        let value = rust_decimal::Decimal::new(833, 1);
        assert_eq!(Lang::De.decimal(value), "83,3");
        assert_eq!(Lang::En.decimal(value), "83.3");
    }

    async fn status_of(router: Router, authorization: Option<&str>) -> Response {
        use tower_service::Service as _;
        let mut request = axum::http::Request::builder().uri("/");
        if let Some(value) = authorization {
            request = request.header(header::AUTHORIZATION, value);
        }
        let mut router = router;
        router
            .call(request.body(axum::body::Body::empty()).unwrap())
            .await
            .unwrap()
    }

    fn page_router(tokens: Vec<&str>) -> Router {
        let operators = crate::authority::Operators::new(
            tokens
                .into_iter()
                .map(|token| {
                    (
                        crate::Actor::operator("ops").unwrap(),
                        crate::authority::Token::new(token),
                    )
                })
                .collect(),
        );
        operator_page(
            Router::new().route("/", get(|| async { html("<p>x</p>".to_owned()) })),
            operators,
        )
    }

    fn basic(user_password: &str) -> String {
        use base64::Engine as _;
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(user_password)
        )
    }

    #[tokio::test]
    async fn sr_050_a_browser_gets_in_with_an_operator_token_as_its_password_and_nothing_else() {
        let ok = status_of(
            page_router(vec!["op-token"]),
            Some(&basic("anyone:op-token")),
        )
        .await;
        assert_eq!(ok.status(), axum::http::StatusCode::OK);

        for offered in [
            None,
            Some(basic("anyone:wrong")),
            Some(basic("op-token")),
            Some("Bearer op-token".to_owned()),
            Some("Basic ???".to_owned()),
        ] {
            let refused = status_of(page_router(vec!["op-token"]), offered.as_deref()).await;
            assert_eq!(
                refused.status(),
                axum::http::StatusCode::UNAUTHORIZED,
                "{offered:?}"
            );
            assert!(
                refused.headers()[header::WWW_AUTHENTICATE]
                    .to_str()
                    .unwrap()
                    .starts_with("Basic realm=")
            );
            // SR-052: the refusal carries the page headers too.
            assert_eq!(refused.headers()[header::CONTENT_SECURITY_POLICY], CSP);
            assert_eq!(refused.headers()[header::X_FRAME_OPTIONS], "DENY");
            assert_eq!(refused.headers()[header::CACHE_CONTROL], "no-store");
        }

        // Nobody configured: nobody admitted, not everybody.
        let empty = status_of(page_router(Vec::new()), Some(&basic("a:"))).await;
        assert_eq!(empty.status(), axum::http::StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn every_page_carries_its_policy() {
        let response = html("<p>x</p>".to_owned());
        let headers = response.headers();
        assert_eq!(headers[header::CONTENT_SECURITY_POLICY], CSP);
        assert_eq!(headers[header::X_FRAME_OPTIONS], "DENY");
        assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert!(CSP.contains("frame-ancestors 'none'"));
        assert!(!CSP.contains("unsafe-inline"));
    }
}
