//! The settings graph: the connector's identity and the catalog it serves.
//!
//! Datasets and data services are named by their own `@id` IRIs (they are
//! operator-configured, not counterparty input), so the settings graph is
//! directly SPARQL-addressable by the same ids the catalog serves. A
//! dataset's distributions point at their data service by IRI; the data
//! service node itself (its `dcat:endpointURL`) is shared, so moving the
//! connector to a new base URL rewrites one triple, not every dataset.

use std::collections::HashMap;

use control_plane_core::catalog::{DataService, Dataset, Distribution};
use oxigraph::model::{Literal, NamedNode, NamedOrBlankNode, NamedOrBlankNodeRef, Term};

use crate::policy::{read_offer, write_offer};
use crate::store::{ControlPlaneGraph, GraphError, Reader, Writer, iri, lookup_iri};
use crate::vocab::{SETTINGS_GRAPH, SETTINGS_NODE, dcat, dcterms, dscp, dspace, odrl, xsd};

/// The connector's own identity, as stored in the settings graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectorSettings {
    /// `participantId` on the catalogs and agreements this connector issues.
    pub agent_id: String,
    /// Externally reachable base URL, no trailing slash.
    pub base_url: String,
    /// Path segment for every versioned DSP endpoint, e.g. `/dsp/2025-1`.
    pub dsp_path: String,
    /// `@id` of the `Catalog` this connector serves.
    pub catalog_id: String,
    /// `@id` of this connector's own `DataService`.
    pub data_service_id: String,
}

impl ConnectorSettings {
    /// `base_url` + `dsp_path`: this connector's own DSP endpoint root.
    pub fn dsp_base_url(&self) -> String {
        format!("{}{}", self.base_url, self.dsp_path)
    }

    /// This connector's own `DataService`, with its endpoint derived from
    /// these settings.
    pub fn data_service(&self) -> DataService {
        DataService {
            id: self.data_service_id.clone(),
            type_: "DataService".to_string(),
            endpoint_url: self.dsp_base_url(),
        }
    }
}

fn settings_graph() -> NamedNode {
    iri(SETTINGS_GRAPH.to_string())
}

fn write_data_service(w: &mut Writer, service: &DataService) -> Result<NamedNode, GraphError> {
    let node = NamedNode::new(&service.id)?;
    w.add_type(node.clone(), dcat("DataService"));
    w.add(
        node.clone(),
        dcat("endpointURL"),
        Literal::new_typed_literal(&service.endpoint_url, iri(xsd("anyURI"))),
    );
    Ok(node)
}

fn read_data_service(r: &Reader<'_>, node: &NamedNode) -> Result<DataService, GraphError> {
    Ok(DataService {
        id: node.as_str().to_string(),
        type_: "DataService".to_string(),
        endpoint_url: r.str(node.as_ref().into(), &dcat("endpointURL"))?,
    })
}

fn read_dataset(r: &Reader<'_>, node: &NamedNode) -> Result<Dataset, GraphError> {
    let subject: NamedOrBlankNodeRef<'_> = node.as_ref().into();
    let mut has_policy = Vec::new();
    for offer in r.ordered_children(subject, &odrl("hasPolicy"))? {
        has_policy.push(read_offer(r, offer.as_ref())?);
    }
    let mut distribution = Vec::new();
    for d in r.ordered_children(subject, &dcat("distribution"))? {
        let service = match r
            .objects(d.as_ref(), &dcat("accessService"))?
            .into_iter()
            .next()
        {
            Some(Term::NamedNode(n)) => n,
            _ => {
                return Err(GraphError::Missing {
                    node: d.to_string(),
                    property: dcat("accessService"),
                });
            }
        };
        distribution.push(Distribution {
            type_: "Distribution".to_string(),
            format: r.str(d.as_ref(), &dcterms("format"))?,
            access_service: read_data_service(r, &service)?,
        });
    }
    Ok(Dataset {
        id: node.as_str().to_string(),
        type_: "Dataset".to_string(),
        has_policy,
        distribution,
    })
}

/// Refuses to write `node` as a `class` record when it is the settings
/// node or already holds a record of another type: `replace` would
/// otherwise silently delete that record.
fn check_kind(r: &Reader<'_>, node: &NamedNode, class: &str) -> Result<(), GraphError> {
    let conflict = |reason: String| GraphError::Conflict {
        node: node.as_str().to_string(),
        kind: class.to_string(),
        reason,
    };
    if node.as_str() == SETTINGS_NODE {
        return Err(conflict("it is the connector settings node".to_string()));
    }
    if let Some(other) = r
        .types(node.as_ref().into())?
        .into_iter()
        .find(|t| t != class)
    {
        return Err(conflict(format!("it is already a <{other}>")));
    }
    Ok(())
}

impl ControlPlaneGraph {
    /// The stored connector settings, or `None` before any were written.
    pub fn settings(&self) -> Result<Option<ConnectorSettings>, GraphError> {
        let _guard = self.read_guard();
        let graph = settings_graph();
        let r = self.reader(&graph);
        let node = iri(SETTINGS_NODE.to_string());
        let subject: NamedOrBlankNodeRef<'_> = node.as_ref().into();
        if !r.has_type(subject, &dscp("ConnectorSettings"))? {
            return Ok(None);
        }
        Ok(Some(ConnectorSettings {
            agent_id: r.str(subject, &dspace("participantId"))?,
            base_url: r.str(subject, &dscp("baseUrl"))?,
            dsp_path: r.str(subject, &dscp("dspPath"))?,
            catalog_id: r.str(subject, &dscp("catalogId"))?,
            data_service_id: r.str(subject, &dcat("service"))?,
        }))
    }

    /// Stores `settings`, replacing any earlier ones, and (re)writes this
    /// connector's own data service node so every dataset distribution
    /// pointing at it picks up the new endpoint.
    pub fn put_settings(&self, settings: &ConnectorSettings) -> Result<(), GraphError> {
        let _guard = self.write_guard();
        let graph = settings_graph();
        let node = iri(SETTINGS_NODE.to_string());
        let mut w = Writer::new(&graph);
        w.add_type(node.clone(), dscp("ConnectorSettings"));
        w.add_str(node.clone(), dspace("participantId"), &settings.agent_id);
        w.add_str(node.clone(), dscp("baseUrl"), &settings.base_url);
        w.add_str(node.clone(), dscp("dspPath"), &settings.dsp_path);
        w.add_str(node.clone(), dscp("catalogId"), &settings.catalog_id);
        let service = NamedNode::new(&settings.data_service_id)?;
        check_kind(&self.reader(&graph), &service, &dcat("DataService"))?;
        w.add(node.clone(), dcat("service"), service.clone());

        let mut sw = Writer::new(&graph);
        write_data_service(&mut sw, &settings.data_service())?;
        self.replace_all(&graph, vec![(node, w.quads), (service, sw.quads)])
    }

    /// The stored data service `id`, if any.
    pub fn data_service(&self, id: &str) -> Result<Option<DataService>, GraphError> {
        let _guard = self.read_guard();
        let graph = settings_graph();
        let r = self.reader(&graph);
        let Some(node) = lookup_iri(id) else {
            return Ok(None);
        };
        if !r.has_type(node.as_ref().into(), &dcat("DataService"))? {
            return Ok(None);
        }
        read_data_service(&r, &node).map(Some)
    }

    /// Every catalog dataset in the settings graph, sorted by id.
    pub fn datasets(&self) -> Result<Vec<Dataset>, GraphError> {
        let _guard = self.read_guard();
        let graph = settings_graph();
        let r = self.reader(&graph);
        let mut nodes: Vec<NamedNode> = r
            .instances(&dcat("Dataset"))?
            .into_iter()
            .filter_map(|n| match n {
                NamedOrBlankNode::NamedNode(n) => Some(n),
                NamedOrBlankNode::BlankNode(_) => None,
            })
            .collect();
        nodes.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        nodes.iter().map(|n| read_dataset(&r, n)).collect()
    }

    /// One catalog dataset by `@id`, or `None` if there is no such dataset
    /// (including when `id` is not a valid IRI).
    pub fn dataset(&self, id: &str) -> Result<Option<Dataset>, GraphError> {
        let _guard = self.read_guard();
        let graph = settings_graph();
        let r = self.reader(&graph);
        let Some(node) = lookup_iri(id) else {
            return Ok(None);
        };
        if !r.has_type(node.as_ref().into(), &dcat("Dataset"))? {
            return Ok(None);
        }
        read_dataset(&r, &node).map(Some)
    }

    /// Adds `dataset` to the catalog, replacing a dataset with the same id.
    /// The data services its distributions embed are written too, all in
    /// one transaction. Fails with [`GraphError::Conflict`], writing
    /// nothing, when the dataset id or a data service id names the settings
    /// node or a record of another kind, or a data service id equals the
    /// dataset's own, or a data service already stored (or named earlier in
    /// the same dataset) with a different endpoint: data service nodes are
    /// shared, so a push only adds new ones and never moves an existing
    /// endpoint. The connector's own service is written by
    /// [`Self::put_settings`] alone.
    pub fn put_dataset(&self, dataset: &Dataset) -> Result<(), GraphError> {
        let _guard = self.write_guard();
        let graph = settings_graph();
        let node = NamedNode::new(&dataset.id)?;
        let r = self.reader(&graph);
        check_kind(&r, &node, &dcat("Dataset"))?;
        let mut w = Writer::new(&graph);
        w.add_type(node.clone(), dcat("Dataset"));
        for (i, offer) in dataset.has_policy.iter().enumerate() {
            write_offer(&mut w, node.clone(), odrl("hasPolicy"), i, offer);
        }
        let mut records = Vec::new();
        let mut pushed: HashMap<NamedNode, String> = HashMap::new();
        for (i, d) in dataset.distribution.iter().enumerate() {
            let child = w.add_child(node.clone(), dcat("distribution"), i);
            w.add_type(child.clone(), dcat("Distribution"));
            w.add_str(child.clone(), dcterms("format"), &d.format);
            let mut sw = Writer::new(&graph);
            let service = write_data_service(&mut sw, &d.access_service)?;
            if service == node {
                return Err(GraphError::Conflict {
                    node: service.as_str().to_string(),
                    kind: dcat("DataService"),
                    reason: "it is the dataset's own id".to_string(),
                });
            }
            check_kind(&r, &service, &dcat("DataService"))?;
            w.add(child, dcat("accessService"), service.clone());
            let endpoint_conflict = |existing: &str| GraphError::Conflict {
                node: service.as_str().to_string(),
                kind: dcat("DataService"),
                reason: format!(
                    "it already has endpoint {existing}, not {}",
                    d.access_service.endpoint_url
                ),
            };
            // A service node is shared by every distribution naming it, so
            // a dataset push only ever adds new ones: rewriting an existing
            // one (the connector's own is written by `put_settings`) would
            // move the endpoint of every other dataset too.
            if r.has_type(service.as_ref().into(), &dcat("DataService"))? {
                let existing = read_data_service(&r, &service)?;
                if existing.endpoint_url != d.access_service.endpoint_url {
                    return Err(endpoint_conflict(&existing.endpoint_url));
                }
                continue;
            }
            if let Some(existing) = pushed.get(&service) {
                if *existing != d.access_service.endpoint_url {
                    return Err(endpoint_conflict(existing));
                }
                continue;
            }
            pushed.insert(service.clone(), d.access_service.endpoint_url.clone());
            records.push((service, sw.quads));
        }
        records.insert(0, (node, w.quads));
        self.replace_all(&graph, records)
    }

    /// Removes the dataset `id` from the catalog. Returns whether it existed.
    pub fn remove_dataset(&self, id: &str) -> Result<bool, GraphError> {
        let _guard = self.write_guard();
        let graph = settings_graph();
        let Some(node) = lookup_iri(id) else {
            return Ok(false);
        };
        if !self
            .reader(&graph)
            .has_type(node.as_ref().into(), &dcat("Dataset"))?
        {
            return Ok(false);
        }
        self.replace(&graph, &node, Vec::new())?;
        Ok(true)
    }
}
