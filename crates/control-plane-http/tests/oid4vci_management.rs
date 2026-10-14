//! The OID4VCI holder management API end to end: API-key gating, and an
//! offer accepted against a small in-process issuer, with the resulting
//! credential then listed by the control plane.

use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use control_plane_http::oid4vci::{Oid4vciConfig, router};
use serde_json::{Value, json};

const API_KEY: &str = "test-management-key";

async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

/// A minimal issuer that is its own authorization server, issues one
/// key-bound credential immediately, and requires a nonce-bearing proof.
async fn start_issuer() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let b = base.clone();
    let app = Router::new()
        .route(
            "/.well-known/openid-credential-issuer",
            get({
                let b = b.clone();
                move || async move {
                    Json(json!({
                        "credential_issuer": b,
                        "credential_endpoint": format!("{b}/credential"),
                        "nonce_endpoint": format!("{b}/nonce"),
                        "credential_configurations_supported": {
                            "MembershipCredential": {
                                "format": "jwt_vc_json",
                                "cryptographic_binding_methods_supported": ["jwk"],
                                "proof_types_supported": { "jwt": { "proof_signing_alg_values_supported": ["ES256"] } }
                            }
                        }
                    }))
                }
            }),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get({
                let b = b.clone();
                move || async move { Json(json!({ "issuer": b, "token_endpoint": format!("{b}/token") })) }
            }),
        )
        .route(
            "/token",
            post(|| async { Json(json!({ "access_token": "at", "token_type": "Bearer" })) }),
        )
        .route("/nonce", post(|| async { Json(json!({ "c_nonce": "n-1" })) }))
        .route(
            "/credential",
            post(|headers: HeaderMap, Json(body): Json<Value>| async move {
                let authorized = headers.get("authorization").and_then(|v| v.to_str().ok())
                    == Some("Bearer at");
                if !authorized || body["proofs"]["jwt"][0].as_str().is_none() {
                    return (StatusCode::BAD_REQUEST, Json(json!({ "error": "invalid_proof" })))
                        .into_response();
                }
                Json(json!({ "credentials": [{ "credential": "eyJ.membership.vc" }] }))
                    .into_response()
            }),
        );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

#[test]
fn not_mounted_without_an_api_key() {
    assert!(router(Oid4vciConfig::default()).unwrap().is_none());
}

#[tokio::test]
async fn accepts_an_offer_and_lists_the_credential() {
    let app = router(Oid4vciConfig {
        management_api_key: Some(API_KEY.into()),
        holder_jwk: None,
    })
    .unwrap()
    .unwrap();
    let cp = serve(app).await;
    let issuer = start_issuer().await;
    let http = reqwest::Client::new();

    // No key, wrong key: rejected before anything else runs.
    let r = http
        .get(format!("{cp}/management/oid4vci/holder"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    let r = http
        .get(format!("{cp}/management/oid4vci/holder"))
        .header("X-Api-Key", "nope")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);

    let holder: Value = http
        .get(format!("{cp}/management/oid4vci/holder"))
        .header("X-Api-Key", API_KEY)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(holder["did"].as_str().unwrap().starts_with("did:jwk:"));
    assert_eq!(holder["jwk"]["crv"], "P-256");
    assert!(
        holder["jwk"].get("d").is_none(),
        "private key must not leak"
    );

    let offer = json!({
        "credential_issuer": issuer,
        "credential_configuration_ids": ["MembershipCredential"],
        "grants": { "urn:ietf:params:oauth:grant-type:pre-authorized_code": { "pre-authorized_code": "c" } }
    });
    let mut offer_uri = url::Url::parse("openid-credential-offer://").unwrap();
    offer_uri
        .query_pairs_mut()
        .append_pair("credential_offer", &offer.to_string());

    let r = http
        .post(format!("{cp}/management/oid4vci/offers"))
        .header("X-Api-Key", API_KEY)
        .json(&json!({ "offer": offer_uri.as_str() }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let body: Value = r.json().await.unwrap();
    assert_eq!(body["credential_issuer"], issuer.as_str());
    assert_eq!(body["credentials"][0]["credential"], "eyJ.membership.vc");
    assert_eq!(body["deferred"], json!([]));
    let id = body["credentials"][0]["id"].as_str().unwrap().to_string();

    let listed: Value = http
        .get(format!("{cp}/management/oid4vci/credentials"))
        .header("X-Api-Key", API_KEY)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(
        listed[0]["credential_configuration_id"],
        "MembershipCredential"
    );

    let one = http
        .get(format!("{cp}/management/oid4vci/credentials/{id}"))
        .header("X-Api-Key", API_KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(one.status(), 200);
}

#[tokio::test]
async fn bad_offer_is_a_400_and_unreachable_issuer_a_502() {
    let app = router(Oid4vciConfig {
        management_api_key: Some(API_KEY.into()),
        holder_jwk: None,
    })
    .unwrap()
    .unwrap();
    let cp = serve(app).await;
    let http = reqwest::Client::new();

    let r = http
        .post(format!("{cp}/management/oid4vci/offers"))
        .header("X-Api-Key", API_KEY)
        .json(&json!({ "offer": "openid-credential-offer://?nothing=here" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);

    let r = http
        .post(format!("{cp}/management/oid4vci/offers"))
        .header("X-Api-Key", API_KEY)
        .json(&json!({ "offer": {
            "credential_issuer": "http://127.0.0.1:1",
            "credential_configuration_ids": ["X"],
            "grants": { "urn:ietf:params:oauth:grant-type:pre-authorized_code": { "pre-authorized_code": "c" } }
        } }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 502);
}
