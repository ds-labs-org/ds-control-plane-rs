//! Implements Contreforts' [`ContrefortsConnector`] trait over
//! `control-plane-graph`, so the control plane's catalog datasets (its
//! settings) and its contract negotiations and transfer processes (its
//! runtime state) are reachable through Contreforts' connector interface.
//! See `../../ARCHITECTURE.md`, "Contreforts: settings and runtime
//! persistence".
//!
//! As in `ds-identity-hub-rs`'s and `ds-sql-dps-rs`'s connectors, only the
//! interface and declaration mechanism are reused: the entity kinds and
//! namespace are this project's own (`declaration.ttl`), not Contreforts'
//! business vocabulary.
//!
//! Datasets are the one writable kind: `push` adds a new dataset and
//! `update` replaces an existing one, which is how a Contreforts driver
//! manages this connector's settings. Negotiations and transfers are read-only here;
//! they only change through the DSP protocol itself.

use std::sync::Arc;

use chrono::NaiveDateTime;
use contreforts_core::{ConnectorError, ContrefortsConnector, Document, EntityKind};
use control_plane_core::catalog::Dataset;
use control_plane_core::negotiation::ContractNegotiation;
use control_plane_core::transfer::TransferProcess;
use control_plane_graph::{ControlPlaneGraph, GraphError, Record};

/// A catalog dataset in the settings graph.
pub const DATASET_KIND: &str = "ds-control-plane-rs:dataset";
/// A contract negotiation in the runtime graph.
pub const NEGOTIATION_KIND: &str = "ds-control-plane-rs:contract-negotiation";
/// A transfer process in the runtime graph.
pub const TRANSFER_KIND: &str = "ds-control-plane-rs:transfer-process";

/// Every kind this connector serves.
pub const KINDS: [&str; 3] = [DATASET_KIND, NEGOTIATION_KIND, TRANSFER_KIND];

const SOURCE: &str = "ds-control-plane-rs";

/// A [`GraphError::Conflict`] is bad input (an id colliding with another
/// record), so it is a `Config` error; anything else is a store failure.
fn to_api_err(e: GraphError) -> ConnectorError {
    match e {
        GraphError::Conflict { .. } => ConnectorError::Config(e.to_string()),
        e => ConnectorError::Api {
            message: e.to_string(),
        },
    }
}

fn not_found(kind: &EntityKind, id: &str) -> ConnectorError {
    ConnectorError::NotFound {
        kind: kind.as_str().to_string(),
        id: id.to_string(),
    }
}

fn dataset_document(
    dataset: Dataset,
    modified: Option<NaiveDateTime>,
) -> Result<Document, ConnectorError> {
    Ok(Document {
        name: dataset.id.clone(),
        remote_id: dataset.id.clone(),
        kind: EntityKind::new(DATASET_KIND),
        source: SOURCE.to_string(),
        modified,
        fields: serde_json::to_value(dataset)?,
    })
}

fn negotiation_document(
    n: ContractNegotiation,
    modified: Option<NaiveDateTime>,
) -> Result<Document, ConnectorError> {
    Ok(Document {
        name: format!("contract-negotiation-{}", n.consumer_pid),
        remote_id: n.provider_pid.clone(),
        kind: EntityKind::new(NEGOTIATION_KIND),
        source: SOURCE.to_string(),
        modified,
        fields: serde_json::json!({
            "providerPid": n.provider_pid,
            "consumerPid": n.consumer_pid,
            "state": n.state,
            "datasetId": n.dataset_id,
            "offer": n.offer,
            "agreement": n.agreement,
        }),
    })
}

fn transfer_document(
    t: TransferProcess,
    modified: Option<NaiveDateTime>,
) -> Result<Document, ConnectorError> {
    Ok(Document {
        name: format!("transfer-process-{}", t.consumer_pid),
        remote_id: t.provider_pid.clone(),
        kind: EntityKind::new(TRANSFER_KIND),
        source: SOURCE.to_string(),
        modified,
        fields: serde_json::json!({
            "providerPid": t.provider_pid,
            "consumerPid": t.consumer_pid,
            "state": t.state,
            "agreementId": t.agreement_id,
            "format": t.format,
        }),
    })
}

/// Records modified strictly after `since` (all of them when `since` is
/// `None`).
fn changed_since<T>(
    records: Vec<Record<T>>,
    since: Option<NaiveDateTime>,
) -> impl Iterator<Item = (T, NaiveDateTime)> {
    records
        .into_iter()
        .map(|r| (r.value, r.modified.naive_utc()))
        .filter(move |(_, modified)| since.is_none_or(|since| *modified > since))
}

/// Adapts a [`ControlPlaneGraph`] to Contreforts' connector interface.
/// Answers to exactly [`KINDS`]; any other kind is
/// [`ConnectorError::UnsupportedKind`], per the trait's fallback policy.
pub struct ControlPlaneConnector {
    graph: Arc<ControlPlaneGraph>,
}

impl ControlPlaneConnector {
    pub fn new(graph: Arc<ControlPlaneGraph>) -> Self {
        Self { graph }
    }

    fn unsupported_kind(&self, kind: &EntityKind) -> ConnectorError {
        ConnectorError::UnsupportedKind {
            connector: self.source_name().to_string(),
            kind: kind.as_str().to_string(),
        }
    }

    /// Only datasets are writable: an undeclared kind is `UnsupportedKind`,
    /// a declared read-only kind (negotiation, transfer) is `Unsupported`.
    fn dataset_of(&self, doc: &Document) -> Result<Dataset, ConnectorError> {
        if !KINDS.contains(&doc.kind.as_str()) {
            return Err(self.unsupported_kind(&doc.kind));
        }
        if doc.kind.as_str() != DATASET_KIND {
            return Err(ConnectorError::Unsupported {
                connector: self.source_name().to_string(),
                operation: format!("write (kind: {})", doc.kind),
            });
        }
        Ok(serde_json::from_value(doc.fields.clone())?)
    }
}

#[async_trait::async_trait]
impl ContrefortsConnector for ControlPlaneConnector {
    fn source_name(&self) -> &str {
        SOURCE
    }

    fn declaration_ttl(&self) -> &'static str {
        include_str!("declaration.ttl")
    }

    /// Every kind carries `dcterms:modified`, which `since` filters on.
    async fn pull(
        &self,
        kind: EntityKind,
        since: Option<NaiveDateTime>,
    ) -> Result<Vec<Document>, ConnectorError> {
        match kind.as_str() {
            DATASET_KIND => changed_since(self.graph.dataset_records().map_err(to_api_err)?, since)
                .map(|(d, m)| dataset_document(d, Some(m)))
                .collect(),
            NEGOTIATION_KIND => {
                changed_since(self.graph.negotiations().map_err(to_api_err)?, since)
                    .map(|(n, m)| negotiation_document(n, Some(m)))
                    .collect()
            }
            TRANSFER_KIND => changed_since(self.graph.transfers().map_err(to_api_err)?, since)
                .map(|(t, m)| transfer_document(t, Some(m)))
                .collect(),
            _ => Err(self.unsupported_kind(&kind)),
        }
    }

    async fn get(&self, kind: EntityKind, remote_id: &str) -> Result<Document, ConnectorError> {
        match kind.as_str() {
            DATASET_KIND => {
                let r = self
                    .graph
                    .dataset_record(remote_id)
                    .map_err(to_api_err)?
                    .ok_or_else(|| not_found(&kind, remote_id))?;
                dataset_document(r.value, Some(r.modified.naive_utc()))
            }
            NEGOTIATION_KIND => {
                let r = self
                    .graph
                    .negotiation_record(remote_id)
                    .map_err(to_api_err)?
                    .ok_or_else(|| not_found(&kind, remote_id))?;
                negotiation_document(r.value, Some(r.modified.naive_utc()))
            }
            TRANSFER_KIND => {
                let r = self
                    .graph
                    .transfer_record(remote_id)
                    .map_err(to_api_err)?
                    .ok_or_else(|| not_found(&kind, remote_id))?;
                transfer_document(r.value, Some(r.modified.naive_utc()))
            }
            _ => Err(self.unsupported_kind(&kind)),
        }
    }

    /// Adds a dataset to the catalog; `doc.fields` is the dataset's DSP
    /// JSON (`@id`, `hasPolicy`, `distribution`). A dataset with that id
    /// already existing is a `Config` error and changes nothing: replacing
    /// one is `update`'s job.
    async fn push(&self, doc: &Document) -> Result<Document, ConnectorError> {
        let dataset = self.dataset_of(doc)?;
        if !self
            .graph
            .put_dataset_if_absent(&dataset)
            .map_err(to_api_err)?
        {
            return Err(ConnectorError::Config(format!(
                "dataset '{}' already exists; use update to replace it",
                dataset.id
            )));
        }
        self.get(EntityKind::new(DATASET_KIND), &dataset.id).await
    }

    /// Replaces the dataset `doc.remote_id` with `doc.fields`, whose `@id`
    /// must match it. An unknown `remote_id` is `NotFound` and creates
    /// nothing.
    async fn update(&self, doc: &Document) -> Result<Document, ConnectorError> {
        if doc.kind.as_str() == DATASET_KIND
            && doc.fields.get("@id").and_then(|v| v.as_str()) != Some(doc.remote_id.as_str())
        {
            return Err(ConnectorError::Config(format!(
                "dataset @id must match remote_id '{}'",
                doc.remote_id
            )));
        }
        let dataset = self.dataset_of(doc)?;
        if !self
            .graph
            .replace_dataset_if_present(&dataset)
            .map_err(to_api_err)?
        {
            return Err(not_found(&doc.kind, &doc.remote_id));
        }
        self.get(EntityKind::new(DATASET_KIND), &dataset.id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use control_plane_core::negotiation::NegotiationState;
    use control_plane_graph::ConnectorSettings;

    fn graph() -> Arc<ControlPlaneGraph> {
        let graph = ControlPlaneGraph::open_in_memory().unwrap();
        graph
            .put_settings(&ConnectorSettings {
                agent_id: "urn:connector:test".to_string(),
                base_url: "http://localhost:8080".to_string(),
                dsp_path: "/dsp/2025-1".to_string(),
                catalog_id: "urn:uuid:catalog".to_string(),
                data_service_id: "urn:uuid:service".to_string(),
            })
            .unwrap();
        graph
            .put_negotiation(&ContractNegotiation::new("urn:uuid:n1", "urn:uuid:c1"))
            .unwrap();
        graph
            .put_transfer(&TransferProcess::new(
                "urn:uuid:t1",
                "urn:uuid:c1",
                "urn:uuid:a1",
                "HttpData-PULL",
            ))
            .unwrap();
        Arc::new(graph)
    }

    fn dataset_doc(id: &str) -> Document {
        Document {
            name: id.to_string(),
            remote_id: id.to_string(),
            kind: EntityKind::new(DATASET_KIND),
            source: SOURCE.to_string(),
            modified: None,
            fields: serde_json::json!({
                "@id": id,
                "@type": "Dataset",
                "hasPolicy": [{
                    "@id": "urn:uuid:offer",
                    "@type": "Offer",
                    "permission": [{ "action": "use" }],
                }],
                "distribution": [{
                    "@type": "Distribution",
                    "format": "HttpData-PULL",
                    "accessService": {
                        "@id": "urn:uuid:service",
                        "@type": "DataService",
                        "endpointURL": "http://localhost:8080/dsp/2025-1",
                    },
                }],
            }),
        }
    }

    #[tokio::test]
    async fn push_adds_a_dataset_that_pull_and_get_then_return() {
        let connector = ControlPlaneConnector::new(graph());
        let pushed = connector.push(&dataset_doc("urn:uuid:ds")).await.unwrap();
        assert_eq!(pushed.remote_id, "urn:uuid:ds");

        let pulled = connector
            .pull(EntityKind::new(DATASET_KIND), None)
            .await
            .unwrap();
        assert_eq!(pulled.len(), 1);
        assert_eq!(pulled[0].fields, dataset_doc("urn:uuid:ds").fields);

        let got = connector
            .get(EntityKind::new(DATASET_KIND), "urn:uuid:ds")
            .await
            .unwrap();
        assert_eq!(got.fields["hasPolicy"][0]["permission"][0]["action"], "use");
    }

    #[tokio::test]
    async fn push_rejects_a_dataset_colliding_with_the_data_service() {
        let graph = graph();
        let connector = ControlPlaneConnector::new(graph.clone());
        connector.push(&dataset_doc("urn:uuid:ds")).await.unwrap();
        let snapshot = || {
            (
                graph.settings().unwrap(),
                serde_json::to_value(graph.datasets().unwrap()).unwrap(),
            )
        };
        let before = snapshot();

        let result = connector.push(&dataset_doc("urn:uuid:service")).await;
        assert!(
            matches!(result, Err(ConnectorError::Config(_))),
            "{result:?}"
        );
        assert_eq!(snapshot(), before);
    }

    #[tokio::test]
    async fn update_of_a_missing_dataset_is_not_found_and_creates_nothing() {
        let graph = graph();
        let connector = ControlPlaneConnector::new(graph.clone());
        match connector.update(&dataset_doc("urn:uuid:never")).await {
            Err(ConnectorError::NotFound { kind, id }) => {
                assert_eq!(kind, DATASET_KIND);
                assert_eq!(id, "urn:uuid:never");
            }
            other => panic!("expected NotFound, got {other:?}"),
        }
        assert!(graph.dataset("urn:uuid:never").unwrap().is_none());
    }

    #[tokio::test]
    async fn push_of_an_existing_dataset_errors_and_leaves_it_unchanged() {
        let graph = graph();
        let connector = ControlPlaneConnector::new(graph.clone());
        connector.push(&dataset_doc("urn:uuid:ds")).await.unwrap();
        let before = serde_json::to_value(graph.datasets().unwrap()).unwrap();

        let mut doc = dataset_doc("urn:uuid:ds");
        doc.fields["distribution"][0]["format"] = serde_json::json!("OTHER");
        let result = connector.push(&doc).await;
        assert!(
            matches!(result, Err(ConnectorError::Config(_))),
            "{result:?}"
        );
        assert_eq!(
            serde_json::to_value(graph.datasets().unwrap()).unwrap(),
            before
        );

        // update is how an existing dataset is replaced.
        let updated = connector.update(&doc).await.unwrap();
        assert_eq!(updated.fields["distribution"][0]["format"], "OTHER");
    }

    #[tokio::test]
    async fn update_rejects_a_mismatched_id() {
        let connector = ControlPlaneConnector::new(graph());
        let mut doc = dataset_doc("urn:uuid:ds");
        doc.remote_id = "urn:uuid:other".to_string();
        assert!(matches!(
            connector.update(&doc).await,
            Err(ConnectorError::Config(_))
        ));
    }

    #[tokio::test]
    async fn runtime_kinds_are_readable_but_not_writable() {
        let graph = graph();
        let connector = ControlPlaneConnector::new(graph.clone());

        let negotiations = connector
            .pull(EntityKind::new(NEGOTIATION_KIND), None)
            .await
            .unwrap();
        assert_eq!(negotiations.len(), 1);
        assert_eq!(negotiations[0].fields["state"], "REQUESTED");
        assert!(negotiations[0].modified.is_some());

        let transfer = connector
            .get(EntityKind::new(TRANSFER_KIND), "urn:uuid:t1")
            .await
            .unwrap();
        assert_eq!(transfer.fields["format"], "HttpData-PULL");

        let mut doc = negotiations[0].clone();
        doc.fields["state"] = serde_json::json!("FINALIZED");
        assert!(matches!(
            connector.push(&doc).await,
            Err(ConnectorError::Unsupported { .. })
        ));
        assert_eq!(
            graph.negotiation("urn:uuid:n1").unwrap().unwrap().state,
            NegotiationState::Requested
        );
    }

    #[tokio::test]
    async fn pull_since_returns_only_records_changed_after_it() {
        let connector = ControlPlaneConnector::new(graph());
        let future = (chrono::Utc::now() + chrono::Duration::hours(1)).naive_utc();
        let past = (chrono::Utc::now() - chrono::Duration::hours(1)).naive_utc();
        let kind = EntityKind::new(TRANSFER_KIND);
        assert!(
            connector
                .pull(kind.clone(), Some(future))
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(connector.pull(kind, Some(past)).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn get_and_pull_agree_on_modified_and_datasets_honour_since() {
        let connector = ControlPlaneConnector::new(graph());
        for (kind, id) in [
            (NEGOTIATION_KIND, "urn:uuid:n1"),
            (TRANSFER_KIND, "urn:uuid:t1"),
        ] {
            let kind = EntityKind::new(kind);
            let pulled = connector.pull(kind.clone(), None).await.unwrap();
            let got = connector.get(kind, id).await.unwrap();
            assert!(got.modified.is_some(), "{id}");
            assert_eq!(got.modified, pulled[0].modified, "{id}");
        }

        connector.push(&dataset_doc("urn:uuid:ds")).await.unwrap();
        let kind = EntityKind::new(DATASET_KIND);
        let got = connector.get(kind.clone(), "urn:uuid:ds").await.unwrap();
        let pulled = connector.pull(kind.clone(), None).await.unwrap();
        assert!(got.modified.is_some());
        assert_eq!(got.modified, pulled[0].modified);
        let future = (chrono::Utc::now() + chrono::Duration::hours(1)).naive_utc();
        let past = (chrono::Utc::now() - chrono::Duration::hours(1)).naive_utc();
        assert!(
            connector
                .pull(kind.clone(), Some(future))
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(connector.pull(kind, Some(past)).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn unknown_ids_and_kinds_error() {
        let connector = ControlPlaneConnector::new(graph());
        assert!(matches!(
            connector
                .get(EntityKind::new(NEGOTIATION_KIND), "urn:uuid:missing")
                .await,
            Err(ConnectorError::NotFound { .. })
        ));
        match connector.pull(EntityKind::CUSTOMER, None).await {
            Err(ConnectorError::UnsupportedKind { connector, kind }) => {
                assert_eq!(connector, SOURCE);
                assert_eq!(kind, "customer");
            }
            other => panic!("expected UnsupportedKind, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn writing_an_undeclared_kind_is_unsupported_kind() {
        let connector = ControlPlaneConnector::new(graph());
        let mut doc = dataset_doc("urn:uuid:ds");
        doc.kind = EntityKind::CUSTOMER;
        for result in [connector.push(&doc).await, connector.update(&doc).await] {
            match result {
                Err(ConnectorError::UnsupportedKind { connector, kind }) => {
                    assert_eq!(connector, SOURCE);
                    assert_eq!(kind, "customer");
                }
                other => panic!("expected UnsupportedKind, got {other:?}"),
            }
        }
    }
}
