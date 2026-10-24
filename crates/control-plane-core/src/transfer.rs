//! Transfer Process protocol: the state machine and message types. See the
//! spec's
//! [Transfer Process Protocol](https://eclipse-dataspace-protocol-base.github.io/DataspaceProtocol/HEAD/#transfer-process-protocol).
//!
//! The state machine below is reconstructed from the spec's prose ("States"
//! section) the same way, and with the same caveat, as
//! [`crate::negotiation`]'s — the normative diagram
//! (`transfer/figures/transfer-process-state-machine.png`) is a rendered
//! image, not machine-readable source, in the spec repo as fetched for this
//! bootstrap.

use serde::{Deserialize, Serialize};

use crate::common::{DataAddress, dsp_context};

/// A Transfer Process's lifecycle state (spec section "States").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TransferState {
    Requested,
    Started,
    Completed,
    Suspended,
    Terminated,
}

impl TransferState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Terminated)
    }
}

/// Returns whether `to` is a spec-permitted transition from `from`.
///
/// - `REQUESTED -> STARTED`: the Provider starts the transfer (or, for a
///   pull transfer, the Consumer's `POST .../start` per the HTTPS binding's
///   "attempt to start a Transfer Process" wording, generously read to also
///   cover the initial start since the binding's own prose only says
///   "after it has been suspended" for the *re*-start case).
/// - `STARTED -> SUSPENDED -> STARTED`: suspend/resume.
/// - `STARTED -> COMPLETED`, `SUSPENDED -> COMPLETED`: completion.
/// - `* -> TERMINATED`: from any non-terminal state, either party.
pub fn is_valid_transition(from: TransferState, to: TransferState) -> bool {
    use TransferState::*;
    if from.is_terminal() {
        return false;
    }
    if to == Terminated {
        return true;
    }
    matches!(
        (from, to),
        (Requested, Started)
            | (Started, Suspended)
            | (Started, Completed)
            | (Suspended, Started)
            | (Suspended, Completed)
    )
}

#[derive(Debug, thiserror::Error)]
pub enum TransferError {
    #[error("invalid transfer process state transition from {from:?} to {to:?}")]
    InvalidTransition {
        from: TransferState,
        to: TransferState,
    },
    #[error("no transfer process with providerPid '{0}'")]
    NotFound(String),
}

/// Server-side record of one Transfer Process.
#[derive(Debug, Clone)]
pub struct TransferProcess {
    pub provider_pid: String,
    pub consumer_pid: String,
    pub state: TransferState,
    pub agreement_id: String,
    pub format: String,
    /// The DID of the participant DCP authenticated when this transfer was
    /// requested, the assignee of its agreement: the only participant whose
    /// later messages on it are accepted. `None` when the request was not
    /// authenticated (DCP off).
    pub participant_id: Option<String>,
}

impl TransferProcess {
    pub fn new(
        provider_pid: impl Into<String>,
        consumer_pid: impl Into<String>,
        agreement_id: impl Into<String>,
        format: impl Into<String>,
    ) -> Self {
        Self {
            provider_pid: provider_pid.into(),
            consumer_pid: consumer_pid.into(),
            state: TransferState::Requested,
            agreement_id: agreement_id.into(),
            format: format.into(),
            participant_id: None,
        }
    }

    pub fn transition(&mut self, to: TransferState) -> Result<(), TransferError> {
        if !is_valid_transition(self.state, to) {
            return Err(TransferError::InvalidTransition {
                from: self.state,
                to,
            });
        }
        self.state = to;
        Ok(())
    }

    pub fn to_ack(&self) -> TransferProcessAck {
        TransferProcessAck {
            context: dsp_context(),
            type_: transfer_process_ack_type(),
            provider_pid: self.provider_pid.clone(),
            consumer_pid: self.consumer_pid.clone(),
            state: self.state,
        }
    }
}

fn transfer_process_ack_type() -> String {
    "TransferProcess".to_string()
}

/// The `ACK - Transfer Process` wire type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferProcessAck {
    #[serde(rename = "@context", default = "dsp_context")]
    pub context: Vec<String>,
    #[serde(rename = "@type", default = "transfer_process_ack_type")]
    pub type_: String,
    #[serde(rename = "providerPid")]
    pub provider_pid: String,
    #[serde(rename = "consumerPid")]
    pub consumer_pid: String,
    pub state: TransferState,
}

fn transfer_request_type() -> String {
    "TransferRequestMessage".to_string()
}

/// Sent by a Consumer to initiate a Transfer Process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferRequestMessage {
    #[serde(rename = "@context", default = "dsp_context")]
    pub context: Vec<String>,
    #[serde(rename = "@type", default = "transfer_request_type")]
    pub type_: String,
    #[serde(rename = "consumerPid")]
    pub consumer_pid: String,
    #[serde(rename = "agreementId")]
    pub agreement_id: String,
    pub format: String,
    #[serde(rename = "dataAddress", skip_serializing_if = "Option::is_none")]
    pub data_address: Option<DataAddress>,
    #[serde(rename = "callbackAddress", skip_serializing_if = "Option::is_none")]
    pub callback_address: Option<String>,
}

fn transfer_start_type() -> String {
    "TransferStartMessage".to_string()
}

/// Sent by a Provider (or, per the HTTPS binding, a Consumer resuming a
/// suspended pull transfer) to indicate the transfer has been (re)started.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferStartMessage {
    #[serde(rename = "@context", default = "dsp_context")]
    pub context: Vec<String>,
    #[serde(rename = "@type", default = "transfer_start_type")]
    pub type_: String,
    #[serde(rename = "providerPid")]
    pub provider_pid: String,
    #[serde(rename = "consumerPid")]
    pub consumer_pid: String,
    #[serde(rename = "dataAddress", skip_serializing_if = "Option::is_none")]
    pub data_address: Option<DataAddress>,
}

fn transfer_suspension_type() -> String {
    "TransferSuspensionMessage".to_string()
}

/// Sent by either party to temporarily suspend a Transfer Process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferSuspensionMessage {
    #[serde(rename = "@context", default = "dsp_context")]
    pub context: Vec<String>,
    #[serde(rename = "@type", default = "transfer_suspension_type")]
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

fn transfer_completion_type() -> String {
    "TransferCompletionMessage".to_string()
}

/// Sent by either party when the data transfer has completed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferCompletionMessage {
    #[serde(rename = "@context", default = "dsp_context")]
    pub context: Vec<String>,
    #[serde(rename = "@type", default = "transfer_completion_type")]
    pub type_: String,
    #[serde(rename = "providerPid")]
    pub provider_pid: String,
    #[serde(rename = "consumerPid")]
    pub consumer_pid: String,
}

fn transfer_termination_type() -> String {
    "TransferTerminationMessage".to_string()
}

/// Sent by either party to terminate a Transfer Process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferTerminationMessage {
    #[serde(rename = "@context", default = "dsp_context")]
    pub context: Vec<String>,
    #[serde(rename = "@type", default = "transfer_termination_type")]
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

fn transfer_error_type() -> String {
    "TransferError".to_string()
}

/// Returned to indicate an error; does not cause a state transition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferErrorMessage {
    #[serde(rename = "@context", default = "dsp_context")]
    pub context: Vec<String>,
    #[serde(rename = "@type", default = "transfer_error_type")]
    pub type_: String,
    #[serde(rename = "providerPid", skip_serializing_if = "Option::is_none")]
    pub provider_pid: Option<String>,
    #[serde(rename = "consumerPid", skip_serializing_if = "Option::is_none")]
    pub consumer_pid: Option<String>,
    pub code: String,
    pub reason: Vec<String>,
}

impl TransferErrorMessage {
    pub fn new(
        provider_pid: Option<String>,
        consumer_pid: Option<String>,
        code: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            context: dsp_context(),
            type_: transfer_error_type(),
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
    fn happy_path_pull_flow_is_valid() {
        let mut t = TransferProcess::new("p1", "c1", "agreement-1", "HttpData-PULL");
        assert_eq!(t.state, TransferState::Requested);
        t.transition(TransferState::Started).unwrap();
        t.transition(TransferState::Completed).unwrap();
        assert!(t.state.is_terminal());
    }

    #[test]
    fn suspend_and_resume_round_trips() {
        let mut t = TransferProcess::new("p1", "c1", "agreement-1", "HttpData-PULL");
        t.transition(TransferState::Started).unwrap();
        t.transition(TransferState::Suspended).unwrap();
        t.transition(TransferState::Started).unwrap();
        assert_eq!(t.state, TransferState::Started);
    }

    #[test]
    fn terminated_is_reachable_from_any_nonterminal_state() {
        for state in [
            TransferState::Requested,
            TransferState::Started,
            TransferState::Suspended,
        ] {
            assert!(is_valid_transition(state, TransferState::Terminated));
        }
    }

    #[test]
    fn terminal_states_reject_every_transition() {
        for to in [
            TransferState::Requested,
            TransferState::Started,
            TransferState::Completed,
            TransferState::Suspended,
            TransferState::Terminated,
        ] {
            assert!(!is_valid_transition(TransferState::Completed, to));
            assert!(!is_valid_transition(TransferState::Terminated, to));
        }
    }
}
