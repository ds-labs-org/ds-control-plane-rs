//! Transfer Process protocol HTTP surface, Provider role only, per the
//! [Transfer Process HTTPS Binding](https://eclipse-dataspace-protocol-base.github.io/DataspaceProtocol/HEAD/#transfer-http)'s
//! "Provider Path Bindings" table. Consumer callback endpoints are out of
//! scope for this pass — see `../../../ARCHITECTURE.md`.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{Extension, Json};
use control_plane_core::transfer::{
    TransferCompletionMessage, TransferErrorMessage, TransferProcess, TransferRequestMessage,
    TransferStartMessage, TransferState, TransferSuspensionMessage, TransferTerminationMessage,
};

use control_plane_graph::GraphError;

use crate::auth::Participant;
use crate::state::AppState;

fn store_error(
    provider_pid: Option<&str>,
    consumer_pid: Option<&str>,
    e: GraphError,
) -> axum::response::Response {
    tracing::error!(error = %e, "transfer store error");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(TransferErrorMessage::new(
            provider_pid.map(str::to_string),
            consumer_pid.map(str::to_string),
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

/// A 403: the transfer process (or the agreement it names) belongs to
/// another participant.
fn forbidden(provider_pid: &str, reason: &str) -> axum::response::Response {
    (
        StatusCode::FORBIDDEN,
        Json(TransferErrorMessage::new(
            Some(provider_pid.to_string()),
            None,
            "403",
            reason,
        )),
    )
        .into_response()
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
    Extension(participant): Extension<Participant>,
    Path(provider_pid): Path<String>,
) -> impl IntoResponse {
    let lookup = provider_pid.clone();
    match state.with_graph(move |g| g.transfer(&lookup)).await {
        Ok(Some(t)) if !participant.may_act_on(t.participant_id.as_deref()) => {
            forbidden(&provider_pid, OTHER_PARTICIPANT)
        }
        Ok(Some(t)) => (StatusCode::OK, Json(t.to_ack())).into_response(),
        Ok(None) => not_found(&provider_pid).into_response(),
        Err(e) => store_error(Some(&provider_pid), None, e),
    }
}

const OTHER_PARTICIPANT: &str = "the transfer process belongs to another participant";

/// What [`initiate_transfer`] found for the requested agreement.
enum Grant {
    /// Finalized and granted to the caller: the transfer was stored.
    Granted,
    /// No finalized agreement with that id.
    NoAgreement,
    /// Finalized, but granted to another participant.
    OtherAssignee,
}

/// `POST {dsp}/transfers/request` — accepted only under an `agreementId`
/// this connector has finalized a negotiation for
/// ([`ControlPlaneGraph::agreement`](control_plane_graph::ControlPlaneGraph::agreement));
/// any other is a `TransferError` 400. With DCP on, the agreement's
/// `assignee` must also be the authenticated participant, or it is a 403;
/// the transfer is then bound to that participant.
pub async fn initiate_transfer(
    State(state): State<Arc<AppState>>,
    Extension(participant): Extension<Participant>,
    Json(msg): Json<TransferRequestMessage>,
) -> impl IntoResponse {
    let provider_pid = format!("urn:uuid:{}", uuid::Uuid::new_v4());
    let mut transfer = TransferProcess::new(
        provider_pid,
        msg.consumer_pid.clone(),
        msg.agreement_id,
        msg.format,
    );
    transfer.participant_id = participant.0.clone();
    let stored = transfer.clone();
    let result = state
        .with_graph(move |g| match g.agreement(&stored.agreement_id)? {
            Some(a) if !participant.may_act_on(Some(&a.assignee)) => Ok(Grant::OtherAssignee),
            Some(_) => g.put_transfer(&stored).map(|()| Grant::Granted),
            None => Ok(Grant::NoAgreement),
        })
        .await;
    match result {
        Ok(Grant::Granted) => (StatusCode::CREATED, Json(transfer.to_ack())).into_response(),
        Ok(Grant::OtherAssignee) => forbidden(
            &transfer.provider_pid,
            "the contract agreement was granted to another participant",
        ),
        Ok(Grant::NoAgreement) => (
            StatusCode::BAD_REQUEST,
            Json(TransferErrorMessage::new(
                Some(transfer.provider_pid.clone()),
                Some(transfer.consumer_pid.clone()),
                "400",
                format!(
                    "no finalized contract agreement '{}'",
                    transfer.agreement_id
                ),
            )),
        )
            .into_response(),
        Err(e) => store_error(
            Some(&transfer.provider_pid),
            Some(&transfer.consumer_pid),
            e,
        ),
    }
}

/// `POST {dsp}/transfers/:providerPid/start`.
pub async fn start_transfer(
    State(state): State<Arc<AppState>>,
    Extension(participant): Extension<Participant>,
    Path(provider_pid): Path<String>,
    Json(_msg): Json<TransferStartMessage>,
) -> impl IntoResponse {
    transition(&state, participant, &provider_pid, TransferState::Started).await
}

/// `POST {dsp}/transfers/:providerPid/completion`.
pub async fn complete_transfer(
    State(state): State<Arc<AppState>>,
    Extension(participant): Extension<Participant>,
    Path(provider_pid): Path<String>,
    Json(_msg): Json<TransferCompletionMessage>,
) -> impl IntoResponse {
    transition(&state, participant, &provider_pid, TransferState::Completed).await
}

/// `POST {dsp}/transfers/:providerPid/termination`.
pub async fn terminate_transfer(
    State(state): State<Arc<AppState>>,
    Extension(participant): Extension<Participant>,
    Path(provider_pid): Path<String>,
    Json(_msg): Json<TransferTerminationMessage>,
) -> impl IntoResponse {
    transition(
        &state,
        participant,
        &provider_pid,
        TransferState::Terminated,
    )
    .await
}

/// `POST {dsp}/transfers/:providerPid/suspension`.
pub async fn suspend_transfer(
    State(state): State<Arc<AppState>>,
    Extension(participant): Extension<Participant>,
    Path(provider_pid): Path<String>,
    Json(_msg): Json<TransferSuspensionMessage>,
) -> impl IntoResponse {
    transition(&state, participant, &provider_pid, TransferState::Suspended).await
}

async fn transition(
    state: &Arc<AppState>,
    participant: Participant,
    provider_pid: &str,
    to: TransferState,
) -> axum::response::Response {
    let pid = provider_pid.to_string();
    // `Err(None)`: bound to another participant.
    let result = state
        .with_graph(move |g| {
            g.update_transfer(&pid, |t| {
                if !participant.may_act_on(t.participant_id.as_deref()) {
                    return Err(None);
                }
                let from = t.state;
                t.transition(to)
                    .map_err(|_| Some((from, t.consumer_pid.clone())))
            })
        })
        .await;
    match result {
        Ok(Some(Ok(()))) => StatusCode::OK.into_response(),
        Ok(Some(Err(None))) => forbidden(provider_pid, OTHER_PARTICIPANT),
        Ok(Some(Err(Some((from, consumer_pid))))) => {
            bad_transition(provider_pid, &consumer_pid, from, to).into_response()
        }
        Ok(None) => not_found(provider_pid).into_response(),
        Err(e) => store_error(Some(provider_pid), None, e),
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
