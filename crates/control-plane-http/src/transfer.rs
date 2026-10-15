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

use control_plane_graph::GraphError;

use crate::state::AppState;

fn store_error(provider_pid: Option<&str>, e: GraphError) -> axum::response::Response {
    tracing::error!(error = %e, "transfer store error");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(TransferErrorMessage::new(
            provider_pid.map(str::to_string),
            None,
            "500",
            "transfer process store error",
        )),
    )
        .into_response()
}

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
    let lookup = provider_pid.clone();
    match state.with_graph(move |g| g.transfer(&lookup)).await {
        Ok(Some(t)) => (StatusCode::OK, Json(t.to_ack())).into_response(),
        Ok(None) => not_found(&provider_pid).into_response(),
        Err(e) => store_error(Some(&provider_pid), e),
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
        provider_pid,
        msg.consumer_pid.clone(),
        msg.agreement_id,
        msg.format,
    );
    let stored = transfer.clone();
    match state.with_graph(move |g| g.put_transfer(&stored)).await {
        Ok(()) => (StatusCode::CREATED, Json(transfer.to_ack())).into_response(),
        Err(e) => store_error(None, e),
    }
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
    let pid = provider_pid.to_string();
    let result = state
        .with_graph(move |g| {
            g.update_transfer(&pid, |t| {
                let from = t.state;
                t.transition(to).map_err(|_| (from, t.consumer_pid.clone()))
            })
        })
        .await;
    match result {
        Ok(Some(Ok(()))) => StatusCode::OK.into_response(),
        Ok(Some(Err((from, consumer_pid)))) => {
            bad_transition(provider_pid, &consumer_pid, from, to).into_response()
        }
        Ok(None) => not_found(provider_pid).into_response(),
        Err(e) => store_error(Some(provider_pid), e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A store failure while handling a request whose consumerPid and
    /// providerPid are known reports both, so the consumer can match the
    /// error to its process.
    #[tokio::test]
    async fn store_errors_carry_the_known_pids() {
        let e = GraphError::Missing {
            node: "urn:uuid:p1".to_string(),
            property: "state".to_string(),
        };
        let response = store_error(Some("urn:uuid:p1"), Some("urn:uuid:c1"), e);
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["providerPid"], "urn:uuid:p1");
        assert_eq!(body["consumerPid"], "urn:uuid:c1");
    }
}
