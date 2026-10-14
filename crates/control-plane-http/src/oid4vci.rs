//! OID4VCI holder management API: lets the connector's operator hand the
//! control plane a Credential Offer and have it obtain key-bound
//! credentials for this connector, using `control-plane-oid4vci`'s
//! Pre-Authorized Code flow.
//!
//! This is an operator-facing management surface, not part of DSP, so it
//! is self-contained: its own router, state and configuration, merged into
//! the main router only when `MANAGEMENT_API_KEY` is set. Every route
//! requires that key in `X-Api-Key`; without it nothing is mounted, since
//! these routes make outbound requests to caller-chosen URLs.
//!
//! Credentials and pending deferred issuances are held in memory, like the
//! rest of this bootstrap's state.
//!
//! - `GET  /management/oid4vci/holder`              holder key (public JWK, `did:jwk`)
//! - `POST /management/oid4vci/offers`              accept a Credential Offer
//! - `GET  /management/oid4vci/credentials`         list received credentials
//! - `GET  /management/oid4vci/credentials/{id}`    one received credential
//! - `GET  /management/oid4vci/deferred`            list pending deferred issuances
//! - `POST /management/oid4vci/deferred/{id}`       poll one deferred issuance

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use control_plane_oid4vci::{
    DeferredIssuance, DeferredPoll, HolderKey, IssuanceRequest, IssuedCredentials, OfferSource,
    Oid4vciError, Wallet,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

pub const API_KEY_HEADER: &str = "x-api-key";

/// Configuration for the OID4VCI management API, read from its own
/// environment variables.
#[derive(Clone, Default)]
pub struct Oid4vciConfig {
    /// `MANAGEMENT_API_KEY`. When unset, the management API is not mounted.
    pub management_api_key: Option<String>,
    /// `OID4VCI_HOLDER_JWK`: the holder's private P-256 key as an EC JWK.
    /// When unset, a fresh key is generated at startup, so credentials
    /// obtained before a restart are bound to a key that no longer exists.
    pub holder_jwk: Option<String>,
}

impl std::fmt::Debug for Oid4vciConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Oid4vciConfig")
            .field(
                "management_api_key",
                &self.management_api_key.as_ref().map(|_| "<set>"),
            )
            .field("holder_jwk", &self.holder_jwk.as_ref().map(|_| "<set>"))
            .finish()
    }
}

impl Oid4vciConfig {
    pub fn from_env() -> Self {
        let non_empty = |k| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        Self {
            management_api_key: non_empty("MANAGEMENT_API_KEY"),
            holder_jwk: non_empty("OID4VCI_HOLDER_JWK"),
        }
    }
}

/// A credential the control plane received and holds.
#[derive(Debug, Clone, Serialize)]
pub struct StoredCredential {
    pub id: String,
    pub credential_issuer: String,
    pub credential_configuration_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_identifier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    pub credential: serde_json::Value,
    pub received_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PendingDeferred {
    pub id: String,
    #[serde(flatten)]
    pub issuance: DeferredIssuance,
    pub created_at: DateTime<Utc>,
}

pub struct Oid4vciState {
    api_key: String,
    wallet: Wallet,
    credentials: Mutex<Vec<StoredCredential>>,
    deferred: Mutex<HashMap<String, PendingDeferred>>,
}

impl Oid4vciState {
    fn store(&self, credential_issuer: &str, issued: IssuedCredentials) -> Vec<StoredCredential> {
        let now = Utc::now();
        let stored: Vec<StoredCredential> = issued
            .credentials
            .into_iter()
            .map(|credential| StoredCredential {
                id: uuid::Uuid::new_v4().to_string(),
                credential_issuer: credential_issuer.to_string(),
                credential_configuration_id: issued.credential_configuration_id.clone(),
                credential_identifier: issued.credential_identifier.clone(),
                format: issued.format.clone(),
                credential,
                received_at: now,
            })
            .collect();
        self.credentials
            .lock()
            .expect("credentials lock poisoned")
            .extend(stored.iter().cloned());
        stored
    }
}

/// Builds the management router, or `None` when no API key is configured.
pub fn router(config: Oid4vciConfig) -> anyhow::Result<Option<Router>> {
    let Some(api_key) = config.management_api_key else {
        return Ok(None);
    };
    let key = match &config.holder_jwk {
        Some(jwk) => HolderKey::from_private_jwk(jwk)?,
        None => {
            tracing::warn!(
                "OID4VCI_HOLDER_JWK not set: generated an ephemeral holder key; \
                 credentials will be bound to a key lost on restart"
            );
            HolderKey::generate()
        }
    };
    tracing::info!(holder = %key.did_jwk(), "OID4VCI holder management API enabled");
    let state = Arc::new(Oid4vciState {
        api_key,
        wallet: Wallet::with_key(key)?,
        credentials: Mutex::new(Vec::new()),
        deferred: Mutex::new(HashMap::new()),
    });

    Ok(Some(
        Router::new()
            .route("/management/oid4vci/holder", get(holder))
            .route("/management/oid4vci/offers", post(accept_offer))
            .route("/management/oid4vci/credentials", get(list_credentials))
            .route("/management/oid4vci/credentials/{id}", get(get_credential))
            .route("/management/oid4vci/deferred", get(list_deferred))
            .route("/management/oid4vci/deferred/{id}", post(poll_deferred))
            .route_layer(middleware::from_fn_with_state(
                state.clone(),
                require_api_key,
            ))
            .with_state(state),
    ))
}

async fn require_api_key(
    State(state): State<Arc<Oid4vciState>>,
    request: Request,
    next: Next,
) -> Response {
    let presented = request
        .headers()
        .get(API_KEY_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if constant_time_eq(presented.as_bytes(), state.api_key.as_bytes()) {
        next.run(request).await
    } else {
        error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "missing or invalid X-Api-Key".into(),
        )
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn error(status: StatusCode, code: &str, message: String) -> Response {
    (status, Json(json!({ "error": code, "message": message }))).into_response()
}

fn wallet_error(e: Oid4vciError) -> Response {
    let (status, code) = match &e {
        Oid4vciError::InvalidOffer(_)
        | Oid4vciError::UnsupportedGrant
        | Oid4vciError::TxCodeRequired
        | Oid4vciError::UnknownCredentialConfiguration(_) => {
            (StatusCode::BAD_REQUEST, "invalid_request")
        }
        Oid4vciError::Issuer { .. }
        | Oid4vciError::Http { .. }
        | Oid4vciError::InvalidMetadata(_) => (StatusCode::BAD_GATEWAY, "issuer_error"),
        Oid4vciError::Proof(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal_error"),
    };
    let mut body = json!({ "error": code, "message": e.to_string() });
    if let Oid4vciError::Issuer {
        body: Some(issuer), ..
    } = &e
    {
        body["issuer_error"] = json!(issuer);
    }
    (status, Json(body)).into_response()
}

async fn holder(State(state): State<Arc<Oid4vciState>>) -> impl IntoResponse {
    let key = state.wallet.holder_key();
    let jwk: serde_json::Value =
        serde_json::from_str(&key.public_jwk().to_string()).unwrap_or_default();
    Json(json!({ "did": key.did_jwk(), "jwk": jwk }))
}

/// `offer` is either an offer URI (`openid-credential-offer://?...`) or the
/// Credential Offer JSON object itself.
#[derive(Debug, Deserialize)]
pub struct AcceptOfferRequest {
    pub offer: serde_json::Value,
    #[serde(default)]
    pub tx_code: Option<String>,
    #[serde(default)]
    pub credential_configuration_ids: Option<Vec<String>>,
}

async fn accept_offer(
    State(state): State<Arc<Oid4vciState>>,
    Json(req): Json<AcceptOfferRequest>,
) -> Response {
    let source = match &req.offer {
        serde_json::Value::String(uri) => OfferSource::parse(uri),
        obj @ serde_json::Value::Object(_) => serde_json::from_value(obj.clone())
            .map(OfferSource::ByValue)
            .map_err(|e| Oid4vciError::InvalidOffer(e.to_string())),
        _ => Err(Oid4vciError::InvalidOffer(
            "offer must be an offer URI string or a Credential Offer object".into(),
        )),
    };
    let source = match source {
        Ok(s) => s,
        Err(e) => return wallet_error(e),
    };

    let result = match state
        .wallet
        .accept_offer(
            source,
            IssuanceRequest {
                tx_code: req.tx_code,
                credential_configuration_ids: req.credential_configuration_ids,
            },
        )
        .await
    {
        Ok(r) => r,
        Err(e) => return wallet_error(e),
    };

    let credentials: Vec<StoredCredential> = result
        .issued
        .into_iter()
        .flat_map(|issued| state.store(&result.credential_issuer, issued))
        .collect();
    let now = Utc::now();
    let deferred: Vec<PendingDeferred> = result
        .deferred
        .into_iter()
        .map(|issuance| PendingDeferred {
            id: uuid::Uuid::new_v4().to_string(),
            issuance,
            created_at: now,
        })
        .collect();
    state
        .deferred
        .lock()
        .expect("deferred lock poisoned")
        .extend(deferred.iter().map(|d| (d.id.clone(), d.clone())));

    Json(json!({
        "credential_issuer": result.credential_issuer,
        "credentials": credentials,
        "deferred": deferred,
    }))
    .into_response()
}

async fn list_credentials(State(state): State<Arc<Oid4vciState>>) -> impl IntoResponse {
    Json(
        state
            .credentials
            .lock()
            .expect("credentials lock poisoned")
            .clone(),
    )
}

async fn get_credential(
    State(state): State<Arc<Oid4vciState>>,
    Path(id): Path<String>,
) -> Response {
    let credentials = state.credentials.lock().expect("credentials lock poisoned");
    match credentials.iter().find(|c| c.id == id) {
        Some(c) => Json(c.clone()).into_response(),
        None => error(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("no credential '{id}'"),
        ),
    }
}

async fn list_deferred(State(state): State<Arc<Oid4vciState>>) -> impl IntoResponse {
    let deferred = state.deferred.lock().expect("deferred lock poisoned");
    Json(deferred.values().cloned().collect::<Vec<_>>())
}

async fn poll_deferred(State(state): State<Arc<Oid4vciState>>, Path(id): Path<String>) -> Response {
    let pending = state
        .deferred
        .lock()
        .expect("deferred lock poisoned")
        .get(&id)
        .cloned();
    let Some(pending) = pending else {
        return error(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("no deferred issuance '{id}'"),
        );
    };

    match state.wallet.poll_deferred(&pending.issuance).await {
        Ok(DeferredPoll::Pending { interval }) => (
            StatusCode::ACCEPTED,
            Json(json!({ "status": "pending", "interval": interval })),
        )
            .into_response(),
        Ok(DeferredPoll::Issued(issued)) => {
            state
                .deferred
                .lock()
                .expect("deferred lock poisoned")
                .remove(&id);
            let credentials = state.store(&pending.issuance.credential_issuer, issued);
            Json(json!({ "status": "issued", "credentials": credentials })).into_response()
        }
        Err(e) => wallet_error(e),
    }
}
