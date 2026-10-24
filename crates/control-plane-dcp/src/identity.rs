//! The control plane's own DCP identity: a `did:web` DID, its P-256
//! signing key and the DID document that publishes the key (listed under
//! `capabilityInvocation`, so a Credential Service accepts the tokens it
//! signs).

use std::path::Path;

use dcp_core::{DcpKeyPair, b64_decode, now_secs, sign_jws};
use p256::ecdsa::{SigningKey, VerifyingKey};
use serde_json::{Value, json};

use crate::DcpError;

/// How long a token this connector mints stays valid.
pub const TOKEN_TTL_SECS: u64 = 300;

/// The fragment of this connector's key id: `<did>#dsp-key`, as
/// `DcpKeyPair::generate` names it.
const KEY_FRAGMENT: &str = "dsp-key";

#[derive(Debug, Clone)]
pub struct Identity {
    key_pair: DcpKeyPair,
}

impl Identity {
    /// A fresh random key for `did`.
    pub fn generate(did: String) -> Self {
        Self {
            key_pair: DcpKeyPair::generate(did),
        }
    }

    /// `did` with `key`.
    pub fn from_signing_key(did: String, key: &SigningKey) -> Self {
        let point = VerifyingKey::from(key).to_encoded_point(false);
        let x: [u8; 32] = (*point.x().expect("uncompressed point has x")).into();
        let y: [u8; 32] = (*point.y().expect("uncompressed point has y")).into();
        Self {
            key_pair: DcpKeyPair {
                own_key_id: format!("{did}#{KEY_FRAGMENT}"),
                own_did: did,
                signing_key_bytes: key.to_bytes().into(),
                public_key_xy: (x, y),
            },
        }
    }

    /// `did` with the P-256 private key in `path`: a PKCS#8 (`PRIVATE
    /// KEY`) or SEC1 (`EC PRIVATE KEY`) PEM, or an EC JWK with `d` (whose
    /// `x`/`y`, if present, must match it).
    pub fn from_key_file(did: String, path: &Path) -> Result<Self, DcpError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| DcpError::Key(format!("cannot read {}: {e}", path.display())))?;
        let key =
            parse_key(&text).map_err(|e| DcpError::Key(format!("{}: {e}", path.display())))?;
        Ok(Self::from_signing_key(did, &key))
    }

    pub fn did(&self) -> &str {
        &self.key_pair.own_did
    }

    pub fn key_id(&self) -> &str {
        &self.key_pair.own_key_id
    }

    pub fn verifying_key(&self) -> VerifyingKey {
        VerifyingKey::from(&self.key_pair.signing_key())
    }

    /// This connector's DID document: one `JsonWebKey2020` verification
    /// method, referenced by `authentication`, `assertionMethod` and
    /// `capabilityInvocation`, and no services.
    pub fn did_document(&self) -> Value {
        self.key_pair.did_document(&[])
    }

    /// A Self-Issued ID Token from this connector to `audience`
    /// (`iss` = `sub` = own DID), valid for [`TOKEN_TTL_SECS`], carrying
    /// `nested_token` as its `token` claim when given.
    pub fn mint_self_issued_token(&self, audience: &str, nested_token: Option<&str>) -> String {
        let now = now_secs();
        let mut claims = json!({
            "iss": self.did(),
            "sub": self.did(),
            "aud": audience,
            "iat": now,
            "nbf": now,
            "exp": now + TOKEN_TTL_SECS,
            "jti": uuid::Uuid::new_v4().to_string(),
        });
        if let Some(token) = nested_token {
            claims["token"] = json!(token);
        }
        sign_jws(&claims, &self.key_pair.signing_key(), self.key_id())
    }
}

fn parse_key(text: &str) -> Result<SigningKey, String> {
    let text = text.trim();
    if text.starts_with("-----BEGIN") {
        use p256::pkcs8::DecodePrivateKey;
        let secret = p256::SecretKey::from_pkcs8_pem(text)
            .or_else(|_| p256::SecretKey::from_sec1_pem(text))
            .map_err(|e| format!("not a P-256 PKCS#8 or SEC1 private key PEM: {e}"))?;
        return Ok(SigningKey::from(&secret));
    }
    let jwk: Value =
        serde_json::from_str(text).map_err(|e| format!("neither a PEM nor a JSON JWK: {e}"))?;
    if jwk.get("kty").and_then(Value::as_str) != Some("EC")
        || jwk.get("crv").and_then(Value::as_str) != Some("P-256")
    {
        return Err("the JWK must have kty EC and crv P-256".to_string());
    }
    let d = jwk
        .get("d")
        .and_then(Value::as_str)
        .ok_or("the JWK has no private key `d`")?;
    let d = b64_decode(d)?;
    let key = SigningKey::from_slice(&d).map_err(|e| format!("invalid JWK `d`: {e}"))?;
    let point = VerifyingKey::from(&key).to_encoded_point(false);
    for (name, coordinate) in [("x", point.x()), ("y", point.y())] {
        if let Some(given) = jwk.get(name).and_then(Value::as_str) {
            let given = b64_decode(given)?;
            if Some(given.as_slice()) != coordinate.map(|c| &c[..]) {
                return Err(format!("the JWK's `{name}` does not match its `d`"));
            }
        }
    }
    Ok(key)
}

/// The URL path a `did:web` DID resolves to: `did:web:host:a:b` ->
/// `/a/b/did.json`, `did:web:host` -> `/.well-known/did.json`. The control
/// plane serves its own DID document there.
pub fn did_web_path(did: &str) -> Result<String, String> {
    let host = did
        .strip_prefix("did:web:")
        .ok_or_else(|| format!("{did} is not a did:web DID"))?
        .split(':')
        .next()
        .unwrap_or_default();
    if host.is_empty() {
        return Err(format!("{did} has no host"));
    }
    let url = dcp_core::did_web_to_url(did, false)?;
    let parsed = reqwest::Url::parse(&url).map_err(|e| format!("{did}: {e}"))?;
    Ok(parsed.path().to_string())
}
