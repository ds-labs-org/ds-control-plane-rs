//! The verifier side of the Decentralized Claims Protocol (DCP) for
//! `ds-control-plane-rs`: authenticates the counterparty of a DSP request.
//!
//! A Consumer calls a DSP endpoint with `Authorization: Bearer <SI token>`,
//! a Self-Issued ID Token it signed with a key of its `did:web` DID, `aud`
//! the control plane's own DID, carrying in its `token` claim an access
//! token for its own Credential Service.
//! [`Dcp::verify_self_issued_token`] checks that envelope;
//! [`Dcp::verify_credentials`] then resolves the Consumer's DID document,
//! finds its `CredentialService`, queries
//! `{endpoint}/presentations/query` with a token of the control plane's own
//! (`aud` the Consumer's DID, the Consumer's access token forwarded as its
//! `token` claim), and verifies the presentation and every credential in
//! it. [`Dcp::authenticate`] does both with the configured scopes.
//!
//! Every outbound destination is checked against [`OutboundPolicy`] before
//! the request; issuers are trusted only when configured
//! ([`DcpConfig::trusted_issuers`], empty trusts nobody).
//!
//! Ported from `ds-identity-hub-rs` (`identity-hub-http/src/{auth,outbound,
//! validation}.rs`, `SeenJtiCache`), whose Credential Service this verifier
//! interoperates with; it depends only on `ds-dcp-core-rs`.

mod identity;
mod jti;
mod outbound;
mod verify;

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

pub use identity::{Identity, TOKEN_TTL_SECS, did_web_path};
pub use jti::{DEFAULT_JTI_CAPACITY, SeenJtiCache};
pub use outbound::{OutboundError, OutboundPolicy, did_web_host};
pub use verify::{
    CredentialClaims, VerifiedCredential, VerifiedToken, check_credential_claims,
    credential_type_of_scope,
};

/// The scope a DSP counterparty must hold credentials for unless configured
/// otherwise: a `MembershipCredential`.
pub const DEFAULT_REQUIRED_SCOPE: &str = "org.eclipse.dspace.dcp.vc.type:MembershipCredential:read";

/// How long one outbound request (DID resolution, presentation query) may
/// take.
pub const OUTBOUND_TIMEOUT: Duration = Duration::from_secs(10);

/// The verifier's configuration.
#[derive(Debug, Clone)]
pub struct DcpConfig {
    /// This connector's own `did:web` DID: the `aud` incoming tokens must
    /// name, and the `iss` of the tokens it mints.
    pub did: String,
    /// A P-256 private key (PKCS#8 or SEC1 PEM, or an EC JWK). `None`
    /// generates a fresh key at startup.
    pub key_file: Option<PathBuf>,
    /// Issuer DIDs whose credentials are accepted. Empty trusts nobody.
    pub trusted_issuers: Vec<String>,
    /// The scopes a counterparty must present credentials for, each
    /// `org.eclipse.dspace.dcp.vc.type:<Type>[:<suffix>]`.
    pub required_scopes: Vec<String>,
    /// Hosts the verifier may contact (counterparty DIDs and Credential
    /// Services). The hosts of [`Self::trusted_issuers`] are added.
    pub allowed_hosts: Vec<String>,
    /// Resolve `did:web` over `http` and accept `http` Credential Service
    /// endpoints. Tests and local setups only.
    pub insecure_http: bool,
}

impl DcpConfig {
    /// `did` with the defaults: a generated key, no trusted issuer, the
    /// [`DEFAULT_REQUIRED_SCOPE`], no allowed host, `https` only.
    pub fn new(did: impl Into<String>) -> Self {
        Self {
            did: did.into(),
            key_file: None,
            trusted_issuers: Vec::new(),
            required_scopes: vec![DEFAULT_REQUIRED_SCOPE.to_string()],
            allowed_hosts: Vec::new(),
            insecure_http: false,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DcpError {
    #[error("invalid DCP configuration: {0}")]
    Config(String),
    #[error("cannot load the DCP signing key: {0}")]
    Key(String),
    #[error("missing or malformed Authorization: Bearer header")]
    MissingToken,
    #[error("malformed token: {0}")]
    Malformed(String),
    #[error("could not resolve DID: {0}")]
    DidResolution(String),
    #[error("signature verification failed: {0}")]
    InvalidSignature(String),
    #[error("signing key '{0}' is not listed under the caller's capabilityInvocation")]
    KeyNotAuthorizedForInvocation(String),
    #[error("token sub does not match iss")]
    IssuerSubjectMismatch,
    #[error("token audience does not name this connector's DID")]
    WrongAudience,
    #[error("token has expired")]
    Expired,
    #[error("token is not yet valid (nbf is in the future)")]
    NotYetValid,
    #[error("token iat is in the future")]
    IssuedInFuture,
    #[error("token has no jti")]
    MissingJti,
    #[error("token jti has already been used (replay)")]
    TokenReplayed,
    #[error("outbound destination is not allowed: {0}")]
    OutboundDestinationNotAllowed(String),
    #[error("token carries no access token (`token` claim) for the Credential Service")]
    NoAccessToken,
    #[error("Credential Service query failed: {0}")]
    CredentialService(String),
    #[error("invalid presentation: {0}")]
    InvalidPresentation(String),
    #[error("invalid credential: {0}")]
    InvalidCredential(String),
    #[error("credential issuer '{0}' is not trusted")]
    UntrustedIssuer(String),
    #[error("credential subject is not the authenticated participant")]
    CredentialSubjectMismatch,
    #[error("credential has expired")]
    CredentialExpired,
    #[error("no valid credential of type '{0}' was presented")]
    MissingCredentialType(String),
}

/// The verifier: this connector's identity plus everything needed to check
/// a counterparty.
pub struct Dcp {
    config: DcpConfig,
    identity: Identity,
    required_types: Vec<String>,
    http: reqwest::Client,
    outbound: OutboundPolicy,
    seen_jti: Mutex<SeenJtiCache>,
}

impl Dcp {
    /// Checks `config` and loads (or, without a key file, generates with a
    /// warning) the signing key.
    pub fn new(config: DcpConfig) -> Result<Self, DcpError> {
        did_web_path(&config.did).map_err(DcpError::Config)?;
        if config.required_scopes.is_empty() {
            return Err(DcpError::Config(
                "at least one required scope must be configured".to_string(),
            ));
        }
        let required_types = config
            .required_scopes
            .iter()
            .map(|scope| {
                credential_type_of_scope(scope).ok_or_else(|| {
                    DcpError::Config(format!(
                        "scope '{scope}' is not org.eclipse.dspace.dcp.vc.type:<Type>[:<suffix>]"
                    ))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        for issuer in &config.trusted_issuers {
            if did_web_host(issuer).is_none() {
                return Err(DcpError::Config(format!(
                    "trusted issuer '{issuer}' is not a did:web DID"
                )));
            }
        }
        let identity = match &config.key_file {
            Some(path) => Identity::from_key_file(config.did.clone(), path)?,
            None => {
                tracing::warn!(
                    did = %config.did,
                    "no DCP key file configured: generated a signing key that lasts until this process exits"
                );
                Identity::generate(config.did.clone())
            }
        };
        let outbound = OutboundPolicy::new(
            config.allowed_hosts.iter().cloned().chain(
                config
                    .trusted_issuers
                    .iter()
                    .filter_map(|d| did_web_host(d)),
            ),
        );
        let http = reqwest::Client::builder()
            .timeout(OUTBOUND_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| DcpError::Config(e.to_string()))?;
        Ok(Self {
            config,
            identity,
            required_types,
            http,
            outbound,
            seen_jti: Mutex::new(SeenJtiCache::new(DEFAULT_JTI_CAPACITY)),
        })
    }

    pub fn config(&self) -> &DcpConfig {
        &self.config
    }

    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    pub fn did(&self) -> &str {
        self.identity.did()
    }

    pub fn outbound(&self) -> &OutboundPolicy {
        &self.outbound
    }

    /// The credential types [`DcpConfig::required_scopes`] name.
    pub fn required_types(&self) -> &[String] {
        &self.required_types
    }

    /// A Self-Issued ID Token from this connector to `audience`.
    pub fn mint_self_issued_token(&self, audience: &str, nested_token: Option<&str>) -> String {
        self.identity.mint_self_issued_token(audience, nested_token)
    }

    /// [`Self::verify_self_issued_token`] then [`Self::verify_credentials`]
    /// with the configured scopes: the participant behind a DSP request.
    pub async fn authenticate(
        &self,
        authorization: Option<&str>,
    ) -> Result<(VerifiedToken, Vec<VerifiedCredential>), DcpError> {
        let verified = self.verify_self_issued_token(authorization).await?;
        let credentials = self
            .verify_credentials(&verified, &self.config.required_scopes)
            .await?;
        Ok((verified, credentials))
    }
}

#[cfg(test)]
mod tests;
