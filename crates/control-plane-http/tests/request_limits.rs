//! Requests that would make the store hold an outsized record are refused
//! before anything is written: an offer with too many rules, a rule with
//! too many constraints, an oversized `rightOperand`, or a body past the
//! DSP routes' limit.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use control_plane_graph::ControlPlaneGraph;
use control_plane_http::config::Config;
use tower::ServiceExt;

const DSP: &str = "/dsp/2025-1";

fn config(store: &std::path::Path) -> Config {
    Config {
        port: 0,
        store_path: Some(store.to_path_buf()),
        ..Config::default()
    }
}

async fn post(router: &axum::Router, uri: &str, body: String) -> StatusCode {
    let request = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();
    router.clone().oneshot(request).await.unwrap().status()
}

fn request(rules: serde_json::Value) -> String {
    serde_json::json!({
        "@context": ["https://w3id.org/dspace/2025/1/context.jsonld"],
        "@type": "ContractRequestMessage",
        "consumerPid": "urn:uuid:consumer",
        "offer": {
            "@id": "urn:uuid:offer",
            "@type": "Offer",
            "target": "urn:uuid:control-plane-rs-seed-dataset",
            "permission": rules,
        },
    })
    .to_string()
}

fn rule(constraints: usize, operand: &str) -> serde_json::Value {
    let constraint: Vec<_> = (0..constraints)
        .map(|_| serde_json::json!({ "leftOperand": "purpose", "operator": "eq", "rightOperand": operand }))
        .collect();
    serde_json::json!({ "action": "use", "constraint": constraint })
}

#[tokio::test]
async fn oversized_offers_are_rejected_and_store_nothing() {
    let dir = tempfile::tempdir().unwrap();
    {
        let app = control_plane_http::build(config(dir.path())).await.unwrap();
        let negotiations = format!("{DSP}/negotiations/request");

        let many_rules = serde_json::Value::Array((0..1000).map(|_| rule(0, "x")).collect());
        assert_eq!(
            post(&app.router, &negotiations, request(many_rules)).await,
            StatusCode::BAD_REQUEST
        );

        let many_constraints = serde_json::json!([rule(1000, "x")]);
        assert_eq!(
            post(&app.router, &negotiations, request(many_constraints)).await,
            StatusCode::BAD_REQUEST
        );

        let big_operand = serde_json::json!([rule(1, &"x".repeat(20_000))]);
        assert_eq!(
            post(&app.router, &negotiations, request(big_operand)).await,
            StatusCode::BAD_REQUEST
        );

        let huge_body = serde_json::json!([rule(1, &"x".repeat(1_500_000))]);
        assert_eq!(
            post(&app.router, &negotiations, request(huge_body)).await,
            StatusCode::PAYLOAD_TOO_LARGE
        );

        // A normal request still goes through.
        let (status, pid) = {
            let request = Request::builder()
                .method("POST")
                .uri(&negotiations)
                .header("content-type", "application/json")
                .body(Body::from(request(serde_json::json!([rule(2, "x")]))))
                .unwrap();
            let response = app.router.clone().oneshot(request).await.unwrap();
            let status = response.status();
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            let ack: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            (status, ack["providerPid"].as_str().unwrap().to_string())
        };
        assert_eq!(status, StatusCode::CREATED);

        // A counter-offer to an existing negotiation is held to the same
        // limits.
        let many_rules = serde_json::Value::Array((0..1000).map(|_| rule(0, "x")).collect());
        assert_eq!(
            post(
                &app.router,
                &format!("{DSP}/negotiations/{pid}/request"),
                request(many_rules)
            )
            .await,
            StatusCode::BAD_REQUEST
        );
    }

    let graph = ControlPlaneGraph::open(dir.path()).unwrap();
    let stored = graph.negotiations().unwrap();
    assert_eq!(stored.len(), 1, "only the in-limits request was stored");
    let offer = stored[0].value.offer.as_ref().unwrap();
    assert_eq!(
        offer.permission.len(),
        1,
        "the counter-offer was not stored"
    );
}
