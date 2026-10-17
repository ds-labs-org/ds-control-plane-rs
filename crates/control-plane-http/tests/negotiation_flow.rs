//! The Provider moves a negotiation on by itself: it agrees to a request
//! and sends the Agreement to the Consumer's `callbackAddress`, finalizes
//! once the Consumer verifies, and only starts transfers under an
//! agreement it has finalized. A small axum server stands in for the
//! Consumer and records what it receives.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{Request, StatusCode};
use axum::routing::post;
use control_plane_http::config::Config;
use serde_json::{Value, json};
use tower::ServiceExt;

const DSP: &str = "/dsp/2025-1";

#[derive(Clone, Default)]
struct Consumer {
    received: Arc<Mutex<Vec<(String, Value)>>>,
    /// Answers 500 to the agreement callback, after recording it.
    fails_agreement: bool,
    /// Verifies the Agreement with this Provider before answering 502 to
    /// the agreement callback, as when the Consumer processed the
    /// Agreement but a proxy fails its reply.
    verifies_then_fails: Option<axum::Router>,
    verifications: Arc<Mutex<Vec<(String, StatusCode)>>>,
}

impl Consumer {
    fn received(&self) -> Vec<(String, Value)> {
        self.received.lock().unwrap().clone()
    }
}

async fn record(
    State(consumer): State<Consumer>,
    Path((pid, kind)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> StatusCode {
    consumer
        .received
        .lock()
        .unwrap()
        .push((format!("/negotiations/{pid}/{kind}"), body.clone()));
    if let (Some(provider), "agreement") = (&consumer.verifies_then_fails, kind.as_str()) {
        let provider_pid = body["providerPid"].as_str().unwrap().to_string();
        let (status, _) = call(
            provider,
            "POST",
            &format!("/negotiations/{provider_pid}/agreement/verification"),
            Some(json!({
                "@context": ["https://w3id.org/dspace/2025/1/context.jsonld"],
                "@type": "ContractAgreementVerificationMessage",
                "providerPid": provider_pid,
                "consumerPid": pid
            })),
        )
        .await;
        consumer
            .verifications
            .lock()
            .unwrap()
            .push((provider_pid, status));
        return StatusCode::BAD_GATEWAY;
    }
    if consumer.fails_agreement && kind == "agreement" {
        StatusCode::INTERNAL_SERVER_ERROR
    } else {
        StatusCode::OK
    }
}

/// Starts the stand-in Consumer; returns it and its callback address.
async fn consumer() -> (Consumer, String) {
    serve(Consumer::default()).await
}

async fn serve(consumer: Consumer) -> (Consumer, String) {
    let router = axum::Router::new()
        .route("/callback/negotiations/{pid}/{kind}", post(record))
        .with_state(consumer.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (consumer, format!("http://{addr}/callback"))
}

async fn app() -> control_plane_http::App {
    control_plane_http::build(Config {
        port: 0,
        ..Config::default()
    })
    .await
    .unwrap()
}

async fn call(
    router: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(format!("{DSP}{uri}"))
        .header("content-type", "application/json")
        .body(match body {
            Some(b) => Body::from(b.to_string()),
            None => Body::empty(),
        })
        .unwrap();
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn request(consumer_pid: &str, callback: Option<&str>) -> Value {
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

/// Polls the negotiation until it reaches `state`, failing after a while.
async fn wait_for_state(router: &axum::Router, provider_pid: &str, state: &str) -> Value {
    let mut last = Value::Null;
    for _ in 0..100 {
        let (status, ack) = call(
            router,
            "GET",
            &format!("/negotiations/{provider_pid}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        if ack["state"] == state {
            return ack;
        }
        last = ack;
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("negotiation never reached {state}; last seen {last}");
}

async fn wait_for_messages(consumer: &Consumer, n: usize) -> Vec<(String, Value)> {
    for _ in 0..100 {
        let received = consumer.received();
        if received.len() >= n {
            return received;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!(
        "consumer received {:?}, expected {n} messages",
        consumer.received()
    );
}

/// Runs a negotiation to FINALIZED; returns its providerPid and agreement id.
async fn finalized(router: &axum::Router, consumer: &Consumer, callback: &str) -> (String, String) {
    let (status, ack) = call(
        router,
        "POST",
        "/negotiations/request",
        Some(request("urn:uuid:c1", Some(callback))),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let provider_pid = ack["providerPid"].as_str().unwrap().to_string();
    wait_for_state(router, &provider_pid, "AGREED").await;
    let agreement = wait_for_messages(consumer, 1).await[0].1["agreement"]["@id"]
        .as_str()
        .unwrap()
        .to_string();
    let (status, _) = call(
        router,
        "POST",
        &format!("/negotiations/{provider_pid}/agreement/verification"),
        Some(json!({
            "@context": ["https://w3id.org/dspace/2025/1/context.jsonld"],
            "@type": "ContractAgreementVerificationMessage",
            "providerPid": provider_pid,
            "consumerPid": "urn:uuid:c1"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    wait_for_state(router, &provider_pid, "FINALIZED").await;
    (provider_pid, agreement)
}

#[tokio::test]
async fn the_provider_agrees_to_a_request_and_sends_the_agreement() {
    let (consumer, callback) = consumer().await;
    let app = app().await;
    let (status, ack) = call(
        &app.router,
        "POST",
        "/negotiations/request",
        Some(request("urn:uuid:c1", Some(&callback))),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(ack["state"], "REQUESTED");
    let provider_pid = ack["providerPid"].as_str().unwrap().to_string();

    wait_for_state(&app.router, &provider_pid, "AGREED").await;
    let received = wait_for_messages(&consumer, 1).await;
    let (path, msg) = &received[0];
    assert_eq!(path, "/negotiations/urn:uuid:c1/agreement");
    assert_eq!(msg["@type"], "ContractAgreementMessage");
    assert_eq!(msg["providerPid"], provider_pid.as_str());
    assert_eq!(msg["consumerPid"], "urn:uuid:c1");
    let agreement = &msg["agreement"];
    assert_eq!(agreement["@type"], "Agreement");
    assert_eq!(
        agreement["target"],
        "urn:uuid:control-plane-rs-seed-dataset"
    );
    assert_eq!(agreement["assigner"], "urn:connector:ds-control-plane-rs");
    assert_eq!(agreement["permission"][0]["action"], "use");
    assert!(agreement["@id"].as_str().unwrap().starts_with("urn:uuid:"));
    assert!(agreement["timestamp"].is_string());

    let stored = app.graph.negotiation(&provider_pid).unwrap().unwrap();
    assert_eq!(
        stored.agreement.unwrap().id,
        agreement["@id"].as_str().unwrap()
    );
}

#[tokio::test]
async fn the_provider_finalizes_once_the_consumer_verifies() {
    let (consumer, callback) = consumer().await;
    let app = app().await;
    let (provider_pid, agreement_id) = finalized(&app.router, &consumer, &callback).await;

    let received = wait_for_messages(&consumer, 2).await;
    let (path, msg) = &received[1];
    assert_eq!(path, "/negotiations/urn:uuid:c1/events");
    assert_eq!(msg["@type"], "ContractNegotiationEventMessage");
    assert_eq!(msg["eventType"], "FINALIZED");
    assert_eq!(msg["providerPid"], provider_pid.as_str());
    assert_eq!(msg["consumerPid"], "urn:uuid:c1");
    assert!(app.graph.agreement(&agreement_id).unwrap().is_some());
}

#[tokio::test]
async fn an_unreachable_consumer_terminates_the_negotiation() {
    // Bind and drop a listener so the port is very likely closed.
    let closed = {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap()
    };
    let app = app().await;
    let (_, ack) = call(
        &app.router,
        "POST",
        "/negotiations/request",
        Some(request(
            "urn:uuid:c1",
            Some(&format!("http://{closed}/callback")),
        )),
    )
    .await;
    let provider_pid = ack["providerPid"].as_str().unwrap().to_string();
    wait_for_state(&app.router, &provider_pid, "TERMINATED").await;
}

/// A Consumer that rejects the Agreement is told the Provider terminated
/// the negotiation, so both sides end up TERMINATED.
#[tokio::test]
async fn a_rejected_agreement_terminates_and_tells_the_consumer() {
    let (consumer, callback) = serve(Consumer {
        fails_agreement: true,
        ..Consumer::default()
    })
    .await;
    let app = app().await;
    let (_, ack) = call(
        &app.router,
        "POST",
        "/negotiations/request",
        Some(request("urn:uuid:c1", Some(&callback))),
    )
    .await;
    let provider_pid = ack["providerPid"].as_str().unwrap().to_string();
    wait_for_state(&app.router, &provider_pid, "TERMINATED").await;
    let received = wait_for_messages(&consumer, 2).await;
    assert_eq!(received[0].0, "/negotiations/urn:uuid:c1/agreement");
    let (path, msg) = &received[1];
    assert_eq!(path, "/negotiations/urn:uuid:c1/termination");
    assert_eq!(msg["@type"], "ContractNegotiationTerminationMessage");
    assert_eq!(msg["providerPid"], provider_pid.as_str());
    assert_eq!(msg["consumerPid"], "urn:uuid:c1");
    assert!(msg["code"].is_string());
}

/// A Consumer whose verification the Provider answered 200 holds the
/// Agreement, so a failed reply to the agreement callback that arrives
/// after the verification must not terminate the negotiation.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_verified_negotiation_is_finalized_even_if_the_agreement_reply_failed() {
    let app = app().await;
    let (consumer, callback) = serve(Consumer {
        verifies_then_fails: Some(app.router.clone()),
        ..Consumer::default()
    })
    .await;
    let mut provider_pids = Vec::new();
    for i in 0..20 {
        let (status, ack) = call(
            &app.router,
            "POST",
            "/negotiations/request",
            Some(request(&format!("urn:uuid:c{i}"), Some(&callback))),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        provider_pids.push(ack["providerPid"].as_str().unwrap().to_string());
    }
    for _ in 0..100 {
        if consumer.verifications.lock().unwrap().len() == provider_pids.len() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let verifications = consumer.verifications.lock().unwrap().clone();
    assert_eq!(verifications.len(), provider_pids.len());
    for (provider_pid, status) in verifications {
        assert_eq!(status, StatusCode::OK);
        let mut state = Value::Null;
        for _ in 0..100 {
            let (_, ack) = call(
                &app.router,
                "GET",
                &format!("/negotiations/{provider_pid}"),
                None,
            )
            .await;
            state = ack["state"].clone();
            if state == "FINALIZED" || state == "TERMINATED" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(state, "FINALIZED", "negotiation {provider_pid}");
    }
}

/// Offers carrying terms the Provider cannot hold (a duty, a remedy, a
/// policy profile, a constraint's unit) are refused rather than agreed as
/// a weaker copy without them.
#[tokio::test]
async fn an_offer_with_terms_the_provider_cannot_hold_is_refused_not_weakened() {
    let (consumer, callback) = consumer().await;
    let app = app().await;
    let lossy = [
        (
            "/offer/permission/0/duty",
            json!([{"action": "compensate"}]),
        ),
        ("/offer/permission/0/remedy", json!([{"action": "delete"}])),
        ("/offer/profile", json!("urn:profile")),
        (
            "/offer/permission/0/constraint",
            json!([{"leftOperand": "count", "operator": "lteq", "rightOperand": 5, "unit": "urn:unit"}]),
        ),
    ];
    for (pointer, value) in lossy {
        let mut msg = request("urn:uuid:c1", Some(&callback));
        let (parent, key) = pointer.rsplit_once('/').unwrap();
        msg.pointer_mut(parent).unwrap()[key] = value;
        let (status, error) = call(&app.router, "POST", "/negotiations/request", Some(msg)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{pointer} was accepted");
        assert_eq!(error["@type"], "ContractNegotiationError");
        assert_eq!(error["consumerPid"], "urn:uuid:c1");
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        consumer.received().is_empty(),
        "an agreement was sent: {:?}",
        consumer.received()
    );
    assert!(app.graph.negotiations().unwrap().is_empty());

    // Empty term lists lose nothing, and the TCK's offer shape (assigner,
    // assignee, an empty `constraints`) is still agreed.
    let mut msg = request("urn:uuid:c2", Some(&callback));
    msg["offer"]["assigner"] = json!("urn:provider");
    msg["offer"]["assignee"] = json!("urn:consumer");
    msg["offer"]["permission"][0]["duty"] = json!([]);
    msg["offer"]["permission"][0]["constraints"] = json!([]);
    let (status, _) = call(&app.router, "POST", "/negotiations/request", Some(msg)).await;
    assert_eq!(status, StatusCode::CREATED);
}

/// The DSP Offer and Agreement schemas require a permission or a
/// prohibition: an offer with neither (no rules, or only an obligation) is
/// refused, so the Provider never sends a schema-invalid Agreement.
#[tokio::test]
async fn an_offer_without_a_permission_or_prohibition_is_refused() {
    let (consumer, callback) = consumer().await;
    let app = app().await;
    for rules in [json!({}), json!({"obligation": [{"action": "compensate"}]})] {
        let mut msg = request("urn:uuid:c1", Some(&callback));
        let offer = msg["offer"].as_object_mut().unwrap();
        offer.remove("permission");
        for (k, v) in rules.as_object().unwrap() {
            offer.insert(k.clone(), v.clone());
        }
        let (status, error) = call(&app.router, "POST", "/negotiations/request", Some(msg)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{rules} was accepted");
        assert_eq!(error["@type"], "ContractNegotiationError");
        assert_eq!(error["consumerPid"], "urn:uuid:c1");
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        consumer.received().is_empty(),
        "an agreement was sent: {:?}",
        consumer.received()
    );
    assert!(app.graph.negotiations().unwrap().is_empty());
}

/// The DSP path is appended to the `callbackAddress`, so one with a query
/// or fragment (or a non-HTTP scheme) would put it in the wrong place, and
/// a `consumerPid` of `.` or `..` would be resolved away by the URL parser:
/// all refused at request time rather than sent somewhere else.
#[tokio::test]
async fn a_callback_url_that_cannot_hold_the_dsp_path_is_refused() {
    let (consumer, callback) = consumer().await;
    let app = app().await;
    let cases = [
        ("urn:uuid:c1", format!("{callback}?x=1")),
        ("urn:uuid:c1", format!("{callback}#frag")),
        ("urn:uuid:c1", "ftp://consumer.example/cb".to_string()),
        ("urn:uuid:c1", "not a url".to_string()),
        ("..", callback.clone()),
        (".", callback.clone()),
    ];
    for (consumer_pid, callback) in cases {
        let msg = request(consumer_pid, Some(&callback));
        let (status, error) = call(&app.router, "POST", "/negotiations/request", Some(msg)).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{consumer_pid} at {callback} was accepted"
        );
        assert_eq!(error["@type"], "ContractNegotiationError");
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        consumer.received().is_empty(),
        "a message was sent: {:?}",
        consumer.received()
    );
    assert!(app.graph.negotiations().unwrap().is_empty());
}

#[tokio::test]
async fn without_a_callback_address_the_negotiation_waits() {
    let app = app().await;
    let (_, ack) = call(
        &app.router,
        "POST",
        "/negotiations/request",
        Some(request("urn:uuid:c1", None)),
    )
    .await;
    let provider_pid = ack["providerPid"].as_str().unwrap().to_string();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (_, ack) = call(
        &app.router,
        "GET",
        &format!("/negotiations/{provider_pid}"),
        None,
    )
    .await;
    assert_eq!(ack["state"], "REQUESTED");
}

fn transfer_request(agreement_id: &str) -> Value {
    json!({
        "@context": ["https://w3id.org/dspace/2025/1/context.jsonld"],
        "@type": "TransferRequestMessage",
        "consumerPid": "urn:uuid:t1",
        "agreementId": agreement_id,
        "format": "HttpData-PULL",
        "callbackAddress": "http://consumer.example/callback"
    })
}

#[tokio::test]
async fn a_transfer_needs_a_finalized_agreement() {
    let (consumer, callback) = consumer().await;
    let app = app().await;

    let (status, err) = call(
        &app.router,
        "POST",
        "/transfers/request",
        Some(transfer_request("urn:uuid:unknown")),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(err["@type"], "TransferError");
    assert_eq!(err["consumerPid"], "urn:uuid:t1");
    assert_eq!(app.graph.transfer_count().unwrap(), 0);

    let (_, agreement_id) = finalized(&app.router, &consumer, &callback).await;
    let (status, ack) = call(
        &app.router,
        "POST",
        "/transfers/request",
        Some(transfer_request(&agreement_id)),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(ack["state"], "REQUESTED");
}

#[tokio::test]
async fn an_agreed_but_unverified_agreement_does_not_start_a_transfer() {
    let (consumer, callback) = consumer().await;
    let app = app().await;
    let (_, ack) = call(
        &app.router,
        "POST",
        "/negotiations/request",
        Some(request("urn:uuid:c1", Some(&callback))),
    )
    .await;
    let provider_pid = ack["providerPid"].as_str().unwrap().to_string();
    wait_for_state(&app.router, &provider_pid, "AGREED").await;
    let agreement_id = wait_for_messages(&consumer, 1).await[0].1["agreement"]["@id"]
        .as_str()
        .unwrap()
        .to_string();
    let (status, _) = call(
        &app.router,
        "POST",
        "/transfers/request",
        Some(transfer_request(&agreement_id)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

fn termination(provider_pid: &str) -> Value {
    json!({
        "@context": ["https://w3id.org/dspace/2025/1/context.jsonld"],
        "@type": "ContractNegotiationTerminationMessage",
        "providerPid": provider_pid,
        "consumerPid": "urn:uuid:c1",
        "code": "cancelled"
    })
}

/// The Provider can terminate a negotiation on its own (an undeliverable
/// agreement), so a Consumer terminating it too must not be an error.
#[tokio::test]
async fn terminating_an_already_terminated_negotiation_succeeds() {
    let app = app().await;
    let (_, ack) = call(
        &app.router,
        "POST",
        "/negotiations/request",
        Some(request("urn:uuid:c1", None)),
    )
    .await;
    let provider_pid = ack["providerPid"].as_str().unwrap().to_string();
    let uri = format!("/negotiations/{provider_pid}/termination");
    let (status, _) = call(&app.router, "POST", &uri, Some(termination(&provider_pid))).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(&app.router, "POST", &uri, Some(termination(&provider_pid))).await;
    assert_eq!(status, StatusCode::OK);
    let (_, ack) = call(
        &app.router,
        "GET",
        &format!("/negotiations/{provider_pid}"),
        None,
    )
    .await;
    assert_eq!(ack["state"], "TERMINATED");
}

/// Two terminations racing each other (a retry, or the Consumer and the
/// Provider's own termination of an undeliverable agreement) both succeed:
/// the already-TERMINATED check and the write happen under one lock.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_terminations_all_succeed() {
    let app = app().await;
    for i in 0..100 {
        let (_, ack) = call(
            &app.router,
            "POST",
            "/negotiations/request",
            Some(request(&format!("urn:uuid:c{i}"), None)),
        )
        .await;
        let provider_pid = ack["providerPid"].as_str().unwrap().to_string();
        let uri = format!("/negotiations/{provider_pid}/termination");
        let sends: Vec<_> = (0..2)
            .map(|_| {
                let router = app.router.clone();
                let uri = uri.clone();
                let body = termination(&provider_pid);
                tokio::spawn(async move { call(&router, "POST", &uri, Some(body)).await })
            })
            .collect();
        for send in sends {
            let (status, err) = send.await.unwrap();
            assert_eq!(status, StatusCode::OK, "negotiation {provider_pid}: {err}");
        }
    }
}

/// A finalized negotiation stays final: terminating it is still an error.
#[tokio::test]
async fn terminating_a_finalized_negotiation_is_still_rejected() {
    let (consumer, callback) = consumer().await;
    let app = app().await;
    let (provider_pid, _) = finalized(&app.router, &consumer, &callback).await;
    let uri = format!("/negotiations/{provider_pid}/termination");
    let (status, err) = call(&app.router, "POST", &uri, Some(termination(&provider_pid))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(err["@type"], "ContractNegotiationError");
}

/// A Consumer may counter only an Offer; once the Provider has agreed, a
/// counter-request is an error and leaves the agreed offer alone.
#[tokio::test]
async fn a_counter_request_after_agreement_is_rejected() {
    let (consumer, callback) = consumer().await;
    let app = app().await;
    let (_, ack) = call(
        &app.router,
        "POST",
        "/negotiations/request",
        Some(request("urn:uuid:c1", Some(&callback))),
    )
    .await;
    let provider_pid = ack["providerPid"].as_str().unwrap().to_string();
    wait_for_state(&app.router, &provider_pid, "AGREED").await;
    wait_for_messages(&consumer, 1).await;

    let mut counter = request("urn:uuid:c1", None);
    counter["providerPid"] = json!(provider_pid);
    counter["offer"]["@id"] = json!("urn:uuid:counter");
    counter["offer"]["permission"][0]["action"] = json!("distribute");
    let (status, err) = call(
        &app.router,
        "POST",
        &format!("/negotiations/{provider_pid}/request"),
        Some(counter),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(err["@type"], "ContractNegotiationError");
    let stored = app.graph.negotiation(&provider_pid).unwrap().unwrap();
    assert_eq!(
        stored.offer.unwrap().id,
        "urn:uuid:control-plane-rs-seed-offer"
    );
}

/// A Consumer retrying its verification (its first reply was lost, or the
/// Provider stopped before finalizing) is answered 200, and a negotiation
/// still VERIFIED is finalized then.
#[tokio::test]
async fn a_repeated_verification_is_answered_and_finalizes() {
    use control_plane_core::negotiation::{ContractNegotiation, NegotiationState};
    let (consumer, callback) = consumer().await;
    let app = app().await;
    let (status, ack) = call(
        &app.router,
        "POST",
        "/negotiations/request",
        Some(request("urn:uuid:c1", Some(&callback))),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let agreed_pid = ack["providerPid"].as_str().unwrap().to_string();
    wait_for_state(&app.router, &agreed_pid, "AGREED").await;
    wait_for_messages(&consumer, 1).await;
    // As a restart leaves it: VERIFIED, with finalize never run.
    let mut stuck: ContractNegotiation = app.graph.negotiation(&agreed_pid).unwrap().unwrap();
    stuck.state = NegotiationState::Verified;
    app.graph.put_negotiation(&stuck).unwrap();

    let verification = json!({
        "@context": ["https://w3id.org/dspace/2025/1/context.jsonld"],
        "@type": "ContractAgreementVerificationMessage",
        "providerPid": agreed_pid,
        "consumerPid": "urn:uuid:c1"
    });
    let uri = format!("/negotiations/{agreed_pid}/agreement/verification");
    let (status, _) = call(&app.router, "POST", &uri, Some(verification.clone())).await;
    assert_eq!(status, StatusCode::OK);
    wait_for_state(&app.router, &agreed_pid, "FINALIZED").await;
    let received = wait_for_messages(&consumer, 2).await;
    assert_eq!(received[1].0, "/negotiations/urn:uuid:c1/events");

    // Once FINALIZED, the retry is still answered 200, without a change.
    let (status, _) = call(&app.router, "POST", &uri, Some(verification)).await;
    assert_eq!(status, StatusCode::OK);
    let (_, ack) = call(
        &app.router,
        "GET",
        &format!("/negotiations/{agreed_pid}"),
        None,
    )
    .await;
    assert_eq!(ack["state"], "FINALIZED");
}
