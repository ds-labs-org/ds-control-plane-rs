//! Unit tests for the verifier side. Token checks run against a stand-in
//! `did:web` server on loopback, so DID resolution, the outbound
//! allow-list and signature checks are the real code paths.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use dcp_core::{DcpKeyPair, b64_encode, decode_jws_unverified, now_secs, sign_jws};
use serde_json::{Value, json};

use crate::{
    Dcp, DcpConfig, DcpError, Identity, OutboundPolicy, SeenJtiCache, check_credential_claims,
    credential_type_of_scope, did_web_path,
};

// ---- outbound policy ----

#[test]
fn outbound_policy_matches_the_host_only_and_denies_by_default() {
    let policy = OutboundPolicy::new(["Example.com".to_string()]);
    assert!(policy.check_url("https://example.com/x").is_ok());
    assert!(policy.check_url("http://EXAMPLE.com:8443/x").is_ok());
    assert!(policy.check_url("https://evil.example.com/x").is_err());
    assert!(policy.check_url("ftp://example.com/x").is_err());
    assert!(policy.check_url("not a url").is_err());
    assert!(policy.check_did("did:web:example.com:a", false).is_ok());
    assert!(policy.check_did("did:web:other.com", false).is_err());
    assert!(policy.check_did("did:key:z6Mk", false).is_err());

    let empty = OutboundPolicy::new(Vec::<String>::new());
    assert!(empty.check_url("https://example.com/").is_err());
}

// ---- jti cache ----

#[test]
fn the_jti_cache_refuses_a_replay_and_stays_bounded() {
    let mut cache = SeenJtiCache::new(2);
    assert!(cache.insert("a"));
    assert!(!cache.insert("a"));
    assert!(cache.insert("b"));
    assert!(cache.insert("c"));
    assert_eq!(cache.len(), 2);
    // "a" was the oldest and has been forgotten; "c" is still remembered.
    assert!(cache.insert("a"));
    assert!(!cache.insert("c"));
}

// ---- scopes ----

#[test]
fn a_vc_type_scope_names_its_credential_type() {
    assert_eq!(
        credential_type_of_scope("org.eclipse.dspace.dcp.vc.type:MembershipCredential:read"),
        Some("MembershipCredential".to_string())
    );
    assert_eq!(
        credential_type_of_scope("org.eclipse.dspace.dcp.vc.type:Foo"),
        Some("Foo".to_string())
    );
    assert_eq!(credential_type_of_scope("openid"), None);
    assert_eq!(
        credential_type_of_scope("org.eclipse.dspace.dcp.vc.type:"),
        None
    );
}

// ---- identity ----

#[test]
fn a_did_web_resolves_to_the_path_it_is_served_at() {
    assert_eq!(
        did_web_path("did:web:localhost%3A8080:cp").unwrap(),
        "/cp/did.json"
    );
    assert_eq!(
        did_web_path("did:web:example.com:a:b").unwrap(),
        "/a/b/did.json"
    );
    assert_eq!(
        did_web_path("did:web:example.com").unwrap(),
        "/.well-known/did.json"
    );
    assert!(did_web_path("did:key:z6Mk").is_err());
    assert!(did_web_path("did:web:").is_err());
}

#[test]
fn the_own_did_document_authorizes_its_key_for_capability_invocation() {
    let identity = Identity::generate("did:web:cp.example".to_string());
    let doc = identity.did_document();
    assert_eq!(doc["id"], "did:web:cp.example");
    assert_eq!(doc["capabilityInvocation"][0], identity.key_id());
    assert_eq!(doc["verificationMethod"][0]["id"], identity.key_id());
    assert!(identity.key_id().starts_with("did:web:cp.example#"));
}

#[test]
fn a_minted_token_is_a_self_issued_id_token_signed_by_the_own_key() {
    let identity = Identity::generate("did:web:cp.example".to_string());
    let token = identity.mint_self_issued_token("did:web:consumer.example", Some("nested"));
    let (_, header, payload) = decode_jws_unverified(&token).unwrap();
    assert_eq!(header["kid"], identity.key_id());
    assert_eq!(header["alg"], "ES256");
    dcp_core::verify_jws_signature(&token, &identity.verifying_key()).unwrap();
    assert_eq!(payload["iss"], "did:web:cp.example");
    assert_eq!(payload["sub"], "did:web:cp.example");
    assert_eq!(payload["aud"], "did:web:consumer.example");
    assert_eq!(payload["token"], "nested");
    assert!(payload["exp"].as_u64().unwrap() > now_secs());
    assert!(payload["jti"].is_string());

    let bare = identity.mint_self_issued_token("did:web:consumer.example", None);
    let (_, _, payload) = decode_jws_unverified(&bare).unwrap();
    assert!(payload.get("token").is_none());
}

#[test]
fn the_signing_key_loads_from_a_pem_or_jwk_file() {
    use p256::pkcs8::EncodePrivateKey;
    let generated = DcpKeyPair::generate("did:web:x".to_string());
    let secret = p256::SecretKey::from_bytes((&generated.signing_key_bytes).into()).unwrap();
    let dir = tempfile::tempdir().unwrap();

    let pem_path = dir.path().join("key.pem");
    let pem = secret.to_pkcs8_pem(p256::pkcs8::LineEnding::LF).unwrap();
    std::fs::write(&pem_path, pem.as_bytes()).unwrap();
    let from_pem = Identity::from_key_file("did:web:cp.example".to_string(), &pem_path).unwrap();

    let sec1_path = dir.path().join("key.sec1.pem");
    let sec1 = secret.to_sec1_pem(p256::pkcs8::LineEnding::LF).unwrap();
    std::fs::write(&sec1_path, sec1.as_bytes()).unwrap();
    let from_sec1 = Identity::from_key_file("did:web:cp.example".to_string(), &sec1_path).unwrap();

    let jwk_path = dir.path().join("key.jwk");
    let public = p256::ecdsa::VerifyingKey::from(&p256::ecdsa::SigningKey::from(&secret));
    let point = public.to_encoded_point(false);
    let jwk = json!({
        "kty": "EC",
        "crv": "P-256",
        "d": b64_encode(secret.to_bytes()),
        "x": b64_encode(point.x().unwrap()),
        "y": b64_encode(point.y().unwrap()),
    });
    std::fs::write(&jwk_path, jwk.to_string()).unwrap();
    let from_jwk = Identity::from_key_file("did:web:cp.example".to_string(), &jwk_path).unwrap();

    for identity in [&from_pem, &from_sec1, &from_jwk] {
        assert_eq!(identity.verifying_key(), public);
    }

    // A JWK whose public half does not match its private half is refused.
    let mut wrong = jwk.clone();
    wrong["x"] = json!(b64_encode([1u8; 32]));
    std::fs::write(&jwk_path, wrong.to_string()).unwrap();
    assert!(Identity::from_key_file("did:web:cp.example".to_string(), &jwk_path).is_err());

    std::fs::write(&jwk_path, "garbage").unwrap();
    assert!(Identity::from_key_file("did:web:cp.example".to_string(), &jwk_path).is_err());
    assert!(
        Identity::from_key_file("did:web:cp.example".to_string(), &dir.path().join("none"))
            .is_err()
    );
}

// ---- configuration ----

#[test]
fn the_configuration_is_checked() {
    let mut config = DcpConfig::new("did:web:cp.example");
    assert_eq!(
        config.required_scopes,
        ["org.eclipse.dspace.dcp.vc.type:MembershipCredential:read"]
    );
    assert!(config.trusted_issuers.is_empty());
    assert!(!config.insecure_http);
    assert!(Dcp::new(config.clone()).is_ok());

    config.required_scopes = vec!["openid".to_string()];
    assert!(matches!(Dcp::new(config.clone()), Err(DcpError::Config(_))));
    config.required_scopes = vec![];
    assert!(matches!(Dcp::new(config.clone()), Err(DcpError::Config(_))));

    let config = DcpConfig::new("did:key:z6Mk");
    assert!(matches!(Dcp::new(config), Err(DcpError::Config(_))));
}

#[test]
fn trusted_issuer_hosts_are_reachable_but_nothing_else_by_default() {
    let mut config = DcpConfig::new("did:web:cp.example");
    config.trusted_issuers = vec!["did:web:issuer.example%3A9000:i".to_string()];
    let dcp = Dcp::new(config).unwrap();
    assert!(dcp.outbound().check_did("did:web:issuer.example", false).is_ok());
    assert!(
        dcp.outbound()
            .check_did("did:web:consumer.example", false)
            .is_err()
    );
}

// ---- Self-Issued ID Token verification ----

/// A `did:web` server on loopback serving the DID documents of `parties`,
/// counting the requests it gets.
struct DidServer {
    host: String,
    hits: Arc<AtomicUsize>,
}

async fn did_server(documents: Arc<std::sync::Mutex<Vec<(String, Value)>>>) -> DidServer {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let host = listener.local_addr().unwrap().to_string();
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let router = axum::Router::new().route(
        "/{name}/did.json",
        axum::routing::get(
            move |axum::extract::Path(name): axum::extract::Path<String>| {
                let documents = documents.clone();
                let counter = counter.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    let documents = documents.lock().unwrap();
                    match documents.iter().find(|(n, _)| *n == name) {
                        Some((_, doc)) => axum::Json(doc.clone()).into_response(),
                        None => axum::http::StatusCode::NOT_FOUND.into_response(),
                    }
                }
            },
        ),
    );
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    DidServer { host, hits }
}

use axum::response::IntoResponse;

struct Fixture {
    dcp: Dcp,
    consumer: DcpKeyPair,
    server: DidServer,
    documents: Arc<std::sync::Mutex<Vec<(String, Value)>>>,
}

const OWN_DID: &str = "did:web:cp.example";

async fn fixture() -> Fixture {
    let documents = Arc::new(std::sync::Mutex::new(Vec::new()));
    let server = did_server(documents.clone()).await;
    let did = format!("did:web:{}:consumer", server.host.replace(':', "%3A"));
    let consumer = DcpKeyPair::generate(did);
    documents
        .lock()
        .unwrap()
        .push(("consumer".to_string(), consumer.did_document(&[])));
    let mut config = DcpConfig::new(OWN_DID);
    config.allowed_hosts = vec!["127.0.0.1".to_string()];
    config.insecure_http = true;
    Fixture {
        dcp: Dcp::new(config).unwrap(),
        consumer,
        server,
        documents,
    }
}

fn claims(consumer: &DcpKeyPair) -> Value {
    let now = now_secs();
    json!({
        "iss": consumer.own_did,
        "sub": consumer.own_did,
        "aud": OWN_DID,
        "token": "access-token",
        "iat": now,
        "nbf": now,
        "exp": now + 300,
        "jti": uuid::Uuid::new_v4().to_string(),
    })
}

fn bearer(consumer: &DcpKeyPair, claims: &Value) -> String {
    format!(
        "Bearer {}",
        sign_jws(claims, &consumer.signing_key(), &consumer.own_key_id)
    )
}

#[tokio::test]
async fn a_valid_token_names_its_participant_and_access_token() {
    let f = fixture().await;
    let verified = f
        .dcp
        .verify_self_issued_token(Some(&bearer(&f.consumer, &claims(&f.consumer))))
        .await
        .unwrap();
    assert_eq!(verified.participant_did, f.consumer.own_did);
    assert_eq!(verified.access_token.as_deref(), Some("access-token"));
    assert_eq!(verified.claims["aud"], OWN_DID);
}

#[tokio::test]
async fn a_missing_or_malformed_header_is_refused() {
    let f = fixture().await;
    for header in [None, Some("Basic abc"), Some("Bearer "), Some("Bearer a.b")] {
        assert!(
            f.dcp.verify_self_issued_token(header).await.is_err(),
            "{header:?}"
        );
    }
    assert!(matches!(
        f.dcp.verify_self_issued_token(None).await,
        Err(DcpError::MissingToken)
    ));
}

async fn refused(f: &Fixture, claims: Value) -> DcpError {
    f.dcp
        .verify_self_issued_token(Some(&bearer(&f.consumer, &claims)))
        .await
        .expect_err("the token must be refused")
}

#[tokio::test]
async fn each_claim_is_checked() {
    let f = fixture().await;
    let mut c = claims(&f.consumer);
    c["aud"] = json!("did:web:someone.else");
    assert!(matches!(refused(&f, c).await, DcpError::WrongAudience));

    let mut c = claims(&f.consumer);
    c["exp"] = json!(now_secs() - 1);
    assert!(matches!(refused(&f, c).await, DcpError::Expired));

    let mut c = claims(&f.consumer);
    c["nbf"] = json!(now_secs() + 3600);
    assert!(matches!(refused(&f, c).await, DcpError::NotYetValid));

    let mut c = claims(&f.consumer);
    c["iat"] = json!(now_secs() + 3600);
    assert!(matches!(refused(&f, c).await, DcpError::IssuedInFuture));

    let mut c = claims(&f.consumer);
    c["sub"] = json!("did:web:someone.else");
    assert!(matches!(
        refused(&f, c).await,
        DcpError::IssuerSubjectMismatch
    ));

    let mut c = claims(&f.consumer);
    c.as_object_mut().unwrap().remove("jti");
    assert!(matches!(refused(&f, c).await, DcpError::MissingJti));
}

#[tokio::test]
async fn an_audience_list_naming_this_connector_is_accepted() {
    let f = fixture().await;
    let mut c = claims(&f.consumer);
    c["aud"] = json!(["did:web:other", OWN_DID]);
    f.dcp
        .verify_self_issued_token(Some(&bearer(&f.consumer, &c)))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_token_signed_by_another_key_is_refused() {
    let f = fixture().await;
    let forger = DcpKeyPair::generate(f.consumer.own_did.clone());
    let token = sign_jws(
        &claims(&f.consumer),
        &forger.signing_key(),
        &f.consumer.own_key_id,
    );
    assert!(matches!(
        f.dcp
            .verify_self_issued_token(Some(&format!("Bearer {token}")))
            .await,
        Err(DcpError::InvalidSignature(_))
    ));
}

#[tokio::test]
async fn a_replayed_token_is_refused() {
    let f = fixture().await;
    let header = bearer(&f.consumer, &claims(&f.consumer));
    f.dcp.verify_self_issued_token(Some(&header)).await.unwrap();
    assert!(matches!(
        f.dcp.verify_self_issued_token(Some(&header)).await,
        Err(DcpError::TokenReplayed)
    ));
}

#[tokio::test]
async fn a_key_not_listed_for_capability_invocation_is_refused() {
    let f = fixture().await;
    let mut doc = f.consumer.did_document(&[]);
    doc["capabilityInvocation"] = json!([]);
    f.documents.lock().unwrap()[0].1 = doc;
    assert!(matches!(
        refused(&f, claims(&f.consumer)).await,
        DcpError::KeyNotAuthorizedForInvocation(_)
    ));
}

#[tokio::test]
async fn a_did_outside_the_allow_list_is_never_resolved() {
    let f = fixture().await;
    let outsider = DcpKeyPair::generate(format!(
        "did:web:{}:consumer",
        f.server.host.replace("127.0.0.1", "localhost").replace(':', "%3A")
    ));
    let error = f
        .dcp
        .verify_self_issued_token(Some(&bearer(&outsider, &claims(&outsider))))
        .await
        .unwrap_err();
    assert!(matches!(error, DcpError::OutboundDestinationNotAllowed(_)));
    assert_eq!(f.server.hits.load(Ordering::SeqCst), 0);
}

// ---- credential claims ----

fn vc(issuer: &str, subject: &str, types: &[&str]) -> Value {
    let mut all = vec!["VerifiableCredential"];
    all.extend_from_slice(types);
    json!({
        "iss": issuer,
        "sub": subject,
        "iat": now_secs(),
        "exp": now_secs() + 3600,
        "vc": {
            "@context": ["https://www.w3.org/2018/credentials/v1"],
            "type": all,
            "issuer": issuer,
            "credentialSubject": {"id": subject},
        }
    })
}

#[test]
fn credential_claims_name_issuer_and_types() {
    let claims = check_credential_claims(
        &vc("did:web:issuer", "did:web:p", &["MembershipCredential"]),
        "did:web:p",
        now_secs(),
    )
    .unwrap();
    assert_eq!(claims.issuer, "did:web:issuer");
    assert_eq!(claims.types, ["VerifiableCredential", "MembershipCredential"]);
}

#[test]
fn credential_claims_are_checked() {
    let now = now_secs();
    let check = |payload: Value| check_credential_claims(&payload, "did:web:p", now);

    assert!(matches!(
        check(vc("did:web:issuer", "did:web:other", &["X"])),
        Err(DcpError::CredentialSubjectMismatch)
    ));

    let mut c = vc("did:web:issuer", "did:web:p", &["X"]);
    c["vc"]["credentialSubject"]["id"] = json!("did:web:other");
    assert!(matches!(check(c), Err(DcpError::CredentialSubjectMismatch)));

    let mut c = vc("did:web:issuer", "did:web:p", &["X"]);
    c.as_object_mut().unwrap().remove("sub");
    c["vc"]["credentialSubject"] = json!({});
    assert!(matches!(check(c), Err(DcpError::CredentialSubjectMismatch)));

    let mut c = vc("did:web:issuer", "did:web:p", &["X"]);
    c["exp"] = json!(now - 1);
    assert!(matches!(check(c), Err(DcpError::CredentialExpired)));

    let mut c = vc("did:web:issuer", "did:web:p", &["X"]);
    c["vc"]["expirationDate"] = json!("2001-01-01T00:00:00Z");
    assert!(matches!(check(c), Err(DcpError::CredentialExpired)));

    let mut c = vc("did:web:issuer", "did:web:p", &["X"]);
    c["nbf"] = json!(now + 3600);
    assert!(matches!(check(c), Err(DcpError::InvalidCredential(_))));

    let mut c = vc("did:web:issuer", "did:web:p", &["X"]);
    c["vc"]["issuer"] = json!({"id": "did:web:someone-else"});
    assert!(matches!(check(c), Err(DcpError::InvalidCredential(_))));

    let mut c = vc("did:web:issuer", "did:web:p", &["X"]);
    c["vc"]["issuer"] = json!({"id": "did:web:issuer"});
    assert!(check(c).is_ok());

    let mut c = vc("did:web:issuer", "did:web:p", &["X"]);
    c.as_object_mut().unwrap().remove("vc");
    assert!(matches!(check(c), Err(DcpError::InvalidCredential(_))));
}
