//! Exact, bounded operator card searches. Only empd has the key and the issue
//! facts needed to resolve the identifier a driver reads from their card.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound::{Excluded, Unbounded};

use emob_core::Emaid;
use emob_session::TokenRef;
use serde::{Deserialize, Serialize};

use crate::{Empd, Token, TokenType};

const PAGE: usize = 20;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    Account(Emaid),
    Printed(String),
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Index(BTreeMap<Key, BTreeSet<TokenRef>>);

impl Index {
    fn remove(&mut self, key: &Key, reference: &TokenRef) {
        if let Some(cards) = self.0.get_mut(key) {
            cards.remove(reference);
            if cards.is_empty() {
                self.0.remove(key);
            }
        }
    }
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Kind {
    Reference,
    Visual,
    Emaid,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Query {
    kind: Kind,
    value: String,
    after: Option<TokenRef>,
}

#[derive(Serialize)]
pub(crate) struct Matches {
    items: Vec<Card>,
    next: Option<TokenRef>,
}

#[derive(Serialize)]
struct Card {
    reference: TokenRef,
    visual: String,
    token_type: TokenType,
    blocked: bool,
    retired: bool,
    history: Vec<crate::actions::ResultView>,
}

impl Card {
    fn of(reference: &TokenRef, token: &Token, retired: bool) -> Self {
        Self {
            reference: reference.clone(),
            visual: label(reference),
            token_type: token.token_type,
            blocked: token.blocked,
            retired,
            history: Vec::new(),
        }
    }
}

pub(crate) fn label(reference: &TokenRef) -> String {
    format!("EM-{}", reference.as_str()[..12].to_ascii_uppercase())
}

impl Empd {
    fn visual_key(&self, text: &str) -> String {
        self.key
            .digest("empd/card-label", text.as_bytes())
            .hex()
            .to_owned()
    }

    pub(crate) fn has_printed_digest(&self, digest: &str) -> bool {
        self.cards.0.contains_key(&Key::Printed(digest.to_owned()))
    }

    pub(crate) fn printed_digest(&self, text: &str) -> Option<String> {
        emob_service::estate::printed_card_number(text).map(|value| self.visual_key(value))
    }

    fn card_keys(&self, reference: &TokenRef, token: &Token) -> impl Iterator<Item = Key> + use<> {
        [
            Some(Key::Printed(self.visual_key(&label(reference)))),
            token.visual_digest.clone().map(Key::Printed),
            Emaid::parse(token.contract.as_str()).ok().map(Key::Account),
        ]
        .into_iter()
        .flatten()
    }

    pub(crate) fn store_card(&mut self, reference: &TokenRef, mut token: Token) {
        if token.token_type == TokenType::Rfid && !self.tokens.contains_key(reference.as_str()) {
            match self.key.open_str(crate::UID, &token.uid) {
                Ok(uid) => {
                    self.issued_rfids
                        .insert(self.reference_for(&uid.to_ascii_uppercase()));
                }
                // Older issuances cannot be compared after loss of the sealing key.
                // New native allocation must then fail closed.
                Err(_) => self.foreign_uid_key = true,
            }
        }
        if let Some(prior) = self.tokens.get(reference.as_str()) {
            token.revision = prior.revision.saturating_add(1);
            for key in self.card_keys(reference, prior) {
                self.cards.remove(&key, reference);
            }
        }
        for key in self.card_keys(reference, &token) {
            self.cards
                .0
                .entry(key)
                .or_default()
                .insert(reference.clone());
        }
        self.tokens.insert(reference.as_str().to_owned(), token);
    }

    pub(crate) fn set_label(&mut self, reference: &TokenRef, digest: Option<String>) {
        if let Some(mut card) = self.token(reference).cloned() {
            card.visual_digest = digest;
            self.store_card(reference, card);
        }
    }

    /// No UID is decrypted while building or querying the operator projection.
    pub(crate) fn lookup_cards(&self, query: Query) -> Option<Matches> {
        let key = match query.kind {
            Kind::Reference => {
                let reference = TokenRef::new(query.value).ok()?;
                let items = self
                    .token(&reference)
                    .filter(|_| query.after.as_ref().is_none_or(|after| reference > *after))
                    .map(|token| {
                        let mut card =
                            Card::of(&reference, token, self.changes.retired(&reference));
                        if let Some(history) = self.card_history.get(&reference) {
                            card.history = history
                                .iter()
                                .cloned()
                                .map(crate::actions::ResultView::of)
                                .collect();
                        }
                        card
                    })
                    .into_iter()
                    .collect();
                return Some(Matches { items, next: None });
            }
            Kind::Visual => Key::Printed(self.printed_digest(&query.value)?),
            Kind::Emaid => Key::Account(Emaid::parse(query.value.trim()).ok()?),
        };
        let mut items = Vec::new();
        let mut next = None;
        if let Some(group) = self.cards.0.get(&key) {
            let range = (query.after.as_ref().map_or(Unbounded, Excluded), Unbounded);
            let mut refs = group.range(range);
            for reference in refs.by_ref().take(PAGE) {
                if let Some(token) = self.token(reference) {
                    items.push(Card::of(reference, token, self.changes.retired(reference)));
                }
            }
            if refs.next().is_some() {
                next = items.last().map(|card| card.reference.clone());
            }
        }
        Some(Matches { items, next })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Book, Contract, Whitelist};
    use emob_service::{
        Actor,
        journal::{Journal, Memory},
        vault::DataKey,
    };

    fn empty() -> Empd {
        Empd::new("DE*MSP".parse().unwrap(), DataKey::from_bytes(&[7; 32]))
    }

    fn search(state: &Empd, kind: Kind, value: &str, after: Option<TokenRef>) -> Matches {
        state
            .lookup_cards(Query {
                kind,
                value: value.to_owned(),
                after,
            })
            .unwrap()
    }

    // SR-033: a shared account and duplicate printed numbers are paged in
    // keyed-reference order even after replay and reissue.
    #[test]
    fn lookups_page_exact_subjects_and_replay_label_changes_without_plaintext() {
        let id: emob_core::ContractId = "DE-MSP-C00000001-W".parse().unwrap();
        let mut book = Book::open(empty(), Memory::new()).unwrap();
        book.with(
            &Actor::Estate,
            Contract::new(id.clone(), time::macros::date!(2025 - 01 - 01)),
        )
        .unwrap();
        let digest = book.state().printed_digest("Über 123");
        let mut issued = Vec::new();
        for n in 0..23 {
            let card = book
                .issue_token(
                    &Actor::Estate,
                    &id,
                    format!("04DEAD{n:08}"),
                    TokenType::Rfid,
                    Whitelist::Allowed,
                )
                .unwrap();
            book.label(&Actor::Estate, &card, digest.clone()).unwrap();
            issued.push(card);
        }
        book.label(
            &Actor::Estate,
            &issued[0],
            book.state().printed_digest("New"),
        )
        .unwrap();
        book.label(&Actor::Estate, &issued[1], None).unwrap();
        let records = book.journal().records().unwrap();
        let restored = Memory::new();
        for row in records {
            assert!(!row.body.contains("Über 123"));
            assert!(!row.body.contains("04DEAD"));
            restored.append(&row.kind, &row.actor, &row.body).unwrap();
        }
        let book = Book::open(empty(), restored).unwrap();
        for (kind, query, count) in [
            (Kind::Visual, "Über 123", 21),
            (Kind::Emaid, "DEMSPC00000001", 23),
        ] {
            let first = search(book.state(), kind, query, None);
            assert_eq!(first.items.len(), PAGE);
            let after = first.next.unwrap();
            let second = search(book.state(), kind, query, Some(after.clone()));
            assert!(second.next.is_none());
            assert_eq!(count - PAGE, second.items.len());
            assert!(second.items.iter().all(|card| card.reference > after));
        }
        assert!(
            search(book.state(), Kind::Visual, "über 123", None)
                .items
                .is_empty()
        );
        assert_eq!(
            search(book.state(), Kind::Visual, "New", None).items[0].reference,
            issued[0]
        );
    }
}
