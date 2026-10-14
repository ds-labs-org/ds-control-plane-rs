//! Holder-side [OpenID for Verifiable Credential Issuance 1.0](https://openid.net/specs/openid-4-verifiable-credential-issuance-1_0.html)
//! (OID4VCI) for the connector control plane: lets the connector accept a
//! Credential Offer and obtain key-bound credentials from an issuer using
//! the Pre-Authorized Code flow.
//!
//! Kept as a thin, in-repo client rather than a dependency on an existing
//! Rust OID4VCI library: at the time of writing none of the published ones
//! was both 1.0-conformant on the wire and light enough to pull into a
//! control plane (see `../../ARCHITECTURE.md`, "OID4VCI holder").
//!
//! - [`offer`]: parsing `openid-credential-offer://` URIs and well-known
//!   URL construction.
//! - [`types`]: the 1.0 wire types.
//! - [`proof`]: the holder's P-256 key and `openid4vci-proof+jwt` proofs.
//! - [`wallet`]: the HTTP flow itself.

pub mod error;
pub mod offer;
pub mod proof;
pub mod types;
pub mod wallet;

pub use error::Oid4vciError;
pub use offer::OfferSource;
pub use proof::HolderKey;
pub use wallet::{
    DeferredIssuance, DeferredPoll, IssuanceRequest, IssuanceResult, IssuedCredentials, Wallet,
};
