//! Shared application state: the connector's settings and the RDF store
//! (`control-plane-graph`) that holds its catalog and its negotiation/
//! transfer-process records, keyed by `providerPid` since this bootstrap
//! only ever plays the Provider role.

use std::sync::{Arc, Weak};

use control_plane_core::catalog::{Catalog, DataService, Dataset, Distribution};
use control_plane_core::common::{Offer, Rule};
use control_plane_graph::{ConnectorSettings, ControlPlaneGraph, GraphError};

use crate::callback::Callbacks;
use crate::config::Config;

/// The dataset id the catalog is seeded with when the settings graph holds
/// no datasets yet — see `../../../ARCHITECTURE.md` for why per-test
/// dataset seeding (`CAT_NN_NN_DATASETID` etc.) is out of scope for this
/// pass.
pub const SEED_DATASET_ID: &str = "urn:uuid:control-plane-rs-seed-dataset";
const SEED_OFFER_ID: &str = "urn:uuid:control-plane-rs-seed-offer";
const SEED_SERVICE_ID: &str = "urn:uuid:control-plane-rs-dataspace";
const SEED_CATALOG_ID: &str = "urn:uuid:control-plane-rs-catalog";
const SEED_FORMAT: &str = "HttpData-PULL";

pub struct AppState {
    pub config: Config,
    /// The settings this process resolved at startup and wrote to the
    /// settings graph.
    pub settings: ConnectorSettings,
    pub graph: Arc<ControlPlaneGraph>,
    /// Sends this Provider's messages to Consumers' callback addresses.
    pub callbacks: Callbacks,
}

impl AppState {
    /// Opens the store `config` names (in memory when it names none),
    /// writes this process's connector settings into the settings graph,
    /// and seeds the catalog with [`seed_dataset`] the first time.
    ///
    /// The connector's identity (`agent_id`, `base_url`, `dsp_path`) always
    /// comes from `config` — it describes this deployment — and is
    /// rewritten on every start; the catalog lives only in the settings
    /// graph and is never overwritten once present.
    pub fn open(config: Config) -> Result<Self, GraphError> {
        let graph = match &config.store_path {
            Some(path) => ControlPlaneGraph::open(path)?,
            None => ControlPlaneGraph::open_in_memory()?,
        };
        let settings = ConnectorSettings {
            agent_id: config.agent_id.clone(),
            base_url: config.base_url.clone(),
            dsp_path: config.dsp_path.clone(),
            catalog_id: SEED_CATALOG_ID.to_string(),
            data_service_id: SEED_SERVICE_ID.to_string(),
        };
        graph.put_settings(&settings)?;
        if graph.dataset_count()? == 0 {
            graph.put_dataset(&seed_dataset(settings.data_service()))?;
        }
        Ok(Self {
            config,
            settings,
            graph: Arc::new(graph),
            callbacks: Callbacks::new(),
        })
    }

    /// The `DataService` this connector advertises: its own DSP endpoint
    /// root, where a Contract Negotiation / Transfer Process is initiated.
    pub fn data_service(&self) -> DataService {
        self.settings.data_service()
    }

    /// Runs `f` against the graph on Tokio's blocking pool. Every graph
    /// call is synchronous: a write waits on the store's write lock and
    /// then commits (to RocksDB when persistent), and a read waits on the
    /// same lock while a write holds it. Called directly from a handler,
    /// that wait parks the whole worker thread, and once every worker is
    /// parked no request is served, even one that never touches the store.
    /// A panic in `f` is resumed in the caller.
    pub async fn with_graph<R: Send + 'static>(
        &self,
        f: impl FnOnce(&ControlPlaneGraph) -> R + Send + 'static,
    ) -> R {
        let graph = Arc::clone(&self.graph);
        match tokio::task::spawn_blocking(move || f(&graph)).await {
            Ok(r) => r,
            Err(e) => std::panic::resume_unwind(e.into_panic()),
        }
    }

    /// [`Self::with_graph`] for a background task, which holds only a
    /// [`Weak`] handle so that it never keeps the store open (and RocksDB's
    /// exclusive lock held) after the app is dropped, e.g. while it waits
    /// up to [`crate::callback::TIMEOUT`] on a Consumer. `None` once the
    /// app is gone.
    pub async fn with_graph_of<R: Send + 'static>(
        state: &Weak<Self>,
        f: impl FnOnce(&ControlPlaneGraph) -> R + Send + 'static,
    ) -> Option<R> {
        let state = state.upgrade()?;
        Some(state.with_graph(f).await)
    }

    /// The catalog, built from the datasets in the settings graph.
    pub async fn catalog(&self) -> Result<Catalog, GraphError> {
        Ok(Catalog {
            context: control_plane_core::dsp_context(),
            id: self.settings.catalog_id.clone(),
            type_: "Catalog".to_string(),
            participant_id: self.settings.agent_id.clone(),
            service: vec![self.data_service()],
            dataset: self.with_graph(|g| g.datasets()).await?,
        })
    }
}

/// The dataset a fresh settings graph is seeded with: an always-allow `use`
/// policy with no constraints, one `HttpData-PULL` distribution.
pub fn seed_dataset(service: DataService) -> Dataset {
    Dataset {
        id: SEED_DATASET_ID.to_string(),
        type_: "Dataset".to_string(),
        has_policy: vec![Offer {
            id: SEED_OFFER_ID.to_string(),
            type_: "Offer".to_string(),
            target: None,
            permission: vec![Rule {
                action: "use".to_string(),
                constraint: vec![],
            }],
            prohibition: vec![],
            obligation: vec![],
        }],
        distribution: vec![Distribution {
            type_: "Distribution".to_string(),
            format: SEED_FORMAT.to_string(),
            access_service: service,
        }],
    }
}
