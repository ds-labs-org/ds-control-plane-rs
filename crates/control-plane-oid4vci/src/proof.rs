//! The holder's key and the `openid4vci-proof+jwt` key proof (spec
//! Appendix F.1) that binds an issued credential to it.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::elliptic_curve::JwkEcKey;
use serde_json::json;

use crate::error::Oid4vciError;
use crate::types::PROOF_JWT_TYP;

/// The P-256 key the control plane binds requested credentials to. Proofs
/// carry its public JWK in the `jwk` header, which every OID4VCI issuer
/// that supports `jwk` binding can verify without resolving a DID.
#[derive(Clone)]
pub struct HolderKey {
    signing_key: SigningKey,
}

impl std::fmt::Debug for HolderKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HolderKey")
            .field("did", &self.did_jwk())
            .finish_non_exhaustive()
    }
}

impl HolderKey {
    /// A fresh random key.
    pub fn generate() -> Self {
        Self {
            signing_key: SigningKey::random(&mut rand_core::OsRng),
        }
    }

    /// Loads a key from a private EC JWK (`{"kty":"EC","crv":"P-256","x":..,"y":..,"d":..}`).
    pub fn from_private_jwk(jwk: &str) -> Result<Self, Oid4vciError> {
        let secret = p256::SecretKey::from_jwk_str(jwk)
            .map_err(|e| Oid4vciError::Proof(format!("holder JWK: {e}")))?;
        Ok(Self {
            signing_key: SigningKey::from(secret),
        })
    }

    /// The public half, as a JWK.
    pub fn public_jwk(&self) -> JwkEcKey {
        p256::PublicKey::from(self.signing_key.verifying_key()).to_jwk()
    }

    /// The `did:jwk` identifier of the public key — informational, so
    /// operators can tell which key credentials were bound to.
    pub fn did_jwk(&self) -> String {
        let jwk = self.public_jwk().to_string();
        format!("did:jwk:{}", URL_SAFE_NO_PAD.encode(jwk))
    }

    /// Builds and signs a key proof JWT for `audience` (the Credential
    /// Issuer Identifier) over `nonce`. `client_id` becomes `iss` when the
    /// wallet authenticated as a client; for anonymous Pre-Authorized Code
    /// access the spec says to omit it.
    pub fn proof_jwt(
        &self,
        audience: &str,
        nonce: Option<&str>,
        client_id: Option<&str>,
    ) -> Result<String, Oid4vciError> {
        let jwk: serde_json::Value = serde_json::from_str(&self.public_jwk().to_string())
            .map_err(|e| Oid4vciError::Proof(e.to_string()))?;
        let header = json!({ "typ": PROOF_JWT_TYP, "alg": "ES256", "jwk": jwk });

        let mut claims = serde_json::Map::new();
        if let Some(iss) = client_id {
            claims.insert("iss".into(), iss.into());
        }
        claims.insert("aud".into(), audience.into());
        claims.insert("iat".into(), chrono::Utc::now().timestamp().into());
        if let Some(nonce) = nonce {
            claims.insert("nonce".into(), nonce.into());
        }

        let signing_input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(serde_json::Value::Object(claims).to_string())
        );
        let signature: Signature = self.signing_key.sign(signing_input.as_bytes());
        Ok(format!(
            "{signing_input}.{}",
            URL_SAFE_NO_PAD.encode(signature.to_bytes())
        ))
    }
}

#[cfg(test)]
mod tests {
    use p256::ecdsa::VerifyingKey;
    use p256::ecdsa::signature::Verifier;

    use super::*;

    fn decode(part: &str) -> serde_json::Value {
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(part).unwrap()).unwrap()
    }

    #[test]
    fn proof_jwt_verifies_against_its_own_jwk_header() {
        let key = HolderKey::generate();
        let jwt = key
            .proof_jwt("https://issuer.example", Some("n-0S6_WzA2Mj"), None)
            .unwrap();
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);

        let header = decode(parts[0]);
        assert_eq!(header["typ"], PROOF_JWT_TYP);
        assert_eq!(header["alg"], "ES256");
        assert!(header.get("kid").is_none());

        let claims = decode(parts[1]);
        assert_eq!(claims["aud"], "https://issuer.example");
        assert_eq!(claims["nonce"], "n-0S6_WzA2Mj");
        assert!(claims.get("iss").is_none());
        assert!(claims["iat"].is_i64());

        let jwk = JwkEcKey::from_str(&header["jwk"].to_string()).unwrap();
        let vk = VerifyingKey::from(p256::PublicKey::from_jwk(&jwk).unwrap());
        let sig = Signature::from_slice(&URL_SAFE_NO_PAD.decode(parts[2]).unwrap()).unwrap();
        vk.verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &sig)
            .unwrap();
    }

    #[test]
    fn private_jwk_round_trips() {
        let key = HolderKey::generate();
        let private = p256::SecretKey::from(key.signing_key.clone()).to_jwk_string();
        let loaded = HolderKey::from_private_jwk(&private).unwrap();
        assert_eq!(loaded.did_jwk(), key.did_jwk());
        assert!(key.did_jwk().starts_with("did:jwk:"));
    }

    use std::str::FromStr;
}
