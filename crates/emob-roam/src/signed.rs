//! The signed meter records, as a partner's verifier needs them.
//!
//! # Why this is not part of either wire's module
//!
//! A canonical [`EvidenceRef`](emob_cdr::EvidenceRef) names its records by
//! **digest**: a CDR travels through roaming and a full OCMF blob per reading
//! makes it enormous. Both wires want the records themselves — OCPI as
//! `signed_data`, OICP as `SignedMeteringValues` — so the payloads are supplied
//! by whoever holds the evidence store, at the edge, on exactly the records
//! that are leaving.
//!
//! Two wires reading one shape is the point. An evidence store that produced a
//! different structure per protocol would be the place where a record leaving
//! over OICP and the same record leaving over OCPI could quietly differ, and
//! the whole argument for one canonical model is that they cannot.

/// One signed meter record, on its way out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedPayload {
    /// What the record is — `Start`, `End`, or the OCMF pagination it carries.
    ///
    /// Free text because the two wires want different things from it: OCPI
    /// writes it into `signed_data.nature` verbatim, and OICP maps it onto a
    /// three-valued `MeteringStatus`. Neither is a superset of the other, so
    /// the canonical form keeps what the meter said and each crossing narrows
    /// it and reports what the narrowing cost.
    pub nature: String,
    /// The record verbatim, exactly as the meter signed it.
    ///
    /// Verbatim is not a nicety. The signature covers the bytes as written, so
    /// a payload that has been re-serialised on the way through does not
    /// verify at the far end, and the partner's only conclusion is that the
    /// operator tampered with it.
    pub signed_data: String,
    /// The human-readable rendering OCPI asks to accompany it.
    ///
    /// OICP has no field for it: a `SignedMeteringValue` is the signed string
    /// and its status, and nothing else. The crossing says so rather than
    /// dropping it in silence.
    pub plain_data: String,
}
