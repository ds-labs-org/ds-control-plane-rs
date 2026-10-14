use crate::types::ErrorResponse;

#[derive(Debug, thiserror::Error)]
pub enum Oid4vciError {
    #[error("invalid credential offer: {0}")]
    InvalidOffer(String),
    #[error("invalid issuer metadata: {0}")]
    InvalidMetadata(String),
    #[error(
        "credential offer has no pre-authorized code grant (authorization code flow is not supported)"
    )]
    UnsupportedGrant,
    #[error("issuer requires a transaction code (tx_code) but none was supplied")]
    TxCodeRequired,
    #[error("credential configuration '{0}' is not offered or not advertised by the issuer")]
    UnknownCredentialConfiguration(String),
    #[error("issuer rejected the request at {endpoint} (HTTP {status}): {}", .body.as_ref().map(|b| b.error.as_str()).unwrap_or("no error body"))]
    Issuer {
        endpoint: String,
        status: u16,
        body: Option<ErrorResponse>,
    },
    #[error("HTTP error talking to {endpoint}: {source}")]
    Http {
        endpoint: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("key proof signing failed: {0}")]
    Proof(String),
}
