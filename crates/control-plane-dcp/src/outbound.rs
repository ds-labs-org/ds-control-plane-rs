//! Confines every outbound request the verifier makes (resolving a
//! counterparty's or an issuer's `did:web`, querying a Credential Service)
//! to a deny-by-default host allow-list, checked **before** the request.
//! Ported from `ds-identity-hub-rs`'s `identity-hub-http/src/outbound.rs`.
//!
//! Matching rule: case-insensitive exact match on the URL's host; the port
//! is ignored; no wildcard, suffix or CIDR matching. Host granularity lets
//! tests run stand-in servers on ephemeral loopback ports.

use reqwest::Url;

#[derive(Debug, thiserror::Error)]
pub enum OutboundError {
    #[error("outbound host '{0}' is not on this connector's allow-list")]
    HostNotAllowed(String),
    #[error("outbound URL scheme '{0}' is not supported (only http/https)")]
    UnsupportedScheme(String),
    #[error("outbound destination is not a usable URL: {0}")]
    Malformed(String),
}

/// A deny-by-default host allow-list: an empty one allows nothing.
#[derive(Debug, Clone)]
pub struct OutboundPolicy {
    allowed_hosts: Vec<String>,
}

impl OutboundPolicy {
    /// Builds the policy from hosts (no port), lower-cased and
    /// de-duplicated.
    pub fn new(hosts: impl IntoIterator<Item = String>) -> Self {
        let mut allowed_hosts: Vec<String> = hosts.into_iter().map(|h| h.to_lowercase()).collect();
        allowed_hosts.sort();
        allowed_hosts.dedup();
        Self { allowed_hosts }
    }

    /// The hosts this policy allows.
    pub fn hosts(&self) -> &[String] {
        &self.allowed_hosts
    }

    /// Checks a destination URL: `http` or `https`, and its host (port
    /// ignored) on the allow-list.
    pub fn check_url(&self, url: &str) -> Result<(), OutboundError> {
        let parsed = Url::parse(url).map_err(|e| OutboundError::Malformed(e.to_string()))?;
        match parsed.scheme() {
            "http" | "https" => {}
            other => return Err(OutboundError::UnsupportedScheme(other.to_string())),
        }
        let host = parsed
            .host_str()
            .ok_or_else(|| OutboundError::Malformed(format!("'{url}' has no host component")))?
            .to_lowercase();
        if self.allowed_hosts.iter().any(|allowed| allowed == &host) {
            Ok(())
        } else {
            Err(OutboundError::HostNotAllowed(host))
        }
    }

    /// Checks the URL a `did:web` DID resolves to, with the same function
    /// `dcp_core::resolve_did` uses, so the check and the resolution can
    /// never disagree about the destination.
    pub fn check_did(&self, did: &str, insecure_http: bool) -> Result<(), OutboundError> {
        let url = dcp_core::did_web_to_url(did, insecure_http).map_err(OutboundError::Malformed)?;
        self.check_url(&url)
    }
}

/// The host a `did:web` DID resolves to, if it is one.
pub fn did_web_host(did: &str) -> Option<String> {
    let url = dcp_core::did_web_to_url(did, false).ok()?;
    Url::parse(&url).ok()?.host_str().map(str::to_lowercase)
}
