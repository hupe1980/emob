//! Why a Plug & Charge artefact does not establish what it claims to.
//!
//! Every variant names the certificate it is about and, where there is one, the
//! **remedy** — because the four things that go wrong here have four different
//! fixes and only one of them involves the driver. A pool nobody connected is a
//! deployment; an anchor outside its window is a trust decision that expired; a
//! chain that does not validate is a certificate; and a revocation status
//! nobody holds is a responder that was never wired up.

use iso15118::pnc::pki::ChainError;

use crate::{PoolId, Standing};

/// Why a chain does not establish a contract, a vehicle, or anything else.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PncError {
    /// The chain was offered against a pool nobody connected.
    ///
    /// Never answered by trying another pool's anchors. See
    /// [`crate::pool`] for what that would cost.
    #[error(
        "no pool {pool} is connected to this platform, and a certificate is never validated \
         against another one's anchors [V2G2-925]: connected pools are {connected:?}"
    )]
    UnknownPool {
        /// Which pool the caller named.
        pool: PoolId,
        /// The ones that are connected.
        connected: Vec<String>,
    },

    /// The pool is connected and this side trusted none of its roots then.
    ///
    /// A configuration fault rather than a bad certificate, and told apart
    /// because the remedy is to load a root rather than to ask a driver for
    /// another card.
    #[error(
        "pool {pool} holds {held} root(s) and this side trusted none of them at {at}: a trust \
         decision that has expired, not a certificate that has"
    )]
    NoTrustedAnchor {
        /// Which pool.
        pool: PoolId,
        /// The instant the question was asked at.
        at: time::OffsetDateTime,
        /// How many roots the pool holds, inside their windows or not.
        held: usize,
    },

    /// An anchor window covers no instant.
    ///
    /// The `KeyRegistry` rule, one PKI out: a window nobody can be inside reads
    /// exactly like an anchor nobody configured, and the two have different
    /// fixes.
    #[error(
        "a trust anchor's window runs from {from} to {until}, which contains no instant: an \
         anchor nobody can use reads exactly like an anchor nobody configured"
    )]
    EmptyAnchorWindow {
        /// The first instant.
        from: time::OffsetDateTime,
        /// The one that is not after it.
        until: time::OffsetDateTime,
    },

    /// The path validation refused it.
    ///
    /// The field is `refusal` rather than `source` on purpose: `ChainError` is
    /// a `no_std` enumeration and does not implement `std::error::Error`, so
    /// naming it `source` would make `thiserror` try to chain something that
    /// cannot be chained. Its own `Debug` is the whole of what it has to say.
    #[error("the chain offered to pool {pool} does not validate: {refusal:?}")]
    Chain {
        /// Which pool it was offered against.
        pool: PoolId,
        /// What ISO 15118's own Annex F path validation said.
        refusal: ChainError,
    },

    /// A certificate on the path is not in good standing.
    ///
    /// Over the **whole** path, not only the leaf —
    /// `[OPCP §Provisioning Certificate Pool]`'s third control step is *"each
    /// delivered certificate (leaf and chain)"*, and a sub-CA is every driver
    /// under it.
    #[error("{subject} is {standing}")]
    Revoked {
        /// Whose certificate.
        subject: String,
        /// What this side could say about it.
        ///
        /// Boxed: it is much the largest thing this enumeration carries, and
        /// every other variant would otherwise be sized for it.
        standing: Box<Standing>,
    },

    /// The leaf's Common Name is not a contract identifier.
    ///
    /// `[V2G2-108]` and Annex F Table F.4 both make it the EMAID. A leaf whose
    /// Common Name is something else is a certificate this platform cannot
    /// attribute a session to — and the answer is to refuse it rather than to
    /// take an identifier from wherever else the caller had one.
    #[error(
        "the leaf's Common Name {common_name:?} is not a contract identifier, and \
         [V2G2-108] makes it the EMAID: {detail}"
    )]
    CommonNameIsNotAnEmaid {
        /// What the certificate carried, where it carried anything.
        common_name: Option<String>,
        /// What reading it as an eMAID said.
        detail: String,
    },

    /// The leaf's Common Name is not a provisioning certificate identifier.
    ///
    /// `[V2G2-933]` makes it the `CertID`, which `[OPCP §Handling of IDs]`
    /// spells PCID.
    #[error(
        "the leaf's Common Name {common_name:?} is not a PCID, and [V2G2-933] makes it the \
         CertID: {detail}"
    )]
    CommonNameIsNotAPcid {
        /// What the certificate carried, where it carried anything.
        common_name: Option<String>,
        /// What reading it as a PCID said.
        detail: String,
    },

    /// A valid contract certificate, belonging to another Mobility Operator.
    ///
    /// `[OPCP §Contract Certificate Pool]` keeps each MO's contracts separated
    /// and *"the defined access rules prevent unauthorized requests to others
    /// MOs contracts"*. The identifier's own provider section is what says
    /// which MO a contract belongs to, so a caller that knows which provider it
    /// asked about has the comparison made here rather than at whichever of its
    /// call sites remembered to.
    #[error(
        "{emaid} is a contract of provider {provider} and this installation is for {expected}: a \
         chain that validates is not a chain that belongs to you \
         [OPCP §Contract Certificate Pool]"
    )]
    NotThisProvider {
        /// The identifier off the certificate.
        emaid: String,
        /// The provider its own grammar names.
        provider: String,
        /// The provider the caller asked about.
        expected: String,
    },

    /// A contract was installed against a vehicle whose WMI is not the OEM's.
    ///
    /// The Provisioning Certificate Pool's **first** control step: *"Verifies
    /// the PCIDs world manufacturer identifier (WMI) against the OEM accounts
    /// authorized WMI list"* `[OPCP §Provisioning Certificate Pool]`. It is
    /// first for a reason — every later step is arithmetic over a certificate
    /// that may belong to somebody else's production line.
    #[error(
        "{pcid} names manufacturer {wmi}, which is not one this account may publish for \
         ({authorized:?}) [OPCP §Provisioning Certificate Pool]"
    )]
    NotThisManufacturer {
        /// The vehicle identifier.
        pcid: String,
        /// The manufacturer its first three characters name.
        wmi: String,
        /// The manufacturers the account is enrolled for.
        authorized: Vec<String>,
    },
}
