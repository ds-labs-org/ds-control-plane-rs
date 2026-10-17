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

/// Offer, rule and constraint keys this bootstrap's model cannot hold:
/// serde would drop them on parsing, and the Provider would then agree to
/// a different, usually weaker, policy than the one offered (e.g. a
/// permission without its duty).
const UNREPRESENTABLE_OFFER_KEYS: &[&str] = &["profile"];
const UNREPRESENTABLE_RULE_KEYS: &[&str] = &["duty", "remedy", "consequence", "constraints"];
const UNREPRESENTABLE_CONSTRAINT_KEYS: &[&str] =
    &["unit", "rightOperandReference", "dataType", "status"];

/// The first of `keys` that `object` carries with something in it: an
/// empty list or `null` loses nothing when dropped.
fn present(object: &serde_json::Value, keys: &[&'static str]) -> Option<&'static str> {
    keys.iter().copied().find(|key| match object.get(*key) {
        None | Some(serde_json::Value::Null) => false,
        Some(serde_json::Value::Array(items)) => !items.is_empty(),
        Some(_) => true,
    })
}

/// The first term of the raw `offer` this bootstrap cannot represent, if
/// any. An empty list or `null` loses nothing and is let through (the TCK
/// sends `constraints: []`).
fn unrepresentable(offer: &serde_json::Value) -> Option<String> {
    let refuse = |key: &str| Some(format!("an offer's `{key}` is not supported"));
    if let Some(key) = present(offer, UNREPRESENTABLE_OFFER_KEYS) {
        return refuse(key);
    }
    let rules = ["permission", "prohibition", "obligation"]
        .into_iter()
        .filter_map(|kind| offer.get(kind)?.as_array())
        .flatten();
    for rule in rules {
        if let Some(key) = present(rule, UNREPRESENTABLE_RULE_KEYS) {
            return refuse(key);
        }
        let constraints = rule.get("constraint").and_then(|c| c.as_array());
        for constraint in constraints.into_iter().flatten() {
            if let Some(key) = present(constraint, UNREPRESENTABLE_CONSTRAINT_KEYS) {
                return refuse(key);
            }
        }
    }
    None
}

/// A `ContractRequestMessage` body, plus whether its raw `offer` carried
/// terms that parsing into [`ContractRequestMessage`] dropped.
pub struct InboundRequest {
    msg: ContractRequestMessage,
    unrepresentable: Option<String>,
}

impl<'de> serde::Deserialize<'de> for InboundRequest {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = serde_json::Value::deserialize(deserializer)?;
        let unrepresentable = raw.get("offer").and_then(unrepresentable);
        let msg = serde_json::from_value(raw).map_err(serde::de::Error::custom)?;
        Ok(Self {
            msg,
            unrepresentable,
        })
    }
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

fn store_error(
    provider_pid: Option<&str>,
    consumer_pid: Option<&str>,
    e: GraphError,
) -> axum::response::Response {
    tracing::error!(error = %e, "negotiation store error");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ContractNegotiationError::new(
            provider_pid.map(str::to_string),
            consumer_pid.map(str::to_string),
            "500",
            "negotiation store error",
        )),
    )
        .into_response()
}

/// Applies `to` to the stored negotiation `provider_pid` and maps the
/// outcome to the HTTP response every state-changing endpoint shares.
async fn transition(
    state: &AppState,
    provider_pid: &str,
    to: NegotiationState,
) -> axum::response::Response {
    let pid = provider_pid.to_string();
    let result = state
        .with_graph(move |g| {
            g.update_negotiation(&pid, |n| {
                let from = n.state;
                n.transition(to).map_err(|_| (from, n.consumer_pid.clone()))
            })
        })
        .await;
    match result {
        Ok(Some(Ok(()))) => StatusCode::OK.into_response(),
        Ok(Some(Err((from, consumer_pid)))) => {
            bad_transition(provider_pid, &consumer_pid, from, to).into_response()
        }
        Ok(None) => not_found(provider_pid).into_response(),
        Err(e) => store_error(Some(provider_pid), None, e),
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
    let lookup = provider_pid.clone();
    match state.with_graph(move |g| g.negotiation(&lookup)).await {
        Ok(Some(n)) => (StatusCode::OK, Json(n.to_ack())).into_response(),
        Ok(None) => not_found(&provider_pid).into_response(),
        Err(e) => store_error(Some(&provider_pid), None, e),
    }
}

/// `POST {dsp}/negotiations/request` — initiates a new negotiation,
/// `REQUESTED`. Always accepted: this bootstrap's stub policy never
/// rejects an offer at this stage (see `../../../ARCHITECTURE.md`), unless
/// it is past the size limits above or carries terms the model cannot hold
/// ([`unrepresentable`]). With a `callbackAddress`, the
/// Provider then agrees on its own ([`agree`]).
pub async fn initiate_negotiation(
    State(state): State<Arc<AppState>>,
    Json(msg): Json<InboundRequest>,
) -> impl IntoResponse {
    let InboundRequest {
        msg,
        unrepresentable,
    } = msg;
    if let Some(reason) = unrepresentable.or_else(|| oversized(&msg.offer)) {
        return rejected_offer(None, &msg.consumer_pid, reason);
    }
    let provider_pid = format!("urn:uuid:{}", uuid::Uuid::new_v4());
    let mut negotiation = ContractNegotiation::new(provider_pid, msg.consumer_pid.clone());
    negotiation.dataset_id = msg.offer.target.clone();
    negotiation.offer = Some(msg.offer);
    negotiation.callback_address = msg.callback_address;
    let stored = negotiation.clone();
    match state.with_graph(move |g| g.put_negotiation(&stored)).await {
        Ok(()) => {
            if negotiation.callback_address.is_some() {
                tokio::spawn(agree(state.clone(), negotiation.provider_pid.clone()));
            }
            (StatusCode::CREATED, Json(negotiation.to_ack())).into_response()
        }
        Err(e) => store_error(
            Some(&negotiation.provider_pid),
            Some(&negotiation.consumer_pid),
            e,
        ),
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
    Json(msg): Json<InboundRequest>,
) -> impl IntoResponse {
    let InboundRequest {
        msg,
        unrepresentable,
    } = msg;
    if let Some(reason) = unrepresentable.or_else(|| oversized(&msg.offer)) {
        return rejected_offer(Some(&provider_pid), &msg.consumer_pid, reason);
    }
    let pid = provider_pid.clone();
    let result = state
        .with_graph(move |g| {
            g.update_negotiation(&pid, |n| {
                n.offer = Some(msg.offer);
                Ok::<(), ()>(())
            })
        })
        .await;
    match result {
        Ok(Some(_)) => StatusCode::OK.into_response(),
        Ok(None) => not_found(&provider_pid).into_response(),
        Err(e) => store_error(Some(&provider_pid), None, e),
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
        let lookup = provider_pid.clone();
        let consumer_pid = match state.with_graph(move |g| g.negotiation(&lookup)).await {
            Ok(Some(n)) => n.consumer_pid,
            Ok(None) => return not_found(&provider_pid).into_response(),
            Err(e) => return store_error(Some(&provider_pid), None, e),
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
    let response = transition(&state, &provider_pid, NegotiationState::Accepted).await;
    if response.status() == StatusCode::OK {
        tokio::spawn(agree(state.clone(), provider_pid));
    }
    response
}

/// `POST {dsp}/negotiations/:providerPid/agreement/verification` — the
/// Consumer verifies the Agreement, `VERIFIED`; the Provider then
/// finalizes on its own ([`finalize`]).
pub async fn verify_agreement(
    State(state): State<Arc<AppState>>,
    Path(provider_pid): Path<String>,
    Json(_msg): Json<ContractAgreementVerificationMessage>,
) -> impl IntoResponse {
    let response = transition(&state, &provider_pid, NegotiationState::Verified).await;
    if response.status() == StatusCode::OK {
        tokio::spawn(finalize(state.clone(), provider_pid));
    }
    response
}

/// The Provider agrees to the negotiation `provider_pid`'s current offer
/// (the stub policy agrees to every offer): it stores the Agreement,
/// moves to `AGREED`, then sends the `ContractAgreementMessage` to the
/// Consumer's callback address. The negotiation is `AGREED` before the
/// message leaves, so a Consumer that verifies at once finds it there. If
/// the Consumer cannot be reached the Provider terminates the negotiation,
/// since the Consumer would otherwise never learn of the Agreement, and
/// sends it a best-effort `ContractNegotiationTerminationMessage` -- unless
/// the negotiation has left `AGREED` meanwhile (a verification proves the
/// Consumer got the Agreement despite the failed reply).
pub async fn agree(state: Arc<AppState>, provider_pid: String) {
    let assigner = state.config.agent_id.clone();
    let pid = provider_pid.clone();
    let agreed = state
        .with_graph(move |g| {
            g.update_negotiation(&pid, |n| {
                let callback = n
                    .callback_address
                    .clone()
                    .ok_or_else(|| "no callbackAddress to send the agreement to".to_string())?;
                // Without DCP the Consumer has no verified participant id
                // yet; its consumerPid is the only name this Provider has
                // for it.
                let assignee = n.consumer_pid.clone();
                let id = format!("urn:uuid:{}", uuid::Uuid::new_v4());
                let agreement = n
                    .agree(id, assigner, assignee, chrono::Utc::now())
                    .map_err(|e| e.to_string())?
                    .clone();
                Ok::<_, String>((agreement, n.consumer_pid.clone(), callback))
            })
        })
        .await;
    let (agreement, consumer_pid, callback) = match agreed {
        Ok(Some(Ok(agreed))) => agreed,
        Ok(Some(Err(reason))) => {
            tracing::warn!(%provider_pid, %reason, "not agreeing to negotiation");
            return;
        }
        Ok(None) => return,
        Err(error) => {
            tracing::error!(%provider_pid, %error, "negotiation store error while agreeing");
            return;
        }
    };
    let agreement_id = agreement.id.clone();
    if let Err(error) = state
        .callbacks
        .send_agreement(&callback, &provider_pid, &consumer_pid, agreement)
        .await
    {
        tracing::warn!(%provider_pid, %callback, %error, "could not send the agreement; terminating");
        let pid = provider_pid.clone();
        // Terminate only the negotiation as this call left it: a Consumer
        // that has verified meanwhile holds the Agreement after all.
        let terminated = state
            .with_graph(move |g| {
                g.update_negotiation(&pid, |n| {
                    let unanswered = n.state == NegotiationState::Agreed
                        && n.agreement.as_ref().map(|a| &a.id) == Some(&agreement_id);
                    if unanswered {
                        n.transition(NegotiationState::Terminated).map(|()| true)
                    } else {
                        Ok(false)
                    }
                })
            })
            .await;
        match terminated {
            Ok(Some(Ok(true))) => {}
            Ok(Some(Ok(false))) => {
                tracing::info!(%provider_pid, "the consumer verified the agreement anyway; not terminating");
                return;
            }
            Ok(Some(Err(reason))) => {
                tracing::warn!(%provider_pid, %reason, "not terminating negotiation");
                return;
            }
            Ok(None) => return,
            Err(error) => {
                tracing::error!(%provider_pid, %error, "negotiation store error while terminating");
                return;
            }
        }
        // Best effort: the Consumer may have stored the Agreement anyway
        // (a lost response, a timeout), so tell it the negotiation is over.
        if let Err(error) = state
            .callbacks
            .send_terminated(
                &callback,
                &provider_pid,
                &consumer_pid,
                "agreement-undeliverable",
                format!("the agreement could not be delivered: {error}"),
            )
            .await
        {
            tracing::warn!(%provider_pid, %callback, %error, "could not send the termination");
        }
    }
}

/// The Provider finalizes the verified negotiation `provider_pid`:
/// `FINALIZED`, which is what makes its Agreement grant transfers, then
/// the `FINALIZED` event to the Consumer. The Consumer has already
/// verified, so a failed delivery is logged and the negotiation stays
/// finalized.
pub async fn finalize(state: Arc<AppState>, provider_pid: String) {
    let pid = provider_pid.clone();
    let finalized = state
        .with_graph(move |g| {
            g.update_negotiation(&pid, |n| {
                n.transition(NegotiationState::Finalized)
                    .map_err(|e| e.to_string())?;
                Ok::<_, String>((n.consumer_pid.clone(), n.callback_address.clone()))
            })
        })
        .await;
    let (consumer_pid, callback) = match finalized {
        Ok(Some(Ok(finalized))) => finalized,
        Ok(Some(Err(reason))) => {
            tracing::warn!(%provider_pid, %reason, "not finalizing negotiation");
            return;
        }
        Ok(None) => return,
        Err(error) => {
            tracing::error!(%provider_pid, %error, "negotiation store error while finalizing");
            return;
        }
    };
    let Some(callback) = callback else {
        tracing::warn!(%provider_pid, "finalized, but no callbackAddress to tell the consumer");
        return;
    };
    if let Err(error) = state
        .callbacks
        .send_finalized(&callback, &provider_pid, &consumer_pid)
        .await
    {
        tracing::warn!(%provider_pid, %callback, %error, "could not send the finalized event");
    }
}

/// `POST {dsp}/negotiations/:providerPid/termination`. Terminating an
/// already `TERMINATED` negotiation succeeds without a change: the
/// Provider terminates on its own when it cannot deliver an agreement
/// ([`agree`]), and the Consumer may not have heard of that yet.
pub async fn terminate_negotiation(
    State(state): State<Arc<AppState>>,
    Path(provider_pid): Path<String>,
    Json(_msg): Json<ContractNegotiationTerminationMessage>,
) -> impl IntoResponse {
    let lookup = provider_pid.clone();
    match state.with_graph(move |g| g.negotiation(&lookup)).await {
        Ok(Some(n)) if n.state == NegotiationState::Terminated => {
            return StatusCode::OK.into_response();
        }
        Ok(_) => {}
        Err(e) => return store_error(Some(&provider_pid), None, e),
    }
    transition(&state, &provider_pid, NegotiationState::Terminated).await
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
