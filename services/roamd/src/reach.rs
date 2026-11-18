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
//! What this does not close: the kit's client resolves the name again when it
//! connects, so a name whose answer changes between the two lookups (DNS
//! rebinding) still has a window. Pinning the resolved address needs the kit to
//! accept one; that is recorded in the threat model rather than worked around.

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

/// Loopback, private, link-local, shared (CGNAT), unspecified, broadcast or
/// multicast — anything that is not a public unicast address.
#[must_use]
pub fn is_internal(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_documentation()
                // 100.64.0.0/10, shared address space.
                || (a == 100 && (64..128).contains(&b))
        }
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_internal(IpAddr::V4(mapped));
            }
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || v6.is_unique_local()
                || v6.is_unicast_link_local()
        }
    }
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
    }
}
