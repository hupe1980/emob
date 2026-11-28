//! Values shared by the finance API, the billing feed and the sealed journal.
use emob_core::{Currency, PartyId};
use emob_service::Actor;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Document {
    pub position: u64,
    pub issuer: PartyId,
    pub account: String,
    pub currency: Currency,
    pub number: String,
    pub kind: Kind,
    pub amount_minor: i64,
    pub predecessor: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Kind {
    Invoice,
    Credit,
    #[serde(rename = "line_credit")]
    LineCredit,
    Replacement,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Entry {
    pub reference: Option<String>,
    pub amount_minor: i64,
    pub status: String,
    pub direction: String,
    #[serde(default)]
    pub reversal: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Import {
    pub version: u8,
    pub source: String,
    pub account: String,
    pub currency: Currency,
    pub entries: Vec<Entry>,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Booking {
    pub reference: String,
    pub amount_minor: i64,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Held {
    pub id: String,
    pub reference: Option<String>,
    pub reason: String,
    pub entry: Entry,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Statement {
    pub issuer: PartyId,
    pub account: String,
    pub currency: Currency,
    pub source: String,
    pub payments: Vec<Booking>,
    pub held: Vec<Held>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Allocation {
    pub source: String,
    pub reference: String,
    pub number: String,
    pub account: String,
    pub currency: Currency,
    pub amount_minor: i64,
}

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct Preview {
    pub review: String,
    pub open_after_minor: i64,
    pub unapplied_after_minor: i64,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Accepted {
    pub allocation: Allocation,
    pub actor: Actor,
    pub result: Preview,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Offset {
    pub source_number: String,
    pub target_number: String,
    pub account: String,
    pub currency: Currency,
    pub amount_minor: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct OffsetReceipt {
    #[serde(flatten)]
    pub selection: Offset,
    pub actor: Actor,
    pub issuer: PartyId,
    pub source_revision: u64,
    pub target_revision: u64,
    pub source_before_minor: i64,
    pub source_after_minor: i64,
    pub target_before_minor: i64,
    pub target_after_minor: i64,
    pub review: String,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "fact", rename_all = "lowercase")]
pub(crate) enum Fact {
    Document { document: Document },
    Statement(Statement),
    Allocation(Accepted),
    Offset(OffsetReceipt),
}

impl Fact {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Document { .. } => "document",
            Self::Statement(_) => "statement",
            Self::Allocation(_) => "allocation",
            Self::Offset(_) => "offset",
        }
    }
}

pub(crate) fn valid_reference(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"/.:_-".contains(&c))
}
