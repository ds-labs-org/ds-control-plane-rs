//! Messages this Provider sends on its own to a Consumer's
//! `callbackAddress`, per the Contract Negotiation HTTPS Binding's
//! "Consumer Callback Path Bindings": the Agreement to
//! `{callback}/negotiations/:consumerPid/agreement`, the `FINALIZED`
//! event to `{callback}/negotiations/:consumerPid/events` and its own
//! termination to `{callback}/negotiations/:consumerPid/termination`.
//!
//! The callback address comes from the Consumer's own request and nothing
//! authenticates that Consumer yet (no DCP), so this client never follows
//! redirects and gives up after [`TIMEOUT`].

use std::time::Duration;

use control_plane_core::common::Agreement;
use control_plane_core::dsp_context;
use control_plane_core::negotiation::{
    ContractAgreementMessage, ContractNegotiationEventMessage,
    ContractNegotiationTerminationMessage, NegotiationEventType,
};

/// How long one callback may take before this Provider gives up on it.
pub const TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub enum CallbackError {
    #[error("callback request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("consumer answered {0}")]
    Status(reqwest::StatusCode),
    #[error("{0}")]
    Unusable(String),
}

/// Why `callback` and `consumer_pid` cannot make the callback URLs, or
/// `Ok` if they can. The DSP path is appended to the callback address, so
/// it must be a plain `http(s)` URL with no query or fragment; and the URL
/// parser resolves a `.` or `..` segment away even when percent-encoded,
/// so such a pid can never be sent as one segment.
pub fn check(callback: &str, consumer_pid: &str) -> Result<(), String> {
    let parsed =
        reqwest::Url::parse(callback).map_err(|e| format!("callbackAddress is not a URL: {e}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("callbackAddress must be an http or https URL".to_string());
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err("callbackAddress must not have a query or fragment".to_string());
    }
    if matches!(consumer_pid, "." | "..") {
        return Err("consumerPid cannot be a URL path segment".to_string());
    }
    Ok(())
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
        check(callback, consumer_pid).map_err(CallbackError::Unusable)?;
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
        check(callback, consumer_pid).map_err(CallbackError::Unusable)?;
        self.post(&url(callback, consumer_pid, "events"), &msg)
            .await
    }

    /// Tells the Consumer this Provider terminated the negotiation.
    pub async fn send_terminated(
        &self,
        callback: &str,
        provider_pid: &str,
        consumer_pid: &str,
        code: &str,
        reason: String,
    ) -> Result<(), CallbackError> {
        let msg = ContractNegotiationTerminationMessage {
            context: dsp_context(),
            type_: "ContractNegotiationTerminationMessage".to_string(),
            provider_pid: provider_pid.to_string(),
            consumer_pid: consumer_pid.to_string(),
            code: Some(code.to_string()),
            reason: vec![reason],
        };
        check(callback, consumer_pid).map_err(CallbackError::Unusable)?;
        self.post(&url(callback, consumer_pid, "termination"), &msg)
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

/// `{callback}/negotiations/{consumer_pid}/{last}`, with every byte of the
/// pid outside RFC 3986 unreserved characters, `:` and `@` percent-encoded,
/// so it stays one path segment for any pid [`check`] accepts.
fn url(callback: &str, consumer_pid: &str, last: &str) -> String {
    format!(
        "{}/negotiations/{}/{last}",
        callback.trim_end_matches('/'),
        segment(consumer_pid)
    )
}

fn segment(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b':' | b'@') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_consumer_pid_is_escaped() {
        assert_eq!(
            url("http://c.example/cb/", "urn:uuid:c1", "agreement"),
            "http://c.example/cb/negotiations/urn:uuid:c1/agreement"
        );
        assert_eq!(
            url("http://c.example/cb", "a/b?c#d%e f", "events"),
            "http://c.example/cb/negotiations/a%2Fb%3Fc%23d%25e%20f/events"
        );
    }

    /// What reqwest actually sends: the WHATWG parser turns `\\` into `/`
    /// and drops tabs, so those must be escaped too.
    #[test]
    fn the_parsed_url_keeps_the_consumer_pid_as_one_segment() {
        for (pid, escaped) in [("a\\b", "a%5Cb"), ("a\tb", "a%09b"), ("a\nb", "a%0Ab")] {
            let parsed =
                reqwest::Url::parse(&url("http://c.example/cb", pid, "agreement")).unwrap();
            let segments: Vec<_> = parsed.path_segments().unwrap().collect();
            assert_eq!(
                segments,
                ["cb", "negotiations", escaped, "agreement"],
                "{pid:?}"
            );
        }
    }
}
