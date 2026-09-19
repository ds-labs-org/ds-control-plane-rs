//! The version metadata / well-known endpoint (spec: "Exposure of
//! Dataspace Protocol Versions"). Unversioned, unauthenticated, served at
//! the connector's root — this is the one protocol surface this bootstrap
//! implements for real and expects to pass its whole TCK group (`MET`).

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::response::IntoResponse;
use serde::Serialize;

use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct VersionEntry {
    pub version: &'static str,
    pub path: String,
    pub binding: &'static str,
}

#[derive(Debug, Serialize)]
pub struct VersionResponse {
    #[serde(rename = "protocolVersions")]
    pub protocol_versions: Vec<VersionEntry>,
}

/// `GET /.well-known/dspace-version`. Per the spec: MUST return an HTTP 200
/// with a `VersionResponse` carrying at least one entry; the entry's
/// `path`, appended to `<root>`, yields `<base>` — the versioned DSP
/// endpoint root every catalog/negotiation/transfer route in this crate is
/// mounted under (see `Config::dsp_path`).
pub async fn well_known_version(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(VersionResponse {
        protocol_versions: vec![VersionEntry {
            version: "2025-1",
            path: state.config.dsp_path.clone(),
            binding: "HTTPS",
        }],
    })
}
