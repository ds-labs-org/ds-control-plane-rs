//! Catalog protocol message and resource types. See the spec's
//! [Catalog Protocol](https://eclipse-dataspace-protocol-base.github.io/DataspaceProtocol/HEAD/#catalog-protocol).

use serde::{Deserialize, Serialize};

use crate::common::{Offer, dsp_context};

fn catalog_request_type() -> String {
    "CatalogRequestMessage".to_string()
}

/// Sent by a Consumer to a Catalog Service to request its `Catalog`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogRequestMessage {
    #[serde(rename = "@context", default = "dsp_context")]
    pub context: Vec<String>,
    #[serde(rename = "@type", default = "catalog_request_type")]
    pub type_: String,
    /// Implementation-specific filter/query expression. Unsupported by this
    /// bootstrap: a non-empty `filter` always yields the full, unfiltered
    /// catalog rather than a 400 — see `../../../ARCHITECTURE.md`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filter: Vec<serde_json::Value>,
}

fn dataset_request_type() -> String {
    "DatasetRequestMessage".to_string()
}

/// Sent by a Consumer to request a single named `Dataset`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetRequestMessage {
    #[serde(rename = "@context", default = "dsp_context")]
    pub context: Vec<String>,
    #[serde(rename = "@type", default = "dataset_request_type")]
    pub type_: String,
    pub dataset: String,
}

fn dataservice_type() -> String {
    "DataService".to_string()
}

/// A `DataService` referenced by a `Distribution.accessService`: the
/// endpoint a Contract Negotiation / Transfer Process is initiated
/// against.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataService {
    #[serde(rename = "@id")]
    pub id: String,
    #[serde(rename = "@type", default = "dataservice_type")]
    pub type_: String,
    #[serde(rename = "endpointURL")]
    pub endpoint_url: String,
}

fn distribution_type() -> String {
    "Distribution".to_string()
}

/// One way of obtaining a `Dataset`: a wire `format` (e.g.
/// `HttpData-PULL`) plus the `DataService` that serves it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Distribution {
    #[serde(rename = "@type", default = "distribution_type")]
    pub type_: String,
    pub format: String,
    /// Per the spec's `dataset.json` example this MAY be an embedded
    /// `DataService` object or a bare `@id` reference; this bootstrap
    /// always embeds it.
    #[serde(rename = "accessService")]
    pub access_service: DataService,
}

fn dataset_type() -> String {
    "Dataset".to_string()
}

/// A single dataset offer: MUST have at least one `hasPolicy` (an
/// ODRL `Offer`) and at least one `distribution`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dataset {
    #[serde(rename = "@id")]
    pub id: String,
    #[serde(rename = "@type", default = "dataset_type")]
    pub type_: String,
    #[serde(rename = "hasPolicy")]
    pub has_policy: Vec<Offer>,
    pub distribution: Vec<Distribution>,
}

/// The standalone `ACK - Dataset` document returned by
/// `GET {dsp}/catalog/datasets/:id` (spec example `catalog/example/dataset.json`):
/// unlike a `Dataset` nested inside a `Catalog.dataset` array, a standalone
/// one is itself a JSON-LD document and carries its own `@context`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetDocument {
    #[serde(rename = "@context", default = "dsp_context")]
    pub context: Vec<String>,
    #[serde(flatten)]
    pub dataset: Dataset,
}

impl From<Dataset> for DatasetDocument {
    fn from(dataset: Dataset) -> Self {
        Self {
            context: dsp_context(),
            dataset,
        }
    }
}

fn catalog_type() -> String {
    "Catalog".to_string()
}

/// The `ACK` response to a `CatalogRequestMessage`/`DatasetRequestMessage`:
/// zero to many `Dataset`s and one to many `DataService`s.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Catalog {
    #[serde(rename = "@context", default = "dsp_context")]
    pub context: Vec<String>,
    #[serde(rename = "@id")]
    pub id: String,
    #[serde(rename = "@type", default = "catalog_type")]
    pub type_: String,
    #[serde(rename = "participantId")]
    pub participant_id: String,
    pub service: Vec<DataService>,
    pub dataset: Vec<Dataset>,
}

fn catalog_error_type() -> String {
    "CatalogError".to_string()
}

/// Returned when a Provider cannot serve its `Catalog`/`Dataset` to a
/// requester.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogError {
    #[serde(rename = "@context", default = "dsp_context")]
    pub context: Vec<String>,
    #[serde(rename = "@type", default = "catalog_error_type")]
    pub type_: String,
    pub code: String,
    pub reason: Vec<String>,
}

impl CatalogError {
    pub fn new(code: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            context: dsp_context(),
            type_: catalog_error_type(),
            code: code.into(),
            reason: vec![reason.into()],
        }
    }
}
