//! A handler waiting on the graph's write lock, or on a slow store write,
//! must not park a Tokio worker: requests that never touch the store keep
//! being served while the store is busy.

use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::{get, post};
use control_plane_core::negotiation::ContractNegotiation;
use control_plane_http::config::Config;
use control_plane_http::state::AppState;
use control_plane_http::{metadata, negotiation};
use tower::ServiceExt;

const DSP: &str = "/dsp/2025-1";
const LOCK_HELD: Duration = Duration::from_secs(3);

fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route(
            "/.well-known/dspace-version",
            get(metadata::well_known_version),
        )
        .route(
            &format!("{DSP}/negotiations/request"),
            post(negotiation::initiate_negotiation),
        )
        .with_state(state)
}

fn initiate_request() -> Request<Body> {
    let body = serde_json::json!({
        "@context": ["https://w3id.org/dspace/2025/1/context.jsonld"],
        "@type": "ContractRequestMessage",
        "consumerPid": "urn:uuid:consumer",
        "offer": {
            "@id": "urn:uuid:offer",
            "@type": "Offer",
            "target": "urn:uuid:control-plane-rs-seed-dataset",
            "permission": [{ "action": "use" }],
        },
    });
    Request::builder()
        .method("POST")
        .uri(format!("{DSP}/negotiations/request"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn store_writes_waiting_on_the_lock_do_not_starve_other_requests() {
    let state = Arc::new(AppState::open(Config::default()).unwrap());
    let held = ContractNegotiation::new("urn:uuid:held", "urn:uuid:c");
    state.graph.put_negotiation(&held).unwrap();

    // Another writer holds the write lock for a while, as a large commit
    // would.
    let (locked_tx, locked_rx) = mpsc::channel();
    let graph = state.graph.clone();
    let holder = std::thread::spawn(move || {
        graph
            .update_negotiation("urn:uuid:held", |_| {
                locked_tx.send(()).unwrap();
                std::thread::sleep(LOCK_HELD);
                Ok::<(), ()>(())
            })
            .unwrap();
    });
    locked_rx.recv().unwrap();

    // As many store writes as there are workers queue on the lock.
    let app = router(state.clone());
    let writer = tokio::spawn(app.clone().oneshot(initiate_request()));
    std::thread::sleep(Duration::from_millis(200));

    // A request that never touches the store is still served promptly.
    // Waited for and timed with the wall clock: a parked worker also
    // stops Tokio's timers.
    let started = Instant::now();
    let version = tokio::spawn(
        app.clone().oneshot(
            Request::builder()
                .uri("/.well-known/dspace-version")
                .body(Body::empty())
                .unwrap(),
        ),
    );
    let response = version.await.unwrap().unwrap();
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(1),
        "GET /.well-known/dspace-version stalled {elapsed:?} behind a store write"
    );
    assert_eq!(response.status(), StatusCode::OK);

    let created = writer.await.unwrap().unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    holder.join().unwrap();
}
