//! Where the credentials handshake may reach back to (SR-010, SR-037, SR-045).
//!
//! `[OCPI 2.3.0 §credentials]` makes the receiver of a registration **fetch**
//! the versions URL the registrant supplied, with the token it supplied. Once
//! the inbound router is mounted, that is a request this node makes to any URL
//! anybody holding a Token A writes down — the cloud metadata endpoint, the
//! journal volume's admin port, the other daemons on the compose network.
//!
//! So before any request: `https` only; `http`, and an address that resolves
//! to loopback, a private or link-local range, the unspecified address or a
//! multicast group, only for a host the estate allowlists. The name is resolved
//! **once** here and every address it resolves to is checked.
//!
//! This check is the early answer, in a sentence the registrant is shown. The
//! enforcement is the kit's: since `ocpi-kit` 0.5 its resolver refuses every
//! address the URL policy does not permit on every connection and every
//! redirect hop, so a name that rebinds to an internal address between this
//! lookup and the connection is refused there too. "Internal" is the kit's own
//! rule — the IANA special-purpose registries — so the two cannot disagree.
//!
//! `[roaming] reach_back_allow` is for development estates: the kit's policy
//! cannot say "private only for these hosts", so a non-empty allowlist opens
//! private addresses to every peer's connection, and only this check still
//! names the hosts. A production estate leaves it empty.

use std::net::IpAddr;

/// The guard, with the estate's allowlist.
#[derive(Debug, Clone, Default)]
pub struct ReachBack {
    allow: Vec<String>,
}

impl ReachBack {
    /// A guard that allows nothing internal.
    #[must_use]
    pub fn strict() -> Self {
        Self::default()
    }

    /// A guard that allows these hosts over `http` and at internal addresses.
    #[must_use]
    pub fn allowing<I, S>(hosts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            allow: hosts
                .into_iter()
                .map(|host| host.into().to_ascii_lowercase())
                .collect(),
        }
    }

    /// Whether `url` may be fetched, resolving its host once.
    ///
    /// # Errors
    ///
    /// Why not, in a sentence the registrant is shown.
    pub async fn admit(&self, url: &str) -> Result<(), String> {
        let uri: http::Uri = url
            .parse()
            .map_err(|error| format!("{url} is not a URL this node will reach back to: {error}"))?;
        let host = uri
            .host()
            .ok_or_else(|| format!("{url} names no host"))?
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_ascii_lowercase();
        let allowed = self.allow.contains(&host);
        let port = match uri.scheme_str() {
            Some("https") => uri.port_u16().unwrap_or(443),
            Some("http") if allowed => uri.port_u16().unwrap_or(80),
            Some("http") => {
                return Err(format!(
                    "{url} is not https: a credentials token sent in clear is one anybody on the \
                     path can present, and {host} is not on this estate's allowlist"
                ));
            }
            _ => return Err(format!("{url} is neither https nor http")),
        };
        if allowed {
            return Ok(());
        }
        let addresses: Vec<IpAddr> = tokio::net::lookup_host((host.as_str(), port))
            .await
            .map_err(|error| format!("{host} does not resolve: {error}"))?
            .map(|socket| socket.ip())
            .collect();
        if addresses.is_empty() {
            return Err(format!("{host} resolves to nothing"));
        }
        if let Some(internal) = addresses.iter().find(|address| is_internal(**address)) {
            return Err(format!(
                "{host} resolves to {internal}, an internal address: this node does not reach \
                 into its own network on a registrant's say-so"
            ));
        }
        Ok(())
    }
}

/// Anything that is not a globally reachable unicast address, by the IANA
/// special-purpose registries — the rule `ocpi-kit`'s resolver enforces on the
/// connection itself.
#[must_use]
pub fn is_internal(address: IpAddr) -> bool {
    !ocpi_kit::types::UrlPolicy::default().permits_address(address)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sr_045_sr_037_the_metadata_endpoint_is_never_reached() {
        let guard = ReachBack::strict();
        let refusal = guard
            .admit("http://169.254.169.254/latest/meta-data")
            .await
            .expect_err("plain http to a link-local address");
        assert!(refusal.contains("not https"), "{refusal}");
        let refusal = guard
            .admit("https://169.254.169.254/ocpi/versions")
            .await
            .expect_err("https does not make an internal address external");
        assert!(refusal.contains("internal address"), "{refusal}");
    }

    #[tokio::test]
    async fn sr_045_sr_037_loopback_and_private_ranges_are_refused_unless_allowlisted() {
        let guard = ReachBack::strict();
        for url in [
            "https://127.0.0.1:8443/ocpi/versions",
            "https://10.0.0.7/ocpi/versions",
            "https://[::1]/ocpi/versions",
            "https://[::ffff:192.168.1.1]/ocpi/versions",
        ] {
            assert!(guard.admit(url).await.is_err(), "{url}");
        }
        let demo = ReachBack::allowing(["127.0.0.1"]);
        assert_eq!(
            demo.admit("http://127.0.0.1:9583/ocpi/versions").await,
            Ok(())
        );
        assert!(
            demo.admit("http://10.0.0.7/ocpi/versions").await.is_err(),
            "the allowlist names hosts, not a licence for http"
        );
    }

    #[test]
    fn a_public_address_is_not_internal() {
        assert!(!is_internal("8.8.8.8".parse().unwrap()));
        assert!(!is_internal("2a00:1450:4001::1".parse().unwrap()));
        assert!(is_internal("100.64.0.1".parse().unwrap()));
        // Ranges the registries name that a hand-written list missed.
        assert!(is_internal("198.18.0.1".parse().unwrap()), "benchmarking");
        assert!(
            is_internal("192.0.0.8".parse().unwrap()),
            "IETF protocol assignments"
        );
    }
}
