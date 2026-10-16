//! Messages this Provider sends on its own to a Consumer's
//! `callbackAddress`, per the Contract Negotiation HTTPS Binding's
//! "Consumer Callback Path Bindings": the Agreement to
//! `{callback}/negotiations/:consumerPid/agreement` and the `FINALIZED`
//! event to `{callback}/negotiations/:consumerPid/events`.
//!
//! The callback address comes from the Consumer's own request and nothing
//! authenticates that Consumer yet (no DCP), so this client never follows
//! redirects and gives up after [`TIMEOUT`].

use std::time::Duration;

use control_plane_core::common::Agreement;
use control_plane_core::dsp_context;
use control_plane_core::negotiation::{
    ContractAgreementMessage, ContractNegotiationEventMessage, NegotiationEventType,
};

/// How long one callback may take before this Provider gives up on it.
pub const TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub enum CallbackError {
    #[error("callback request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("consumer answered {0}")]
    Status(reqwest::StatusCode),
}

/// The HTTP client for Consumer callbacks.
#[derive(Clone)]
pub struct Callbacks {
    client: reqwest::Client,
}

impl Default for Callbacks {
    fn default() -> Self {
        Self::new()
    }
}

impl Callbacks {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("a client with only a timeout and no redirects always builds");
        Self { client }
    }

    /// Sends `agreement` for the negotiation `provider_pid`/`consumer_pid`.
    pub async fn send_agreement(
        &self,
        callback: &str,
        provider_pid: &str,
        consumer_pid: &str,
        agreement: Agreement,
    ) -> Result<(), CallbackError> {
        let msg = ContractAgreementMessage {
            context: dsp_context(),
            type_: "ContractAgreementMessage".to_string(),
            provider_pid: provider_pid.to_string(),
            consumer_pid: consumer_pid.to_string(),
            agreement,
        };
        self.post(&url(callback, consumer_pid, "agreement"), &msg)
            .await
    }

    /// Tells the Consumer the negotiation is `FINALIZED`.
    pub async fn send_finalized(
        &self,
        callback: &str,
        provider_pid: &str,
        consumer_pid: &str,
    ) -> Result<(), CallbackError> {
        let msg = ContractNegotiationEventMessage {
            context: dsp_context(),
            type_: "ContractNegotiationEventMessage".to_string(),
            provider_pid: provider_pid.to_string(),
            consumer_pid: consumer_pid.to_string(),
            event_type: NegotiationEventType::Finalized,
        };
        self.post(&url(callback, consumer_pid, "events"), &msg)
            .await
    }

    async fn post(&self, url: &str, body: &impl serde::Serialize) -> Result<(), CallbackError> {
        let response = self.client.post(url).json(body).send().await?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(CallbackError::Status(response.status()))
        }
    }
}

/// `{callback}/negotiations/{consumer_pid}/{last}`, with the pid escaped
/// just enough that it stays one path segment.
fn url(callback: &str, consumer_pid: &str, last: &str) -> String {
    format!(
        "{}/negotiations/{}/{last}",
        callback.trim_end_matches('/'),
        segment(consumer_pid)
    )
}

fn segment(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '/' | '?' | '#' | '%' | ' ' => out.push_str(&format!("%{:02X}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_consumer_pid_stays_one_path_segment() {
        assert_eq!(
            url("http://c.example/cb/", "urn:uuid:c1", "agreement"),
            "http://c.example/cb/negotiations/urn:uuid:c1/agreement"
        );
        assert_eq!(
            url("http://c.example/cb", "a/b?c#d%e f", "events"),
            "http://c.example/cb/negotiations/a%2Fb%3Fc%23d%25e%20f/events"
        );
    }
}
