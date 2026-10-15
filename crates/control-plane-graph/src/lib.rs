//! The RDF store behind `ds-control-plane-rs`: an embedded Oxigraph store
//! holding the connector's **settings** (its identity and the catalog
//! datasets/ODRL offers it serves) and its **runtime state** (contract
//! negotiations and transfer processes) as real triples in two named
//! graphs, optionally persisted to disk, and reachable through Contreforts'
//! connector interface via the sibling `control-plane-contreforts` crate.
//! See `../../ARCHITECTURE.md`, "Contreforts: settings and runtime
//! persistence".

mod policy;
mod runtime;
mod settings;
mod store;
pub mod vocab;

pub use runtime::{MAX_NEGOTIATIONS, MAX_TRANSFERS, Record};
pub use settings::ConnectorSettings;
pub use store::{ControlPlaneGraph, GraphError};

#[cfg(test)]
mod tests;
