//! The socket the consignment ledger is for.
//!
//! # The bytes that go out are the bytes the account describes
//!
//! `ocpi-kit` will translate a canonical 2.3.0 record onto a peer's version by
//! itself, and this does not use that path. `emob_roam::ocpi::cdr::for_partner`
//! has already produced the partner's copy **and the account of what carrying
//! it there cost**, by JSON Pointer *into that copy*. A second downgrade at the
//! wire would send a document the account does not describe — pointers into a
//! copy nobody received — and both documents are valid, both validate, and only
//! one of them is the one the note is about.
//!
//! So the request carries the bytes the outbox holds, and the version is
//! whatever `Outbound` says it is.
//!
//! # A failure to reach a partner is not a refusal
//!
//! [`Answer`] has three values, and the third is the one a two-value type gets
//! wrong. A receiver that answers `2001 Invalid parameters` has **seen** the
//! record: that is a conversation between two companies, the record is not
//! coming back, and `Node::refused` records it. A connection that times out is
//! not an answer — the partner may hold the record, may not — and the only safe
//! reading is that this node still owes it.
//!
//! That is at-least-once delivery, which is why OCPI keys a CDR by
//! `(country_code, party_id, id)`: a receiver that gets the same record twice
//! is required to treat the second as the first.

use emob_core::PartyId;
use emob_roam::ocpi::cdr::Outbound;

use crate::peering::Registration;
use ocpi_kit::ModuleId;
use ocpi_kit::client::{ClientConfig, OcpiClient, OcpiRequest};
use ocpi_kit::transport::{OcpiError, RoutingHeaders, header_str, headers};
use ocpi_kit::types::{PartyRef, Url};

/// What a partner said, or did not say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// The receiver took it, and said where it now lives.
    ///
    /// `[OCPI 2.3.0 §mod_cdrs]` makes the `Location` header mandatory on the
    /// response precisely so the sender can `GET` back what it sent. `None`
    /// where a peer omits it, which is a quirk rather than a refusal.
    Accepted {
        /// The URL the receiver returned.
        location: Option<String>,
    },

    /// The receiver **saw** it and would not take it.
    ///
    /// A record the partner has judged. It is not coming back on a retry, and
    /// what happens next is a person looking at the reason.
    Refused {
        /// What the receiver said, as near to verbatim as the wire allows.
        reason: String,
    },

    /// Nobody answered, so nothing is known.
    ///
    /// Not a refusal: the partner may hold the record already. The consignment
    /// stays pending and the next drain sends it again, which is why a CDR is
    /// keyed and a receiver de-duplicates.
    Unreached {
        /// What went wrong on this side of the wire.
        why: String,
    },
}

/// Why a node could not even attempt a delivery.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DispatchSetupError {
    /// The HTTP client could not be built — no usable TLS backend.
    ///
    /// Carried as text rather than as `reqwest::Error`: the client is
    /// `ocpi-kit`'s, its HTTP library is that crate's choice, and a daemon that
    /// named the type would pin a second copy of it into this graph the day the
    /// kit moved.
    #[error("the OCPI client could not be built: {0}")]
    Client(String),

    /// This node's own party could not be expressed as an OCPI party.
    #[error("{party} is not a party an OCPI routing header can carry: {source}")]
    Party {
        /// Which party.
        party: String,
        /// Why.
        source: ocpi_kit::types::InvalidString,
    },
}

/// Which of the two failures an error is.
///
/// The question is not *what went wrong* but **did the receiver see the
/// record**, because the answers are opposites: a record a partner judged is
/// out of the queue and waiting for a person, and a record nobody received is
/// still owed and goes again on the next tick.
///
/// `OcpiError::was_answered` is that question, and it is the kit's: it is
/// `false` only for a failure on this side of the wire — a dead socket, a
/// timeout, a URL the policy refused, a document this build could not encode.
fn classify(error: &OcpiError) -> Answer {
    if error.was_answered() {
        Answer::Refused {
            reason: error.to_string(),
        }
    } else {
        Answer::Unreached {
            why: error.to_string(),
        }
    }
}

/// The node's outbound half: an HTTP client, and this node's own party.
///
/// It holds **no peer list**. Who this node can reach is the set of platforms
/// that have registered with it, which is the node's own journalled state — and
/// a second map beside it would be a second answer to *where does a record go*,
/// out of date the moment a partner rotates a token. One inventory, two
/// audiences, which is the argument this workspace makes at every other seam
/// (D330).
#[derive(Debug)]
pub struct Dispatcher {
    client: OcpiClient,
    me: PartyRef,
}

impl Dispatcher {
    /// A dispatcher speaking as this party.
    ///
    /// # Errors
    ///
    /// [`DispatchSetupError`] when the HTTP client cannot be built or this
    /// node's own party is not one an OCPI routing header can carry.
    pub fn new(me: &PartyId, config: ClientConfig) -> Result<Self, DispatchSetupError> {
        let party = PartyRef::new(me.country_code(), me.party_id()).map_err(|source| {
            DispatchSetupError::Party {
                party: me.to_string(),
                source,
            }
        })?;
        Ok(Self {
            client: OcpiClient::with_config(config)
                .map_err(|source| DispatchSetupError::Client(source.to_string()))?,
            me: party,
        })
    }

    /// `POST` one record to the platform that registered for it.
    ///
    /// The document is sent **as this workspace wrote it** — see the module
    /// documentation — and the answer is one of three things, never two.
    pub async fn deliver(
        &self,
        to: &Registration,
        key: &emob_service::vault::DataKey,
        document: &Outbound,
    ) -> Answer {
        let Some(endpoint) = to.endpoint("cdrs", "RECEIVER") else {
            return Answer::Unreached {
                why: format!(
                    "{} published no Receiver interface for the CDRs module when it registered, \
                     so there is nowhere to push a record `[OCPI 2.3.0 §mod_cdrs]`",
                    to.peer_id
                ),
            };
        };
        let peer = match to.as_client_peer(key) {
            Ok(peer) => peer,
            Err(error) => {
                return Answer::Unreached {
                    why: format!("{} cannot be described as a peer: {error}", to.peer_id),
                };
            }
        };

        let request = OcpiRequest::new(
            http::Method::POST,
            Url::new_lenient(endpoint),
            ModuleId::Cdrs,
        )
        .routed(RoutingHeaders {
            to: peer.default_party().cloned(),
            from: self.me.clone(),
        });

        // One `with_body` per version, because the body is the document this
        // side produced and its type differs. Everything after this point is
        // the same request.
        let request = match document {
            Outbound::V2_3_0(cdr) => request.with_body(cdr.as_ref()),
            Outbound::V2_2_1(cdr) => request.with_body(cdr.as_ref()),
            // `Outbound` is `#[non_exhaustive]`: a version the kit adds and
            // this build has never encoded is **not** something to send with a
            // guessed body.
            other => {
                return Answer::Unreached {
                    why: format!(
                        "this build cannot write an OCPI {} document, so the record stays \
                         queued rather than going out in a version nothing here produced",
                        other.version()
                    ),
                };
            }
        };
        let request = match request {
            Ok(request) => request,
            Err(error) => {
                return Answer::Unreached {
                    why: format!("the record could not be encoded for the wire: {error}"),
                };
            }
        };

        match self
            .client
            .transport()
            .send_with_headers::<serde_json::Value>(&request, peer.token(), peer.quirks())
            .await
        {
            Err(error) => classify(&error),
            Ok((response, headers_in)) => {
                if response.is_success() {
                    Answer::Accepted {
                        location: header_str(&headers_in, &headers::LOCATION)
                            .map(ToOwned::to_owned),
                    }
                } else {
                    // The receiver answered. Whatever it said, it has seen the
                    // record — which is the whole difference between this arm
                    // and the one above.
                    Answer::Refused {
                        reason: response.into_result().err().map_or_else(
                            || "the receiver refused it".to_owned(),
                            |e| e.to_string(),
                        ),
                    }
                }
            }
        }
    }
}
