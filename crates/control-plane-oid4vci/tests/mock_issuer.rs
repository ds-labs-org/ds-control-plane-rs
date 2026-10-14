//! Drives the full Pre-Authorized Code flow against an in-process mock
//! issuer that checks what a real OID4VCI 1.0 issuer checks: the
//! pre-authorized code and tx_code, the bearer token, the key proof's
//! signature, `typ`, `aud` and `nonce`, and the 1.0 request shape.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use control_plane_oid4vci::{
    DeferredPoll, HolderKey, IssuanceRequest, OfferSource, Oid4vciError, Wallet,
};
use p256::ecdsa::signature::Verifier;
use p256::ecdsa::{Signature, VerifyingKey};
use serde_json::{Value, json};

const PRE_AUTH_CODE: &str = "pre-auth-123";
const TX_CODE: &str = "4711";
const ACCESS_TOKEN: &str = "at-xyz";

#[derive(Default)]
struct IssuerState {
    base: String,
    nonces: HashSet<String>,
    /// Makes the next credential request fail with `invalid_nonce` once.
    reject_next_nonce: bool,
    /// Configuration id that is issued deferred.
    deferred_ready: bool,
    notifications: Vec<Value>,
    proof_jwks: Vec<Value>,
}

type S = Arc<Mutex<IssuerState>>;

fn err(status: StatusCode, error: &str) -> Response {
    (status, Json(json!({ "error": error }))).into_response()
}

fn bearer_ok(headers: &HeaderMap) -> bool {
    headers.get("authorization").and_then(|v| v.to_str().ok())
        == Some(&format!("Bearer {ACCESS_TOKEN}"))
}

async fn issuer_metadata(State(s): State<S>) -> Json<Value> {
    let base = s.lock().unwrap().base.clone();
    Json(json!({
        "credential_issuer": base,
        "credential_endpoint": format!("{base}/credential"),
        "nonce_endpoint": format!("{base}/nonce"),
        "deferred_credential_endpoint": format!("{base}/deferred"),
        "notification_endpoint": format!("{base}/notification"),
        "credential_configurations_supported": {
            "MembershipCredential": {
                "format": "jwt_vc_json",
                "cryptographic_binding_methods_supported": ["jwk"],
                "proof_types_supported": { "jwt": { "proof_signing_alg_values_supported": ["ES256"] } },
                "credential_definition": { "type": ["VerifiableCredential", "MembershipCredential"] }
            },
            "DataProcessorCredential": {
                "format": "jwt_vc_json",
                "cryptographic_binding_methods_supported": ["jwk"],
                "proof_types_supported": { "jwt": { "proof_signing_alg_values_supported": ["ES256"] } }
            }
        }
    }))
}

async fn as_metadata(State(s): State<S>) -> Json<Value> {
    let base = s.lock().unwrap().base.clone();
    Json(json!({ "issuer": base, "token_endpoint": format!("{base}/token") }))
}

async fn offer(State(s): State<S>) -> Json<Value> {
    let base = s.lock().unwrap().base.clone();
    Json(json!({
        "credential_issuer": base,
        "credential_configuration_ids": ["MembershipCredential", "DataProcessorCredential"],
        "grants": { "urn:ietf:params:oauth:grant-type:pre-authorized_code": {
            "pre-authorized_code": PRE_AUTH_CODE,
            "tx_code": { "input_mode": "numeric", "length": 4 }
        } }
    }))
}

async fn token(Form(form): Form<Vec<(String, String)>>) -> Response {
    let get = |k: &str| form.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
    if get("grant_type") != Some("urn:ietf:params:oauth:grant-type:pre-authorized_code")
        || get("pre-authorized_code") != Some(PRE_AUTH_CODE)
    {
        return err(StatusCode::BAD_REQUEST, "invalid_grant");
    }
    if get("tx_code") != Some(TX_CODE) {
        return err(StatusCode::BAD_REQUEST, "invalid_grant");
    }
    Json(json!({
        "access_token": ACCESS_TOKEN,
        "token_type": "Bearer",
        "expires_in": 300,
        "authorization_details": [{
            "type": "openid_credential",
            "credential_configuration_id": "MembershipCredential",
            "credential_identifiers": ["membership-2026"]
        }]
    }))
    .into_response()
}

async fn nonce(State(s): State<S>) -> Json<Value> {
    let n = uuid::Uuid::new_v4().to_string();
    s.lock().unwrap().nonces.insert(n.clone());
    Json(json!({ "c_nonce": n }))
}

fn b64json(part: &str) -> Value {
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(part).unwrap()).unwrap()
}

async fn credential(State(s): State<S>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    if !bearer_ok(&headers) {
        return err(StatusCode::UNAUTHORIZED, "invalid_token");
    }
    // 1.0 shape: `proofs`, never the draft-era `proof`/`format`.
    assert!(body.get("proof").is_none() && body.get("format").is_none());

    let jwt = body["proofs"]["jwt"][0].as_str().expect("one jwt proof");
    let parts: Vec<&str> = jwt.split('.').collect();
    let header = b64json(parts[0]);
    let claims = b64json(parts[1]);
    assert_eq!(header["typ"], "openid4vci-proof+jwt");
    let jwk = p256::elliptic_curve::JwkEcKey::from_str(&header["jwk"].to_string()).unwrap();
    let vk = VerifyingKey::from(p256::PublicKey::from_jwk(&jwk).unwrap());
    let sig = Signature::from_slice(&URL_SAFE_NO_PAD.decode(parts[2]).unwrap()).unwrap();
    if vk
        .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &sig)
        .is_err()
    {
        return err(StatusCode::BAD_REQUEST, "invalid_proof");
    }

    let mut st = s.lock().unwrap();
    assert_eq!(claims["aud"], st.base.as_str());
    let n = claims["nonce"].as_str().unwrap_or_default().to_string();
    if st.reject_next_nonce {
        st.reject_next_nonce = false;
        return err(StatusCode::BAD_REQUEST, "invalid_nonce");
    }
    if !st.nonces.remove(&n) {
        return err(StatusCode::BAD_REQUEST, "invalid_nonce");
    }
    st.proof_jwks.push(header["jwk"].clone());

    match (
        body["credential_identifier"].as_str(),
        body["credential_configuration_id"].as_str(),
    ) {
        // The token response named an identifier, so it must be used.
        (Some("membership-2026"), None) => Json(json!({
            "credentials": [{ "credential": "eyJ.membership.vc" }],
            "notification_id": "notif-1"
        }))
        .into_response(),
        (None, Some("DataProcessorCredential")) => (
            StatusCode::ACCEPTED,
            Json(json!({ "transaction_id": "tx-dp", "interval": 1 })),
        )
            .into_response(),
        _ => err(StatusCode::BAD_REQUEST, "invalid_credential_request"),
    }
}

async fn deferred(State(s): State<S>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    if !bearer_ok(&headers) {
        return err(StatusCode::UNAUTHORIZED, "invalid_token");
    }
    if body["transaction_id"] != "tx-dp" {
        return err(StatusCode::BAD_REQUEST, "invalid_transaction_id");
    }
    let mut st = s.lock().unwrap();
    if !st.deferred_ready {
        st.deferred_ready = true;
        return (
            StatusCode::ACCEPTED,
            Json(json!({ "transaction_id": "tx-dp", "interval": 2 })),
        )
            .into_response();
    }
    Json(json!({ "credentials": [{ "credential": "eyJ.dataprocessor.vc" }] })).into_response()
}

async fn notification(State(s): State<S>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    assert!(bearer_ok(&headers));
    s.lock().unwrap().notifications.push(body);
    StatusCode::NO_CONTENT.into_response()
}

async fn start_issuer() -> (String, S) {
    let state: S = Arc::default();
    let app = Router::new()
        .route(
            "/.well-known/openid-credential-issuer",
            get(issuer_metadata),
        )
        .route("/.well-known/oauth-authorization-server", get(as_metadata))
        .route("/offers/1", get(offer))
        .route("/token", post(token))
        .route("/nonce", post(nonce))
        .route("/credential", post(credential))
        .route("/deferred", post(deferred))
        .route("/notification", post(notification))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    state.lock().unwrap().base = base.clone();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, state)
}

fn offer_uri(base: &str) -> String {
    let mut uri = url::Url::parse("openid-credential-offer://").unwrap();
    uri.query_pairs_mut()
        .append_pair("credential_offer_uri", &format!("{base}/offers/1"));
    uri.to_string()
}

#[tokio::test]
async fn pre_authorized_flow_issues_and_defers() {
    let (base, state) = start_issuer().await;
    state.lock().unwrap().reject_next_nonce = true;
    let key = HolderKey::generate();
    let wallet = Wallet::new(reqwest::Client::new(), key.clone());

    let result = wallet
        .accept_offer(
            OfferSource::parse(&offer_uri(&base)).unwrap(),
            IssuanceRequest {
                tx_code: Some(TX_CODE.into()),
                credential_configuration_ids: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(result.credential_issuer, base);
    assert_eq!(result.issued.len(), 1);
    let issued = &result.issued[0];
    assert_eq!(issued.credential_configuration_id, "MembershipCredential");
    assert_eq!(
        issued.credential_identifier.as_deref(),
        Some("membership-2026")
    );
    assert_eq!(issued.credentials, vec![json!("eyJ.membership.vc")]);

    assert_eq!(result.deferred.len(), 1);
    let d = &result.deferred[0];
    assert_eq!(d.transaction_id, "tx-dp");

    // First poll: still pending, with the issuer's new interval.
    match wallet.poll_deferred(d).await.unwrap() {
        DeferredPoll::Pending { interval } => assert_eq!(interval, Some(2)),
        other => panic!("expected pending, got {other:?}"),
    }
    match wallet.poll_deferred(d).await.unwrap() {
        DeferredPoll::Issued(c) => {
            assert_eq!(c.credential_configuration_id, "DataProcessorCredential");
            assert_eq!(c.credentials, vec![json!("eyJ.dataprocessor.vc")]);
        }
        other => panic!("expected issued, got {other:?}"),
    }

    let st = state.lock().unwrap();
    assert_eq!(
        st.notifications,
        vec![json!({ "notification_id": "notif-1", "event": "credential_accepted" })]
    );
    // Every accepted proof was bound to the wallet's own key.
    let own: Value = serde_json::from_str(&key.public_jwk().to_string()).unwrap();
    assert!(!st.proof_jwks.is_empty());
    assert!(st.proof_jwks.iter().all(|j| *j == own));
}

#[tokio::test]
async fn missing_tx_code_is_refused_before_any_token_request() {
    let (base, _) = start_issuer().await;
    let wallet = Wallet::new(reqwest::Client::new(), HolderKey::generate());
    let err = wallet
        .accept_offer(
            OfferSource::parse(&offer_uri(&base)).unwrap(),
            IssuanceRequest::default(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Oid4vciError::TxCodeRequired), "{err}");
}

#[tokio::test]
async fn wrong_tx_code_surfaces_the_issuer_error() {
    let (base, _) = start_issuer().await;
    let wallet = Wallet::new(reqwest::Client::new(), HolderKey::generate());
    let err = wallet
        .accept_offer(
            OfferSource::parse(&offer_uri(&base)).unwrap(),
            IssuanceRequest {
                tx_code: Some("0000".into()),
                credential_configuration_ids: None,
            },
        )
        .await
        .unwrap_err();
    match err {
        Oid4vciError::Issuer { status, body, .. } => {
            assert_eq!(status, 400);
            assert_eq!(body.unwrap().error, "invalid_grant");
        }
        other => panic!("unexpected error {other}"),
    }
}

#[tokio::test]
async fn unoffered_configuration_is_rejected() {
    let (base, _) = start_issuer().await;
    let wallet = Wallet::new(reqwest::Client::new(), HolderKey::generate());
    let err = wallet
        .accept_offer(
            OfferSource::parse(&offer_uri(&base)).unwrap(),
            IssuanceRequest {
                tx_code: Some(TX_CODE.into()),
                credential_configuration_ids: Some(vec!["NotOffered".into()]),
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, Oid4vciError::UnknownCredentialConfiguration(ref id) if id == "NotOffered")
    );
}

use std::str::FromStr;
