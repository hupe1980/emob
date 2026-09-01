//! Who this node peers with, and which of them a record is for.
//!
//! # Routing is a question the identifier already answers
//!
//! A contract id is not opaque. `DE-ABC-C00122045-6` states, in its first five
//! characters, the provider that issued the contract — and that provider is
//! the party that holds the driver, owes them an invoice, and will pay this
//! CDR. Most platforms route settlement off a hand-maintained map from
//! *something else* (an OCPI token's `party_id`, a location's operator, a
//! configuration file) to a partner, and that map is where roaming money goes
//! to the wrong company: it is edited by hand, it drifts, and nothing ever
//! reconciles it against the identifiers actually on the records.
//!
//! So [`PartnerRegistry::route`] reads the issuer out of the contract id.
//!
//! # …but the identifier is not the last word, and OCPI says so
//!
//! *"The `party_id` and `country_code` given here have no direct link with the
//! eMSP that issued the contract"* — the id is a name in an eMI3 namespace,
//! and a provider is free to issue contracts under a namespace it does not
//! peer under, or to have been acquired since. So the registry carries an
//! explicit [`Partner::issues`] list, and a partner that claims an issuer
//! takes precedence over the prefix. The prefix is the default, not the rule,
//! and a node that has never had to override it has a routing table it can
//! actually audit.
//!
//! # A hub is not a peer with more addresses
//!
//! Sending to a hub is sending to *whoever the hub decides*, which is a real
//! choice with a real cost — the hub sees the record, and the CPO gives up
//! knowing which provider settled it. So a hub is a distinct
//! [`Reach`](Reach::Hub) rather than a partner that happens to match
//! everything, and [`PartnerRegistry::route`] prefers a direct peer wherever
//! it has one.

use emob_core::{ContractId, PartyId};

/// Which OCPI version a partner speaks.
///
/// The canonical model is 2.3.0 — the richest of the three, and the one
/// `ocpi-kit` defines the others as deltas from. A partner on an older version
/// is normal rather than exceptional; what is not normal is failing to say
/// what the downgrade cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum OcpiVersion {
    /// OCPI 2.3.0.
    #[default]
    V2_3_0,
    /// OCPI 2.2.1 — still the version most of the market runs.
    V2_2_1,
}

impl OcpiVersion {
    /// The version string OCPI itself uses.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::V2_3_0 => "2.3.0",
            Self::V2_2_1 => "2.2.1",
        }
    }
}

impl core::fmt::Display for OcpiVersion {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a party does on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    /// Operates charge points. Sends CDRs.
    Cpo,
    /// Holds driver contracts. Receives CDRs and pays against them.
    Emsp,
    /// Routes for others.
    Hub,
}

/// How a record reaches the party that will pay it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reach {
    /// Straight to the provider that holds the contract.
    Direct(PartyId),
    /// Through a hub, which decides the rest.
    ///
    /// The second field is the provider the contract names, kept because the
    /// CPO still has to be able to answer *"who settled this"* even where it
    /// did not choose the recipient.
    Hub {
        /// The hub the record is handed to.
        hub: PartyId,
        /// The provider the contract identifier names.
        issuer: PartyId,
    },
}

impl Reach {
    /// The party the document is actually sent to.
    #[must_use]
    pub const fn recipient(&self) -> &PartyId {
        match self {
            Self::Direct(party) | Self::Hub { hub: party, .. } => party,
        }
    }
}

/// One party this node peers with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partner {
    /// Its OCPI identity.
    pub party: PartyId,
    /// What it does.
    pub roles: Vec<Role>,
    /// Which OCPI version it speaks.
    pub version: OcpiVersion,
    /// The contract namespaces it issues under, where they differ from its own
    /// `party_id`.
    ///
    /// Empty is the ordinary case and means "the ones matching my own id".
    pub issues: Vec<PartyId>,
    /// Whether this partner settles on signed metering data.
    ///
    /// A partner billing German sessions does `[MessEG §33]`, and a CDR
    /// reaching it without the records is one it cannot put in front of a
    /// driver who disputes the number. Set here rather than assumed, because
    /// the same node peers with parties in jurisdictions that do not ask.
    pub requires_signed_data: bool,
}

impl Partner {
    /// A provider that speaks the canonical version and issues under its own
    /// namespace.
    #[must_use]
    pub fn emsp(party: PartyId) -> Self {
        Self {
            party,
            roles: vec![Role::Emsp],
            version: OcpiVersion::V2_3_0,
            issues: Vec::new(),
            requires_signed_data: false,
        }
    }

    /// A hub.
    #[must_use]
    pub fn hub(party: PartyId) -> Self {
        Self {
            party,
            roles: vec![Role::Hub],
            version: OcpiVersion::V2_3_0,
            issues: Vec::new(),
            requires_signed_data: false,
        }
    }

    /// Speak an older version to this partner.
    #[must_use]
    pub const fn speaking(mut self, version: OcpiVersion) -> Self {
        self.version = version;
        self
    }

    /// Declare that this partner also issues contracts under another namespace.
    #[must_use]
    pub fn issuing(mut self, namespace: PartyId) -> Self {
        self.issues.push(namespace);
        self
    }

    /// Declare that this partner settles on signed metering data.
    #[must_use]
    pub const fn on_signed_data(mut self) -> Self {
        self.requires_signed_data = true;
        self
    }

    /// Whether this partner holds a role.
    #[must_use]
    pub fn has_role(&self, role: Role) -> bool {
        self.roles.contains(&role)
    }

    /// Whether this partner claims the contracts of a namespace.
    #[must_use]
    pub fn issues_for(&self, issuer: &PartyId) -> bool {
        if self.issues.is_empty() {
            &self.party == issuer
        } else {
            self.issues.contains(issuer)
        }
    }
}

/// The parties this node can send to, and itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartnerRegistry {
    me: PartyId,
    partners: Vec<Partner>,
}

impl PartnerRegistry {
    /// An empty registry that knows only who it is.
    ///
    /// Knowing that is not bookkeeping. A session between this operator's own
    /// EMP and its own CPO — the German normal case, where one company wears
    /// both hats — is **self-roaming**, and the point of routing it through
    /// the same path as a partner's is that going multi-party later changes
    /// the transport and nothing about the arithmetic.
    #[must_use]
    pub const fn new(me: PartyId) -> Self {
        Self {
            me,
            partners: Vec::new(),
        }
    }

    /// Add a partner.
    #[must_use]
    pub fn with(mut self, partner: Partner) -> Self {
        self.partners.push(partner);
        self
    }

    /// This node's own party.
    #[must_use]
    pub const fn me(&self) -> &PartyId {
        &self.me
    }

    /// Every partner, in the order they were added.
    #[must_use]
    pub fn partners(&self) -> &[Partner] {
        &self.partners
    }

    /// A partner by its identity.
    #[must_use]
    pub fn get(&self, party: &PartyId) -> Option<&Partner> {
        self.partners.iter().find(|p| &p.party == party)
    }

    /// Who should receive a CDR billed against this contract.
    ///
    /// A direct peer claiming the contract's namespace wins; failing that, a
    /// hub; failing that, nothing — which is [`RoamError::NoRoute`], because a
    /// record sent to a party that never had the driver is settlement money
    /// leaving for the wrong company, and it is far cheaper to notice here.
    ///
    /// [`RoamError::NoRoute`]: crate::RoamError::NoRoute
    #[must_use]
    pub fn route(&self, contract: &ContractId) -> Option<Reach> {
        let issuer = issuer_of(contract)?;

        if let Some(direct) = self
            .partners
            .iter()
            .find(|p| p.has_role(Role::Emsp) && p.issues_for(&issuer))
        {
            return Some(Reach::Direct(direct.party.clone()));
        }

        self.partners
            .iter()
            .find(|p| p.has_role(Role::Hub))
            .map(|hub| Reach::Hub {
                hub: hub.party.clone(),
                issuer,
            })
    }

    /// Whether a party is this node itself.
    #[must_use]
    pub fn is_me(&self, party: &PartyId) -> bool {
        &self.me == party
    }
}

/// The provider a contract identifier names, when it names one in the eMI3
/// shape.
///
/// `None` for an id in a scheme this build does not recognise — which a caller
/// must treat as *"not routable by prefix"* rather than as *"no partner"*. An
/// eMSP is free to use its own scheme, and the registry's explicit
/// [`Partner::issues`] list is how such a contract is routed.
#[must_use]
pub fn issuer_of(contract: &ContractId) -> Option<PartyId> {
    let parts = ocpi_kit::types::ContractIdParts::parse(contract.as_str())?;
    PartyId::new(&parts.country_code, &parts.provider_id).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn party(code: &str, id: &str) -> PartyId {
        PartyId::new(code, id).expect("a valid party")
    }

    fn contract(text: &str) -> ContractId {
        text.parse().expect("a valid contract id")
    }

    #[test]
    fn a_contract_names_the_provider_that_will_pay_it() {
        assert_eq!(
            issuer_of(&contract("DE-ABC-C00122045-6")),
            Some(party("DE", "ABC"))
        );
        // Written without separators, as a card carries it.
        assert_eq!(
            issuer_of(&contract("DEABCC001220456")),
            Some(party("DE", "ABC"))
        );
    }

    #[test]
    fn an_id_in_a_scheme_this_build_does_not_know_is_not_routable_by_prefix() {
        // Not "route it to nobody" — the registry's explicit issuer list is
        // what such a contract is for.
        assert_eq!(issuer_of(&contract("some-providers-own-scheme")), None);
    }

    #[test]
    fn a_direct_peer_beats_a_hub() {
        let registry = PartnerRegistry::new(party("DE", "CPO"))
            .with(Partner::hub(party("DE", "HUB")))
            .with(Partner::emsp(party("DE", "ABC")));

        assert_eq!(
            registry.route(&contract("DE-ABC-C00122045-6")),
            Some(Reach::Direct(party("DE", "ABC"))),
            "a hub sees the record and decides the recipient; a direct peer is \
             strictly less to give up"
        );
    }

    #[test]
    fn a_hub_carries_what_no_peer_claims_and_the_issuer_survives() {
        let registry = PartnerRegistry::new(party("DE", "CPO"))
            .with(Partner::hub(party("DE", "HUB")))
            .with(Partner::emsp(party("DE", "ABC")));

        // A provider this node does not peer with directly.
        let reach = registry.route(&contract("NL-TNM-C00122045-K")).unwrap();
        assert_eq!(
            reach,
            Reach::Hub {
                hub: party("DE", "HUB"),
                issuer: party("NL", "TNM"),
            },
            "the CPO still has to answer `who settled this` even where the hub chose"
        );
        assert_eq!(reach.recipient(), &party("DE", "HUB"));
    }

    #[test]
    fn a_partner_that_issues_under_another_namespace_says_so() {
        // The acquisition case, which OCPI explicitly warns about: the id's
        // prefix and the peering party are not the same fact.
        let registry = PartnerRegistry::new(party("DE", "CPO"))
            .with(Partner::emsp(party("DE", "NEW")).issuing(party("DE", "OLD")));

        assert_eq!(
            registry.route(&contract("DE-OLD-C00122045-6")),
            Some(Reach::Direct(party("DE", "NEW")))
        );
        assert_eq!(
            registry.route(&contract("DE-NEW-C00122045-6")),
            None,
            "an explicit issuer list replaces the default rather than adding to it: \
             a partner that has told us which namespaces it holds has told us all of them"
        );
    }

    #[test]
    fn nothing_routes_where_there_is_no_peer_and_no_hub() {
        let registry = PartnerRegistry::new(party("DE", "CPO"));
        assert_eq!(registry.route(&contract("DE-ABC-C00122045-6")), None);
    }

    #[test]
    fn a_cpo_is_not_somewhere_to_send_a_cdr() {
        let mut cpo = Partner::emsp(party("DE", "ABC"));
        cpo.roles = vec![Role::Cpo];
        let registry = PartnerRegistry::new(party("DE", "CPO")).with(cpo);

        assert_eq!(
            registry.route(&contract("DE-ABC-C00122045-6")),
            None,
            "a party that operates points does not hold the driver's contract"
        );
    }

    #[test]
    fn self_roaming_is_a_party_this_node_recognises_as_itself() {
        let registry = PartnerRegistry::new(party("DE", "CPO"));
        assert!(registry.is_me(&party("DE", "CPO")));
        assert!(!registry.is_me(&party("DE", "ABC")));
    }
}
