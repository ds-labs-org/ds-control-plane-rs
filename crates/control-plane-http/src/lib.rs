//! Library half of the `control-plane-http` crate: everything `main.rs`
//! needs to assemble and run the DSP connector control plane, minus
//! process bootstrap (tracing init, the "listening on" log line). Split
//! out so tests can build and drive the whole application against an
//! ephemeral port without going through a real `main`.
//!
//! See `../../ARCHITECTURE.md` for what this bootstrap implements,
//! stubs, and leaves out of scope.

pub mod catalog;
pub mod config;
pub mod metadata;
pub mod negotiation;
pub mod state;
pub mod transfer;

use std::sync::Arc;

use axum::Router;
use axum::routing::{get, post};

use crate::config::Config;
use crate::state::AppState;

/// The fully assembled application: the listener already bound and the
/// axum router fully built, ready to hand to [`serve`].
pub struct App {
    pub listener: tokio::net::TcpListener,
    pub router: Router,
}

/// Builds the whole application from `config` and binds its listener.
/// Binding happens here (not in [`serve`]) so a caller — a test in
/// particular — can read back the OS-assigned port via
/// `TcpListener::local_addr()` when `config.port == 0`.
pub async fn build(config: Config) -> anyhow::Result<App> {
    let dsp = &config.dsp_path;
    let state = Arc::new(AppState::open(config.clone())?);
    let restored = (
        state.graph.negotiations()?.len(),
        state.graph.transfers()?.len(),
    );
    tracing::info!(
        store = ?config.store_path,
        datasets = state.graph.datasets()?.len(),
        negotiations = restored.0,
        transfers = restored.1,
        "opened control plane graph"
    );

    let router = Router::new()
        .route(
            "/.well-known/dspace-version",
            get(metadata::well_known_version),
        )
        .route(
            &format!("{dsp}/catalog/request"),
            post(catalog::catalog_request),
        )
        .route(
            &format!("{dsp}/catalog/datasets/{{id}}"),
            get(catalog::dataset_request),
        )
        .route(
            &format!("{dsp}/negotiations/{{provider_pid}}"),
            get(negotiation::get_negotiation),
        )
        .route(
            &format!("{dsp}/negotiations/request"),
            post(negotiation::initiate_negotiation),
        )
        .route(
            &format!("{dsp}/negotiations/{{provider_pid}}/request"),
            post(negotiation::continue_negotiation),
        )
        .route(
            &format!("{dsp}/negotiations/{{provider_pid}}/events"),
            post(negotiation::negotiation_event),
        )
        .route(
            &format!("{dsp}/negotiations/{{provider_pid}}/agreement/verification"),
            post(negotiation::verify_agreement),
        )
        .route(
            &format!("{dsp}/negotiations/{{provider_pid}}/termination"),
            post(negotiation::terminate_negotiation),
        )
        .route(
            &format!("{dsp}/transfers/{{provider_pid}}"),
            get(transfer::get_transfer),
        )
        .route(
            &format!("{dsp}/transfers/request"),
            post(transfer::initiate_transfer),
        )
        .route(
            &format!("{dsp}/transfers/{{provider_pid}}/start"),
            post(transfer::start_transfer),
        )
        .route(
            &format!("{dsp}/transfers/{{provider_pid}}/completion"),
            post(transfer::complete_transfer),
        )
        .route(
            &format!("{dsp}/transfers/{{provider_pid}}/termination"),
            post(transfer::terminate_transfer),
        )
        .route(
            &format!("{dsp}/transfers/{{provider_pid}}/suspension"),
            post(transfer::suspend_transfer),
        )
        .with_state(state);

    let addr = format!("0.0.0.0:{}", config.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    Ok(App { listener, router })
}

/// Runs the router against its already-bound listener until either it
/// exits (an error) or the process receives ctrl-c.
pub async fn serve(app: App) -> anyhow::Result<()> {
    tokio::select! {
        result = axum::serve(app.listener, app.router) => result?,
        _ = tokio::signal::ctrl_c() => tracing::info!("shutting down"),
    }
    Ok(())
}
