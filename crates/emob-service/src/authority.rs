//! Who is asking, what they may do, and **whose** records they may do it to.
//!
//! # The failure this exists to prevent
//!
//! A roaming node holds several companies' sessions in one process. The worst
//! thing it can do is not lose one — it is serve party A's CDRs to party B: a
//! competitor's charging volumes, its tariffs, and its drivers' movements, out
//! of an endpoint that answered a perfectly valid token.
//!
//! That is not a deployment detail to be added later behind a reverse proxy,
//! because the proxy does not know which party owns a record. Ownership is a
//! field on the record — a [`PartyId`] — and the check belongs where that field
//! is in scope.
//!
//! # Three questions, and they stay separate
//!
//! A credential answers **who** (a constant-time token comparison), a set of
//! [`Capabilities`] answers **what** they may do, and a [`PartyScope`] answers
//! **whose records**. Collapsing any two produces the same bug: an endpoint that
//! checks the token and not the ownership, which is every roaming data leak
//! there has ever been.
//!
//! So each question a route asks is a **named method** here — [`Principal::may`],
//! [`Principal::may_reach`], [`Principal::may_act_for`] — and a route that
//! invents its own test is the bug rather than the exception.
//!
//! # Capabilities, not roles — because an agent has to hold less
//!
//! `mako`'s authorisation is built on Marktrollen (LF, NB, MSB) because `mako`
//! *is* a market participant; `hems`'s is built on capabilities because a
//! household energy manager is not. emob is a third thing: its principals are
//! **OCPI parties**, which already have a role — and a role is not enough.
//!
//! An `agentd` specialist acting for an operator must be able to hold **less**
//! than that operator: reading a CDR's findings without reading the token that
//! names the driver. `Role::Cpo` delegated to an agent is still `Role::Cpo`. A
//! capability set delegated to an agent is a subset, and
//! [`Capabilities::within`] is what makes "no wider than its delegator"
//! checkable rather than reviewable.
//!
//! Two pattern forms and no more — `emob.cdr.read`, or `emob.cdr.*` — because
//! attenuation has to be decidable by **containment**, and a richer grammar
//! (regex, negation) makes containment undecidable in general.
//!
//! # The role is still there, because OCPI asks
//!
//! A party's [`Role`] is what OCPI's `credentials` module exchanges and what
//! decides which modules a peer may even call: a CPO pushes CDRs and an eMSP
//! receives them. It is carried, and it is *not* the authorisation — a CPO's
//! token does not thereby reach another CPO's records.

use core::fmt;
use std::collections::BTreeSet;

use emob_core::{PartyId, Role};

/// A set of dotted capability patterns.
///
/// Deliberately in the shape `agentplane`'s scopes use, because that is the
/// runtime `agentd` is built on and its authority model is the one emob has to
/// compose with rather than duplicate.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct Capabilities(BTreeSet<String>);

impl Capabilities {
    /// Everything — the root of a delegation chain, held by an operator.
    #[must_use]
    pub fn root() -> Self {
        Self(BTreeSet::from(["*".to_owned()]))
    }

    /// Nothing at all.
    ///
    /// Not an absent grant. An over-attenuated chain ends here, and it permits
    /// no capability rather than every one — which is the direction a mistake
    /// has to fall in.
    #[must_use]
    pub const fn none() -> Self {
        Self(BTreeSet::new())
    }

    /// Build from patterns.
    pub fn of<I, S>(patterns: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self(patterns.into_iter().map(Into::into).collect())
    }

    /// Whether this set permits a concrete capability.
    ///
    /// `emob.cdr.read` is permitted by itself, by `emob.cdr.*`, by `emob.*` and
    /// by `*`. A pattern only ever widens at a **segment** boundary, so
    /// `emob.cdr*` does not match `emob.cdrs.read` — a prefix match on
    /// characters is how a capability grammar grows holes.
    #[must_use]
    pub fn permits(&self, capability: &str) -> bool {
        self.0.iter().any(|pattern| matches(pattern, capability))
    }

    /// Whether this set admits **everything** `other` admits.
    ///
    /// The delegator's side of the question: `operator.covers(&agent)` asks
    /// whether the operator may grant that agent set at all.
    #[must_use]
    pub fn covers(&self, other: &Self) -> bool {
        other.0.iter().all(|pattern| {
            self.0
                .iter()
                .any(|mine| mine == pattern || covers_pattern(mine, pattern))
        })
    }

    /// Whether this set is **no wider** than `other` — the same question from
    /// the delegate's side.
    ///
    /// Both directions exist, and that is deliberate. [`Self::covers`] is the
    /// natural primitive and reads left-to-right as "the operator covers the
    /// agent"; this one reads "the agent is within the operator" and matches
    /// [`PartyScope::within`] exactly. [`Principal::attenuate`] uses **this**
    /// one for both axes, because a check that reads in one direction on
    /// capabilities and the other on tenancy is a check somebody will
    /// eventually invert — in the one place in this workspace where inverting a
    /// comparison hands a credential more than it was granted.
    #[must_use]
    pub fn within(&self, other: &Self) -> bool {
        other.covers(self)
    }

    /// The patterns, in a stable order.
    #[must_use]
    pub fn patterns(&self) -> Vec<&str> {
        self.0.iter().map(String::as_str).collect()
    }

    /// Whether this set permits nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Whether a pattern admits a concrete capability.
fn matches(pattern: &str, capability: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    // `emob.cdr.*` admits `emob.cdr.read` and `emob.cdr.a.b`, and does not admit
    // `emob.cdr` itself: a grant of the children is not a grant of the parent.
    pattern.strip_suffix(".*").map_or_else(
        || pattern == capability,
        |prefix| capability.starts_with(prefix) && capability[prefix.len()..].starts_with('.'),
    )
}

/// Whether one pattern admits everything another admits.
fn covers_pattern(wider: &str, narrower: &str) -> bool {
    if wider == "*" {
        return true;
    }
    let Some(prefix) = wider.strip_suffix(".*") else {
        // A concrete pattern covers only itself, which `covers` already tested.
        return false;
    };
    let candidate = narrower.strip_suffix(".*").unwrap_or(narrower);
    candidate.starts_with(prefix) && candidate[prefix.len()..].starts_with('.')
}

/// Whose records a credential reaches.
///
/// The tenancy is a field on the principal rather than a layer above it,
/// because "which party owns this record" is a field on the record and the two
/// have to be compared somewhere that holds both.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum PartyScope {
    /// Exactly these parties.
    These(BTreeSet<String>),
    /// Every party this deployment holds.
    ///
    /// A single-tenant deployment writes this and has **said** so. It is a
    /// separate variant rather than an empty set for that reason: an
    /// accidentally empty configuration must not mean "everything".
    Every,
}

impl PartyScope {
    /// A scope over one party.
    #[must_use]
    pub fn just(party: &PartyId) -> Self {
        Self::These(BTreeSet::from([party.to_string()]))
    }

    /// A scope over several.
    pub fn over<'a, I>(parties: I) -> Self
    where
        I: IntoIterator<Item = &'a PartyId>,
    {
        Self::These(parties.into_iter().map(ToString::to_string).collect())
    }

    /// Whether this scope reaches a party's records.
    #[must_use]
    pub fn reaches(&self, party: &PartyId) -> bool {
        match self {
            Self::Every => true,
            Self::These(parties) => parties.contains(&party.to_string()),
        }
    }

    /// Whether this scope is **no wider** than `other` — the delegation test,
    /// on the tenancy axis.
    #[must_use]
    pub fn within(&self, other: &Self) -> bool {
        match (self, other) {
            (_, Self::Every) => true,
            (Self::Every, Self::These(_)) => false,
            (Self::These(mine), Self::These(theirs)) => mine.is_subset(theirs),
        }
    }
}

/// A credential's bearer token, compared in constant time.
///
/// # Why the type exists rather than a `String`
///
/// A token compared with `==` leaks where two differ, one byte of timing at a
/// time. That is a real attack on a long-lived OCPI `CREDENTIALS_TOKEN_C`, which
/// a peer holds for months. It also refuses to print itself, because the second
/// way a token escapes is a `Debug` line in an error report.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(Vec<u8>);

impl Token {
    /// A token from its wire form.
    #[must_use]
    pub fn new(value: &str) -> Self {
        Self(value.as_bytes().to_vec())
    }

    /// Whether an offered token is this one.
    ///
    /// Constant time in the length of the shorter of the two, and it compares
    /// the lengths without branching on the contents.
    #[must_use]
    pub fn verify(&self, offered: &str) -> bool {
        use subtle::ConstantTimeEq as _;
        let offered = offered.as_bytes();
        // `ct_eq` on unequal lengths is a `false` this cannot avoid leaking —
        // and a token's *length* is not the secret.
        if offered.len() != self.0.len() {
            return false;
        }
        bool::from(self.0.ct_eq(offered))
    }
}

impl fmt::Debug for Token {
    /// Never the value.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(…)")
    }
}

/// Who is asking.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Principal {
    /// The party this credential belongs to.
    pub party: PartyId,
    /// The role it holds, as OCPI's `credentials` exchange stated it.
    pub role: Role,
    /// What it may do.
    pub capabilities: Capabilities,
    /// Whose records it reaches.
    pub scope: PartyScope,
}

impl Principal {
    /// A party acting for itself, with everything.
    ///
    /// The shape an operator's own daemon holds. A peer never gets this.
    #[must_use]
    pub fn operator(party: PartyId, role: Role) -> Self {
        Self {
            scope: PartyScope::just(&party),
            party,
            role,
            capabilities: Capabilities::root(),
        }
    }

    /// A roaming peer: its own party, its own role, and reaching **only** its
    /// own records.
    ///
    /// The default that makes the leak unrepresentable. A deployment that wants
    /// a hub to reach more says so by building the scope itself.
    #[must_use]
    pub fn peer(party: PartyId, role: Role, capabilities: Capabilities) -> Self {
        Self {
            scope: PartyScope::just(&party),
            party,
            role,
            capabilities,
        }
    }

    /// Whether this principal may perform a capability **at all**.
    #[must_use]
    pub fn may(&self, capability: &str) -> bool {
        self.capabilities.permits(capability)
    }

    /// Whether this principal may reach a record owned by a party.
    #[must_use]
    pub fn may_reach(&self, owner: &PartyId) -> bool {
        self.scope.reaches(owner)
    }

    /// Both questions at once — what a route actually asks.
    ///
    /// The two are separate methods *and* a joined one on purpose: a route that
    /// asks only the first is the leak, and a route that has to remember to ask
    /// both is a route that will forget.
    #[must_use]
    pub fn may_act_for(&self, capability: &str, owner: &PartyId) -> bool {
        self.may(capability) && self.may_reach(owner)
    }

    /// Attenuate: derive a principal that is **no wider** than this one.
    ///
    /// The operation an agent's delegation needs. Returns `None` when the
    /// request would widen either axis, rather than silently clamping — a
    /// delegation that quietly grants less than it was asked for is a bug that
    /// surfaces as a permission error somewhere else entirely.
    #[must_use]
    pub fn attenuate(&self, capabilities: Capabilities, scope: PartyScope) -> Option<Self> {
        // Both axes read the same direction — "the delegate is within the
        // delegator" — because a check that read one way on capabilities and
        // the other on tenancy is one somebody will eventually invert, and this
        // is the one place in the workspace where inverting a comparison hands
        // a credential more than it was granted.
        if !capabilities.within(&self.capabilities) || !scope.within(&self.scope) {
            return None;
        }
        Some(Self {
            party: self.party.clone(),
            role: self.role,
            capabilities,
            scope,
        })
    }
}

/// The capabilities this workspace names.
///
/// Constants rather than string literals at each call site, for the reason
/// `mako-events` gives for its `CloudEvents` catalogue: a rename is a one-line
/// change and a typo is a compile error rather than a route that silently
/// permits nothing — or, worse, one that permits everything because the
/// misspelling was in the *grant*.
pub mod caps {
    /// Read a charge detail record.
    pub const CDR_READ: &str = "emob.cdr.read";
    /// Accept a charge detail record into the ledger.
    pub const CDR_WRITE: &str = "emob.cdr.write";
    /// Read a tariff.
    pub const TARIFF_READ: &str = "emob.tariff.read";
    /// Publish a tariff version.
    pub const TARIFF_WRITE: &str = "emob.tariff.write";
    /// Read locations and their availability.
    pub const LOCATION_READ: &str = "emob.location.read";
    /// Publish locations.
    pub const LOCATION_WRITE: &str = "emob.location.write";
    /// Read a session in progress.
    pub const SESSION_READ: &str = "emob.session.read";
    /// Ask for an authorisation decision.
    pub const TOKEN_AUTHORIZE: &str = "emob.token.authorize";
    /// Read the signed meter records behind a record.
    ///
    /// Separate from [`CDR_READ`] because it is what `[MessEG §33]` gives a
    /// customer and what an agent must be able to hold *without* holding the
    /// token that names the driver.
    pub const EVIDENCE_READ: &str = "emob.evidence.read";
    /// Read an invoice.
    pub const INVOICE_READ: &str = "emob.invoice.read";
    /// Issue an invoice or a collection.
    pub const INVOICE_WRITE: &str = "emob.invoice.write";
}

#[cfg(test)]
mod tests {
    use super::*;

    fn party(id: &str) -> PartyId {
        PartyId::new("DE", id).unwrap()
    }

    #[test]
    fn a_pattern_widens_at_a_segment_boundary_and_nowhere_else() {
        let caps = Capabilities::of(["emob.cdr.*"]);
        assert!(caps.permits("emob.cdr.read"));
        assert!(caps.permits("emob.cdr.a.b"));
        // A prefix match on characters is how a capability grammar grows holes.
        assert!(!caps.permits("emob.cdrs.read"));
        // …and a grant of the children is not a grant of the parent.
        assert!(!caps.permits("emob.cdr"));

        assert!(Capabilities::root().permits("anything.at.all"));
        assert!(!Capabilities::none().permits("emob.cdr.read"));
    }

    #[test]
    fn an_empty_set_permits_nothing_rather_than_everything() {
        // The direction a misconfiguration has to fall in: a deployment where
        // somebody forgot the grants is exactly the one nobody would notice.
        let nothing = Capabilities::none();
        assert!(nothing.is_empty());
        for capability in [caps::CDR_READ, caps::INVOICE_WRITE, "*"] {
            assert!(!nothing.permits(capability), "{capability}");
        }
    }

    #[test]
    fn containment_is_what_makes_a_delegation_checkable() {
        let operator = Capabilities::root();
        let narrower = Capabilities::of(["emob.cdr.read", "emob.evidence.read"]);
        assert!(operator.covers(&narrower));
        assert!(!narrower.covers(&operator));

        let family = Capabilities::of(["emob.cdr.*"]);
        assert!(family.covers(&Capabilities::of(["emob.cdr.read"])));
        assert!(family.covers(&Capabilities::of(["emob.cdr.a.*"])));
        assert!(!family.covers(&Capabilities::of(["emob.tariff.read"])));
        // A set covers itself, which is what makes re-delegation idempotent.
        assert!(family.covers(&family));
    }

    #[test]
    fn the_two_directions_are_the_same_question_and_never_disagree() {
        // `attenuate` reads both axes as "the delegate is within the delegator".
        // A pair that disagreed would be a security check somebody could invert
        // by reading the wrong sibling's documentation.
        let sets = [
            Capabilities::root(),
            Capabilities::none(),
            Capabilities::of(["emob.cdr.*"]),
            Capabilities::of(["emob.cdr.read"]),
            Capabilities::of(["emob.cdr.read", "emob.tariff.read"]),
        ];
        for a in &sets {
            for b in &sets {
                assert_eq!(
                    a.within(b),
                    b.covers(a),
                    "`within` and `covers` are one question from two sides"
                );
            }
        }
        // …and the direction is the one the names claim.
        assert!(Capabilities::of(["emob.cdr.read"]).within(&Capabilities::root()));
        assert!(!Capabilities::root().within(&Capabilities::of(["emob.cdr.read"])));
    }

    #[test]
    fn a_peer_reaches_its_own_records_and_no_others() {
        // The worst thing a roaming node can do, made unrepresentable by the
        // constructor rather than by a check somebody has to remember.
        let theirs = party("XYZ");
        let peer = Principal::peer(
            theirs.clone(),
            Role::Emsp,
            Capabilities::of([caps::CDR_READ]),
        );

        assert!(peer.may_act_for(caps::CDR_READ, &theirs));
        assert!(!peer.may_act_for(caps::CDR_READ, &party("ABC")));
        // …and holding the capability is not holding the record.
        assert!(peer.may(caps::CDR_READ));
        assert!(!peer.may_reach(&party("ABC")));
        // …nor is reaching the record holding the capability.
        assert!(!peer.may_act_for(caps::CDR_WRITE, &theirs));
    }

    #[test]
    fn an_agent_holds_less_than_the_operator_it_acts_for() {
        // The property a closed set of roles cannot express: `Role::Cpo`
        // delegated is still `Role::Cpo`, and a capability set delegated is a
        // subset.
        let operator = Principal::operator(party("ABC"), Role::Cpo);
        let agent = operator
            .attenuate(
                Capabilities::of([caps::CDR_READ, caps::EVIDENCE_READ]),
                PartyScope::just(&party("ABC")),
            )
            .expect("narrower on both axes");

        assert_eq!(agent.role, Role::Cpo, "the role does not attenuate");
        assert!(agent.may(caps::CDR_READ));
        assert!(!agent.may(caps::CDR_WRITE));
        assert!(!agent.may(caps::TOKEN_AUTHORIZE));

        // …and it cannot widen either axis.
        assert!(
            agent
                .attenuate(Capabilities::root(), PartyScope::just(&party("ABC")))
                .is_none(),
            "capabilities may not widen"
        );
        assert!(
            agent
                .attenuate(Capabilities::of([caps::CDR_READ]), PartyScope::Every)
                .is_none(),
            "tenancy may not widen"
        );
    }

    #[test]
    fn every_is_a_thing_a_deployment_says_rather_than_a_thing_it_forgets() {
        // An accidentally empty configuration must not mean "everything".
        let empty = PartyScope::These(BTreeSet::new());
        assert!(!empty.reaches(&party("ABC")));
        assert!(PartyScope::Every.reaches(&party("ABC")));

        assert!(empty.within(&PartyScope::Every));
        assert!(!PartyScope::Every.within(&PartyScope::just(&party("ABC"))));
        assert!(
            PartyScope::just(&party("ABC"))
                .within(&PartyScope::over([&party("ABC"), &party("XYZ")]))
        );
    }

    #[test]
    fn a_token_neither_prints_itself_nor_compares_in_variable_time() {
        let token = Token::new("s3cr3t-credentials-token-c");
        assert!(token.verify("s3cr3t-credentials-token-c"));
        assert!(!token.verify("s3cr3t-credentials-token-d"));
        assert!(!token.verify("s3cr3t"));
        assert!(!token.verify(""));
        // The second way a token escapes is a `Debug` line in an error report.
        assert_eq!(format!("{token:?}"), "Token(…)");
    }

    #[test]
    fn the_role_is_carried_and_is_not_the_authorisation() {
        // A CPO's token does not thereby reach another CPO's records — which is
        // exactly what a role-only model would permit.
        let a = Principal::peer(party("AAA"), Role::Cpo, Capabilities::root());
        let b = party("BBB");
        assert_eq!(a.role, Role::Cpo);
        assert_eq!(Role::Cpo.as_str(), "CPO");
        assert!(a.may(caps::CDR_READ));
        assert!(!a.may_reach(&b));
    }
}
