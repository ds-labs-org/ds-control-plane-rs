//! Shared application state: the in-memory catalog seed and the
//! negotiation/transfer-process stores, both keyed by `providerPid` since
//! this bootstrap only ever plays the Provider role.

use std::collections::HashMap;
use std::sync::Mutex;

use control_plane_core::catalog::{Catalog, DataService, Dataset, Distribution};
use control_plane_core::common::{Offer, Rule};
use control_plane_core::negotiation::ContractNegotiation;
use control_plane_core::transfer::TransferProcess;

use crate::config::Config;

/// A single, fixed dataset id this bootstrap always serves — see
/// `../../../ARCHITECTURE.md` for why per-test dataset seeding
/// (`CAT_NN_NN_DATASETID` etc.) is out of scope for this pass.
pub const SEED_DATASET_ID: &str = "urn:uuid:control-plane-rs-seed-dataset";
const SEED_OFFER_ID: &str = "urn:uuid:control-plane-rs-seed-offer";
const SEED_SERVICE_ID: &str = "urn:uuid:control-plane-rs-dataspace";
const SEED_FORMAT: &str = "HttpData-PULL";

pub struct AppState {
    pub config: Config,
    pub negotiations: Mutex<HashMap<String, ContractNegotiation>>,
    pub transfers: Mutex<HashMap<String, TransferProcess>>,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            negotiations: Mutex::new(HashMap::new()),
            transfers: Mutex::new(HashMap::new()),
        }
    }

    /// The `DataService` this connector advertises: its own DSP endpoint
    /// root, where a Contract Negotiation / Transfer Process for the seed
    /// dataset is initiated.
    pub fn data_service(&self) -> DataService {
        DataService {
            id: SEED_SERVICE_ID.to_string(),
            type_: "DataService".to_string(),
            endpoint_url: self.config.dsp_base_url(),
        }
    }

    /// The single dataset this bootstrap always serves: an always-allow
    /// `use` policy with no constraints, one `HttpData-PULL` distribution.
    pub fn seed_dataset(&self) -> Dataset {
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
                access_service: self.data_service(),
            }],
        }
    }

    pub fn seed_catalog(&self) -> Catalog {
        Catalog {
            context: control_plane_core::dsp_context(),
            id: "urn:uuid:control-plane-rs-catalog".to_string(),
            type_: "Catalog".to_string(),
            participant_id: self.config.agent_id.clone(),
            service: vec![self.data_service()],
            dataset: vec![self.seed_dataset()],
        }
    }
}
