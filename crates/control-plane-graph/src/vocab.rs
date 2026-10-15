//! RDF namespace IRIs this store writes: the real Dataspace Protocol
//! `2025-1` vocabulary (the namespace its JSON-LD context compacts
//! `providerPid`, `state`, `REQUESTED`, … from), DCAT/Dublin Core for the
//! catalog, ODRL for policies, plus this project's own small vocabulary for
//! the facts no standard term covers (connector settings, `order`).
//!
//! `dscp:order` exists for the same reason `ds-sql-dps-rs/config-graph`'s
//! and `ds-identity-hub-rs/identity-hub-graph`'s `ds:order` do: ODRL's
//! `permission`/`prohibition`/`obligation`, a rule's `constraint`, a
//! dataset's `hasPolicy`/`distribution` are *ordered* JSON arrays, but
//! plain RDF triples for a multi-valued property carry no order at all.

pub const DSPACE: &str = "https://w3id.org/dspace/2025/1/";
pub const DCAT: &str = "http://www.w3.org/ns/dcat#";
pub const DCTERMS: &str = "http://purl.org/dc/terms/";
pub const ODRL: &str = "http://www.w3.org/ns/odrl/2/";
pub const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
pub const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

/// This project's own vocabulary: connector settings with no DSP/DCAT
/// term, and `order`.
pub const DSCP: &str = "https://ds42.org/ontologies/ds-control-plane-rs#";

/// Named graph holding the connector's settings: its identity and the
/// catalog it serves. Kept apart from [`RUNTIME_GRAPH`] by graph name
/// inside one store, the same split Contreforts' own `CONFIG_GRAPH` makes
/// between configuration and synced data.
pub const SETTINGS_GRAPH: &str = "urn:ds-control-plane-rs:graph:settings";

/// Named graph holding runtime state: contract negotiations and transfer
/// processes.
pub const RUNTIME_GRAPH: &str = "urn:ds-control-plane-rs:graph:runtime";

/// The single node the connector's settings hang off.
pub const SETTINGS_NODE: &str = "urn:ds-control-plane-rs:settings";

pub fn dspace(term: &str) -> String {
    format!("{DSPACE}{term}")
}

pub fn dcat(term: &str) -> String {
    format!("{DCAT}{term}")
}

pub fn dcterms(term: &str) -> String {
    format!("{DCTERMS}{term}")
}

pub fn odrl(term: &str) -> String {
    format!("{ODRL}{term}")
}

pub fn rdf(term: &str) -> String {
    format!("{RDF}{term}")
}

pub fn xsd(term: &str) -> String {
    format!("{XSD}{term}")
}

pub fn dscp(term: &str) -> String {
    format!("{DSCP}{term}")
}
