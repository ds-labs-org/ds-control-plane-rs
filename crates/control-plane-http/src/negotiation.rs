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

use control_plane_core::common::Offer;
use control_plane_graph::GraphError;

use crate::state::AppState;

/// The most rules (`permission`, `prohibition` and `obligation` together)
/// an offer from a counterparty may carry. Each rule is stored as several
/// triples, so this and the two limits below bound what one request can
/// make the store hold; real offers carry a handful.
pub const MAX_RULES_PER_OFFER: usize = 64;

/// The most constraints one rule of a counterparty's offer may carry.
pub const MAX_CONSTRAINTS_PER_RULE: usize = 32;

/// The largest `rightOperand`, as serialized JSON, a counterparty's offer
/// may carry.
pub const MAX_OPERAND_BYTES: usize = 4096;

/// Why `offer` is too large to store, or `None` if it is within the
/// limits above.
fn oversized(offer: &Offer) -> Option<String> {
    let rules = || {
        offer
            .permission
            .iter()
            .chain(&offer.prohibition)
            .chain(&offer.obligation)
    };
    if rules().count() > MAX_RULES_PER_OFFER {
        return Some(format!(
            "an offer may carry at most {MAX_RULES_PER_OFFER} rules"
        ));
    }
    for rule in rules() {
        if rule.constraint.len() > MAX_CONSTRAINTS_PER_RULE {
            return Some(format!(
                "a rule may carry at most {MAX_CONSTRAINTS_PER_RULE} constraints"
            ));
        }
        for c in &rule.constraint {
            if c.right_operand.to_string().len() > MAX_OPERAND_BYTES {
                return Some(format!(
                    "a rightOperand may be at most {MAX_OPERAND_BYTES} bytes"
                ));
            }
        }
    }
    None
}

fn rejected_offer(
    provider_pid: Option<&str>,
    consumer_pid: &str,
    reason: String,
) -> axum::response::Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ContractNegotiationError::new(
            provider_pid.map(str::to_string),
            Some(consumer_pid.to_string()),
            "400",
            reason,
        )),
    )
        .into_response()
}

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

fn store_error(provider_pid: Option<&str>, e: GraphError) -> axum::response::Response {
    tracing::error!(error = %e, "negotiation store error");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ContractNegotiationError::new(
            provider_pid.map(str::to_string),
            None,
            "500",
            "negotiation store error",
        )),
    )
        .into_response()
}

/// Applies `to` to the stored negotiation `provider_pid` and maps the
/// outcome to the HTTP response every state-changing endpoint shares.
fn transition(
    state: &AppState,
    provider_pid: &str,
    to: NegotiationState,
) -> axum::response::Response {
    let result = state.graph.update_negotiation(provider_pid, |n| {
        let from = n.state;
        n.transition(to).map_err(|_| (from, n.consumer_pid.clone()))
    });
    match result {
        Ok(Some(Ok(()))) => StatusCode::OK.into_response(),
        Ok(Some(Err((from, consumer_pid)))) => {
            bad_transition(provider_pid, &consumer_pid, from, to).into_response()
        }
        Ok(None) => not_found(provider_pid).into_response(),
        Err(e) => store_error(Some(provider_pid), e),
    }
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
    match state.graph.negotiation(&provider_pid) {
        Ok(Some(n)) => (StatusCode::OK, Json(n.to_ack())).into_response(),
        Ok(None) => not_found(&provider_pid).into_response(),
        Err(e) => store_error(Some(&provider_pid), e),
    }
}

/// `POST {dsp}/negotiations/request` — initiates a new negotiation,
/// `REQUESTED`. Always accepted: this bootstrap's stub policy never
/// rejects an offer at this stage (see `../../../ARCHITECTURE.md`), unless
/// it is past the size limits above.
pub async fn initiate_negotiation(
    State(state): State<Arc<AppState>>,
    Json(msg): Json<ContractRequestMessage>,
) -> impl IntoResponse {
    if let Some(reason) = oversized(&msg.offer) {
        return rejected_offer(None, &msg.consumer_pid, reason);
    }
    let provider_pid = format!("urn:uuid:{}", uuid::Uuid::new_v4());
    let mut negotiation = ContractNegotiation::new(provider_pid, msg.consumer_pid.clone());
    negotiation.dataset_id = msg.offer.target.clone();
    negotiation.offer = Some(msg.offer);
    match state.graph.put_negotiation(&negotiation) {
        Ok(()) => (StatusCode::CREATED, Json(negotiation.to_ack())).into_response(),
        Err(e) => store_error(None, e),
    }
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
    if let Some(reason) = oversized(&msg.offer) {
        return rejected_offer(Some(&provider_pid), &msg.consumer_pid, reason);
    }
    let result = state.graph.update_negotiation(&provider_pid, |n| {
        n.offer = Some(msg.offer);
        Ok::<(), ()>(())
    });
    match result {
        Ok(Some(_)) => StatusCode::OK.into_response(),
        Ok(None) => not_found(&provider_pid).into_response(),
        Err(e) => store_error(Some(&provider_pid), e),
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
    if msg.event_type != NegotiationEventType::Accepted {
        let consumer_pid = match state.graph.negotiation(&provider_pid) {
            Ok(Some(n)) => n.consumer_pid,
            Ok(None) => return not_found(&provider_pid).into_response(),
            Err(e) => return store_error(Some(&provider_pid), e),
        };
        return (
            StatusCode::BAD_REQUEST,
            Json(ContractNegotiationError::new(
                Some(provider_pid.clone()),
                Some(consumer_pid),
                "400",
                "a Consumer MUST NOT send a ContractNegotiationEventMessage with eventType FINALIZED",
            )),
        )
            .into_response();
    }
    transition(&state, &provider_pid, NegotiationState::Accepted)
}

/// `POST {dsp}/negotiations/:providerPid/agreement/verification`.
pub async fn verify_agreement(
    State(state): State<Arc<AppState>>,
    Path(provider_pid): Path<String>,
    Json(_msg): Json<ContractAgreementVerificationMessage>,
) -> impl IntoResponse {
    transition(&state, &provider_pid, NegotiationState::Verified)
}

/// `POST {dsp}/negotiations/:providerPid/termination`.
pub async fn terminate_negotiation(
    State(state): State<Arc<AppState>>,
    Path(provider_pid): Path<String>,
    Json(_msg): Json<ContractNegotiationTerminationMessage>,
) -> impl IntoResponse {
    transition(&state, &provider_pid, NegotiationState::Terminated)
}
