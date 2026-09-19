//! Domain types and state machines for a Dataspace Protocol (DSP) connector
//! control plane: the DCAT/ODRL-flavored catalog vocabulary, and the
//! `ContractNegotiation`/`TransferProcess` state machines defined by the
//! [Dataspace Protocol Specification](https://eclipse-dataspace-protocol-base.github.io/DataspaceProtocol)
//! (current TCK-supported version: `2025-1`).
//!
//! This crate has no HTTP/JSON-LD-transport opinions of its own beyond
//! `serde` (de)serialization of the wire message shapes — see
//! `control-plane-http` for the axum server that exposes them as real DSP
//! endpoints.
//!
//! Scope note (see `../../ARCHITECTURE.md` for the full picture): this is a
//! bootstrap. The message *shapes* here are real, spec-correct DSP
//! JSON-LD-ish envelopes (`@context`/`@type`/`dspace:`-flavored property
//! names), and the state machines encode the transitions the spec's prose
//! describes. What is deliberately not attempted yet: JSON-LD processing
//! (expansion/compaction against the real context document — messages are
//! read and written as plain, pre-compacted JSON matching what the context
//! would compact *to*), full ODRL constraint semantics beyond the flat
//! `leftOperand`/`operator`/`rightOperand` shape, and consumer-role message
//! flows.
pub mod catalog;
pub mod common;
pub mod negotiation;
pub mod transfer;

pub use common::{
    Agreement, Constraint, DSP_CONTEXT_URL, DataAddress, EndpointProperty, Offer, Rule, dsp_context,
};
