//! Catalog protocol HTTP surface: `POST {dsp}/catalog/request` and
//! `GET {dsp}/catalog/datasets/:id`, per the
//! [Catalog HTTPS Binding](https://eclipse-dataspace-protocol-base.github.io/DataspaceProtocol/HEAD/#catalog-http).

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use control_plane_core::catalog::{CatalogError, CatalogRequestMessage, DatasetDocument};

use crate::state::{AppState, SEED_DATASET_ID};

/// `POST {dsp}/catalog/request`. This bootstrap always returns the single
/// seed dataset — `filter` is accepted (parsed if present) but never
/// applied; see `../../../ARCHITECTURE.md`.
pub async fn catalog_request(
    State(state): State<Arc<AppState>>,
    body: Option<Json<CatalogRequestMessage>>,
) -> impl IntoResponse {
    if let Some(Json(msg)) = &body {
        tracing::debug!(filter = ?msg.filter, "catalog request received");
    }
    (StatusCode::OK, Json(state.seed_catalog()))
}

/// `GET {dsp}/catalog/datasets/:id`.
pub async fn dataset_request(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if id == SEED_DATASET_ID {
        (
            StatusCode::OK,
            Json(DatasetDocument::from(state.seed_dataset())),
        )
            .into_response()
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(CatalogError::new(
                "404",
                format!("no dataset with id '{id}'"),
            )),
        )
            .into_response()
    }
}
