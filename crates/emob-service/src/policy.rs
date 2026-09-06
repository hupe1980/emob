//! Policies an **operator writes**, evaluated against the entity types
//! [`crate::authority`] already has.
//!
//! Behind the `policy` feature and **off by default**: it costs a hundred and
//! eight crates, and only the roaming node needs one.
//!
//! # Why a policy engine at all, and why only now
//!
//! The typed authority model answers three questions — who, what, and whose —
//! and it answers them from values a deployment hands the daemon. That is
//! already runtime data: [`crate::authority::Principal`] is built from
//! configuration, not from a `const`. So a policy engine buys nothing until
//! there is a question the typed model has **no vocabulary for**, and until the
//! answer has to change without a deploy.
//!
//! `roamd` is where both became true. A partner is onboarded while the node
//! runs, and what an operator wants to say about one is not a set of capability
//! patterns:
//!
//! > this hub may push charge detail records **until the trial ends**
//!
//! > this eMSP may read our records, but not the evidence behind them
//!
//! > this partner may read tariffs and nothing else, from today
//!
//! The first is a condition on an instant, the third is a grant with a start.
//! Neither is expressible as a subset of dotted strings, and inventing a
//! grammar for them is how a capability model becomes a policy language nobody
//! specified. Cedar is that language, specified, with a validator.
//!
//! # A policy may grant, and it may not widen a delegation
//!
//! This is the whole of the composition, and getting it the other way round
//! would undo two of this workspace's structural claims at once.
//!
//! [`Gate::verdict`] asks the typed model **first**, and a `no` there is final.
//! So:
//!
//! - a roaming peer still reaches only its own records, whatever any policy
//!   says — the leak [`crate::authority`] exists to prevent stays
//!   unrepresentable rather than becoming a line of text somebody can get
//!   wrong;
//! - and an agent's principal, derived through
//!   [`Principal::attenuate`](crate::authority::Principal::attenuate), cannot be
//!   widened by a policy file. `agentd`'s claim is that **no agent can hold a
//!   capability that writes**, and it is structural precisely because the
//!   principal is derived rather than configured. A policy engine consulted
//!   *instead of* the typed model would make that claim depend on what an
//!   operator wrote this morning.
//!
//! What the policies decide is everything inside that ceiling: which actions,
//! under which conditions.
//!
//! # Default deny, twice
//!
//! Cedar is default-deny by construction: a request no `permit` matches is
//! denied. That is the same direction [`Capabilities::none`] falls in, and it is
//! why an operator who loads no policies gets nothing rather than everything.
//!
//! [`Capabilities::none`]: crate::authority::Capabilities::none
//!
//! # …and what cannot be evaluated is not assumed open
//!
//! Cedar's own answer to a policy that errors during evaluation is to **skip
//! it** and carry on. That is right for an engine serving a large policy store,
//! and it is wrong here: a `forbid` that could not be evaluated is a refusal
//! that silently did not happen. This workspace already answered the same
//! question in `emob-tariff`, where an element carrying a restriction the build
//! cannot judge is never applied — *"an element whose conditions cannot be
//! checked is one whose prices must not be applied"*.
//!
//! So an evaluation error here is a **denial**, and the reason travels on the
//! [`Verdict`] rather than being swallowed.
//!
//! # The schema is built from `caps`, not written beside it
//!
//! One action per capability, one entity type per resource family, both derived
//! from [`caps::ALL`] by splitting the capability
//! on its own dots. A hand-written schema is a second list of the capabilities
//! this workspace names, and two spellings of one list is the drift every other
//! seam here is built to prevent — with the sharp end pointing at a policy that
//! validates against an action nobody can ever be granted.

use std::collections::BTreeSet;
use std::str::FromStr as _;

use cedar_policy::{
    Authorizer, Context, Decision, Entities, Entity, EntityId, EntityTypeName, EntityUid,
    PolicySet, Request, RestrictedExpression, Schema, ValidationMode, Validator,
};
use emob_core::PartyId;

use crate::authority::{Principal, caps};

/// The Cedar namespace every entity type and action lives in.
const NAMESPACE: &str = "emob";

/// What can be wrong with a policy set before it ever decides anything.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PolicyError {
    /// The schema this crate builds is not a schema.
    ///
    /// Unreachable from any input a caller controls — the schema is derived from
    /// [`caps::ALL`] — and it is an error rather than a panic because a
    /// capability constant with a character Cedar's grammar refuses would
    /// otherwise take a daemon down at start-up with a backtrace instead of a
    /// sentence.
    #[error("the schema built from the capability list is not one Cedar accepts: {reason}")]
    Schema {
        /// What Cedar said.
        reason: String,
    },

    /// The policy text does not parse.
    #[error("the policy text does not parse: {reason}")]
    Unparsable {
        /// What Cedar said.
        reason: String,
    },

    /// The policies parse and do not hold against the schema.
    ///
    /// The half a text file cannot have without one: an action nobody can be
    /// granted, an attribute no entity has, a resource type an action does not
    /// apply to. Every one of them is a policy that would simply never match,
    /// which is the worst way for an authorisation rule to be wrong — it fails
    /// open on a `forbid` and silently on a `permit`.
    #[error(
        "the policies do not hold against the schema this workspace's capabilities make: \
         {reasons:?}. A policy that names an action nobody can be granted is one that never \
         matches, which is a refusal that does not happen"
    )]
    Invalid {
        /// Every validation finding, in the order Cedar reported them.
        reasons: Vec<String>,
    },
}

/// A validated policy set, and the schema it was validated against.
///
/// There is no way to build one that has not been checked: the schema is
/// derived, the validation runs at load, and a set that does not hold is an
/// error rather than a warning. A policy engine whose policies are checked
/// *later* is one whose first bad deploy is a production incident.
#[derive(Debug, Clone)]
pub struct Policies {
    schema: Schema,
    set: PolicySet,
    count: usize,
}

impl Policies {
    /// Parse and validate operator-authored policy text.
    ///
    /// # Errors
    ///
    /// [`PolicyError::Unparsable`] for text Cedar's grammar refuses, and
    /// [`PolicyError::Invalid`] for a policy that parses and does not hold
    /// against the schema — an action nobody can be granted, an attribute no
    /// entity has, a resource an action does not apply to.
    ///
    /// ```
    /// # use emob_service::policy::Policies;
    /// let policies = Policies::load(
    ///     r#"permit (principal, action == emob::Action::"emob.cdr.read", resource)
    ///        when { resource.owner == principal };"#,
    /// )?;
    /// assert_eq!(policies.len(), 1);
    ///
    /// // …and a policy naming an action nobody can be granted is refused at
    /// // load rather than never matching.
    /// assert!(Policies::load(
    ///     r#"permit (principal, action == emob::Action::"emob.cdr.raed", resource);"#
    /// )
    /// .is_err());
    /// # Ok::<(), emob_service::policy::PolicyError>(())
    /// ```
    pub fn load(text: &str) -> Result<Self, PolicyError> {
        let schema = Schema::from_str(&schema_text()).map_err(|error| PolicyError::Schema {
            reason: error.to_string(),
        })?;
        let set = PolicySet::from_str(text).map_err(|error| PolicyError::Unparsable {
            reason: error.to_string(),
        })?;

        // Strict rather than permissive: the permissive mode exists for policy
        // stores that predate their schema, and this schema is generated from
        // the capability list in the same build.
        let result = Validator::new(schema.clone()).validate(&set, ValidationMode::Strict);
        // …and a **warning** is an error here, which is not the usual reading
        // and is the right one for what Cedar's warnings actually are.
        //
        // `ImpossiblePolicy` is the typechecker saying a condition can never be
        // true — a rule that never runs, which is a `permit` nobody notices and
        // a `forbid` that fails open. It is the same sentence this module opens
        // with, reached by the validator rather than by the evaluator.
        //
        // The rest are about **confusable identifiers**: a mixed-script string,
        // a bidirectional control character, a homoglyph. In a policy set whose
        // whole job is to keep one company's records away from another's, a
        // party id that merely *looks* like `NL*TNM` is the leak this module
        // exists to prevent, spelled in Unicode. Neither is a thing to log.
        let reasons: Vec<String> = result
            .validation_errors()
            .map(ToString::to_string)
            .chain(result.validation_warnings().map(ToString::to_string))
            .collect();
        if !reasons.is_empty() {
            return Err(PolicyError::Invalid { reasons });
        }

        let count = set.policies().count();
        Ok(Self { schema, set, count })
    }

    /// How many policies were loaded.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.count
    }

    /// Whether the set is empty — which permits nothing, and is a thing an
    /// operator can do by accident.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }
}

/// The Cedar schema this workspace's capabilities make.
///
/// One entity type per resource family and one action per capability, both read
/// out of [`caps::ALL`] by splitting `emob.<family>.<verb>` on its own dots. See
/// the module documentation for why it is derived rather than written.
///
/// Public because an operator authoring policies needs it — a schema they cannot
/// read is a schema they will write against by guessing.
#[must_use]
pub fn schema_text() -> String {
    use std::fmt::Write as _;

    let families: BTreeSet<&str> = caps::ALL.iter().filter_map(|cap| family_of(cap)).collect();

    let mut out = format!("namespace {NAMESPACE} {{\n");
    // The principal. `role` is what OCPI's `credentials` exchange stated, and it
    // is carried rather than trusted: a CPO's token does not thereby reach
    // another CPO's records, which is the tenancy axis and not this one.
    // **Optional**, and that is the point. A request names two parties — the one
    // asking and the one that owns the record — and this node knows the role of
    // exactly one of them: the asker's, as OCPI's `credentials` exchange stated
    // it. Filling the other in with an empty string would make
    // `resource.owner.role == "CPO"` quietly false forever, which is a policy
    // that never matches wearing the clothes of one that does.
    //
    // Absent instead — and the strict validator then refuses an *unguarded*
    // read at **load**, so the trap is unreachable rather than reported. A
    // policy that wants the role writes `resource.owner has role &&
    // resource.owner.role == "CPO"`, and the guard is `false` for a party whose
    // role this node has no business claiming.
    out.push_str("  entity Party = { \"role\"?: String };\n");
    for family in &families {
        // `owner` is the same word the typed model uses — `may_reach(owner)` —
        // because one fact with two names is how a policy comes to mean
        // something other than the check beside it.
        let _ = writeln!(
            out,
            "  entity {} = {{ \"owner\": Party }};",
            type_of(family)
        );
    }
    for capability in caps::ALL {
        let Some(family) = family_of(capability) else {
            continue;
        };
        let _ = writeln!(
            out,
            "  action \"{capability}\" appliesTo {{ principal: [Party], resource: [{}], context: \
             {{ \"at\": datetime }} }};",
            type_of(family)
        );
    }
    out.push_str("}\n");
    out
}

/// The middle segment of `emob.<family>.<verb>`.
fn family_of(capability: &str) -> Option<&str> {
    let mut segments = capability.split('.');
    let (Some(_), Some(family), Some(_), None) = (
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
    ) else {
        return None;
    };
    Some(family)
}

/// A family as its Cedar entity type — `cdr` is `Cdr`.
fn type_of(family: &str) -> String {
    let mut chars = family.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().collect::<String>() + chars.as_str()
    })
}

/// The thing a request is about: an identifier, its family, and its **owner**.
///
/// Ownership is a field on the record rather than a property of the endpoint,
/// which is the same reason [`crate::authority`] holds the tenancy on the
/// principal: the two have to be compared somewhere that holds both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resource {
    family: String,
    id: String,
    owner: PartyId,
}

impl Resource {
    /// A resource in the family a capability names.
    ///
    /// The family comes from the **capability** rather than from a second
    /// argument, so a caller cannot ask whether a principal may
    /// `emob.cdr.read` a tariff. Cedar's schema would refuse that request too —
    /// the action does not apply to that resource type — and a refusal a caller
    /// cannot reach is better than one it has to be told about.
    ///
    /// # Errors
    ///
    /// [`PolicyError::Invalid`] for a capability this workspace does not name.
    pub fn of(
        capability: &str,
        id: impl Into<String>,
        owner: PartyId,
    ) -> Result<Self, PolicyError> {
        let family = family_of(capability)
            .filter(|_| caps::ALL.contains(&capability))
            .ok_or_else(|| PolicyError::Invalid {
                reasons: vec![format!(
                    "{capability} is not a capability this workspace names, so there is no \
                     resource family for it"
                )],
            })?;
        Ok(Self {
            family: family.to_owned(),
            id: id.into(),
            owner,
        })
    }

    /// Who owns it — the party the typed model's tenancy axis is compared
    /// against.
    #[must_use]
    pub const fn owner(&self) -> &PartyId {
        &self.owner
    }
}

/// What a gate decided, and which policies decided it.
///
/// A boolean and its reasons, for the same reason every other refusal in this
/// workspace carries one: an operator debugging *"why can this partner not read
/// our records"* is answering a question about a policy set, and the policy that
/// matched is the answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    allowed: bool,
    reasons: Vec<String>,
    errors: Vec<String>,
}

impl Verdict {
    /// Whether the request is permitted.
    #[must_use]
    pub const fn allowed(&self) -> bool {
        self.allowed
    }

    /// The policies that decided it, by the id the operator gave them.
    #[must_use]
    pub fn reasons(&self) -> &[String] {
        &self.reasons
    }

    /// Anything that could not be evaluated.
    ///
    /// **Always empty on an allowed verdict**, which is the whole of what this
    /// gate does differently from Cedar's own default: a policy that errored is
    /// a rule that did not run, so the request is denied rather than carried on
    /// with. See the module documentation.
    #[must_use]
    pub fn errors(&self) -> &[String] {
        &self.errors
    }
}

/// The typed ceiling and the operator's policies, in that order.
#[derive(Debug, Clone)]
pub struct Gate {
    policies: Policies,
    authorizer: Authorizer,
}

impl Gate {
    /// A gate over a validated policy set.
    #[must_use]
    pub fn new(policies: Policies) -> Self {
        Self {
            policies,
            authorizer: Authorizer::new(),
        }
    }

    /// The policies this gate holds.
    #[must_use]
    pub const fn policies(&self) -> &Policies {
        &self.policies
    }

    /// Replace the policy set — the whole point of a policy engine, and the one
    /// operation the typed model has no answer for.
    ///
    /// # A reload that does not validate leaves the running set standing
    ///
    /// The failure mode of every hot-reloaded configuration: a file is edited,
    /// the parse fails, and the process is left holding **nothing**. For a
    /// policy engine that is not a degraded state, it is a total outage with a
    /// perfect audit trail — default deny means an empty set refuses every
    /// request the node was serving a second ago.
    ///
    /// So the new text is parsed and validated **before** anything is replaced,
    /// and a set that does not hold is an error the operator reads while the old
    /// one keeps deciding.
    ///
    /// # Errors
    ///
    /// Everything [`Policies::load`] refuses.
    pub fn reload(&mut self, text: &str) -> Result<usize, PolicyError> {
        let replacement = Policies::load(text)?;
        let count = replacement.len();
        self.policies = replacement;
        Ok(count)
    }

    /// Whether a principal may perform a capability against a resource, at an
    /// instant.
    ///
    /// `at` is an argument for the reason every instant in this workspace is
    /// one: a policy bounded by a date is a decision somebody will have to
    /// replay, and a gate that read a clock could not be asked what it would
    /// have answered last March.
    ///
    /// # The order is the whole of the composition
    ///
    /// 1. The **typed ceiling** —
    ///    [`Principal::may_act_for`](crate::authority::Principal::may_act_for).
    ///    A `no` here is final, so no policy can widen a delegation or reach
    ///    another party's records.
    /// 2. The **policies**, default deny.
    /// 3. Any **evaluation error** is a denial, because a rule that did not run
    ///    is not a rule that passed.
    #[must_use]
    pub fn verdict(
        &self,
        principal: &Principal,
        capability: &str,
        resource: &Resource,
        at: time::OffsetDateTime,
    ) -> Verdict {
        // 1. The ceiling. A policy may grant, and it may not widen a delegation.
        if !principal.may_act_for(capability, &resource.owner) {
            return Verdict {
                allowed: false,
                reasons: vec![format!(
                    "the credential's own grant does not reach {capability} on records owned by \
                     {}: a policy may grant inside a delegation and may never widen one",
                    resource.owner
                )],
                errors: Vec::new(),
            };
        }

        let request = match self.request(principal, capability, resource, at) {
            Ok(request) => request,
            Err(reason) => {
                return Verdict {
                    allowed: false,
                    reasons: Vec::new(),
                    errors: vec![reason],
                };
            }
        };
        let entities = match self.entities(principal, resource) {
            Ok(entities) => entities,
            Err(reason) => {
                return Verdict {
                    allowed: false,
                    reasons: Vec::new(),
                    errors: vec![reason],
                };
            }
        };

        let response = self
            .authorizer
            .is_authorized(&request, &self.policies.set, &entities);
        let errors: Vec<String> = response
            .diagnostics()
            .errors()
            .map(ToString::to_string)
            .collect();
        // The **operator's** name for the policy where they gave one. Cedar
        // numbers a policy set parsed from text `policy0`, `policy1`, … and an
        // operator debugging *"why can this partner not read our records"* is
        // reading an answer about a file they wrote: `@id("nl-tnm-trial")` is
        // what they called it, and it is what the verdict says.
        let reasons: Vec<String> = response
            .diagnostics()
            .reason()
            .map(|id| {
                self.policies
                    .set
                    .policy(id)
                    .and_then(|policy| policy.annotation("id"))
                    .map_or_else(|| id.to_string(), ToOwned::to_owned)
            })
            .collect();

        Verdict {
            // 3. A policy that could not be evaluated is a rule that did not
            //    run. Cedar skips one and carries on, which would let a `forbid`
            //    fail open.
            allowed: response.decision() == Decision::Allow && errors.is_empty(),
            reasons,
            errors,
        }
    }

    /// The question, in Cedar's terms — validated against the schema, so an
    /// action that does not apply to a resource type is an error rather than a
    /// silent non-match.
    fn request(
        &self,
        principal: &Principal,
        capability: &str,
        resource: &Resource,
        at: time::OffsetDateTime,
    ) -> Result<Request, String> {
        let context = Context::from_pairs([(
            "at".to_owned(),
            RestrictedExpression::new_datetime(cedar_instant(at)?),
        )])
        .map_err(|error| format!("the instant does not reach the policy context: {error}"))?;

        Request::new(
            uid("Party", &principal.party.to_string())?,
            uid("Action", capability)?,
            uid(&type_of(&resource.family), &resource.id)?,
            context,
            Some(&self.policies.schema),
        )
        .map_err(|error| format!("the request does not hold against the schema: {error}"))
    }

    /// The principal, the resource and the party that owns it.
    fn entities(&self, principal: &Principal, resource: &Resource) -> Result<Entities, String> {
        let owner = uid("Party", &resource.owner.to_string())?;
        let asker = uid("Party", &principal.party.to_string())?;

        let party = |uid: EntityUid, role: Option<&str>| {
            let attributes = role
                .map(|role| {
                    (
                        "role".to_owned(),
                        RestrictedExpression::new_string(role.to_owned()),
                    )
                })
                .into_iter()
                .collect();
            Entity::new(uid, attributes, std::collections::HashSet::new())
                .map_err(|error| format!("the party does not hold against the schema: {error}"))
        };

        let mut entities = vec![party(asker.clone(), Some(principal.role.as_str()))?];
        if owner != asker {
            // The owner's role is not this credential's to state. Absent rather
            // than empty: a policy that reads it gets an evaluation error, and
            // an evaluation error is a denial — where an empty string would have
            // been a comparison that is quietly false forever.
            entities.push(party(owner.clone(), None)?);
        }
        entities.push(
            Entity::new(
                uid(&type_of(&resource.family), &resource.id)?,
                std::collections::HashMap::from([(
                    "owner".to_owned(),
                    RestrictedExpression::new_entity_uid(owner),
                )]),
                std::collections::HashSet::new(),
            )
            .map_err(|error| format!("the resource does not hold against the schema: {error}"))?,
        );

        Entities::from_entities(entities, Some(&self.policies.schema))
            .map_err(|error| format!("the entities do not hold against the schema: {error}"))
    }
}

/// An instant in the one spelling Cedar's `datetime` extension parses.
///
/// *"Valid datetime strings start with `YYYY-MM-DD` and may optionally include
/// `THH:MM:SS` plus `Z`, `.SSSZ`, or an offset like `+0700`"* — note the offset
/// with **no colon**, which is not RFC 3339's and is not what `time` writes. So
/// the instant is moved to UTC and written with a `Z`, which is the one form
/// both agree on. Every policy comparing against it therefore compares in UTC,
/// and an operator writing `datetime("2027-01-01T00:00:00Z")` gets the instant
/// they wrote rather than one shifted by whatever offset the caller's clock
/// carried.
fn cedar_instant(at: time::OffsetDateTime) -> Result<String, String> {
    const UTC: &[time::format_description::BorrowedFormatItem<'_>] = time::macros::format_description!(
        "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z"
    );
    at.to_offset(time::UtcOffset::UTC)
        .format(&UTC)
        .map_err(|error| format!("the instant has no Cedar spelling: {error}"))
}

/// A namespaced entity identifier.
fn uid(type_name: &str, id: &str) -> Result<EntityUid, String> {
    let name = EntityTypeName::from_str(&format!("{NAMESPACE}::{type_name}"))
        .map_err(|error| format!("{NAMESPACE}::{type_name} is not an entity type: {error}"))?;
    Ok(EntityUid::from_type_name_and_id(name, EntityId::new(id)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority::{Capabilities, PartyScope};
    use emob_core::Role;
    use time::macros::datetime;

    fn party(country: &str, id: &str) -> PartyId {
        PartyId::new(country, id).expect("a valid party")
    }

    /// The shape an operator writes when a partner is onboarded: what they may
    /// do, and until when.
    const TRIAL: &str = r#"
        @id("nl-tnm-trial")
        permit (
          principal == emob::Party::"NL*TNM",
          action in [emob::Action::"emob.cdr.read", emob::Action::"emob.tariff.read"],
          resource
        )
        when { resource.owner == principal }
        unless { context.at > datetime("2027-01-01T00:00:00Z") };
    "#;

    fn gate() -> Gate {
        Gate::new(Policies::load(TRIAL).expect("the trial policy validates"))
    }

    #[test]
    fn the_schema_is_the_capability_list_and_not_a_second_copy_of_it() {
        // A hand-written schema is a second list of the capabilities this
        // workspace names, and a policy validating against an action nobody can
        // be granted is a rule that never matches.
        let schema = schema_text();
        for capability in caps::ALL {
            assert!(
                schema.contains(&format!("action \"{capability}\"")),
                "{capability} is a capability nothing can be granted"
            );
        }
        // …and every family has an entity type, so an action applies to
        // something rather than to nothing.
        for family in [
            "Cdr", "Tariff", "Location", "Session", "Token", "Evidence", "Invoice",
        ] {
            assert!(schema.contains(&format!("entity {family} =")), "{family}");
        }
        assert!(
            Schema::from_str(&schema).is_ok(),
            "the derived schema is one Cedar accepts"
        );
    }

    #[test]
    fn every_capability_this_workspace_names_is_in_the_list_the_schema_is_built_from() {
        // The one way `caps::ALL` goes wrong is a constant added beside it and
        // not to it — which is a capability a route can check and no policy can
        // ever grant, so every request for it is denied by a rule nobody wrote.
        let source = include_str!("authority.rs");
        let declared: Vec<&str> = source
            .lines()
            .filter_map(|line| {
                let rest = line.trim().strip_prefix("pub const ")?;
                let (name, _) = rest.split_once(':')?;
                (name != "ALL").then_some(name)
            })
            .collect();
        assert_eq!(
            declared.len(),
            caps::ALL.len(),
            "`caps` declares {declared:?} and `ALL` holds {}",
            caps::ALL.len()
        );
    }

    #[test]
    fn a_policy_naming_something_the_schema_has_not_is_refused_at_load() {
        // The half a text file cannot have without a schema. Every one of these
        // parses perfectly and would simply never match, which fails open on a
        // `forbid` and silently on a `permit`.
        for wrong in [
            // an action nobody can be granted
            r#"permit (principal, action == emob::Action::"emob.cdr.raed", resource);"#,
            // an attribute no entity has
            r"permit (principal, action, resource) when { resource.holder == principal };",
            // an action that does not apply to that resource type
            r#"permit (principal, action == emob::Action::"emob.cdr.read", resource)
               when { resource is emob::Tariff };"#,
        ] {
            assert!(
                matches!(Policies::load(wrong), Err(PolicyError::Invalid { .. })),
                "{wrong}"
            );
        }
        // …and text the grammar refuses is a different answer, because the fix
        // is a different one.
        assert!(matches!(
            Policies::load("permit (principal"),
            Err(PolicyError::Unparsable { .. })
        ));
    }

    #[test]
    fn an_operator_who_writes_no_policy_gets_nothing() {
        // Default deny, and it is the direction `Capabilities::none` already
        // falls in: a deployment where somebody forgot the grants is exactly the
        // one nobody would notice.
        let gate = Gate::new(Policies::load("").expect("no policies is a policy set"));
        assert!(gate.policies().is_empty());
        let peer = Principal::peer(party("NL", "TNM"), Role::Emsp, Capabilities::root());
        let theirs =
            Resource::of(caps::CDR_READ, "cdr-1", party("NL", "TNM")).expect("a known family");
        assert!(
            !gate
                .verdict(
                    &peer,
                    caps::CDR_READ,
                    &theirs,
                    datetime!(2026-06-01 10:00 UTC)
                )
                .allowed()
        );
    }

    #[test]
    fn a_grant_an_operator_bounded_by_a_date_expires_without_a_deploy() {
        // The question the typed model has no vocabulary for, and the reason
        // this module exists: a set of dotted capability patterns cannot say
        // *until*.
        let gate = gate();
        let peer = Principal::peer(party("NL", "TNM"), Role::Emsp, Capabilities::root());
        let theirs =
            Resource::of(caps::CDR_READ, "cdr-1", party("NL", "TNM")).expect("a known family");

        let inside = gate.verdict(
            &peer,
            caps::CDR_READ,
            &theirs,
            datetime!(2026-06-01 10:00 UTC),
        );
        assert!(inside.allowed(), "{inside:?}");
        assert_eq!(
            inside.reasons(),
            ["nl-tnm-trial"],
            "the policy that decided"
        );

        let after = gate.verdict(
            &peer,
            caps::CDR_READ,
            &theirs,
            datetime!(2027-06-01 10:00 UTC),
        );
        assert!(!after.allowed(), "{after:?}");

        // …and the instant is an argument, so the same question two years later
        // gets the answer it got then rather than the answer today's clock gives.
        assert!(
            gate.verdict(
                &peer,
                caps::CDR_READ,
                &theirs,
                datetime!(2026-06-01 10:00 UTC)
            )
            .allowed()
        );
    }

    #[test]
    fn a_policy_may_grant_and_may_never_widen_a_delegation() {
        // The whole of the composition. Getting it the other way round would
        // undo two structural claims at once — the roaming leak and `agentd`.
        let gate = gate();

        // A peer still reaches only its own records, and the policy above says
        // `resource.owner == principal` — so this is denied twice over. The
        // point is *which* refusal answers first: the typed one, whose reason
        // says so, because that is the one no text can turn off.
        let peer = Principal::peer(party("NL", "TNM"), Role::Emsp, Capabilities::root());
        let somebody_elses =
            Resource::of(caps::CDR_READ, "cdr-2", party("DE", "XYZ")).expect("a known family");
        let leak = gate.verdict(
            &peer,
            caps::CDR_READ,
            &somebody_elses,
            datetime!(2026-06-01 10:00 UTC),
        );
        assert!(!leak.allowed());
        assert!(
            leak.reasons()[0].contains("may never widen"),
            "{:?}",
            leak.reasons()
        );

        // …and an agent's principal, which is *derived* rather than configured,
        // cannot be handed a capability by a policy file. `agentd`'s claim is
        // that no agent can hold one that writes.
        let operator = Principal::operator(party("NL", "TNM"), Role::Emsp);
        let agent = operator
            .attenuate(
                Capabilities::of([caps::CDR_READ]),
                PartyScope::just(&party("NL", "TNM")),
            )
            .expect("narrower on both axes");
        let ours =
            Resource::of(caps::CDR_WRITE, "cdr-3", party("NL", "TNM")).expect("a known family");
        let widened = gate.verdict(
            &agent,
            caps::CDR_WRITE,
            &ours,
            datetime!(2026-06-01 10:00 UTC),
        );
        assert!(!widened.allowed(), "{widened:?}");
        assert!(widened.reasons()[0].contains("may never widen"));
    }

    #[test]
    fn a_reload_that_does_not_validate_leaves_the_running_set_standing() {
        // The failure mode of every hot-reloaded configuration, and for a policy
        // engine it is not a degraded state: default deny means an empty set
        // refuses every request the node was serving a second ago.
        let mut gate = gate();
        let peer = Principal::peer(party("NL", "TNM"), Role::Emsp, Capabilities::root());
        let theirs =
            Resource::of(caps::CDR_READ, "cdr-1", party("NL", "TNM")).expect("a known family");
        let ask = |gate: &Gate| {
            gate.verdict(
                &peer,
                caps::CDR_READ,
                &theirs,
                datetime!(2026-06-01 10:00 UTC),
            )
            .allowed()
        };
        assert!(ask(&gate));

        assert!(
            gate.reload("permit (principal").is_err(),
            "it does not parse"
        );
        assert!(
            gate.reload(r#"permit (principal, action == emob::Action::"nope", resource);"#)
                .is_err(),
            "it does not validate"
        );
        assert!(ask(&gate), "the old set is still deciding");

        // …and a reload that does hold replaces it.
        assert_eq!(gate.reload("").expect("an empty set is a valid one"), 0);
        assert!(!ask(&gate), "and now nothing is granted");
    }

    #[test]
    fn a_capability_this_workspace_does_not_name_has_no_resource_to_be_about() {
        // Cedar's schema would refuse the request too — the action does not
        // exist — and a refusal a caller cannot reach is better than one it has
        // to be told about.
        assert!(Resource::of("emob.cdr.destroy", "cdr-1", party("DE", "ABC")).is_err());
        assert!(Resource::of("wildcard", "cdr-1", party("DE", "ABC")).is_err());
    }
}

#[cfg(test)]
mod the_role_is_a_fact_this_node_may_not_have {
    use super::*;
    use crate::authority::Capabilities;
    use emob_core::Role;
    use time::macros::datetime;

    fn party(country: &str, id: &str) -> PartyId {
        PartyId::new(country, id).expect("a valid party")
    }

    /// A request names two parties and this node knows one role: the asker's.
    ///
    /// Filling the other in with an empty string would make
    /// `resource.owner.role == "CPO"` quietly false forever — a policy that
    /// never matches wearing the clothes of one that does. Absent instead, so
    /// reading it is an evaluation error, and an evaluation error is a denial.
    #[test]
    fn a_policy_reading_a_role_this_node_does_not_have_denies_rather_than_comparing() {
        let gate = Gate::new(
            Policies::load(
                r#"@id("cpos-only")
                   permit (principal, action == emob::Action::"emob.cdr.read", resource)
                   when { principal has role && principal.role == "CPO" };"#,
            )
            .expect("it validates"),
        );
        let cpo = Principal::peer(party("DE", "ABC"), Role::Cpo, Capabilities::root());
        let ours = Resource::of(caps::CDR_READ, "cdr-1", party("DE", "ABC")).expect("a family");
        assert!(
            gate.verdict(&cpo, caps::CDR_READ, &ours, datetime!(2026-06-01 10:00 UTC))
                .allowed(),
            "the asker's own role is a fact the credential states"
        );

        let emsp = Principal::peer(party("NL", "TNM"), Role::Emsp, Capabilities::root());
        let theirs = Resource::of(caps::CDR_READ, "cdr-2", party("NL", "TNM")).expect("a family");
        assert!(
            !gate
                .verdict(
                    &emsp,
                    caps::CDR_READ,
                    &theirs,
                    datetime!(2026-06-01 10:00 UTC)
                )
                .allowed()
        );

        // …and a policy reading the **owner's** role is a question about a third
        // party, which this node cannot answer out of somebody else's token.
        // Because the attribute is optional, the strict validator refuses an
        // unguarded read at **load**: the trap is unreachable rather than
        // reported.
        let unguarded = Policies::load(
            r#"permit (principal, action == emob::Action::"emob.cdr.read", resource)
               when { resource.owner.role == "CPO" };"#,
        );
        assert!(
            matches!(unguarded, Err(PolicyError::Invalid { .. })),
            "an unguarded read of an optional attribute is refused at load: {unguarded:?}"
        );

        // Guarded, it loads — and answers `false` for a party whose role this
        // node has no business claiming, rather than comparing against an empty
        // string somebody put there to satisfy a schema.
        let owners = Gate::new(
            Policies::load(
                r#"permit (principal, action == emob::Action::"emob.cdr.read", resource)
                   when { resource.owner has role && resource.owner.role == "CPO" };"#,
            )
            .expect("a guarded read validates"),
        );
        assert!(
            owners
                .verdict(&cpo, caps::CDR_READ, &ours, datetime!(2026-06-01 10:00 UTC))
                .allowed(),
            "the asker owning the record is the one case the role is known"
        );

        // A scope wide enough to reach another party's records — a hub — is the
        // shape where the owner is somebody else, and there the attribute is
        // absent rather than empty.
        let hub = Principal {
            party: party("DE", "HUB"),
            role: Role::Hub,
            capabilities: Capabilities::root(),
            scope: crate::authority::PartyScope::Every,
        };
        let somebody_elses =
            Resource::of(caps::CDR_READ, "cdr-3", party("NL", "TNM")).expect("a family");
        assert!(
            !owners
                .verdict(
                    &hub,
                    caps::CDR_READ,
                    &somebody_elses,
                    datetime!(2026-06-01 10:00 UTC)
                )
                .allowed(),
            "an absent role is not a CPO, and not an empty string either"
        );
    }
}

#[cfg(test)]
mod what_cannot_be_evaluated_is_not_assumed_open {
    use super::*;
    use crate::authority::Capabilities;
    use emob_core::Role;
    use time::macros::datetime;

    /// An allowed verdict carries no errors, and that is the claim.
    ///
    /// Cedar's own answer to a policy that errors during evaluation is to skip
    /// it and carry on — right for a large policy store, and wrong for an
    /// authorisation gate, where a `forbid` that could not be evaluated is a
    /// refusal that silently did not happen. So the two are one condition here,
    /// and a test rather than a sentence, because the sentence was written the
    /// other way round first.
    #[test]
    fn an_allowed_verdict_never_carries_an_error() {
        let gate = Gate::new(
            Policies::load(
                r#"permit (principal, action == emob::Action::"emob.cdr.read", resource)
                   when { resource.owner == principal };"#,
            )
            .expect("it validates"),
        );
        let party = PartyId::new("NL", "TNM").expect("a valid party");
        let peer = Principal::peer(party.clone(), Role::Emsp, Capabilities::root());
        let theirs = Resource::of(caps::CDR_READ, "cdr-1", party).expect("a known family");

        let verdict = gate.verdict(
            &peer,
            caps::CDR_READ,
            &theirs,
            datetime!(2026-06-01 10:00 UTC),
        );
        assert!(verdict.allowed());
        assert!(
            verdict.errors().is_empty(),
            "an allowed verdict is one where every rule ran: {verdict:?}"
        );
    }
}
