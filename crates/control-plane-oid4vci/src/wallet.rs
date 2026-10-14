//! The holder side of the Pre-Authorized Code flow, end to end:
//!
//! 1. resolve the Credential Offer (by value, or GET `credential_offer_uri`);
//! 2. fetch Credential Issuer metadata and check it names the same issuer;
//! 3. fetch the Authorization Server's metadata and redeem the
//!    pre-authorized code (plus `tx_code`, if asked for) at its token
//!    endpoint;
//! 4. per offered credential configuration: get a fresh `c_nonce` from the
//!    Nonce Endpoint, sign a key proof, and call the Credential Endpoint —
//!    retrying once on `invalid_nonce`;
//! 5. hand back issued credentials, or a [`DeferredIssuance`] handle when
//!    the issuer answers with a `transaction_id`;
//! 6. best-effort `credential_accepted` notification.
//!
//! The Authorization Code flow is deliberately not implemented: it needs a
//! user-agent redirect, which a headless control plane does not have.

use reqwest::StatusCode;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::error::Oid4vciError;
use crate::offer::{OfferSource, well_known_url};
use crate::proof::HolderKey;
use crate::types::{
    AuthorizationServerMetadata, CredentialConfiguration, CredentialIssuerMetadata,
    CredentialOffer, CredentialRequest, CredentialResponse, DeferredCredentialRequest,
    ErrorResponse, NonceResponse, NotificationRequest, PRE_AUTHORIZED_CODE_GRANT, Proofs,
    TokenResponse,
};

/// What the caller wants issued.
#[derive(Debug, Clone, Default)]
pub struct IssuanceRequest {
    /// The Transaction Code, when the offer's grant carries a `tx_code`.
    pub tx_code: Option<String>,
    /// Restrict issuance to these configuration ids (each must be in the
    /// offer). `None` requests everything offered.
    pub credential_configuration_ids: Option<Vec<String>>,
}

/// The outcome of one offer.
#[derive(Debug, Clone, Serialize)]
pub struct IssuanceResult {
    pub credential_issuer: String,
    pub issued: Vec<IssuedCredentials>,
    pub deferred: Vec<DeferredIssuance>,
}

/// Credentials issued for one configuration (or one `credential_identifier`).
#[derive(Debug, Clone, Serialize)]
pub struct IssuedCredentials {
    pub credential_configuration_id: String,
    pub credential_identifier: Option<String>,
    pub format: Option<String>,
    pub credentials: Vec<serde_json::Value>,
}

/// Everything needed to poll the Deferred Credential Endpoint later.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeferredIssuance {
    pub credential_issuer: String,
    pub credential_configuration_id: String,
    pub format: Option<String>,
    pub deferred_credential_endpoint: String,
    pub notification_endpoint: Option<String>,
    pub transaction_id: String,
    /// Seconds the issuer asked the wallet to wait before polling.
    pub interval: Option<u64>,
    #[serde(skip_serializing)]
    pub access_token: String,
}

/// Result of polling a deferred issuance once.
#[derive(Debug, Clone)]
pub enum DeferredPoll {
    Issued(IssuedCredentials),
    Pending { interval: Option<u64> },
}

/// One unit of work for the credential endpoint: a configuration id and,
/// when the token response named them, a specific credential identifier.
struct Target {
    configuration_id: String,
    identifier: Option<String>,
}

/// An OID4VCI wallet bound to one holder key.
#[derive(Debug, Clone)]
pub struct Wallet {
    http: reqwest::Client,
    key: HolderKey,
}

impl Wallet {
    pub fn new(http: reqwest::Client, key: HolderKey) -> Self {
        Self { http, key }
    }

    /// A wallet with its own HTTP client and a bounded request timeout, so
    /// an unresponsive issuer cannot hold a caller indefinitely.
    pub fn with_key(key: HolderKey) -> Result<Self, Oid4vciError> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|source| Oid4vciError::Http {
                endpoint: "(client setup)".into(),
                source,
            })?;
        Ok(Self::new(http, key))
    }

    pub fn holder_key(&self) -> &HolderKey {
        &self.key
    }

    /// Resolves an offer source to the offer itself.
    pub async fn resolve_offer(
        &self,
        source: OfferSource,
    ) -> Result<CredentialOffer, Oid4vciError> {
        match source {
            OfferSource::ByValue(offer) => Ok(offer),
            OfferSource::ByReference(uri) => self.get_json(&uri).await,
        }
    }

    /// Fetches Credential Issuer metadata. Tries the 1.0 location (well-known
    /// inserted before the path) first, then the pre-1.0 location
    /// (appended), which several deployed issuers still use; the two are
    /// identical for an issuer identifier without a path.
    pub async fn issuer_metadata(
        &self,
        credential_issuer: &str,
    ) -> Result<CredentialIssuerMetadata, Oid4vciError> {
        let inserted = well_known_url(credential_issuer, "openid-credential-issuer")?;
        let appended = format!(
            "{}/.well-known/openid-credential-issuer",
            credential_issuer.trim_end_matches('/')
        );
        let metadata: CredentialIssuerMetadata = self.get_json_first(&[inserted, appended]).await?;
        if metadata.credential_issuer.trim_end_matches('/')
            != credential_issuer.trim_end_matches('/')
        {
            return Err(Oid4vciError::InvalidMetadata(format!(
                "metadata names credential_issuer '{}', expected '{credential_issuer}'",
                metadata.credential_issuer
            )));
        }
        Ok(metadata)
    }

    /// Fetches RFC 8414 metadata for `issuer`, falling back to OpenID
    /// Connect discovery.
    pub async fn authorization_server_metadata(
        &self,
        issuer: &str,
    ) -> Result<AuthorizationServerMetadata, Oid4vciError> {
        let rfc8414 = well_known_url(issuer, "oauth-authorization-server")?;
        let oidc = format!(
            "{}/.well-known/openid-configuration",
            issuer.trim_end_matches('/')
        );
        self.get_json_first(&[rfc8414, oidc]).await
    }

    /// Runs the whole Pre-Authorized Code flow for `source`.
    pub async fn accept_offer(
        &self,
        source: OfferSource,
        request: IssuanceRequest,
    ) -> Result<IssuanceResult, Oid4vciError> {
        let offer = self.resolve_offer(source).await?;
        let grant = offer
            .pre_authorized_code()
            .ok_or(Oid4vciError::UnsupportedGrant)?
            .clone();
        if grant.tx_code.is_some() && request.tx_code.is_none() {
            return Err(Oid4vciError::TxCodeRequired);
        }

        let wanted = match &request.credential_configuration_ids {
            Some(ids) => {
                if let Some(missing) = ids
                    .iter()
                    .find(|id| !offer.credential_configuration_ids.contains(id))
                {
                    return Err(Oid4vciError::UnknownCredentialConfiguration(
                        missing.clone(),
                    ));
                }
                ids.clone()
            }
            None => offer.credential_configuration_ids.clone(),
        };

        let metadata = self.issuer_metadata(&offer.credential_issuer).await?;
        for id in &wanted {
            if !metadata
                .credential_configurations_supported
                .contains_key(id)
            {
                return Err(Oid4vciError::UnknownCredentialConfiguration(id.clone()));
            }
        }

        let as_issuer = grant
            .authorization_server
            .clone()
            .or_else(|| metadata.authorization_servers.first().cloned())
            .unwrap_or_else(|| metadata.credential_issuer.clone());
        let as_metadata = self.authorization_server_metadata(&as_issuer).await?;

        let mut form = vec![
            ("grant_type", PRE_AUTHORIZED_CODE_GRANT.to_string()),
            ("pre-authorized_code", grant.pre_authorized_code.clone()),
        ];
        if let Some(tx_code) = &request.tx_code {
            form.push(("tx_code", tx_code.clone()));
        }
        let token: TokenResponse = self
            .send_json(
                &as_metadata.token_endpoint,
                self.http.post(&as_metadata.token_endpoint).form(&form),
            )
            .await?
            .1;
        if !token.token_type.eq_ignore_ascii_case("bearer") {
            return Err(Oid4vciError::InvalidMetadata(format!(
                "unsupported token_type '{}' (only Bearer; DPoP is not implemented)",
                token.token_type
            )));
        }

        // Spec §6.2: if the token response carries credential identifiers
        // for a configuration, the credential request MUST use them.
        let mut targets = Vec::new();
        for id in &wanted {
            let identifiers: Vec<String> = token
                .authorization_details
                .iter()
                .filter(|d| {
                    d.type_ == "openid_credential"
                        && d.credential_configuration_id.as_deref() == Some(id)
                })
                .flat_map(|d| d.credential_identifiers.iter().cloned())
                .collect();
            if identifiers.is_empty() {
                targets.push(Target {
                    configuration_id: id.clone(),
                    identifier: None,
                });
            } else {
                targets.extend(identifiers.into_iter().map(|ident| Target {
                    configuration_id: id.clone(),
                    identifier: Some(ident),
                }));
            }
        }

        let mut result = IssuanceResult {
            credential_issuer: metadata.credential_issuer.clone(),
            issued: Vec::new(),
            deferred: Vec::new(),
        };
        let mut token_nonce = token.c_nonce.clone();
        for target in targets {
            let config = &metadata.credential_configurations_supported[&target.configuration_id];
            let response = self
                .request_credential(
                    &metadata,
                    config,
                    &target,
                    &token.access_token,
                    token_nonce.take(),
                )
                .await?;
            let format = Some(config.format.clone());
            if let Some(transaction_id) = response.transaction_id {
                let endpoint = metadata
                    .deferred_credential_endpoint
                    .clone()
                    .ok_or_else(|| {
                        Oid4vciError::InvalidMetadata(
                        "issuer deferred issuance but advertises no deferred_credential_endpoint"
                            .into(),
                    )
                    })?;
                result.deferred.push(DeferredIssuance {
                    credential_issuer: metadata.credential_issuer.clone(),
                    credential_configuration_id: target.configuration_id,
                    format,
                    deferred_credential_endpoint: endpoint,
                    notification_endpoint: metadata.notification_endpoint.clone(),
                    transaction_id,
                    interval: response.interval,
                    access_token: token.access_token.clone(),
                });
            } else {
                self.notify_accepted(
                    metadata.notification_endpoint.as_deref(),
                    response.notification_id.as_deref(),
                    &token.access_token,
                )
                .await;
                result.issued.push(IssuedCredentials {
                    credential_configuration_id: target.configuration_id,
                    credential_identifier: target.identifier,
                    format,
                    credentials: response
                        .credentials
                        .into_iter()
                        .map(|c| c.credential)
                        .collect(),
                });
            }
        }
        Ok(result)
    }

    /// Polls the Deferred Credential Endpoint once.
    pub async fn poll_deferred(&self, d: &DeferredIssuance) -> Result<DeferredPoll, Oid4vciError> {
        let (status, response): (StatusCode, CredentialResponse) = self
            .send_json(
                &d.deferred_credential_endpoint,
                self.http
                    .post(&d.deferred_credential_endpoint)
                    .bearer_auth(&d.access_token)
                    .json(&DeferredCredentialRequest {
                        transaction_id: d.transaction_id.clone(),
                    }),
            )
            .await?;
        if status == StatusCode::ACCEPTED || response.credentials.is_empty() {
            return Ok(DeferredPoll::Pending {
                interval: response.interval.or(d.interval),
            });
        }
        self.notify_accepted(
            d.notification_endpoint.as_deref(),
            response.notification_id.as_deref(),
            &d.access_token,
        )
        .await;
        Ok(DeferredPoll::Issued(IssuedCredentials {
            credential_configuration_id: d.credential_configuration_id.clone(),
            credential_identifier: None,
            format: d.format.clone(),
            credentials: response
                .credentials
                .into_iter()
                .map(|c| c.credential)
                .collect(),
        }))
    }

    async fn request_credential(
        &self,
        metadata: &CredentialIssuerMetadata,
        config: &CredentialConfiguration,
        target: &Target,
        access_token: &str,
        token_nonce: Option<String>,
    ) -> Result<CredentialResponse, Oid4vciError> {
        // No proof types and no binding methods advertised means the
        // credential is not key-bound, and the spec says not to send one.
        let needs_proof = !config.proof_types_supported.is_empty()
            || !config.cryptographic_binding_methods_supported.is_empty();
        if let Some(jwt) = config.proof_types_supported.get("jwt")
            && !jwt.proof_signing_alg_values_supported.is_empty()
            && !jwt
                .proof_signing_alg_values_supported
                .iter()
                .any(|a| a == "ES256")
        {
            return Err(Oid4vciError::InvalidMetadata(format!(
                "configuration '{}' does not accept ES256 key proofs",
                target.configuration_id
            )));
        }

        let mut nonce = match token_nonce {
            Some(n) => Some(n),
            None => self.fresh_nonce(metadata).await?,
        };
        let mut retried = false;
        loop {
            let proofs = if needs_proof {
                Some(Proofs {
                    jwt: vec![self.key.proof_jwt(
                        &metadata.credential_issuer,
                        nonce.as_deref(),
                        None,
                    )?],
                })
            } else {
                None
            };
            let body = CredentialRequest {
                credential_identifier: target.identifier.clone(),
                credential_configuration_id: target
                    .identifier
                    .is_none()
                    .then(|| target.configuration_id.clone()),
                proofs,
            };
            let endpoint = &metadata.credential_endpoint;
            match self
                .send_json::<CredentialResponse>(
                    endpoint,
                    self.http
                        .post(endpoint)
                        .bearer_auth(access_token)
                        .json(&body),
                )
                .await
            {
                Ok((_, response)) => return Ok(response),
                Err(Oid4vciError::Issuer {
                    body: Some(ErrorResponse { error, .. }),
                    ..
                }) if error == "invalid_nonce" && needs_proof && !retried => {
                    retried = true;
                    nonce = self.fresh_nonce(metadata).await?;
                    if nonce.is_none() {
                        return Err(Oid4vciError::InvalidMetadata(
                            "issuer rejected the nonce but advertises no nonce_endpoint".into(),
                        ));
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }

    async fn fresh_nonce(
        &self,
        metadata: &CredentialIssuerMetadata,
    ) -> Result<Option<String>, Oid4vciError> {
        let Some(endpoint) = &metadata.nonce_endpoint else {
            return Ok(None);
        };
        let (_, response): (_, NonceResponse) =
            self.send_json(endpoint, self.http.post(endpoint)).await?;
        Ok(Some(response.c_nonce))
    }

    /// Notifications are advisory (spec §11): a failure is logged, never
    /// surfaced, since the credential is already in hand.
    async fn notify_accepted(
        &self,
        endpoint: Option<&str>,
        notification_id: Option<&str>,
        access_token: &str,
    ) {
        let (Some(endpoint), Some(notification_id)) = (endpoint, notification_id) else {
            return;
        };
        let body = NotificationRequest {
            notification_id: notification_id.to_string(),
            event: "credential_accepted".into(),
            event_description: None,
        };
        let sent = self
            .http
            .post(endpoint)
            .bearer_auth(access_token)
            .json(&body)
            .send()
            .await;
        match sent {
            Ok(r) if r.status().is_success() => {}
            Ok(r) => {
                tracing::warn!(status = %r.status(), endpoint, "credential notification rejected")
            }
            Err(e) => tracing::warn!(error = %e, endpoint, "credential notification failed"),
        }
    }

    async fn get_json<T: DeserializeOwned>(&self, url: &str) -> Result<T, Oid4vciError> {
        Ok(self.send_json(url, self.http.get(url)).await?.1)
    }

    /// GETs each URL in turn and returns the first success; on total
    /// failure, returns the first URL's error.
    async fn get_json_first<T: DeserializeOwned>(
        &self,
        urls: &[String],
    ) -> Result<T, Oid4vciError> {
        let mut first_err = None;
        for url in urls {
            match self.get_json(url).await {
                Ok(v) => return Ok(v),
                Err(e) => {
                    first_err.get_or_insert(e);
                }
            }
        }
        Err(first_err.expect("at least one URL"))
    }

    async fn send_json<T: DeserializeOwned>(
        &self,
        endpoint: &str,
        request: reqwest::RequestBuilder,
    ) -> Result<(StatusCode, T), Oid4vciError> {
        let http_err = |source| Oid4vciError::Http {
            endpoint: endpoint.to_string(),
            source,
        };
        let response = request.send().await.map_err(http_err)?;
        let status = response.status();
        if !status.is_success() {
            let body = response.json::<ErrorResponse>().await.ok();
            return Err(Oid4vciError::Issuer {
                endpoint: endpoint.to_string(),
                status: status.as_u16(),
                body,
            });
        }
        let value = response.json::<T>().await.map_err(http_err)?;
        Ok((status, value))
    }
}
