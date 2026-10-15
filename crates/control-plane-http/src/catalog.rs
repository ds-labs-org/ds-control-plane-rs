//! Catalog protocol HTTP surface: `POST {dsp}/catalog/request` and
//! `GET {dsp}/catalog/datasets/:id`, per the
//! [Catalog HTTPS Binding](https://eclipse-dataspace-protocol-base.github.io/DataspaceProtocol/HEAD/#catalog-http).

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use control_plane_core::catalog::{CatalogError, CatalogRequestMessage, DatasetDocument};

use crate::state::AppState;

fn store_error(e: control_plane_graph::GraphError) -> axum::response::Response {
    tracing::error!(error = %e, "catalog store error");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(CatalogError::new("500", "catalog store error")),
    )
        .into_response()
}

/// `POST {dsp}/catalog/request`. Returns every dataset in the settings
/// graph — `filter` is accepted (parsed if present) but never applied; see
/// `../../../ARCHITECTURE.md`.
pub async fn catalog_request(
    State(state): State<Arc<AppState>>,
    body: Option<Json<CatalogRequestMessage>>,
) -> impl IntoResponse {
    if let Some(Json(msg)) = &body {
        tracing::debug!(filter = ?msg.filter, "catalog request received");
    }
    match state.catalog().await {
        Ok(catalog) => (StatusCode::OK, Json(catalog)).into_response(),
        Err(e) => store_error(e),
    }
}

/// `GET {dsp}/catalog/datasets/:id`.
pub async fn dataset_request(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let lookup = id.clone();
    match state.with_graph(move |g| g.dataset(&lookup)).await {
        Ok(Some(dataset)) => (StatusCode::OK, Json(DatasetDocument::from(dataset))).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(CatalogError::new(
                "404",
                format!("no dataset with id '{id}'"),
            )),
        )
            .into_response(),
        Err(e) => store_error(e),
    }
}
