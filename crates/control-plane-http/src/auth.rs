//! DCP authentication of the DSP endpoints (`control-plane-dcp`).
//!
//! [`authenticate`] runs as a route layer on every catalog, negotiation
//! and transfer route, before the body is read: with DCP configured, the
//! request needs a valid Self-Issued ID Token **and** valid credentials for
//! the required scopes, or it is a 401 with the protocol's own error body
//! (`CatalogError`, `ContractNegotiationError`, `TransferError`). The
//! authenticated DID reaches the handler as [`Participant`]. With DCP off,
//! every request passes as `Participant(None)` and the `Authorization`
//! header is ignored. `/.well-known/dspace-version` and the DID document
//! are not behind it.

use std::sync::Arc;

use axum::Json;
use axum::extract::{RawPathParams, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use control_plane_core::catalog::CatalogError;
use control_plane_core::negotiation::ContractNegotiationError;
use control_plane_core::transfer::TransferErrorMessage;

use crate::state::AppState;

/// Who sent a DSP request: the DID DCP authenticated, or `None` with DCP
/// off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Participant(pub Option<String>);

impl Participant {
    /// Whether this caller may act on a record bound to `owner`: always
    /// with DCP off, otherwise only the participant it is bound to (a
    /// record made with DCP off is bound to no one, so to no caller).
    pub fn may_act_on(&self, owner: Option<&str>) -> bool {
        match &self.0 {
            None => true,
            Some(caller) => owner == Some(caller.as_str()),
        }
    }
}

/// The route layer: see the module doc.
pub async fn authenticate(
    State(state): State<Arc<AppState>>,
    params: Result<RawPathParams, axum::extract::rejection::RawPathParamsRejection>,
    mut request: Request,
    next: Next,
) -> Response {
    let participant = match &state.dcp {
        None => Participant(None),
        Some(dcp) => {
            let authorization = request
                .headers()
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok());
            match dcp.authenticate(authorization).await {
                Ok((token, _)) => Participant(Some(token.participant_did)),
                Err(error) => {
                    tracing::info!(path = %request.uri().path(), %error, "DCP authentication failed");
                    let provider_pid = params.ok().and_then(|p| {
                        p.iter()
                            .find(|(name, _)| *name == "provider_pid")
                            .map(|(_, v)| v.to_string())
                    });
                    return unauthorized(
                        &state,
                        request.uri().path(),
                        provider_pid,
                        error.to_string(),
                    );
                }
            }
        }
    };
    request.extensions_mut().insert(participant);
    next.run(request).await
}

fn unauthorized(
    state: &AppState,
    path: &str,
    provider_pid: Option<String>,
    reason: String,
) -> Response {
    let dsp = &state.config.dsp_path;
    // The DSP error schemas require a providerPid; a request that names
    // none (an initial request) gets one minted for the error, as a
    // refused initial request already does.
    let pid = || provider_pid.unwrap_or_else(|| format!("urn:uuid:{}", uuid::Uuid::new_v4()));
    let mut response = if path.starts_with(&format!("{dsp}/negotiations")) {
        Json(ContractNegotiationError::new(
            Some(pid()),
            None,
            "401",
            reason,
        ))
        .into_response()
    } else if path.starts_with(&format!("{dsp}/transfers")) {
        Json(TransferErrorMessage::new(Some(pid()), None, "401", reason)).into_response()
    } else {
        Json(CatalogError::new("401", reason)).into_response()
    };
    *response.status_mut() = StatusCode::UNAUTHORIZED;
    response
        .headers_mut()
        .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_bound_participant_may_act_with_dcp_on() {
        let alice = Participant(Some("did:web:alice".to_string()));
        assert!(alice.may_act_on(Some("did:web:alice")));
        assert!(!alice.may_act_on(Some("did:web:bob")));
        assert!(!alice.may_act_on(None));
        let anyone = Participant(None);
        assert!(anyone.may_act_on(Some("did:web:alice")));
        assert!(anyone.may_act_on(None));
    }
}
