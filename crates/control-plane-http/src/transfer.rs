//! Transfer Process protocol HTTP surface, Provider role only, per the
//! [Transfer Process HTTPS Binding](https://eclipse-dataspace-protocol-base.github.io/DataspaceProtocol/HEAD/#transfer-http)'s
//! "Provider Path Bindings" table. Consumer callback endpoints are out of
//! scope for this pass — see `../../../ARCHITECTURE.md`.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use control_plane_core::transfer::{
    TransferCompletionMessage, TransferErrorMessage, TransferProcess, TransferRequestMessage,
    TransferStartMessage, TransferState, TransferSuspensionMessage, TransferTerminationMessage,
};

use crate::state::AppState;

fn not_found(provider_pid: &str) -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        Json(TransferErrorMessage::new(
            Some(provider_pid.to_string()),
            None,
            "404",
            format!("no transfer process with providerPid '{provider_pid}'"),
        )),
    )
}

fn bad_transition(
    provider_pid: &str,
    consumer_pid: &str,
    from: TransferState,
    to: TransferState,
) -> impl IntoResponse {
    (
        StatusCode::BAD_REQUEST,
        Json(TransferErrorMessage::new(
            Some(provider_pid.to_string()),
            Some(consumer_pid.to_string()),
            "400",
            format!("cannot transition transfer process from {from:?} to {to:?}"),
        )),
    )
}

/// `GET {dsp}/transfers/:providerPid`.
pub async fn get_transfer(
    State(state): State<Arc<AppState>>,
    Path(provider_pid): Path<String>,
) -> impl IntoResponse {
    let transfers = state.transfers.lock().expect("transfers lock poisoned");
    match transfers.get(&provider_pid) {
        Some(t) => (StatusCode::OK, Json(t.to_ack())).into_response(),
        None => not_found(&provider_pid).into_response(),
    }
}

/// `POST {dsp}/transfers/request` — always accepted by this bootstrap's
/// always-allow stub policy (no cross-check against a real Agreement store
/// yet — see `../../../ARCHITECTURE.md`).
pub async fn initiate_transfer(
    State(state): State<Arc<AppState>>,
    Json(msg): Json<TransferRequestMessage>,
) -> impl IntoResponse {
    let provider_pid = format!("urn:uuid:{}", uuid::Uuid::new_v4());
    let transfer = TransferProcess::new(
        provider_pid.clone(),
        msg.consumer_pid.clone(),
        msg.agreement_id,
        msg.format,
    );
    let ack = transfer.to_ack();
    state
        .transfers
        .lock()
        .expect("transfers lock poisoned")
        .insert(provider_pid, transfer);
    (StatusCode::CREATED, Json(ack))
}

/// `POST {dsp}/transfers/:providerPid/start`.
pub async fn start_transfer(
    State(state): State<Arc<AppState>>,
    Path(provider_pid): Path<String>,
    Json(_msg): Json<TransferStartMessage>,
) -> impl IntoResponse {
    transition(&state, &provider_pid, TransferState::Started).await
}

/// `POST {dsp}/transfers/:providerPid/completion`.
pub async fn complete_transfer(
    State(state): State<Arc<AppState>>,
    Path(provider_pid): Path<String>,
    Json(_msg): Json<TransferCompletionMessage>,
) -> impl IntoResponse {
    transition(&state, &provider_pid, TransferState::Completed).await
}

/// `POST {dsp}/transfers/:providerPid/termination`.
pub async fn terminate_transfer(
    State(state): State<Arc<AppState>>,
    Path(provider_pid): Path<String>,
    Json(_msg): Json<TransferTerminationMessage>,
) -> impl IntoResponse {
    transition(&state, &provider_pid, TransferState::Terminated).await
}

/// `POST {dsp}/transfers/:providerPid/suspension`.
pub async fn suspend_transfer(
    State(state): State<Arc<AppState>>,
    Path(provider_pid): Path<String>,
    Json(_msg): Json<TransferSuspensionMessage>,
) -> impl IntoResponse {
    transition(&state, &provider_pid, TransferState::Suspended).await
}

async fn transition(
    state: &Arc<AppState>,
    provider_pid: &str,
    to: TransferState,
) -> axum::response::Response {
    let mut transfers = state.transfers.lock().expect("transfers lock poisoned");
    let Some(t) = transfers.get_mut(provider_pid) else {
        return not_found(provider_pid).into_response();
    };
    let from = t.state;
    match t.transition(to) {
        Ok(()) => StatusCode::OK.into_response(),
        Err(_) => bad_transition(provider_pid, &t.consumer_pid, from, to).into_response(),
    }
}
