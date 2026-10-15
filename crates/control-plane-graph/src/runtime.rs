//! The runtime graph: contract negotiations and transfer processes.
//!
//! Each record's node is its `providerPid` IRI (this connector mints those
//! itself as `urn:uuid:…`), typed `dspace:ContractNegotiation`/
//! `dspace:TransferProcess`, with its state as the real DSP state IRI
//! (`dspace:REQUESTED`, …) — the same IRI the TCK's own JSON-LD processing
//! expands this connector's bare `"REQUESTED"` to. Every record also
//! carries `dcterms:modified`, which is what Contreforts' `pull(since)`
//! filters on.

use chrono::{DateTime, Utc};
use control_plane_core::negotiation::{ContractNegotiation, NegotiationState};
use control_plane_core::transfer::{TransferProcess, TransferState};
use oxigraph::model::{NamedNode, NamedOrBlankNode, NamedOrBlankNodeRef};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::policy::{read_agreement, read_offer, write_agreement, write_offer};
use crate::store::{ControlPlaneGraph, GraphError, Reader, Writer, iri, lookup_iri, readable};
use crate::vocab::{RUNTIME_GRAPH, dcterms, dscp, dspace};

/// The most contract negotiations the runtime graph keeps. Once a write
/// brings the store past it, the least recently modified negotiations
/// (by `dcterms:modified`) are evicted, in the same write. Anyone who can
/// reach `negotiations/request` can add one, and with RocksDB they stay on
/// disk, so this is the ceiling on what such a caller can make the store
/// hold. `ds-identity-hub-rs` caps its equivalent structures the same way.
pub const MAX_NEGOTIATIONS: usize = 2048;

/// The same ceiling for transfer processes.
pub const MAX_TRANSFERS: usize = 2048;

/// A stored record together with when it was last written.
#[derive(Debug, Clone)]
pub struct Record<T> {
    pub value: T,
    pub modified: DateTime<Utc>,
}

fn runtime_graph() -> NamedNode {
    iri(RUNTIME_GRAPH.to_string())
}

/// `NegotiationState::Requested` -> `dspace:REQUESTED`, via the state's own
/// serde name so the two can never drift apart.
fn state_iri<S: Serialize>(state: S) -> NamedNode {
    let name = serde_json::to_value(state)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .expect("state enums serialize as strings");
    iri(dspace(&name))
}

fn read_state<S: DeserializeOwned>(
    r: &Reader<'_>,
    node: NamedOrBlankNodeRef<'_>,
) -> Result<S, GraphError> {
    let value = r.str(node, &dspace("state"))?;
    let name = value.strip_prefix(crate::vocab::DSPACE).unwrap_or(&value);
    serde_json::from_value(serde_json::Value::String(name.to_string())).map_err(|_| {
        GraphError::Invalid {
            node: node.to_string(),
            property: dspace("state"),
            value: value.clone(),
        }
    })
}

fn named(nodes: Vec<NamedOrBlankNode>) -> Vec<NamedNode> {
    nodes
        .into_iter()
        .filter_map(|n| match n {
            NamedOrBlankNode::NamedNode(n) => Some(n),
            NamedOrBlankNode::BlankNode(_) => None,
        })
        .collect()
}

fn write_negotiation(
    n: &ContractNegotiation,
) -> Result<(NamedNode, Vec<oxigraph::model::Quad>), GraphError> {
    let node = NamedNode::new(&n.provider_pid)?;
    let mut w = Writer::new(&runtime_graph());
    w.add_type(node.clone(), dspace("ContractNegotiation"));
    w.add_str(node.clone(), dspace("providerPid"), &n.provider_pid);
    w.add_str(node.clone(), dspace("consumerPid"), &n.consumer_pid);
    w.add(node.clone(), dspace("state"), state_iri(n.state));
    w.add_opt_str(node.clone(), dscp("datasetId"), n.dataset_id.as_deref());
    if let Some(offer) = &n.offer {
        write_offer(&mut w, node.clone(), dspace("offer"), 0, offer);
    }
    if let Some(agreement) = &n.agreement {
        write_agreement(&mut w, node.clone(), dspace("agreement"), agreement);
    }
    w.add_date_time(node.clone(), dcterms("modified"), Utc::now());
    Ok((node, w.quads))
}

fn read_negotiation(
    r: &Reader<'_>,
    node: &NamedNode,
) -> Result<Record<ContractNegotiation>, GraphError> {
    let s: NamedOrBlankNodeRef<'_> = node.as_ref().into();
    let offer = match r.ordered_children(s, &dspace("offer"))?.first() {
        Some(o) => Some(read_offer(r, o.as_ref())?),
        None => None,
    };
    let agreement = match r.ordered_children(s, &dspace("agreement"))?.first() {
        Some(a) => Some(read_agreement(r, a.as_ref())?),
        None => None,
    };
    Ok(Record {
        value: ContractNegotiation {
            provider_pid: r.str(s, &dspace("providerPid"))?,
            consumer_pid: r.str(s, &dspace("consumerPid"))?,
            state: read_state::<NegotiationState>(r, s)?,
            dataset_id: r.opt_str(s, &dscp("datasetId"))?,
            offer,
            agreement,
        },
        modified: r.date_time(s, &dcterms("modified"))?,
    })
}

fn write_transfer(
    t: &TransferProcess,
) -> Result<(NamedNode, Vec<oxigraph::model::Quad>), GraphError> {
    let node = NamedNode::new(&t.provider_pid)?;
    let mut w = Writer::new(&runtime_graph());
    w.add_type(node.clone(), dspace("TransferProcess"));
    w.add_str(node.clone(), dspace("providerPid"), &t.provider_pid);
    w.add_str(node.clone(), dspace("consumerPid"), &t.consumer_pid);
    w.add(node.clone(), dspace("state"), state_iri(t.state));
    w.add_str(node.clone(), dspace("agreementId"), &t.agreement_id);
    w.add_str(node.clone(), dcterms("format"), &t.format);
    w.add_date_time(node.clone(), dcterms("modified"), Utc::now());
    Ok((node, w.quads))
}

fn read_transfer(r: &Reader<'_>, node: &NamedNode) -> Result<Record<TransferProcess>, GraphError> {
    let s: NamedOrBlankNodeRef<'_> = node.as_ref().into();
    Ok(Record {
        value: TransferProcess {
            provider_pid: r.str(s, &dspace("providerPid"))?,
            consumer_pid: r.str(s, &dspace("consumerPid"))?,
            state: read_state::<TransferState>(r, s)?,
            agreement_id: r.str(s, &dspace("agreementId"))?,
            format: r.str(s, &dcterms("format"))?,
        },
        modified: r.date_time(s, &dcterms("modified"))?,
    })
}

impl ControlPlaneGraph {
    fn find(&self, provider_pid: &str, class: &str) -> Result<Option<NamedNode>, GraphError> {
        let graph = runtime_graph();
        let Some(node) = lookup_iri(provider_pid) else {
            return Ok(None);
        };
        if self.reader(&graph).has_type(node.as_ref().into(), class)? {
            Ok(Some(node))
        } else {
            Ok(None)
        }
    }

    /// The negotiation `provider_pid`, or `None` if there is none.
    pub fn negotiation(
        &self,
        provider_pid: &str,
    ) -> Result<Option<ContractNegotiation>, GraphError> {
        let _guard = self.read_guard();
        let graph = runtime_graph();
        match self.find(provider_pid, &dspace("ContractNegotiation"))? {
            Some(node) => Ok(Some(read_negotiation(&self.reader(&graph), &node)?.value)),
            None => Ok(None),
        }
    }

    /// The number of stored negotiations, without reading them.
    pub fn negotiation_count(&self) -> Result<usize, GraphError> {
        self.count(&runtime_graph(), &dspace("ContractNegotiation"))
    }

    /// Every readable stored negotiation, sorted by `providerPid`. One that
    /// fails to decode is skipped with a warning.
    pub fn negotiations(&self) -> Result<Vec<Record<ContractNegotiation>>, GraphError> {
        let _guard = self.read_guard();
        let graph = runtime_graph();
        let r = self.reader(&graph);
        let mut nodes = named(r.instances(&dspace("ContractNegotiation"))?);
        nodes.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        readable("negotiation", &nodes, |n| read_negotiation(&r, n))
    }

    /// Stores `negotiation`, replacing any record with the same
    /// `providerPid`, then evicts the least recently modified negotiations
    /// past [`MAX_NEGOTIATIONS`].
    pub fn put_negotiation(&self, negotiation: &ContractNegotiation) -> Result<(), GraphError> {
        let _guard = self.write_guard();
        let (node, quads) = write_negotiation(negotiation)?;
        let graph = runtime_graph();
        self.replace(&graph, &node, quads)?;
        self.evict_oldest(&graph, &dspace("ContractNegotiation"), MAX_NEGOTIATIONS)
    }

    /// Reads the negotiation `provider_pid`, applies `f`, and writes it back
    /// only if `f` returns `Ok`, all under the write lock. `Ok(None)` when
    /// there is no such negotiation.
    pub fn update_negotiation<R, E>(
        &self,
        provider_pid: &str,
        f: impl FnOnce(&mut ContractNegotiation) -> Result<R, E>,
    ) -> Result<Option<Result<R, E>>, GraphError> {
        let _guard = self.write_guard();
        let graph = runtime_graph();
        let Some(node) = self.find(provider_pid, &dspace("ContractNegotiation"))? else {
            return Ok(None);
        };
        let mut negotiation = read_negotiation(&self.reader(&graph), &node)?.value;
        let result = f(&mut negotiation);
        if result.is_ok() {
            let (node, quads) = write_negotiation(&negotiation)?;
            self.replace(&graph, &node, quads)?;
        }
        Ok(Some(result))
    }

    /// The transfer process `provider_pid`, or `None` if there is none.
    pub fn transfer(&self, provider_pid: &str) -> Result<Option<TransferProcess>, GraphError> {
        let _guard = self.read_guard();
        let graph = runtime_graph();
        match self.find(provider_pid, &dspace("TransferProcess"))? {
            Some(node) => Ok(Some(read_transfer(&self.reader(&graph), &node)?.value)),
            None => Ok(None),
        }
    }

    /// The number of stored transfer processes, without reading them.
    pub fn transfer_count(&self) -> Result<usize, GraphError> {
        self.count(&runtime_graph(), &dspace("TransferProcess"))
    }

    /// Every readable stored transfer process, sorted by `providerPid`. One
    /// that fails to decode is skipped with a warning.
    pub fn transfers(&self) -> Result<Vec<Record<TransferProcess>>, GraphError> {
        let _guard = self.read_guard();
        let graph = runtime_graph();
        let r = self.reader(&graph);
        let mut nodes = named(r.instances(&dspace("TransferProcess"))?);
        nodes.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        readable("transfer process", &nodes, |n| read_transfer(&r, n))
    }

    /// Stores `transfer`, replacing any record with the same `providerPid`,
    /// then evicts the least recently modified transfers past
    /// [`MAX_TRANSFERS`].
    pub fn put_transfer(&self, transfer: &TransferProcess) -> Result<(), GraphError> {
        let _guard = self.write_guard();
        let (node, quads) = write_transfer(transfer)?;
        let graph = runtime_graph();
        self.replace(&graph, &node, quads)?;
        self.evict_oldest(&graph, &dspace("TransferProcess"), MAX_TRANSFERS)
    }

    /// Same as [`Self::update_negotiation`], for a transfer process.
    pub fn update_transfer<R, E>(
        &self,
        provider_pid: &str,
        f: impl FnOnce(&mut TransferProcess) -> Result<R, E>,
    ) -> Result<Option<Result<R, E>>, GraphError> {
        let _guard = self.write_guard();
        let graph = runtime_graph();
        let Some(node) = self.find(provider_pid, &dspace("TransferProcess"))? else {
            return Ok(None);
        };
        let mut transfer = read_transfer(&self.reader(&graph), &node)?.value;
        let result = f(&mut transfer);
        if result.is_ok() {
            let (node, quads) = write_transfer(&transfer)?;
            self.replace(&graph, &node, quads)?;
        }
        Ok(Some(result))
    }
}
