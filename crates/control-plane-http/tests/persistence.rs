//! Negotiations, transfers and the catalog survive a restart when
//! `Config::store_path` is set: the whole app is built, driven over its
//! real router, dropped, and built again on the same store directory.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use control_plane_http::config::Config;
use tower::ServiceExt;

fn config(store: &std::path::Path, base_url: &str) -> Config {
    Config {
        port: 0,
        base_url: base_url.to_string(),
        store_path: Some(store.to_path_buf()),
        ..Config::default()
    }
}

async fn call(
    router: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
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
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

#[tokio::test]
async fn runtime_state_and_catalog_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();

    let (negotiation_pid, transfer_pid) = {
        let app = control_plane_http::build(config(dir.path(), "http://before.example"))
            .await
            .unwrap();
        let (status, ack) = call(
            &app.router,
            "POST",
            "/dsp/2025-1/negotiations/request",
            Some(serde_json::json!({
                "@context": ["https://w3id.org/dspace/2025/1/context.jsonld"],
                "@type": "ContractRequestMessage",
                "consumerPid": "urn:uuid:consumer-n",
                "offer": {
                    "@id": "urn:uuid:control-plane-rs-seed-offer",
                    "@type": "Offer",
                    "target": "urn:uuid:control-plane-rs-seed-dataset",
                    "permission": [{ "action": "use" }],
                },
                "callbackAddress": "http://consumer.example/callback",
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let negotiation_pid = ack["providerPid"].as_str().unwrap().to_string();

        let (status, _) = call(
            &app.router,
            "POST",
            &format!("/dsp/2025-1/negotiations/{negotiation_pid}/termination"),
            Some(serde_json::json!({
                "@context": ["https://w3id.org/dspace/2025/1/context.jsonld"],
                "@type": "ContractNegotiationTerminationMessage",
                "providerPid": negotiation_pid,
                "consumerPid": "urn:uuid:consumer-n",
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, ack) = call(
            &app.router,
            "POST",
            "/dsp/2025-1/transfers/request",
            Some(serde_json::json!({
                "@context": ["https://w3id.org/dspace/2025/1/context.jsonld"],
                "@type": "TransferRequestMessage",
                "consumerPid": "urn:uuid:consumer-t",
                "agreementId": "urn:uuid:agreement",
                "format": "HttpData-PULL",
                "callbackAddress": "http://consumer.example/callback",
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let transfer_pid = ack["providerPid"].as_str().unwrap().to_string();

        let (status, _) = call(
            &app.router,
            "POST",
            &format!("/dsp/2025-1/transfers/{transfer_pid}/start"),
            Some(serde_json::json!({
                "@context": ["https://w3id.org/dspace/2025/1/context.jsonld"],
                "@type": "TransferStartMessage",
                "providerPid": transfer_pid,
                "consumerPid": "urn:uuid:consumer-t",
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        (negotiation_pid, transfer_pid)
        // `app` drops here, releasing the store's lock.
    };

    let app = control_plane_http::build(config(dir.path(), "http://after.example"))
        .await
        .unwrap();

    let (status, ack) = call(
        &app.router,
        "GET",
        &format!("/dsp/2025-1/negotiations/{negotiation_pid}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ack["state"], "TERMINATED");
    assert_eq!(ack["consumerPid"], "urn:uuid:consumer-n");

    let (status, ack) = call(
        &app.router,
        "GET",
        &format!("/dsp/2025-1/transfers/{transfer_pid}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ack["state"], "STARTED");

    // The catalog was seeded once and kept; its endpoint follows the new
    // base URL from the settings written at the second start.
    let (status, catalog) = call(
        &app.router,
        "POST",
        "/dsp/2025-1/catalog/request",
        Some(serde_json::json!({
            "@context": ["https://w3id.org/dspace/2025/1/context.jsonld"],
            "@type": "CatalogRequestMessage",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let datasets = catalog["dataset"].as_array().unwrap();
    assert_eq!(datasets.len(), 1);
    assert_eq!(
        datasets[0]["distribution"][0]["accessService"]["endpointURL"],
        "http://after.example/dsp/2025-1"
    );
}

#[tokio::test]
async fn without_a_store_path_nothing_is_kept() {
    let app = control_plane_http::build(Config {
        port: 0,
        ..Config::default()
    })
    .await
    .unwrap();
    let (status, _) = call(
        &app.router,
        "GET",
        "/dsp/2025-1/catalog/datasets/urn:uuid:control-plane-rs-seed-dataset",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(
        &app.router,
        "GET",
        "/dsp/2025-1/catalog/datasets/urn:uuid:unknown",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
