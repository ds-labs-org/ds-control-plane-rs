//! Contract Negotiation protocol: the state machine and message types. See
//! the spec's
//! [Contract Negotiation Protocol](https://eclipse-dataspace-protocol-base.github.io/DataspaceProtocol/HEAD/#contract-negotiation-protocol).
//!
//! ## The state machine, and an honesty note on how it was derived
//!
//! The spec's normative state diagram
//! (`negotiation/figures/contract.negotiation.state.machine.png`) is a
//! rendered image, not machine-readable source, in the spec repo as
//! fetched for this bootstrap. [`is_valid_transition`] therefore encodes
//! the transition graph as reconstructed from the spec's own prose
//! description of each state and message type (`negotiation.protocol.md`,
//! "States" and "Message Types") rather than from the diagram directly.
//! The chosen edges:
//!
//! - `REQUESTED -> OFFERED`: a Provider counters a request with a new Offer.
//! - `REQUESTED -> AGREED`: a Provider agrees to the request outright.
//! - `OFFERED -> REQUESTED`: a Consumer counters an Offer with a new request.
//! - `OFFERED -> ACCEPTED`: a Consumer accepts the Provider's Offer.
//! - `ACCEPTED -> AGREED`: a Provider sends the Agreement.
//! - `AGREED -> VERIFIED`: a Consumer verifies the Agreement.
//! - `VERIFIED -> FINALIZED`: a Provider finalizes.
//! - `* -> TERMINATED`: from any non-terminal state, either party.
//!
//! `FINALIZED` and `TERMINATED` are terminal (no outgoing edges), per the
//! spec's "Terminal states are final" rule. If this reconstruction turns
//! out to disagree with the diagram on some edge, that is a documented
//! risk of this bootstrap, not a silent guess — see `../../../ARCHITECTURE.md`.

use serde::{Deserialize, Serialize};

use crate::common::{Agreement, Offer, dsp_context};

/// A Contract Negotiation's lifecycle state (spec section "States").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum NegotiationState {
    Requested,
    Offered,
    Accepted,
    Agreed,
    Verified,
    Finalized,
    Terminated,
}

impl NegotiationState {
    /// Terminal states MUST NOT transition further (spec: "Terminal states
    /// are final; the state machine MUST NOT transition to another state").
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Finalized | Self::Terminated)
    }
}

/// Returns whether `to` is a spec-permitted transition from `from`. See
/// this module's doc comment for how this graph was derived.
pub fn is_valid_transition(from: NegotiationState, to: NegotiationState) -> bool {
    use NegotiationState::*;
    if from.is_terminal() {
        return false;
    }
    if to == Terminated {
        return true;
    }
    matches!(
        (from, to),
        (Requested, Offered)
            | (Requested, Agreed)
            | (Offered, Requested)
            | (Offered, Accepted)
            | (Accepted, Agreed)
            | (Agreed, Verified)
            | (Verified, Finalized)
    )
}

#[derive(Debug, thiserror::Error)]
pub enum NegotiationError {
    #[error("invalid contract negotiation state transition from {from:?} to {to:?}")]
    InvalidTransition {
        from: NegotiationState,
        to: NegotiationState,
    },
    #[error("no contract negotiation with providerPid '{0}'")]
    NotFound(String),
    #[error("the contract negotiation has no offer with a target to agree to")]
    NoOffer,
}

/// Server-side record of one Contract Negotiation. This is this crate's own
/// domain type, not a wire message — see [`NegotiationAck`] for the ACK
/// shape sent over the wire.
#[derive(Debug, Clone)]
pub struct ContractNegotiation {
    pub provider_pid: String,
    pub consumer_pid: String,
    pub state: NegotiationState,
    pub dataset_id: Option<String>,
    pub offer: Option<Offer>,
    pub agreement: Option<Agreement>,
    /// Where the Consumer receives the Provider's messages for this
    /// negotiation (the `callbackAddress` of its initial
    /// `ContractRequestMessage`). `None` means this Provider has no way to
    /// reach the Consumer, so it never moves the negotiation on by itself.
    pub callback_address: Option<String>,
}

impl ContractNegotiation {
    pub fn new(provider_pid: impl Into<String>, consumer_pid: impl Into<String>) -> Self {
        Self {
            provider_pid: provider_pid.into(),
            consumer_pid: consumer_pid.into(),
            state: NegotiationState::Requested,
            dataset_id: None,
            offer: None,
            agreement: None,
            callback_address: None,
        }
    }

    /// Agrees to the current offer as `assigner`, granting it to
    /// `assignee`: builds the `Agreement` (id `agreement_id`, the offer's
    /// rules, its `target` or else this negotiation's dataset, stamped
    /// `now`), stores it on this negotiation and moves to `AGREED`. Leaves
    /// the negotiation unchanged on error.
    pub fn agree(
        &mut self,
        agreement_id: impl Into<String>,
        assigner: impl Into<String>,
        assignee: impl Into<String>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<&Agreement, NegotiationError> {
        if !is_valid_transition(self.state, NegotiationState::Agreed) {
            return Err(NegotiationError::InvalidTransition {
                from: self.state,
                to: NegotiationState::Agreed,
            });
        }
        let offer = self.offer.as_ref().ok_or(NegotiationError::NoOffer)?;
        let target = offer
            .target
            .clone()
            .or_else(|| self.dataset_id.clone())
            .ok_or(NegotiationError::NoOffer)?;
        self.agreement = Some(Agreement {
            id: agreement_id.into(),
            type_: "Agreement".to_string(),
            target,
            timestamp: now,
            assigner: assigner.into(),
            assignee: assignee.into(),
            permission: offer.permission.clone(),
            prohibition: offer.prohibition.clone(),
            obligation: offer.obligation.clone(),
        });
        self.state = NegotiationState::Agreed;
        Ok(self.agreement.as_ref().expect("just set"))
    }

    /// Attempts to move this negotiation to `to`, validating the
    /// transition first. Leaves state unchanged on error.
    pub fn transition(&mut self, to: NegotiationState) -> Result<(), NegotiationError> {
        if !is_valid_transition(self.state, to) {
            return Err(NegotiationError::InvalidTransition {
                from: self.state,
                to,
            });
        }
        self.state = to;
        Ok(())
    }

    pub fn to_ack(&self) -> NegotiationAck {
        NegotiationAck {
            context: dsp_context(),
            type_: negotiation_ack_type(),
            provider_pid: self.provider_pid.clone(),
            consumer_pid: self.consumer_pid.clone(),
            state: self.state,
        }
    }
}

fn negotiation_ack_type() -> String {
    "ContractNegotiation".to_string()
}

/// The `ACK - Contract Negotiation` wire type: returned whenever a
/// negotiation-affecting message is successfully processed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NegotiationAck {
    #[serde(rename = "@context", default = "dsp_context")]
    pub context: Vec<String>,
    #[serde(rename = "@type", default = "negotiation_ack_type")]
    pub type_: String,
    #[serde(rename = "providerPid")]
    pub provider_pid: String,
    #[serde(rename = "consumerPid")]
    pub consumer_pid: String,
    pub state: NegotiationState,
}

fn contract_request_type() -> String {
    "ContractRequestMessage".to_string()
}

/// Sent by a Consumer to initiate a negotiation, or to respond to a
/// `ContractOfferMessage`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractRequestMessage {
    #[serde(rename = "@context", default = "dsp_context")]
    pub context: Vec<String>,
    #[serde(rename = "@type", default = "contract_request_type")]
    pub type_: String,
    #[serde(rename = "providerPid", skip_serializing_if = "Option::is_none")]
    pub provider_pid: Option<String>,
    #[serde(rename = "consumerPid")]
    pub consumer_pid: String,
    pub offer: Offer,
    #[serde(rename = "callbackAddress", skip_serializing_if = "Option::is_none")]
    pub callback_address: Option<String>,
}

fn contract_offer_type() -> String {
    "ContractOfferMessage".to_string()
}

/// Sent by a Provider to initiate a negotiation, or to respond to a
/// `ContractRequestMessage` with a counter-offer. Provided for
/// completeness/consumer-role future work; this bootstrap's HTTP surface
/// never sends one (provider role only — see `../../../ARCHITECTURE.md`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractOfferMessage {
    #[serde(rename = "@context", default = "dsp_context")]
    pub context: Vec<String>,
    #[serde(rename = "@type", default = "contract_offer_type")]
    pub type_: String,
    #[serde(rename = "providerPid")]
    pub provider_pid: String,
    #[serde(rename = "consumerPid", skip_serializing_if = "Option::is_none")]
    pub consumer_pid: Option<String>,
    pub offer: Offer,
    #[serde(rename = "callbackAddress", skip_serializing_if = "Option::is_none")]
    pub callback_address: Option<String>,
}

fn contract_agreement_type() -> String {
    "ContractAgreementMessage".to_string()
}

/// Sent by a Provider when it agrees to a Policy; carries the complete
/// `Agreement`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractAgreementMessage {
    #[serde(rename = "@context", default = "dsp_context")]
    pub context: Vec<String>,
    #[serde(rename = "@type", default = "contract_agreement_type")]
    pub type_: String,
    #[serde(rename = "providerPid")]
    pub provider_pid: String,
    #[serde(rename = "consumerPid")]
    pub consumer_pid: String,
    pub agreement: Agreement,
}

fn contract_agreement_verification_type() -> String {
    "ContractAgreementVerificationMessage".to_string()
}

/// Sent by a Consumer to verify acceptance of an Agreement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractAgreementVerificationMessage {
    #[serde(rename = "@context", default = "dsp_context")]
    pub context: Vec<String>,
    #[serde(rename = "@type", default = "contract_agreement_verification_type")]
    pub type_: String,
    #[serde(rename = "providerPid")]
    pub provider_pid: String,
    #[serde(rename = "consumerPid")]
    pub consumer_pid: String,
}

/// The `eventType` of a `ContractNegotiationEventMessage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum NegotiationEventType {
    Accepted,
    Finalized,
}

fn contract_negotiation_event_type() -> String {
    "ContractNegotiationEventMessage".to_string()
}

/// Sent by a Consumer (`eventType: ACCEPTED`) or a Provider
/// (`eventType: FINALIZED`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractNegotiationEventMessage {
    #[serde(rename = "@context", default = "dsp_context")]
    pub context: Vec<String>,
    #[serde(rename = "@type", default = "contract_negotiation_event_type")]
    pub type_: String,
    #[serde(rename = "providerPid")]
    pub provider_pid: String,
    #[serde(rename = "consumerPid")]
    pub consumer_pid: String,
    #[serde(rename = "eventType")]
    pub event_type: NegotiationEventType,
}

fn contract_negotiation_termination_type() -> String {
    "ContractNegotiationTerminationMessage".to_string()
}

/// Sent by either party to cancel a negotiation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractNegotiationTerminationMessage {
    #[serde(rename = "@context", default = "dsp_context")]
    pub context: Vec<String>,
    #[serde(rename = "@type", default = "contract_negotiation_termination_type")]
    pub type_: String,
    #[serde(rename = "providerPid")]
    pub provider_pid: String,
    #[serde(rename = "consumerPid")]
    pub consumer_pid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reason: Vec<String>,
}

fn contract_negotiation_error_type() -> String {
    "ContractNegotiationError".to_string()
}

/// Returned by either party to indicate an error; does not cause a state
/// transition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractNegotiationError {
    #[serde(rename = "@context", default = "dsp_context")]
    pub context: Vec<String>,
    #[serde(rename = "@type", default = "contract_negotiation_error_type")]
    pub type_: String,
    #[serde(rename = "providerPid", skip_serializing_if = "Option::is_none")]
    pub provider_pid: Option<String>,
    #[serde(rename = "consumerPid", skip_serializing_if = "Option::is_none")]
    pub consumer_pid: Option<String>,
    pub code: String,
    pub reason: Vec<String>,
}

impl ContractNegotiationError {
    pub fn new(
        provider_pid: Option<String>,
        consumer_pid: Option<String>,
        code: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            context: dsp_context(),
            type_: contract_negotiation_error_type(),
            provider_pid,
            consumer_pid,
            code: code.into(),
            reason: vec![reason.into()],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happy_path_provider_flow_is_valid() {
        let mut n = ContractNegotiation::new("p1", "c1");
        assert_eq!(n.state, NegotiationState::Requested);
        n.transition(NegotiationState::Agreed).unwrap();
        n.transition(NegotiationState::Verified).unwrap();
        n.transition(NegotiationState::Finalized).unwrap();
        assert!(n.state.is_terminal());
    }

    #[test]
    fn terminated_is_reachable_from_any_nonterminal_state() {
        for state in [
            NegotiationState::Requested,
            NegotiationState::Offered,
            NegotiationState::Accepted,
            NegotiationState::Agreed,
            NegotiationState::Verified,
        ] {
            assert!(is_valid_transition(state, NegotiationState::Terminated));
        }
    }

    #[test]
    fn terminal_states_have_no_outgoing_transitions() {
        for to in [
            NegotiationState::Requested,
            NegotiationState::Offered,
            NegotiationState::Accepted,
            NegotiationState::Agreed,
            NegotiationState::Verified,
            NegotiationState::Finalized,
            NegotiationState::Terminated,
        ] {
            assert!(!is_valid_transition(NegotiationState::Finalized, to));
            assert!(!is_valid_transition(NegotiationState::Terminated, to));
        }
    }

    #[test]
    fn skipping_a_stage_is_rejected() {
        let mut n = ContractNegotiation::new("p1", "c1");
        assert!(n.transition(NegotiationState::Verified).is_err());
        assert_eq!(
            n.state,
            NegotiationState::Requested,
            "state must not change on a rejected transition"
        );
    }

    fn requested_with_offer() -> ContractNegotiation {
        let mut n = ContractNegotiation::new("p1", "c1");
        n.dataset_id = Some("urn:uuid:dataset".to_string());
        n.offer = Some(Offer {
            id: "urn:uuid:offer".to_string(),
            type_: "Offer".to_string(),
            target: Some("urn:uuid:dataset".to_string()),
            permission: vec![crate::common::Rule {
                action: "use".to_string(),
                constraint: vec![],
            }],
            prohibition: vec![],
            obligation: vec![],
        });
        n
    }

    #[test]
    fn agreeing_builds_the_agreement_from_the_offer() {
        let mut n = requested_with_offer();
        let now = chrono::Utc::now();
        let agreement = n
            .agree("urn:uuid:agreement", "urn:provider", "urn:consumer", now)
            .unwrap()
            .clone();
        assert_eq!(n.state, NegotiationState::Agreed);
        assert_eq!(agreement.id, "urn:uuid:agreement");
        assert_eq!(agreement.target, "urn:uuid:dataset");
        assert_eq!(agreement.assigner, "urn:provider");
        assert_eq!(agreement.assignee, "urn:consumer");
        assert_eq!(agreement.timestamp, now);
        assert_eq!(agreement.permission, n.offer.as_ref().unwrap().permission);
        assert_eq!(n.agreement.as_ref(), Some(&agreement));
    }

    #[test]
    fn agreeing_falls_back_to_the_dataset_when_the_offer_has_no_target() {
        let mut n = requested_with_offer();
        n.offer.as_mut().unwrap().target = None;
        let agreement = n
            .agree("urn:uuid:a", "urn:p", "urn:c", chrono::Utc::now())
            .unwrap();
        assert_eq!(agreement.target, "urn:uuid:dataset");
    }

    #[test]
    fn agreeing_without_an_offer_or_from_a_late_state_changes_nothing() {
        let mut n = ContractNegotiation::new("p1", "c1");
        assert!(matches!(
            n.agree("urn:uuid:a", "urn:p", "urn:c", chrono::Utc::now()),
            Err(NegotiationError::NoOffer)
        ));
        assert_eq!(n.state, NegotiationState::Requested);
        assert!(n.agreement.is_none());

        let mut n = requested_with_offer();
        n.state = NegotiationState::Verified;
        assert!(matches!(
            n.agree("urn:uuid:a", "urn:p", "urn:c", chrono::Utc::now()),
            Err(NegotiationError::InvalidTransition { .. })
        ));
        assert!(n.agreement.is_none());
    }
}
