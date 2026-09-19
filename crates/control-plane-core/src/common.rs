//! Shared DSP wire types: the JSON-LD context constant, ODRL-flavored
//! rule/constraint/offer/agreement shapes, and the transport-neutral
//! `DataAddress`/`EndpointProperty` pair used by the transfer process
//! protocol. See the spec's
//! [Lower Level Type Definitions](https://eclipse-dataspace-protocol-base.github.io/DataspaceProtocol/HEAD/#lower-level-type-definitions).

use serde::{Deserialize, Serialize};

/// The `2025-1` Dataspace Protocol JSON-LD context, as used verbatim by
/// every example message in the spec's own `artifacts/` fixtures (e.g.
/// `catalog/example/catalog-request-message.json`).
pub const DSP_CONTEXT_URL: &str = "https://w3id.org/dspace/2025/1/context.jsonld";

/// Default value for every message's `@context` field.
pub fn dsp_context() -> Vec<String> {
    vec![DSP_CONTEXT_URL.to_string()]
}

/// An ODRL atomic constraint: `leftOperand operator rightOperand`, e.g.
/// `spatial eq http://example.org/EU`. This bootstrap only models the flat,
/// atomic shape (see the spec's `dataset.json` example) — logical
/// constraint groups (`odrl:and`/`odrl:or`/`odrl:xone`/`odrl:andSequence`)
/// are out of scope for this pass.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Constraint {
    #[serde(rename = "leftOperand")]
    pub left_operand: String,
    pub operator: String,
    #[serde(rename = "rightOperand")]
    pub right_operand: serde_json::Value,
}

/// A single ODRL rule (used as a `permission`, `prohibition`, or
/// `obligation` entry): an `action` plus zero or more `constraint`s.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Rule {
    pub action: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraint: Vec<Constraint>,
}

fn offer_type() -> String {
    "Offer".to_string()
}

/// An ODRL `Offer`: the policy shape carried in a `Dataset.hasPolicy`, and
/// as the `offer` property of a `ContractRequestMessage`/
/// `ContractOfferMessage`.
///
/// Per the spec: an `Offer` MUST have a unique `@id`; it MUST NOT carry a
/// `target` when nested in a `Catalog`/`Dataset`, but MUST carry one when
/// used inside a `ContractRequestMessage`/`ContractOfferMessage` — this
/// type does not enforce that distinction itself (both are the same Rust
/// type with an `Option<String>` target), callers are responsible for it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Offer {
    #[serde(rename = "@id")]
    pub id: String,
    #[serde(rename = "@type", default = "offer_type")]
    pub type_: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permission: Vec<Rule>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prohibition: Vec<Rule>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub obligation: Vec<Rule>,
}

fn agreement_type() -> String {
    "Agreement".to_string()
}

/// An ODRL `Agreement`, as carried by a `ContractAgreementMessage`. Per the
/// spec, MUST have `target`, `timestamp`, `assigner`, and `assignee`; its
/// rules MUST NOT carry their own `target`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Agreement {
    #[serde(rename = "@id")]
    pub id: String,
    #[serde(rename = "@type", default = "agreement_type")]
    pub type_: String,
    pub target: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub assigner: String,
    pub assignee: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permission: Vec<Rule>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prohibition: Vec<Rule>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub obligation: Vec<Rule>,
}

fn endpoint_property_type() -> String {
    "EndpointProperty".to_string()
}

/// A single named property of a `DataAddress` (e.g. `authorization`,
/// `authType`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EndpointProperty {
    #[serde(rename = "@type", default = "endpoint_property_type")]
    pub type_: String,
    pub name: String,
    pub value: String,
}

fn data_address_type() -> String {
    "DataAddress".to_string()
}

/// A transport-specific data plane endpoint, carried by a
/// `TransferRequestMessage` (push) or `TransferStartMessage` (pull).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DataAddress {
    #[serde(rename = "@type", default = "data_address_type")]
    pub type_: String,
    #[serde(rename = "endpointType")]
    pub endpoint_type: String,
    pub endpoint: String,
    #[serde(
        rename = "endpointProperties",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    pub endpoint_properties: Vec<EndpointProperty>,
}

impl DataAddress {
    /// Looks up a named endpoint property's value (e.g. `"authorization"`).
    pub fn property(&self, name: &str) -> Option<&str> {
        self.endpoint_properties
            .iter()
            .find(|p| p.name == name)
            .map(|p| p.value.as_str())
    }
}
