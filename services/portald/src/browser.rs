use std::sync::Arc;

use axum::extract::rejection::FormRejection;
use axum::extract::{DefaultBodyLimit, Form, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse as _, Redirect, Response};
use axum::routing::{get, post};
use axum::{Router, middleware};
use emob_service::ui::{self, Lang};
use serde::Deserialize;

use crate::client::{Login, Owners, valid_session};
use crate::views;

#[derive(Deserialize, Default)]
struct Params {
    lang: Option<String>,
    #[serde(default)]
    page: u32,
}

impl Params {
    fn lang(&self, headers: &HeaderMap) -> Lang {
        Lang::negotiate(
            self.lang.as_deref(),
            headers
                .get(header::ACCEPT_LANGUAGE)
                .and_then(|h| h.to_str().ok()),
        )
    }
}

type App = State<Arc<Owners>>;

pub(crate) fn routes(owners: Owners) -> Router {
    let pages = Router::new()
        .route("/", get(root))
        .route("/customer/", get(overview))
        .route("/customer/login", post(login))
        .route("/customer/logout", post(logout))
        .route("/customer/document", post(document))
        .route("/customer/balance", post(balance))
        .layer(DefaultBodyLimit::max(2048))
        .layer(middleware::map_response(private))
        .with_state(Arc::new(owners));
    pages.merge(ui::assets())
}

async fn private(response: Response) -> Response {
    let mut response = ui::hardened(response, ui::CSP);
    // Native same-origin form submissions retain their Origin in browsers.
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("same-origin"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response
}

fn same_origin(app: &Owners, headers: &HeaderMap) -> bool {
    let mut origins = headers.get_all(header::ORIGIN).iter();
    origins
        .next()
        .is_some_and(|value| value.as_bytes() == app.origin.as_bytes())
        && origins.next().is_none()
        && headers
            .get("sec-fetch-site")
            .is_none_or(|site| site.as_bytes() == b"same-origin")
}

fn cookie_name(app: &Owners) -> &'static str {
    if app.secure {
        "__Secure-emob_customer"
    } else {
        "emob_customer_local"
    }
}

fn session<'a>(app: &Owners, headers: &'a HeaderMap) -> Option<&'a str> {
    let mut matched = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|h| h.to_str().ok())
        .flat_map(|h| h.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .filter(|(key, _)| *key == cookie_name(app));
    let (_, token) = matched.next()?;
    (matched.next().is_none() && valid_session(token)).then_some(token)
}

fn set_cookie(app: &Owners, value: Option<&str>, response: &mut Response) {
    let secure = if app.secure { "; Secure" } else { "" };
    let expiry = if value.is_none() { "; Max-Age=0" } else { "" };
    let cookie = format!(
        "{}={}; Path=/customer/; HttpOnly; SameSite=Strict{secure}{expiry}",
        cookie_name(app),
        value.unwrap_or("")
    );
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie).expect("validated hex session and static attributes"),
    );
}

fn home(lang: Lang) -> Response {
    Redirect::to(&format!("/customer/?lang={}", lang.tag())).into_response()
}

async fn root(Query(q): Query<Params>, headers: HeaderMap) -> Response {
    home(q.lang(&headers))
}

fn expired(app: &Owners, lang: Lang) -> Response {
    let mut r = views::sign_in(
        lang,
        Some(lang.pick("Bitte melden Sie sich erneut an.", "Please sign in again.")),
    );
    *r.status_mut() = StatusCode::UNAUTHORIZED;
    set_cookie(app, None, &mut r);
    r
}

async fn overview(State(app): App, Query(q): Query<Params>, headers: HeaderMap) -> Response {
    let lang = q.lang(&headers);
    let Some(token) = session(&app, &headers) else {
        return views::sign_in(lang, None);
    };
    let customer = match app.customer(token).await {
        Ok(me) => me,
        Err(StatusCode::UNAUTHORIZED) => return expired(&app, lang),
        Err(status) => return views::problem(lang, status, true),
    };
    match app.history(token, q.page).await {
        Ok(history) => views::overview(lang, &customer, Some(&history), q.page, &app),
        Err(StatusCode::UNAUTHORIZED) => expired(&app, lang),
        Err(_) => views::overview(lang, &customer, None, q.page, &app),
    }
}

async fn login(
    State(app): App,
    Query(q): Query<Params>,
    headers: HeaderMap,
    data: Result<Form<Login>, FormRejection>,
) -> Response {
    let lang = q.lang(&headers);
    if !same_origin(&app, &headers) {
        return views::problem(lang, StatusCode::FORBIDDEN, false);
    }
    let Ok(Form(input)) = data else {
        return views::problem(lang, StatusCode::BAD_REQUEST, false);
    };
    match app.login(&input).await {
        Ok(token) => {
            let mut r = home(lang);
            set_cookie(&app, Some(&token), &mut r);
            r
        }
        Err(status @ (StatusCode::UNAUTHORIZED | StatusCode::TOO_MANY_REQUESTS)) => {
            let hint = if status == StatusCode::TOO_MANY_REQUESTS {
                lang.pick(
                    "Zu viele Anmeldeversuche. Versuchen Sie es später erneut.",
                    "Too many sign-in attempts. Please try again later.",
                )
            } else {
                lang.pick(
                    "Benutzerkennung oder Zugangsschlüssel ungültig.",
                    "Incorrect user ID or access key.",
                )
            };
            let mut r = views::sign_in(lang, Some(hint));
            *r.status_mut() = status;
            r
        }
        Err(status) => views::problem(lang, status, false),
    }
}

async fn logout(State(app): App, Query(q): Query<Params>, headers: HeaderMap) -> Response {
    let lang = q.lang(&headers);
    if !same_origin(&app, &headers) {
        return views::problem(lang, StatusCode::FORBIDDEN, true);
    }
    if let Some(token) = session(&app, &headers) {
        match app.logout(token).await {
            Ok(()) | Err(StatusCode::UNAUTHORIZED) => {}
            Err(status) => return views::problem(lang, status, true),
        }
    }
    let mut r = home(lang);
    set_cookie(&app, None, &mut r);
    r
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    number: String,
}

fn document_request<'a>(
    app: &Owners,
    headers: &'a HeaderMap,
    data: Result<Form<Document>, FormRejection>,
) -> Result<(&'a str, String), StatusCode> {
    if !same_origin(app, headers) {
        return Err(StatusCode::FORBIDDEN);
    }
    let token = session(app, headers).ok_or(StatusCode::UNAUTHORIZED)?;
    let Form(input) = data.map_err(|_| StatusCode::BAD_REQUEST)?;
    if input.number.is_empty() || input.number.len() > 96 {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok((token, input.number))
}

async fn document(
    State(app): App,
    Query(q): Query<Params>,
    headers: HeaderMap,
    data: Result<Form<Document>, FormRejection>,
) -> Response {
    let lang = q.lang(&headers);
    let (token, number) = match document_request(&app, &headers, data) {
        Ok(input) => input,
        Err(StatusCode::UNAUTHORIZED) => return expired(&app, lang),
        Err(status) => return views::problem(lang, status, true),
    };
    match app.document(token, &number).await {
        Ok(body) => (
            [
                (header::CONTENT_TYPE, "application/xml"),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=\"invoice.xml\"",
                ),
            ],
            body,
        )
            .into_response(),
        Err(StatusCode::UNAUTHORIZED) => expired(&app, lang),
        Err(status) => views::problem(lang, status, true),
    }
}

async fn balance(
    State(app): App,
    Query(q): Query<Params>,
    headers: HeaderMap,
    data: Result<Form<Document>, FormRejection>,
) -> Response {
    let lang = q.lang(&headers);
    let result = match document_request(&app, &headers, data) {
        Ok((token, number)) => app.balance(token, &number).await,
        Err(status) => Err(status),
    };
    match result {
        Ok(balance) => views::balance(&balance, lang, q.page),
        Err(StatusCode::UNAUTHORIZED) => expired(&app, lang),
        Err(status) => views::problem(lang, status, true),
    }
}
