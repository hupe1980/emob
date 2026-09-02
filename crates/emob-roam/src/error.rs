//! What the roaming edge refuses, and why.
//!
//! The line this crate draws is between a crossing that **costs** something —
//! a rounded quantity, a collapsed distinction, a fact with no field to live
//! in — and one that would produce a document saying something untrue. The
//! first is a [`Note`](crate::Note) that travels with the value. The second is
//! an error, because a partner cannot act on a note attached to a number that
//! is simply wrong.

use emob_core::{Direction, PartyId};

/// A crossing that cannot be made.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RoamError {
    /// The record has not been priced, and OCPI has nowhere to say so.
    ///
    /// `total_cost` is a required member of an OCPI CDR, and the specification
    /// gives the obvious placeholder its own meaning: *"A `total_cost` of 0.00
    /// means free of charge"* `[OCPI 2.3.0 §mod_cdrs_cdr_object]`. So a
    /// record that has not been rated cannot be expressed — sending it as zero
    /// does not defer the question, it answers it, in the partner's favour and
    /// permanently.
    #[error(
        "this CDR has not been rated, and OCPI's `total_cost` is required with 0.00 meaning \
         free of charge [OCPI 2.3.0 §mod_cdrs_cdr_object] — rate it before it crosses, or the \
         partner settles it at nothing"
    )]
    NotRated,

    /// The energy went the other way, and an OCPI CDR cannot say so.
    ///
    /// `ENERGY_EXPORT` exists in `CdrDimensionType` and the specification
    /// marks it *Session Only* — *"Some of these values are not useful for
    /// CDRs, and SHALL therefore only be used in Sessions"*
    /// `[OCPI 2.3.0 §mod_cdrs_cdrdimensiontype_enum]`. What is left is
    /// `ENERGY`, and a CDR's `total_energy` carries no sign convention at all.
    ///
    /// So a V2G discharge crossing to OCPI arrives at the eMSP as an ordinary
    /// draw, and the settlement runs backwards: the provider pays the operator
    /// for energy the **driver** supplied. Import and export never net, which
    /// is enforced against the OBIS code the meter signed one layer down — and
    /// a translation that quietly re-signs it as import is that invariant
    /// being broken at the last possible moment, by us.
    #[error(
        "an export CDR cannot be expressed in OCPI: ENERGY_EXPORT is Session-only \
         [OCPI 2.3.0 §mod_cdrs_cdrdimensiontype_enum] and `total_energy` has no sign, so the \
         partner would read {energy} as a draw and pay the wrong way round — settle a V2G \
         discharge on a contract that has terms for it"
    )]
    ExportNotExpressible {
        /// The energy the record measured, in the direction it measured it.
        energy: String,
        /// The direction the signed register stated.
        direction: Direction,
    },

    /// The contract identifier does not check out.
    ///
    /// The last digit of a contract id exists to catch a transcription error,
    /// and this is the last moment anybody will look: once the CDR is at the
    /// eMSP, an id that has lost a character still parses, still routes, and
    /// bills the session to somebody else's contract.
    #[error(
        "the contract id `{id}` fails its own check digit, and it is what routes the money — \
         a transcribed id bills this session to a contract nobody holds"
    )]
    ContractCheckDigit {
        /// The identifier as it was presented.
        id: String,
    },

    /// A value OCPI bounds is longer than the bound.
    ///
    /// Truncating would produce a document that validates and names a
    /// different object.
    #[error(
        "{field} is {len} characters and OCPI bounds it at {max} \
         [OCPI 2.3.0 §mod_cdrs_cdr_object] — truncating it would name a different object"
    )]
    TooLong {
        /// Which member.
        field: &'static str,
        /// How long the value is.
        len: usize,
        /// What the specification allows.
        max: usize,
    },

    /// The record has no charging periods.
    ///
    /// OCPI gives `charging_periods` cardinality `+`. A record with none is
    /// a total with nothing behind it, which is precisely what a receiving
    /// party cannot check.
    #[error(
        "a CDR crossing to OCPI needs at least one charging period \
         [OCPI 2.3.0 §mod_cdrs_cdr_object]: a total with nothing behind it cannot be re-rated"
    )]
    NoPeriods,

    /// The periods do not sum to the total.
    ///
    /// Checked on the way **in**, where the record was built by somebody
    /// else's code. On the way out it is an invariant of [`emob_cdr::Cdr`].
    #[error(
        "the charging periods sum to {sum} and `total_energy` says {total}: one of the two is \
         wrong and this side cannot say which"
    )]
    DoesNotConserve {
        /// What the periods add up to.
        sum: String,
        /// What the record claims.
        total: String,
    },

    /// A tariff element carries a restriction this build cannot evaluate, and
    /// the crossing would drop it.
    ///
    /// Dropping a restriction on the way out is not a loss, it is an
    /// **enlargement**: the element then matches at the partner in conditions
    /// nobody checked, and the price applies where the CPO never meant it to.
    /// The rating engine already refuses to match such an element on this
    /// side; publishing it stripped would make the partner's answer differ
    /// from ours on the same session.
    #[error(
        "tariff element {element} restricts on `{restriction}`, which this build cannot \
         evaluate — publishing the element without it makes the element match at the partner \
         in conditions nobody checked, which is a wider price than the tariff states"
    )]
    UnevaluableRestriction {
        /// Which element, by index.
        element: usize,
        /// The restriction, as the partner spelled it.
        restriction: String,
    },

    /// A gross tariff's price bound has no pre-tax figure to publish.
    ///
    /// OCPI requires `before_taxes` on a `min_price`/`max_price` and means it
    /// literally: the bound constrains the session's cost **before taxes**
    /// `[OCPI 2.3.0 §Tariff]`. Writing the gross amount into it publishes a
    /// bound the partner will enforce a VAT rate too high, against the driver,
    /// from a document this operator signed off — so the figure is converted at
    /// the rate the tariff's components carry, and where they carry more than
    /// one there is no such rate and no honest figure.
    #[error(
        "this tariff's prices are gross and its components carry more than one VAT rate, so \
         `{field}` has no pre-tax figure: OCPI's bound constrains the cost before taxes, and \
         publishing the gross amount there is a bound a partner enforces a VAT rate too high. \
         State the bound on a tariff with one rate, or price the tariff net"
    )]
    NoRateForPriceLimit {
        /// Which bound — `min_price` or `max_price`.
        field: String,
    },

    /// A field of a partner's document is one this side's types refuse.
    ///
    /// A refusal rather than a repair. Every repair is a number invented on
    /// behalf of somebody who will be invoiced for it, and a canonical record
    /// built out of one is a record that reconciles against nothing.
    #[error("`{field}` cannot be read into the canonical model: {detail}")]
    UnreadableField {
        /// Which field, by its OCPI name.
        field: String,
        /// Why this side's type refused it.
        detail: String,
    },

    /// Nothing in the registry can receive this record.
    #[error(
        "no partner routes a contract issued by {issuer}: a CDR sent to the wrong party is \
         settlement money leaving for a company that never had the driver"
    )]
    NoRoute {
        /// The provider the contract identifier names.
        issuer: String,
    },

    /// The partner requires signed metering data and this record has none
    /// attached.
    ///
    /// Not every partner does. One settling German sessions must, because
    /// `[MessEG §33]` lets a measured value be billed only where the affected
    /// party can check it, and a CDR that arrives without the signed records
    /// is one the eMSP cannot put in front of a driver who disputes it.
    #[error(
        "{partner} settles on signed metering data and this CDR carries none — under \
         [MessEG §33] a value the customer cannot check is not one they can be billed for"
    )]
    SignedDataRequired {
        /// The partner that asked for it.
        partner: PartyId,
    },

    /// A string OCPI constrains carries a character it does not allow.
    #[error("{field}: {source}")]
    InvalidString {
        /// Which member.
        field: &'static str,
        /// What the kit said about it.
        #[source]
        source: ocpi_kit::types::InvalidString,
    },

    /// The record names a party this side does not recognise.
    #[error("{0} is not a party this node knows")]
    UnknownParty(PartyId),

    /// A connector this build cannot name in OCPI's vocabulary.
    ///
    /// Publishing the nearest plug instead is how a driver is routed to a
    /// socket that is not there, which is the one failure location data exists
    /// to prevent. The two vocabularies were written from the same IEC
    /// standard, so this can only fire on a connector the register has learned
    /// and the crossing has not.
    #[error(
        "the register calls this connector `{kind}` and OCPI has no name for it \
         [OCPI 2.3.0 §mod_locations_connectortype_enum] — publishing the nearest plug routes \
         drivers to a socket that is not there"
    )]
    UnmappedConnector {
        /// The register's own spelling.
        kind: String,
    },
}
