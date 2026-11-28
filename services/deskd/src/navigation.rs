//! Shared operator navigation. The active request supplies presentation context;
//! each owner's access checks still decide whether an operation is allowed.

use std::fmt::Write as _;

use axum::Extension;
use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use emob_service::authority::OperatorAccess;
use emob_service::ui::{self, Lang};

#[derive(Clone, Copy)]
struct Navigation {
    access: OperatorAccess,
    accounting: bool,
    workspace: &'static str,
    at_root: bool,
}

tokio::task_local! {
    static NAVIGATION: Navigation;
}

pub(super) async fn scope(
    State(accounting): State<bool>,
    Extension(access): Extension<OperatorAccess>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path();
    let workspace = match path.split('/').nth(1).unwrap_or_default() {
        "station" | "stations" => "/stations",
        "sessions" | "cards" | "lookup" => "/lookup",
        "contracts" => "/contracts",
        "retail-invoices" | "line-credits" => "/retail-invoices",
        "receivables" => "/receivables",
        "customer-access" => "/customer-access",
        "customer-erasure" => "/customer-erasure",
        _ => "/",
    };
    let navigation = Navigation {
        access,
        accounting,
        workspace,
        at_root: path == workspace && request.method() == axum::http::Method::GET,
    };
    NAVIGATION.scope(navigation, next.run(request)).await
}

impl Navigation {
    fn link(self, path: &str, name: &str, class: &str, lang: Lang) -> String {
        let current = if self.workspace != path {
            ""
        } else if self.at_root {
            r#" aria-current="page""#
        } else {
            r#" aria-current="location""#
        };
        let description = if path == "/lookup" {
            lang.pick(
                r#" aria-label="Suche nach Vorgang oder Karte" title="Vorgang oder Karte suchen""#,
                r#" aria-label="Search for a session or card" title="Find a session or card""#,
            )
        } else {
            ""
        };
        format!(
            r#"<a class="{class}" href="{path}?lang={}"{current}{description}>{name}</a>"#,
            lang.tag()
        )
    }

    fn render(self, lang: Lang) -> String {
        let mut nav = format!(
            r#"<nav class="workspace-nav" id="workspaces" aria-label="{}">"#,
            lang.pick("Arbeitsbereiche", "Workspaces")
        );
        for (path, name) in [
            ("/", lang.pick("Heute", "Today")),
            ("/stations", lang.pick("Stationen", "Stations")),
            ("/lookup", lang.pick("Suche", "Search")),
        ] {
            nav.push_str(&self.link(path, name, "primary", lang));
        }
        let mut menu = String::new();
        for (path, name) in [
            ("/contracts", lang.pick("Ladeverträge", "Driver contracts")),
            (
                "/retail-invoices",
                lang.pick("Kundenrechnungen", "Retail invoices"),
            ),
        ] {
            nav.push_str(&self.link(path, name, "wide-link", lang));
            menu.push_str(&self.link(path, name, "narrow-link", lang));
        }
        let mut specialist = false;
        let mut current_tool = false;
        for (allowed, path, name) in [
            (
                self.access.can_write(),
                "/customer-access",
                lang.pick("Portalzugriff", "Customer access"),
            ),
            (
                self.accounting && self.access.can_finance(),
                "/receivables",
                lang.pick("Forderungen", "Receivables"),
            ),
            (
                self.access.can_privacy(),
                "/customer-erasure",
                lang.pick("Kundendaten löschen", "Customer erasure"),
            ),
        ] {
            if allowed {
                specialist = true;
                current_tool |= path == self.workspace;
                menu.push_str(&self.link(path, name, "tool", lang));
            }
        }
        let wide = if specialist { "" } else { " compact-only" };
        let current = if current_tool {
            " current"
        } else if matches!(self.workspace, "/contracts" | "/retail-invoices") {
            " compact-current"
        } else {
            ""
        };
        let _ = write!(
            nav,
            r#"<button type="button" class="workspace-more{wide}{current}" popovertarget="workspace-menu">{}</button><div id="workspace-menu" class="workspace-menu" popover="auto">{menu}</div></nav>"#,
            lang.pick("Mehr", "More")
        );
        nav
    }
}

/// Most pages are rendered inside the authenticated desk router.
pub(super) fn page(shell: &ui::Shell<'_>, body: &str) -> String {
    let nav = NAVIGATION
        .try_with(|context| context.render(shell.lang))
        .unwrap_or_default();
    ui::page_with_navigation(shell, body, &nav)
}

/// Today can also be rendered directly from a captured shift snapshot.
pub(super) fn today(
    shell: &ui::Shell<'_>,
    body: &str,
    access: OperatorAccess,
    accounting: bool,
) -> String {
    let nav = Navigation {
        access,
        accounting,
        workspace: "/",
        at_root: true,
    }
    .render(shell.lang);
    ui::page_with_navigation(shell, body, &nav)
}
