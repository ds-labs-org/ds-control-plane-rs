//! Contract Negotiation protocol HTTP surface, Provider role only, per the
//! [Contract Negotiation HTTPS Binding](https://eclipse-dataspace-protocol-base.github.io/DataspaceProtocol/HEAD/#negotiation-http)'s
//! "Provider Path Bindings" table. Consumer-role endpoints
//! (`/:callback/negotiations/...`) are out of scope for this pass — see
//! `../../../ARCHITECTURE.md`.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use control_plane_core::negotiation::{
    ContractAgreementVerificationMessage, ContractNegotiation, ContractNegotiationError,
    ContractNegotiationEventMessage, ContractNegotiationTerminationMessage, ContractRequestMessage,
    NegotiationEventType, NegotiationState,
};

use crate::state::AppState;

fn not_found(provider_pid: &str) -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        Json(ContractNegotiationError::new(
            Some(provider_pid.to_string()),
            None,
            "404",
            format!("no contract negotiation with providerPid '{provider_pid}'"),
        )),
    )
}

fn bad_transition(
    provider_pid: &str,
    consumer_pid: &str,
    from: NegotiationState,
    to: NegotiationState,
) -> impl IntoResponse {
    (
        StatusCode::BAD_REQUEST,
        Json(ContractNegotiationError::new(
            Some(provider_pid.to_string()),
            Some(consumer_pid.to_string()),
            "400",
            format!("cannot transition contract negotiation from {from:?} to {to:?}"),
        )),
    )
}

/// `GET {dsp}/negotiations/:providerPid`.
pub async fn get_negotiation(
    State(state): State<Arc<AppState>>,
    Path(provider_pid): Path<String>,
) -> impl IntoResponse {
    let negotiations = state
        .negotiations
        .lock()
        .expect("negotiations lock poisoned");
    match negotiations.get(&provider_pid) {
        Some(n) => (StatusCode::OK, Json(n.to_ack())).into_response(),
        None => not_found(&provider_pid).into_response(),
    }
}

/// `POST {dsp}/negotiations/request` — initiates a new negotiation,
/// `REQUESTED`. Always accepted: this bootstrap's stub policy never
/// rejects an offer at this stage (see `../../../ARCHITECTURE.md`).
pub async fn initiate_negotiation(
    State(state): State<Arc<AppState>>,
    Json(msg): Json<ContractRequestMessage>,
) -> impl IntoResponse {
    let provider_pid = format!("urn:uuid:{}", uuid::Uuid::new_v4());
    let mut negotiation = ContractNegotiation::new(provider_pid.clone(), msg.consumer_pid.clone());
    negotiation.dataset_id = msg.offer.target.clone();
    negotiation.offer = Some(msg.offer);
    let ack = negotiation.to_ack();
    state
        .negotiations
        .lock()
        .expect("negotiations lock poisoned")
        .insert(provider_pid, negotiation);
    (StatusCode::CREATED, Json(ack))
}

/// `POST {dsp}/negotiations/:providerPid/request` — a Consumer counter-offer
/// against an existing negotiation. This bootstrap records the new offer
/// but does not itself decide to move to `OFFERED`/`AGREED` — see
/// `../../../ARCHITECTURE.md` for why real auto-negotiation logic is out of
/// scope for this pass.
pub async fn continue_negotiation(
    State(state): State<Arc<AppState>>,
    Path(provider_pid): Path<String>,
    Json(msg): Json<ContractRequestMessage>,
) -> impl IntoResponse {
    let mut negotiations = state
        .negotiations
        .lock()
        .expect("negotiations lock poisoned");
    match negotiations.get_mut(&provider_pid) {
        Some(n) => {
            n.offer = Some(msg.offer);
            StatusCode::OK.into_response()
        }
        None => not_found(&provider_pid).into_response(),
    }
}

/// `POST {dsp}/negotiations/:providerPid/events` — a Consumer accepting the
/// current Offer (`eventType: ACCEPTED`). Per the spec it is an error for a
/// Consumer to send `FINALIZED` to a Provider.
pub async fn negotiation_event(
    State(state): State<Arc<AppState>>,
    Path(provider_pid): Path<String>,
    Json(msg): Json<ContractNegotiationEventMessage>,
) -> impl IntoResponse {
    let mut negotiations = state
        .negotiations
        .lock()
        .expect("negotiations lock poisoned");
    let Some(n) = negotiations.get_mut(&provider_pid) else {
        return not_found(&provider_pid).into_response();
    };
    if msg.event_type != NegotiationEventType::Accepted {
        return (
            StatusCode::BAD_REQUEST,
            Json(ContractNegotiationError::new(
                Some(provider_pid.clone()),
                Some(n.consumer_pid.clone()),
                "400",
                "a Consumer MUST NOT send a ContractNegotiationEventMessage with eventType FINALIZED",
            )),
        )
            .into_response();
    }
    let from = n.state;
    match n.transition(NegotiationState::Accepted) {
        Ok(()) => StatusCode::OK.into_response(),
        Err(_) => bad_transition(
            &provider_pid,
            &n.consumer_pid,
            from,
            NegotiationState::Accepted,
        )
        .into_response(),
    }
}

/// `POST {dsp}/negotiations/:providerPid/agreement/verification`.
pub async fn verify_agreement(
    State(state): State<Arc<AppState>>,
    Path(provider_pid): Path<String>,
    Json(_msg): Json<ContractAgreementVerificationMessage>,
) -> impl IntoResponse {
    let mut negotiations = state
        .negotiations
        .lock()
        .expect("negotiations lock poisoned");
    let Some(n) = negotiations.get_mut(&provider_pid) else {
        return not_found(&provider_pid).into_response();
    };
    let from = n.state;
    match n.transition(NegotiationState::Verified) {
        Ok(()) => StatusCode::OK.into_response(),
        Err(_) => bad_transition(
            &provider_pid,
            &n.consumer_pid,
            from,
            NegotiationState::Verified,
        )
        .into_response(),
    }
}

/// `POST {dsp}/negotiations/:providerPid/termination`.
pub async fn terminate_negotiation(
    State(state): State<Arc<AppState>>,
    Path(provider_pid): Path<String>,
    Json(_msg): Json<ContractNegotiationTerminationMessage>,
) -> impl IntoResponse {
    let mut negotiations = state
        .negotiations
        .lock()
        .expect("negotiations lock poisoned");
    let Some(n) = negotiations.get_mut(&provider_pid) else {
        return not_found(&provider_pid).into_response();
    };
    let from = n.state;
    match n.transition(NegotiationState::Terminated) {
        Ok(()) => StatusCode::OK.into_response(),
        Err(_) => bad_transition(
            &provider_pid,
            &n.consumer_pid,
            from,
            NegotiationState::Terminated,
        )
        .into_response(),
    }
}
