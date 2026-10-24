//! DCP authentication of the DSP endpoints. Stand-in servers on loopback
//! play the Consumer (its `did:web` document, a Credential Service that
//! checks the control plane's token the way `ds-identity-hub-rs` does and
//! answers with a signed presentation, and its callback address) and the
//! credential issuers, all through `dcp_core`.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use control_plane_dcp::DcpConfig;
use control_plane_http::config::Config;
use dcp_core::{
    DcpKeyPair, decode_jws_unverified, find_verifying_key, now_secs, resolve_did, sign_jws,
    verify_jws_signature,
};
use serde_json::{Value, json};

const DSP: &str = "/dsp/2025-1";
const MEMBERSHIP: &str = "MembershipCredential";

fn did_at(addr: SocketAddr, path: &str) -> String {
    did_on("127.0.0.1", addr, path)
}

fn did_on(host: &str, addr: SocketAddr, path: &str) -> String {
    format!("did:web:{host}%3A{}:{path}", addr.port())
}

async fn listen() -> (tokio::net::TcpListener, SocketAddr) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    (listener, addr)
}

// ---- issuers ----

struct Issuer {
    key: DcpKeyPair,
}

impl Issuer {
    async fn start() -> Self {
        let (listener, addr) = listen().await;
        let key = DcpKeyPair::generate(did_at(addr, "issuer"));
        let doc = key.did_document(&[]);
        let router = axum::Router::new().route(
            "/issuer/did.json",
            get(move || {
                let doc = doc.clone();
                async move { Json(doc) }
            }),
        );
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Self { key }
    }

    fn did(&self) -> &str {
        &self.key.own_did
    }

    /// A JWT VC of `ty` for `subject`, shaped like the identity hub's.
    fn issue(&self, subject: &str, ty: &str) -> String {
        let now = now_secs();
        let payload = json!({
            "iss": self.did(),
            "sub": subject,
            "iat": now,
            "exp": now + 3600,
            "jti": format!("urn:uuid:{}", uuid::Uuid::new_v4()),
            "vc": {
                "@context": ["https://www.w3.org/2018/credentials/v1"],
                "type": ["VerifiableCredential", ty],
                "issuer": self.did(),
                "credentialSubject": {"id": subject},
            }
        });
        sign_jws(&payload, &self.key.signing_key(), &self.key.own_key_id)
    }
}

// ---- consumers ----

/// A callback the Consumer received: (path, Authorization header, body).
type Callback = (String, Option<String>, Value);

#[derive(Clone)]
struct Consumer {
    key: DcpKeyPair,
    base: String,
    /// The JWT VCs its Credential Service holds.
    credentials: Arc<Mutex<Vec<String>>>,
    /// Every presentation query that passed the token checks.
    queries: Arc<Mutex<Vec<Value>>>,
    /// Every callback: (path, Authorization header, body).
    callbacks: Arc<Mutex<Vec<Callback>>>,
}

impl Consumer {
    async fn start() -> Self {
        Self::start_on("127.0.0.1").await
    }

    /// A consumer whose DID and Credential Service name `host`, which
    /// must resolve to loopback.
    async fn start_on(host: &str) -> Self {
        let (listener, addr) = listen().await;
        let consumer = Self {
            key: DcpKeyPair::generate(did_on(host, addr, "consumer")),
            base: format!("http://{host}:{}", addr.port()),
            credentials: Arc::default(),
            queries: Arc::default(),
            callbacks: Arc::default(),
        };
        let router = axum::Router::new()
            .route("/consumer/did.json", get(consumer_did))
            .route("/cs/presentations/query", post(presentation_query))
            .route("/cb/negotiations/{pid}/{kind}", post(callback))
            .with_state(consumer.clone());
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        consumer
    }

    async fn with_credential(issuer: &Issuer, ty: &str) -> Self {
        let consumer = Self::start().await;
        let vc = issuer.issue(consumer.did(), ty);
        consumer.credentials.lock().unwrap().push(vc);
        consumer
    }

    fn did(&self) -> &str {
        &self.key.own_did
    }

    fn callback_address(&self) -> String {
        format!("{}/cb", self.base)
    }

    /// The claims of a valid Self-Issued ID Token to `audience`, with an
    /// access token (as the Consumer's STS would mint it: bound to the
    /// verifier) in its `token` claim.
    fn claims(&self, audience: &str) -> Value {
        let now = now_secs();
        let access_token = sign_jws(
            &json!({
                "iss": self.did(),
                "sub": self.did(),
                "aud": audience,
                "scope": format!("org.eclipse.dspace.dcp.vc.type:{MEMBERSHIP}:read"),
                "iat": now,
                "exp": now + 300,
                "jti": uuid::Uuid::new_v4().to_string(),
            }),
            &self.key.signing_key(),
            &self.key.own_key_id,
        );
        json!({
            "iss": self.did(),
            "sub": self.did(),
            "aud": audience,
            "token": access_token,
            "iat": now,
            "nbf": now,
            "exp": now + 300,
            "jti": uuid::Uuid::new_v4().to_string(),
        })
    }

    fn sign(&self, claims: &Value) -> String {
        sign_jws(claims, &self.key.signing_key(), &self.key.own_key_id)
    }

    fn token(&self, audience: &str) -> String {
        self.sign(&self.claims(audience))
    }

    fn callbacks(&self) -> Vec<Callback> {
        self.callbacks.lock().unwrap().clone()
    }
}

async fn consumer_did(State(c): State<Consumer>) -> Json<Value> {
    Json(
        c.key
            .did_document(&[("CredentialService".to_string(), format!("{}/cs", c.base))]),
    )
}

async fn callback(
    State(c): State<Consumer>,
    Path((pid, kind)): Path<(String, String)>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> StatusCode {
    let authorization = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    c.callbacks
        .lock()
        .unwrap()
        .push((format!("/negotiations/{pid}/{kind}"), authorization, body));
    StatusCode::OK
}

/// The Credential Service: checks the verifier's Self-Issued ID Token as
/// the identity hub's `presentation_query` does (signature by a key of the
/// resolved `iss` DID listed for capabilityInvocation, `iss` = `sub`, `aud`
/// this holder, not expired, the nested access token ours and bound to
/// the verifier), then answers with a VP of the credentials of the
/// requested types, signed by the holder, `aud` the verifier.
async fn presentation_query(
    State(c): State<Consumer>,
    headers: HeaderMap,
    Json(query): Json<Value>,
) -> axum::response::Response {
    let refuse = |why: &str| (StatusCode::UNAUTHORIZED, why.to_string()).into_response();
    let Some(token) = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    else {
        return refuse("no token");
    };
    let Ok((_, header, claims)) = decode_jws_unverified(token) else {
        return refuse("malformed");
    };
    let verifier = claims["iss"].as_str().unwrap_or_default().to_string();
    let kid = header["kid"].as_str().unwrap_or_default();
    let http = reqwest::Client::new();
    let Ok(doc) = resolve_did(&http, &verifier, true).await else {
        return refuse("cannot resolve the verifier");
    };
    let Ok(key) = find_verifying_key(&doc, kid) else {
        return refuse("unknown kid");
    };
    if verify_jws_signature(token, &key).is_err() {
        return refuse("bad signature");
    }
    if !doc.capability_invocation.iter().any(|k| k == kid) {
        return refuse("kid not for capabilityInvocation");
    }
    if claims["sub"] != claims["iss"] || claims["aud"] != c.did() {
        return refuse("iss/sub/aud");
    }
    if claims["exp"].as_u64().unwrap_or(0) <= now_secs() {
        return refuse("expired");
    }
    let nested = claims["token"].as_str().unwrap_or_default();
    let nested_ok = verify_jws_signature(nested, &c.key.signing_key().verifying_key().clone())
        .is_ok()
        && decode_jws_unverified(nested)
            .map(|(_, _, p)| p["aud"] == verifier.as_str() && p["iss"] == c.did())
            .unwrap_or(false);
    if !nested_ok {
        return refuse("nested access token not ours or not bound to the verifier");
    }
    if query["type"] != "PresentationQueryMessage" {
        return (StatusCode::BAD_REQUEST, "type").into_response();
    }
    let requested: Vec<String> = query["scope"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s.as_str())
        .filter_map(|s| s.strip_prefix("org.eclipse.dspace.dcp.vc.type:"))
        .map(|s| s.split(':').next().unwrap().to_string())
        .collect();
    c.queries.lock().unwrap().push(query.clone());
    let held = c.credentials.lock().unwrap().clone();
    let matched: Vec<String> = held
        .into_iter()
        .filter(|vc| {
            let (_, _, p) = decode_jws_unverified(vc).unwrap();
            p["vc"]["type"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| requested.iter().any(|r| t == r.as_str()))
        })
        .collect();
    let now = now_secs();
    let vp = sign_jws(
        &json!({
            "iss": c.did(),
            "sub": c.did(),
            "aud": verifier,
            "vp": {
                "@context": ["https://www.w3.org/2018/credentials/v1"],
                "type": ["VerifiablePresentation"],
                "holder": c.did(),
                "verifiableCredential": matched,
            },
            "iat": now,
            "exp": now + 300,
            "jti": uuid::Uuid::new_v4().to_string(),
        }),
        &c.key.signing_key(),
        &c.key.own_key_id,
    );
    Json(json!({
        "@context": ["https://w3id.org/dspace-dcp/v1.0/dcp.jsonld"],
        "type": "PresentationResponseMessage",
        "presentation": [vp],
    }))
    .into_response()
}

// ---- the control plane ----

struct ControlPlane {
    did: String,
    base: String,
    graph: Arc<control_plane_graph::ControlPlaneGraph>,
    http: reqwest::Client,
}

fn dcp_config(did: &str, trusted: &[&Issuer]) -> DcpConfig {
    let mut dcp = DcpConfig::new(did);
    dcp.trusted_issuers = trusted.iter().map(|i| i.did().to_string()).collect();
    dcp.allowed_hosts = vec!["127.0.0.1".to_string()];
    dcp.insecure_http = true;
    dcp
}

impl ControlPlane {
    /// A control plane with DCP on, trusting `trusted`.
    async fn start(trusted: &[&Issuer]) -> Self {
        Self::start_with(trusted, |_| {}).await
    }

    async fn start_with(trusted: &[&Issuer], tweak: impl FnOnce(&mut Config)) -> Self {
        let (listener, addr) = listen().await;
        let did = did_at(addr, "cp");
        let mut config = Config {
            port: 0,
            base_url: format!("http://{addr}"),
            dcp: Some(dcp_config(&did, trusted)),
            ..Config::default()
        };
        tweak(&mut config);
        Self::serve(config, listener, addr, did).await
    }

    /// A control plane with DCP off.
    async fn start_unauthenticated() -> Self {
        let (listener, addr) = listen().await;
        let config = Config {
            port: 0,
            base_url: format!("http://{addr}"),
            ..Config::default()
        };
        Self::serve(config, listener, addr, String::new()).await
    }

    async fn serve(
        config: Config,
        listener: tokio::net::TcpListener,
        addr: SocketAddr,
        did: String,
    ) -> Self {
        let app = control_plane_http::build_with_listener(config, listener)
            .await
            .unwrap();
        let graph = app.graph.clone();
        tokio::spawn(control_plane_http::serve(app));
        Self {
            did,
            base: format!("http://{addr}"),
            graph,
            http: reqwest::Client::new(),
        }
    }

    async fn call(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let url = format!("{}{path}", self.base);
        let mut request = match method {
            "GET" => self.http.get(url),
            _ => self.http.post(url),
        };
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.unwrap();
        let status = StatusCode::from_u16(response.status().as_u16()).unwrap();
        let body = response.json().await.unwrap_or(Value::Null);
        (status, body)
    }

    /// `method` on the DSP path `path` as `consumer`, with a fresh token.
    async fn dsp(
        &self,
        consumer: &Consumer,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let token = consumer.token(&self.did);
        self.call(method, &format!("{DSP}{path}"), Some(&token), body)
            .await
    }
}

fn catalog_request() -> Value {
    json!({
        "@context": ["https://w3id.org/dspace/2025/1/context.jsonld"],
        "@type": "CatalogRequestMessage",
    })
}

fn contract_request(consumer_pid: &str, callback: Option<String>) -> Value {
    let mut msg = json!({
        "@context": ["https://w3id.org/dspace/2025/1/context.jsonld"],
        "@type": "ContractRequestMessage",
        "consumerPid": consumer_pid,
        "offer": {
            "@id": "urn:uuid:control-plane-rs-seed-offer",
            "@type": "Offer",
            "target": "urn:uuid:control-plane-rs-seed-dataset",
            "permission": [{"action": "use"}]
        }
    });
    if let Some(callback) = callback {
        msg["callbackAddress"] = json!(callback);
    }
    msg
}

fn transfer_request(consumer_pid: &str, agreement_id: &str) -> Value {
    json!({
        "@context": ["https://w3id.org/dspace/2025/1/context.jsonld"],
        "@type": "TransferRequestMessage",
        "consumerPid": consumer_pid,
        "agreementId": agreement_id,
        "format": "HttpData-PULL",
    })
}

/// Waits until `consumer` has received `n` callbacks.
async fn wait_for_callbacks(consumer: &Consumer, n: usize) -> Vec<Callback> {
    for _ in 0..200 {
        let received = consumer.callbacks();
        if received.len() >= n {
            return received;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("expected {n} callbacks, got {:?}", consumer.callbacks());
}

// ---- authentication ----

#[tokio::test]
async fn every_dsp_endpoint_needs_a_token_and_answers_with_its_own_error() {
    let issuer = Issuer::start().await;
    let cp = ControlPlane::start(&[&issuer]).await;

    let (status, body) = cp
        .call(
            "POST",
            &format!("{DSP}/catalog/request"),
            None,
            Some(catalog_request()),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["@type"], "CatalogError");

    let (status, _) = cp
        .call(
            "GET",
            &format!("{DSP}/catalog/datasets/urn:uuid:control-plane-rs-seed-dataset"),
            None,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, body) = cp
        .call(
            "POST",
            &format!("{DSP}/negotiations/request"),
            None,
            Some(contract_request("urn:uuid:c1", None)),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["@type"], "ContractNegotiationError");
    assert_eq!(body["code"], "401");
    assert!(body["providerPid"].is_string());

    let (status, body) = cp
        .call(
            "GET",
            &format!("{DSP}/negotiations/urn:uuid:p1"),
            None,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["@type"], "ContractNegotiationError");
    assert_eq!(body["providerPid"], "urn:uuid:p1");

    let (status, body) = cp
        .call(
            "POST",
            &format!("{DSP}/transfers/request"),
            None,
            Some(transfer_request("urn:uuid:c1", "urn:uuid:a")),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["@type"], "TransferError");

    let (status, body) = cp
        .call(
            "POST",
            &format!("{DSP}/transfers/urn:uuid:t1/start"),
            None,
            Some(json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["@type"], "TransferError");
    assert_eq!(body["providerPid"], "urn:uuid:t1");
}

#[tokio::test]
async fn the_version_endpoint_and_the_did_document_need_no_token() {
    let issuer = Issuer::start().await;
    let cp = ControlPlane::start(&[&issuer]).await;
    let (status, _) = cp
        .call("GET", "/.well-known/dspace-version", None, None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, doc) = cp.call("GET", "/cp/did.json", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(doc["id"], cp.did);
    let kid = doc["verificationMethod"][0]["id"].clone();
    assert_eq!(doc["capabilityInvocation"][0], kid);
}

#[tokio::test]
async fn a_member_reads_the_catalog() {
    let issuer = Issuer::start().await;
    let cp = ControlPlane::start(&[&issuer]).await;
    let consumer = Consumer::with_credential(&issuer, MEMBERSHIP).await;
    let (status, body) = cp
        .dsp(
            &consumer,
            "POST",
            "/catalog/request",
            Some(catalog_request()),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["@type"], "Catalog");
    // The Credential Service was asked for the configured scope.
    let queries = consumer.queries.lock().unwrap().clone();
    assert_eq!(queries.len(), 1);
    assert_eq!(
        queries[0]["scope"],
        json!([format!("org.eclipse.dspace.dcp.vc.type:{MEMBERSHIP}:read")])
    );
}

async fn assert_refused(cp: &ControlPlane, token: &str) {
    let (status, body) = cp
        .call(
            "POST",
            &format!("{DSP}/catalog/request"),
            Some(token),
            Some(catalog_request()),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["@type"], "CatalogError");
}

#[tokio::test]
async fn a_token_signed_by_another_key_is_401() {
    let issuer = Issuer::start().await;
    let cp = ControlPlane::start(&[&issuer]).await;
    let consumer = Consumer::with_credential(&issuer, MEMBERSHIP).await;
    let forger = DcpKeyPair::generate(consumer.did().to_string());
    let token = sign_jws(
        &consumer.claims(&cp.did),
        &forger.signing_key(),
        &consumer.key.own_key_id,
    );
    assert_refused(&cp, &token).await;
}

#[tokio::test]
async fn a_token_for_another_audience_is_401() {
    let issuer = Issuer::start().await;
    let cp = ControlPlane::start(&[&issuer]).await;
    let consumer = Consumer::with_credential(&issuer, MEMBERSHIP).await;
    assert_refused(&cp, &consumer.token("did:web:someone-else.example")).await;
}

#[tokio::test]
async fn an_expired_token_is_401() {
    let issuer = Issuer::start().await;
    let cp = ControlPlane::start(&[&issuer]).await;
    let consumer = Consumer::with_credential(&issuer, MEMBERSHIP).await;
    let mut claims = consumer.claims(&cp.did);
    claims["iat"] = json!(now_secs() - 600);
    claims["nbf"] = json!(now_secs() - 600);
    claims["exp"] = json!(now_secs() - 300);
    assert_refused(&cp, &consumer.sign(&claims)).await;
}

#[tokio::test]
async fn a_replayed_token_is_401() {
    let issuer = Issuer::start().await;
    let cp = ControlPlane::start(&[&issuer]).await;
    let consumer = Consumer::with_credential(&issuer, MEMBERSHIP).await;
    let token = consumer.token(&cp.did);
    let (status, body) = cp
        .call(
            "POST",
            &format!("{DSP}/catalog/request"),
            Some(&token),
            Some(catalog_request()),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_refused(&cp, &token).await;
}

#[tokio::test]
async fn a_credential_from_an_untrusted_issuer_is_401() {
    let trusted = Issuer::start().await;
    let untrusted = Issuer::start().await;
    let cp = ControlPlane::start(&[&trusted]).await;
    let consumer = Consumer::with_credential(&untrusted, MEMBERSHIP).await;
    assert_refused(&cp, &consumer.token(&cp.did)).await;
}

#[tokio::test]
async fn no_trusted_issuer_trusts_nobody() {
    let issuer = Issuer::start().await;
    let cp = ControlPlane::start(&[]).await;
    let consumer = Consumer::with_credential(&issuer, MEMBERSHIP).await;
    assert_refused(&cp, &consumer.token(&cp.did)).await;
}

#[tokio::test]
async fn a_participant_without_the_required_credential_type_is_401() {
    let issuer = Issuer::start().await;
    let cp = ControlPlane::start(&[&issuer]).await;
    let consumer = Consumer::with_credential(&issuer, "SomeOtherCredential").await;
    assert_refused(&cp, &consumer.token(&cp.did)).await;
}

#[tokio::test]
async fn a_credential_for_another_subject_is_401() {
    let issuer = Issuer::start().await;
    let cp = ControlPlane::start(&[&issuer]).await;
    let consumer = Consumer::start().await;
    let vc = issuer.issue("did:web:someone-else.example", MEMBERSHIP);
    consumer.credentials.lock().unwrap().push(vc);
    assert_refused(&cp, &consumer.token(&cp.did)).await;
}

/// The issuer runs on 127.0.0.1, which trusting it allows; the consumer
/// runs on `localhost`, allowed only when listed.
#[tokio::test]
async fn a_participant_outside_the_allow_list_is_401() {
    let issuer = Issuer::start().await;
    let consumer = Consumer::start_on("localhost").await;
    let vc = issuer.issue(consumer.did(), MEMBERSHIP);
    consumer.credentials.lock().unwrap().push(vc);

    let cp = ControlPlane::start_with(&[&issuer], |config| {
        config.dcp.as_mut().unwrap().allowed_hosts = vec!["example.com".to_string()];
    })
    .await;
    assert_refused(&cp, &consumer.token(&cp.did)).await;
    assert!(consumer.queries.lock().unwrap().is_empty());

    let cp = ControlPlane::start_with(&[&issuer], |config| {
        config.dcp.as_mut().unwrap().allowed_hosts = vec!["localhost".to_string()];
    })
    .await;
    let (status, body) = cp
        .dsp(
            &consumer,
            "POST",
            "/catalog/request",
            Some(catalog_request()),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn with_dcp_off_nothing_changes() {
    let cp = ControlPlane::start_unauthenticated().await;
    let (status, body) = cp
        .call(
            "POST",
            &format!("{DSP}/catalog/request"),
            None,
            Some(catalog_request()),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, ack) = cp
        .call(
            "POST",
            &format!("{DSP}/negotiations/request"),
            None,
            Some(contract_request("urn:uuid:c-off", None)),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{ack}");
    let pid = ack["providerPid"].as_str().unwrap();
    let stored = cp.graph.negotiation(pid).unwrap().unwrap();
    assert_eq!(stored.participant_id, None);
    // A token is ignored, not checked.
    let (status, _) = cp
        .call(
            "GET",
            &format!("{DSP}/negotiations/{pid}"),
            Some("not-a-token"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = cp.call("GET", "/cp/did.json", None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---- binding to the authenticated participant ----

/// Negotiates to FINALIZED as `consumer`; returns (providerPid, the
/// Agreement as the Consumer received it).
async fn finalized_agreement(cp: &ControlPlane, consumer: &Consumer) -> (String, Value) {
    let consumer_pid = format!("urn:uuid:{}", uuid::Uuid::new_v4());
    let (status, ack) = cp
        .dsp(
            consumer,
            "POST",
            "/negotiations/request",
            Some(contract_request(
                &consumer_pid,
                Some(consumer.callback_address()),
            )),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{ack}");
    let provider_pid = ack["providerPid"].as_str().unwrap().to_string();
    let received = wait_for_callbacks(consumer, 1).await;
    let (path, _, message) = received.last().unwrap().clone();
    assert_eq!(path, format!("/negotiations/{consumer_pid}/agreement"));
    let agreement = message["agreement"].clone();

    let (status, body) = cp
        .dsp(
            consumer,
            "POST",
            &format!("/negotiations/{provider_pid}/agreement/verification"),
            Some(json!({
                "@context": ["https://w3id.org/dspace/2025/1/context.jsonld"],
                "@type": "ContractAgreementVerificationMessage",
                "providerPid": provider_pid,
                "consumerPid": consumer_pid,
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let received = wait_for_callbacks(consumer, 2).await;
    assert_eq!(received[1].2["eventType"], "FINALIZED");
    (provider_pid, agreement)
}

#[tokio::test]
async fn the_agreement_is_granted_to_the_authenticated_did_and_opens_a_transfer() {
    let issuer = Issuer::start().await;
    let cp = ControlPlane::start(&[&issuer]).await;
    let consumer = Consumer::with_credential(&issuer, MEMBERSHIP).await;

    let (provider_pid, agreement) = finalized_agreement(&cp, &consumer).await;
    assert_eq!(agreement["assignee"], consumer.did());
    let stored = cp.graph.negotiation(&provider_pid).unwrap().unwrap();
    assert_eq!(stored.participant_id.as_deref(), Some(consumer.did()));
    let (status, ack) = cp
        .dsp(
            &consumer,
            "GET",
            &format!("/negotiations/{provider_pid}"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ack["state"], "FINALIZED");

    let agreement_id = agreement["@id"].as_str().unwrap();
    let (status, ack) = cp
        .dsp(
            &consumer,
            "POST",
            "/transfers/request",
            Some(transfer_request("urn:uuid:ct1", agreement_id)),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{ack}");
    let transfer_pid = ack["providerPid"].as_str().unwrap();
    let stored = cp.graph.transfer(transfer_pid).unwrap().unwrap();
    assert_eq!(stored.participant_id.as_deref(), Some(consumer.did()));
    let (status, _) = cp
        .dsp(
            &consumer,
            "GET",
            &format!("/transfers/{transfer_pid}"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn another_member_cannot_use_someone_elses_agreement_or_transfer() {
    let issuer = Issuer::start().await;
    let cp = ControlPlane::start(&[&issuer]).await;
    let owner = Consumer::with_credential(&issuer, MEMBERSHIP).await;
    let other = Consumer::with_credential(&issuer, MEMBERSHIP).await;

    let (_, agreement) = finalized_agreement(&cp, &owner).await;
    let agreement_id = agreement["@id"].as_str().unwrap();

    let (status, body) = cp
        .dsp(
            &other,
            "POST",
            "/transfers/request",
            Some(transfer_request("urn:uuid:other-t", agreement_id)),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["@type"], "TransferError");
    assert_eq!(cp.graph.transfers().unwrap().len(), 0);

    let (status, ack) = cp
        .dsp(
            &owner,
            "POST",
            "/transfers/request",
            Some(transfer_request("urn:uuid:owner-t", agreement_id)),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{ack}");
    let transfer_pid = ack["providerPid"].as_str().unwrap();
    for (method, path) in [
        ("GET", format!("/transfers/{transfer_pid}")),
        ("POST", format!("/transfers/{transfer_pid}/termination")),
        ("POST", format!("/transfers/{transfer_pid}/suspension")),
        ("POST", format!("/transfers/{transfer_pid}/completion")),
        ("POST", format!("/transfers/{transfer_pid}/start")),
    ] {
        let body = (method == "POST").then(|| {
            json!({
                "@context": ["https://w3id.org/dspace/2025/1/context.jsonld"],
                "@type": "TransferTerminationMessage",
                "providerPid": transfer_pid,
                "consumerPid": "urn:uuid:owner-t",
                "code": "x",
            })
        });
        let (status, body) = cp.dsp(&other, method, &path, body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}: {body}");
        assert_eq!(body["@type"], "TransferError");
    }
    let stored = cp.graph.transfer(transfer_pid).unwrap().unwrap();
    assert_eq!(
        serde_json::to_value(stored.state).unwrap(),
        json!("REQUESTED")
    );
}

#[tokio::test]
async fn another_member_cannot_act_on_someone_elses_negotiation() {
    let issuer = Issuer::start().await;
    let cp = ControlPlane::start(&[&issuer]).await;
    let owner = Consumer::with_credential(&issuer, MEMBERSHIP).await;
    let other = Consumer::with_credential(&issuer, MEMBERSHIP).await;

    // No callback: the negotiation stays REQUESTED.
    let (status, ack) = cp
        .dsp(
            &owner,
            "POST",
            "/negotiations/request",
            Some(contract_request("urn:uuid:owner-n", None)),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{ack}");
    let pid = ack["providerPid"].as_str().unwrap();

    let context = json!(["https://w3id.org/dspace/2025/1/context.jsonld"]);
    let attempts = [
        ("GET", format!("/negotiations/{pid}"), None),
        (
            "POST",
            format!("/negotiations/{pid}/events"),
            Some(json!({
                "@context": context, "@type": "ContractNegotiationEventMessage",
                "providerPid": pid, "consumerPid": "urn:uuid:owner-n", "eventType": "ACCEPTED",
            })),
        ),
        (
            "POST",
            format!("/negotiations/{pid}/events"),
            Some(json!({
                "@context": context, "@type": "ContractNegotiationEventMessage",
                "providerPid": pid, "consumerPid": "urn:uuid:owner-n", "eventType": "FINALIZED",
            })),
        ),
        (
            "POST",
            format!("/negotiations/{pid}/agreement/verification"),
            Some(json!({
                "@context": context, "@type": "ContractAgreementVerificationMessage",
                "providerPid": pid, "consumerPid": "urn:uuid:owner-n",
            })),
        ),
        (
            "POST",
            format!("/negotiations/{pid}/request"),
            Some(contract_request("urn:uuid:owner-n", None)),
        ),
        (
            "POST",
            format!("/negotiations/{pid}/termination"),
            Some(json!({
                "@context": context, "@type": "ContractNegotiationTerminationMessage",
                "providerPid": pid, "consumerPid": "urn:uuid:owner-n", "code": "x",
            })),
        ),
    ];
    for (method, path, body) in attempts {
        let (status, body) = cp.dsp(&other, method, &path, body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}: {body}");
        assert_eq!(body["@type"], "ContractNegotiationError");
        // The owner's consumerPid is not disclosed.
        assert!(body.get("consumerPid").is_none_or(Value::is_null), "{body}");
    }
    let stored = cp.graph.negotiation(pid).unwrap().unwrap();
    assert_eq!(
        serde_json::to_value(stored.state).unwrap(),
        json!("REQUESTED")
    );
    // The owner still can.
    let (status, _) = cp
        .dsp(&owner, "GET", &format!("/negotiations/{pid}"), None)
        .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn an_agreement_made_without_dcp_grants_nothing_once_dcp_is_on() {
    let issuer = Issuer::start().await;
    let cp = ControlPlane::start(&[&issuer]).await;
    let consumer = Consumer::with_credential(&issuer, MEMBERSHIP).await;
    let mut n = control_plane_core::negotiation::ContractNegotiation::new(
        "urn:uuid:old-n",
        "urn:uuid:old-c",
    );
    n.state = control_plane_core::negotiation::NegotiationState::Finalized;
    n.agreement = Some(control_plane_core::common::Agreement {
        id: "urn:uuid:old-agreement".to_string(),
        type_: "Agreement".to_string(),
        target: "urn:uuid:control-plane-rs-seed-dataset".to_string(),
        timestamp: chrono::Utc::now(),
        assigner: "urn:connector:ds-control-plane-rs".to_string(),
        assignee: "urn:uuid:old-c".to_string(),
        permission: vec![],
        prohibition: vec![],
        obligation: vec![],
    });
    cp.graph.put_negotiation(&n).unwrap();
    let (status, _) = cp
        .dsp(
            &consumer,
            "POST",
            "/transfers/request",
            Some(transfer_request("urn:uuid:t", "urn:uuid:old-agreement")),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = cp
        .dsp(&consumer, "GET", "/negotiations/urn:uuid:old-n", None)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn with_dcp_off_the_assignee_is_still_the_consumer_pid() {
    let cp = ControlPlane::start_unauthenticated().await;
    let consumer = Consumer::start().await;
    let (status, ack) = cp
        .call(
            "POST",
            &format!("{DSP}/negotiations/request"),
            None,
            Some(contract_request(
                "urn:uuid:c-off",
                Some(consumer.callback_address()),
            )),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{ack}");
    let received = wait_for_callbacks(&consumer, 1).await;
    assert_eq!(received[0].2["agreement"]["assignee"], "urn:uuid:c-off");
    // No token on callbacks without DCP.
    assert_eq!(received[0].1, None);
}

// ---- callbacks ----

/// Checks a callback's `Authorization` header: a Self-Issued ID Token of
/// the control plane (signature by the key its served DID document lists
/// for capabilityInvocation), addressed to `audience`.
async fn assert_control_plane_token(
    cp: &ControlPlane,
    authorization: Option<&str>,
    audience: &str,
) {
    let token = authorization
        .and_then(|h| h.strip_prefix("Bearer "))
        .expect("the callback carries a bearer token");
    let (_, header, claims) = decode_jws_unverified(token).unwrap();
    let kid = header["kid"].as_str().unwrap();
    let doc = resolve_did(&reqwest::Client::new(), &cp.did, true)
        .await
        .unwrap();
    verify_jws_signature(token, &find_verifying_key(&doc, kid).unwrap()).unwrap();
    assert!(doc.capability_invocation.iter().any(|k| k == kid));
    assert_eq!(claims["iss"], cp.did);
    assert_eq!(claims["sub"], cp.did);
    assert_eq!(claims["aud"], audience);
    assert!(claims["exp"].as_u64().unwrap() > now_secs());
    assert!(claims["jti"].is_string());
}

#[tokio::test]
async fn callbacks_carry_a_token_the_consumer_can_verify() {
    let issuer = Issuer::start().await;
    let cp = ControlPlane::start(&[&issuer]).await;
    let consumer = Consumer::with_credential(&issuer, MEMBERSHIP).await;
    finalized_agreement(&cp, &consumer).await;
    let received = consumer.callbacks();
    assert_eq!(received.len(), 2);
    for (_, authorization, _) in &received {
        assert_control_plane_token(&cp, authorization.as_deref(), consumer.did()).await;
    }
    assert_ne!(
        received[0].1, received[1].1,
        "each callback has its own token"
    );
}

/// An AGREED negotiation a restart interrupted is resumed: the Agreement
/// is sent again, still to the authenticated participant and with a token
/// addressed to it.
#[tokio::test]
async fn a_resumed_negotiation_keeps_its_participant() {
    let issuer = Issuer::start().await;
    let consumer = Consumer::start().await;
    let dir = tempfile::tempdir().unwrap();
    {
        let graph = control_plane_graph::ControlPlaneGraph::open(dir.path()).unwrap();
        let mut n = control_plane_core::negotiation::ContractNegotiation::new(
            "urn:uuid:resumed",
            "urn:uuid:resumed-c",
        );
        n.offer = Some(
            serde_json::from_value(json!({
                "@id": "urn:uuid:control-plane-rs-seed-offer",
                "@type": "Offer",
                "target": "urn:uuid:control-plane-rs-seed-dataset",
                "permission": [{"action": "use"}]
            }))
            .unwrap(),
        );
        n.callback_address = Some(consumer.callback_address());
        n.participant_id = Some(consumer.did().to_string());
        graph.put_negotiation(&n).unwrap();
    }
    let store = dir.path().to_path_buf();
    let cp = ControlPlane::start_with(&[&issuer], |config| {
        config.store_path = Some(store);
    })
    .await;
    let received = wait_for_callbacks(&consumer, 1).await;
    assert_eq!(received[0].0, "/negotiations/urn:uuid:resumed-c/agreement");
    assert_eq!(received[0].2["agreement"]["assignee"], consumer.did());
    assert_control_plane_token(&cp, received[0].1.as_deref(), consumer.did()).await;
}
