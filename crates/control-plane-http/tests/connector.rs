//! The Contreforts connector reaches the running server's own store: a
//! dataset pushed through a `ControlPlaneConnector` built over
//! `App::graph` is served by the catalog at once, with no restart and no
//! second process opening the (RocksDB-locked) store directory.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use contreforts_core::{ContrefortsConnector, Document, EntityKind};
use control_plane_contreforts::{ControlPlaneConnector, DATASET_KIND};
use control_plane_http::config::Config;
use tower::ServiceExt;

async fn catalog(router: &axum::Router) -> serde_json::Value {
    let request = Request::builder()
        .method("POST")
        .uri("/dsp/2025-1/catalog/request")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({
                "@context": ["https://w3id.org/dspace/2025/1/context.jsonld"],
                "@type": "CatalogRequestMessage",
            })
            .to_string(),
        ))
        .unwrap();
    let response = router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn a_dataset_pushed_through_the_connector_is_served_without_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let app = control_plane_http::build(Config {
        port: 0,
        store_path: Some(dir.path().to_path_buf()),
        ..Config::default()
    })
    .await
    .unwrap();

    let mut dataset = catalog(&app.router).await["dataset"][0].clone();
    dataset["@id"] = "urn:uuid:pushed-dataset".into();

    let connector = ControlPlaneConnector::new(app.graph.clone());
    connector
        .push(&Document {
            name: "urn:uuid:pushed-dataset".to_string(),
            remote_id: "urn:uuid:pushed-dataset".to_string(),
            kind: EntityKind::new(DATASET_KIND),
            source: "test".to_string(),
            modified: None,
            fields: dataset,
        })
        .await
        .unwrap();

    let ids: Vec<_> = catalog(&app.router).await["dataset"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["@id"].as_str().unwrap().to_string())
        .collect();
    assert!(ids.contains(&"urn:uuid:pushed-dataset".to_string()), "{ids:?}");
}
