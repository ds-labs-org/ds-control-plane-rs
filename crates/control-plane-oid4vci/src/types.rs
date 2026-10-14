//! OID4VCI wire types: the Credential Offer, Credential Issuer metadata,
//! the OAuth 2.0 Authorization Server metadata and token exchange the
//! Pre-Authorized Code flow needs, and the Nonce / Credential / Deferred
//! Credential endpoint shapes. Property names follow
//! [OpenID for Verifiable Credential Issuance 1.0](https://openid.net/specs/openid-4-verifiable-credential-issuance-1_0.html)
//! exactly; anything the spec defines as an open extension point is kept
//! as `serde_json::Value` rather than modeled.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Grant type identifier for the Pre-Authorized Code flow (spec §4.1.1).
pub const PRE_AUTHORIZED_CODE_GRANT: &str = "urn:ietf:params:oauth:grant-type:pre-authorized_code";

/// Grant type identifier for the Authorization Code flow (spec §4.1.1).
pub const AUTHORIZATION_CODE_GRANT: &str = "authorization_code";

/// JWT `typ` header value for a key proof (spec Appendix F.1).
pub const PROOF_JWT_TYP: &str = "openid4vci-proof+jwt";

/// A Credential Offer object (spec §4.1.1), as sent by value in a
/// `credential_offer` parameter or served at a `credential_offer_uri`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CredentialOffer {
    pub credential_issuer: String,
    pub credential_configuration_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grants: Option<Grants>,
}

impl CredentialOffer {
    /// The Pre-Authorized Code grant, if the offer carries one.
    pub fn pre_authorized_code(&self) -> Option<&PreAuthorizedCodeGrant> {
        self.grants
            .as_ref()
            .and_then(|g| g.pre_authorized_code.as_ref())
    }
}

/// The `grants` object of a Credential Offer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Grants {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorization_code: Option<AuthorizationCodeGrant>,
    #[serde(
        rename = "urn:ietf:params:oauth:grant-type:pre-authorized_code",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub pre_authorized_code: Option<PreAuthorizedCodeGrant>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AuthorizationCodeGrant {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issuer_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorization_server: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreAuthorizedCodeGrant {
    #[serde(rename = "pre-authorized_code")]
    pub pre_authorized_code: String,
    /// Present when the issuer expects a Transaction Code (e.g. a PIN sent
    /// out of band) alongside the pre-authorized code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tx_code: Option<TxCode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorization_server: Option<String>,
}

/// Describes the Transaction Code the holder must supply (spec §4.1.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TxCode {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Credential Issuer metadata (spec §12.2), as served at
/// `/.well-known/openid-credential-issuer`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CredentialIssuerMetadata {
    pub credential_issuer: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub authorization_servers: Vec<String>,
    pub credential_endpoint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonce_endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deferred_credential_endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notification_endpoint: Option<String>,
    #[serde(default)]
    pub credential_configurations_supported: BTreeMap<String, CredentialConfiguration>,
}

/// One entry of `credential_configurations_supported`. Format-specific
/// parameters (`vct`, `credential_definition`, `doctype`, ...) are kept in
/// `extra` untouched.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CredentialConfiguration {
    pub format: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cryptographic_binding_methods_supported: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub credential_signing_alg_values_supported: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub proof_types_supported: BTreeMap<String, ProofTypeMetadata>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ProofTypeMetadata {
    #[serde(default)]
    pub proof_signing_alg_values_supported: Vec<String>,
}

/// The subset of OAuth 2.0 Authorization Server metadata (RFC 8414) this
/// wallet needs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuthorizationServerMetadata {
    pub issuer: String,
    pub token_endpoint: String,
    #[serde(
        rename = "pre-authorized_grant_anonymous_access_supported",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub pre_authorized_grant_anonymous_access_supported: Option<bool>,
}

/// Successful Token Response (spec §6.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub token_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in: Option<u64>,
    /// Only issued by pre-1.0 drafts; 1.0 moved it to the Nonce Endpoint.
    /// Still honoured when present so older issuers keep working.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub c_nonce: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub authorization_details: Vec<AuthorizationDetail>,
}

/// An `openid_credential` authorization detail returned in the Token
/// Response. When present, its `credential_identifiers` MUST be used in
/// the Credential Request instead of the configuration id (spec §6.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuthorizationDetail {
    #[serde(rename = "type")]
    pub type_: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_configuration_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub credential_identifiers: Vec<String>,
}

/// Nonce Response (spec §7.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NonceResponse {
    pub c_nonce: String,
}

/// Credential Request (spec §8.2). Exactly one of
/// `credential_identifier`/`credential_configuration_id` is set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CredentialRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_identifier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_configuration_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proofs: Option<Proofs>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Proofs {
    pub jwt: Vec<String>,
}

/// Credential Response (spec §8.3): either the issued credentials
/// (HTTP 200) or a `transaction_id` for deferred issuance (HTTP 202).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CredentialResponse {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub credentials: Vec<IssuedCredential>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transaction_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notification_id: Option<String>,
}

/// One entry of `credentials`. The credential itself is a string (e.g. a
/// JWT/SD-JWT) or a JSON object (e.g. an unsecured `ldp_vc`) depending on
/// the format, so it stays a raw `Value`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IssuedCredential {
    pub credential: serde_json::Value,
}

/// Deferred Credential Request (spec §9.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeferredCredentialRequest {
    pub transaction_id: String,
}

/// Notification Request (spec §11.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NotificationRequest {
    pub notification_id: String,
    pub event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_description: Option<String>,
}

/// An OAuth-style error body, as returned by the token, credential and
/// deferred credential endpoints.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_description: Option<String>,
}
