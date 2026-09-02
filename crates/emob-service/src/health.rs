//! Whether a daemon is alive, and whether it is ready — which are not the same
//! question.
//!
//! # The probe that lies
//!
//! Almost every readiness endpoint in this industry returns `200` unconditionally,
//! because it was written before there was anything to check and never revisited.
//! An orchestrator then sends traffic to a `csmsd` whose key registry has not
//! loaded, and every station that connects in the next thirty seconds has its
//! session refused for want of a key — which looks like a fleet fault and is a
//! deployment one.
//!
//! So readiness here is a **set of named dependencies**, each of which reports
//! for itself, and the endpoint is ready when all of them are. A daemon that
//! registers none is not ready by default: an empty set is a daemon that has not
//! said what it needs, which is the state to fail in rather than the state to
//! pass in.
//!
//! # Liveness is not readiness, and conflating them restarts the wrong thing
//!
//! Liveness answers "is this process wedged" and its only honest answer is that
//! the runtime is still scheduling — because anything else makes a restart the
//! cure for a dependency being down, and restarting a CSMS drops every station's
//! WebSocket for a reason that was never in this process.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

/// What one dependency says about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// It is there.
    Up,
    /// It is not, and this is why.
    Down {
        /// What an operator needs to read.
        reason: String,
    },
}

impl Probe {
    /// A dependency that is down, with a reason.
    #[must_use]
    pub fn down(reason: impl Into<String>) -> Self {
        Self::Down {
            reason: reason.into(),
        }
    }

    /// Whether it is up.
    #[must_use]
    pub const fn is_up(&self) -> bool {
        matches!(self, Self::Up)
    }
}

/// The dependencies a daemon needs before it may take traffic.
///
/// Cheap to clone and safe to share: a handler reads it, a background task that
/// reconnects something writes it.
#[derive(Debug, Clone, Default)]
pub struct Readiness {
    probes: Arc<RwLock<BTreeMap<String, Probe>>>,
}

impl Readiness {
    /// A daemon that has not said what it needs.
    ///
    /// Not ready. See the module documentation.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Declare a dependency, initially down.
    ///
    /// Declared rather than discovered, so the readiness surface lists what a
    /// daemon is waiting *for* while it waits — which is the one moment an
    /// operator needs it.
    #[must_use]
    pub fn expecting(self, name: impl Into<String>) -> Self {
        self.set(name, Probe::down("not started"));
        self
    }

    /// Report on a dependency.
    pub fn set(&self, name: impl Into<String>, probe: Probe) {
        // A poisoned lock guards a map of statuses. Recovering is right for the
        // same reason `csmsd`'s ledger locks recover: a daemon that answers a
        // panic elsewhere by reporting itself permanently unready has turned one
        // fault into an outage.
        let mut probes = self
            .probes
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        probes.insert(name.into(), probe);
    }

    /// Mark a dependency up.
    pub fn up(&self, name: impl Into<String>) {
        self.set(name, Probe::Up);
    }

    /// Whether every declared dependency is up, and at least one was declared.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        let probes = self
            .probes
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        !probes.is_empty() && probes.values().all(Probe::is_up)
    }

    /// Every dependency and what it says, in name order.
    #[must_use]
    pub fn report(&self) -> Vec<(String, Probe)> {
        let probes = self
            .probes
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        probes
            .iter()
            .map(|(name, probe)| (name.clone(), probe.clone()))
            .collect()
    }

    /// The reasons a daemon is not ready, for a log line and for the endpoint's
    /// body.
    #[must_use]
    pub fn blockers(&self) -> Vec<String> {
        self.report()
            .into_iter()
            .filter_map(|(name, probe)| match probe {
                Probe::Up => None,
                Probe::Down { reason } => Some(format!("{name}: {reason}")),
            })
            .collect()
    }
}

/// A daemon's own name and version, as they appear in its logs and on its
/// health endpoint.
///
/// Taken from the calling crate rather than typed out, because a version string
/// that has to be remembered is one that is wrong after the first release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Identity {
    /// The daemon's name.
    pub name: &'static str,
    /// Its version.
    pub version: &'static str,
}

impl Identity {
    /// A name and a version.
    #[must_use]
    pub const fn new(name: &'static str, version: &'static str) -> Self {
        Self { name, version }
    }
}

/// The [`Identity`] of the crate this macro is expanded in.
///
/// ```
/// let me = emob_service::identity!();
/// assert_eq!(me.name, "emob-service");
/// ```
#[macro_export]
macro_rules! identity {
    () => {
        $crate::health::Identity::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_daemon_that_has_declared_nothing_is_not_ready() {
        // An empty set is a daemon that has not said what it needs, which is
        // the state to fail in rather than the state to pass in.
        assert!(!Readiness::new().is_ready());
    }

    #[test]
    fn readiness_waits_for_every_dependency_and_says_which() {
        let readiness = Readiness::new()
            .expecting("key-registry")
            .expecting("tariff");

        assert!(!readiness.is_ready());
        assert_eq!(
            readiness.blockers(),
            vec![
                "key-registry: not started".to_owned(),
                "tariff: not started".to_owned()
            ],
            "the surface lists what it is waiting for while it waits"
        );

        readiness.up("key-registry");
        assert!(!readiness.is_ready(), "one is not all");
        assert_eq!(readiness.blockers(), vec!["tariff: not started".to_owned()]);

        readiness.up("tariff");
        assert!(readiness.is_ready());
        assert!(readiness.blockers().is_empty());
    }

    #[test]
    fn a_dependency_that_goes_down_takes_readiness_with_it() {
        // The point of a probe over a flag: a daemon whose registry reload
        // failed has to stop taking traffic, not keep the answer it had at boot.
        let readiness = Readiness::new().expecting("registry");
        readiness.up("registry");
        assert!(readiness.is_ready());

        readiness.set("registry", Probe::down("the provisioning API returned 503"));
        assert!(!readiness.is_ready());
        assert_eq!(
            readiness.blockers(),
            vec!["registry: the provisioning API returned 503".to_owned()]
        );
    }

    #[test]
    fn a_report_is_in_name_order_so_two_readings_can_be_diffed() {
        let readiness = Readiness::new().expecting("zeta").expecting("alpha");
        let names: Vec<String> = readiness.report().into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, vec!["alpha".to_owned(), "zeta".to_owned()]);
    }
}
