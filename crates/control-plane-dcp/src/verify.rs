//! Checking a counterparty: its Self-Issued ID Token, then the presentation
//! its Credential Service returns and the credentials in it.

use std::sync::Arc;

use dcp_core::{
    DidDocument, PresentationResponseMessage, decode_jws_unverified, find_verifying_key, now_secs,
    resolve_did, service_endpoint_url, verify_jws_signature,
};
use serde_json::{Value, json};

use crate::{Dcp, DcpError};

/// How far ahead of this clock an `nbf`/`iat` may be (clock skew).
const LEEWAY_SECS: u64 = 30;

/// The largest Credential Service response the verifier reads.
const MAX_PRESENTATION_RESPONSE_BYTES: usize = 1024 * 1024;

/// The DCP JSON-LD context of a `PresentationQueryMessage`.
const DCP_CONTEXT: &str = "https://w3id.org/dspace-dcp/v1.0/dcp.jsonld";

const VC_TYPE_SCOPE_PREFIX: &str = "org.eclipse.dspace.dcp.vc.type:";

/// A Self-Issued ID Token that passed every check.
#[derive(Debug, Clone)]
pub struct VerifiedToken {
    /// The caller's DID (`iss` = `sub`).
    pub participant_did: String,
    /// The `token` claim: the access token for the caller's Credential
    /// Service, forwarded as is.
    pub access_token: Option<String>,
    /// The whole verified payload.
    pub claims: Value,
    /// The caller's resolved DID document.
    document: Arc<DidDocument>,
}

/// A credential that passed every check.
#[derive(Debug, Clone)]
pub struct VerifiedCredential {
    pub id: Option<String>,
    pub issuer: String,
    pub types: Vec<String>,
    /// The verified JWT payload.
    pub claims: Value,
}

/// What [`check_credential_claims`] reads off a JWT VC payload.
#[derive(Debug, Clone)]
pub struct CredentialClaims {
    pub id: Option<String>,
    pub issuer: String,
    pub types: Vec<String>,
}

/// The credential type a scope `org.eclipse.dspace.dcp.vc.type:<Type>[:<suffix>]`
/// names, the `ds-identity-hub-rs`/dcp-tck default scope alias.
pub fn credential_type_of_scope(scope: &str) -> Option<String> {
    let rest = scope.strip_prefix(VC_TYPE_SCOPE_PREFIX)?;
    let ty = rest.split(':').next()?;
    (!ty.is_empty()).then(|| ty.to_string())
}

fn str_claim<'a>(payload: &'a Value, name: &str) -> Option<&'a str> {
    payload.get(name).and_then(Value::as_str)
}

/// Whether the JWT `aud` (a string or a list) names `did`.
fn audience_names(payload: &Value, did: &str) -> bool {
    match payload.get("aud") {
        Some(Value::String(aud)) => aud == did,
        Some(Value::Array(auds)) => auds.iter().any(|a| a.as_str() == Some(did)),
        _ => false,
    }
}

/// `exp` (required), `nbf` and `iat` against `now`.
fn check_times(payload: &Value, now: u64) -> Result<(), DcpError> {
    let exp = payload.get("exp").and_then(Value::as_u64).unwrap_or(0);
    if exp <= now {
        return Err(DcpError::Expired);
    }
    if let Some(nbf) = payload.get("nbf").and_then(Value::as_u64)
        && nbf > now + LEEWAY_SECS
    {
        return Err(DcpError::NotYetValid);
    }
    if let Some(iat) = payload.get("iat").and_then(Value::as_u64)
        && iat > now + LEEWAY_SECS
    {
        return Err(DcpError::IssuedInFuture);
    }
    Ok(())
}

/// Checks a JWT VC payload's claims, all but its signature and issuer
/// trust: the subject (`sub` and `vc.credentialSubject.id`, every one
/// present, at least one) is `participant`; it has not expired (`exp`,
/// `vc.expirationDate`) and is already valid (`nbf`, `vc.issuanceDate`);
/// `iss` and `vc.issuer` agree. Returns its issuer, types and id.
pub fn check_credential_claims(
    payload: &Value,
    participant: &str,
    now: u64,
) -> Result<CredentialClaims, DcpError> {
    let invalid = |m: &str| DcpError::InvalidCredential(m.to_string());
    let vc = payload
        .get("vc")
        .filter(|v| v.is_object())
        .ok_or_else(|| invalid("the JWT has no `vc` claim"))?;

    let vc_issuer = match vc.get("issuer") {
        Some(Value::String(s)) => Some(s.as_str()),
        Some(Value::Object(o)) => o.get("id").and_then(Value::as_str),
        _ => None,
    };
    let issuer = match (str_claim(payload, "iss"), vc_issuer) {
        (Some(a), Some(b)) if a != b => return Err(invalid("`iss` and `vc.issuer` differ")),
        (Some(a), _) => a,
        (None, Some(b)) => b,
        (None, None) => return Err(invalid("the credential names no issuer")),
    };

    let subject_ids: Vec<Option<&str>> = match vc.get("credentialSubject") {
        Some(Value::Array(subjects)) => subjects
            .iter()
            .map(|s| s.get("id").and_then(Value::as_str))
            .collect(),
        Some(subject) => vec![subject.get("id").and_then(Value::as_str)],
        None => vec![],
    };
    let subjects: Vec<&str> = str_claim(payload, "sub")
        .into_iter()
        .chain(subject_ids.iter().flatten().copied())
        .collect();
    if subjects.is_empty() || subjects.iter().any(|s| *s != participant) {
        return Err(DcpError::CredentialSubjectMismatch);
    }

    if let Some(exp) = payload.get("exp").and_then(Value::as_u64)
        && exp <= now
    {
        return Err(DcpError::CredentialExpired);
    }
    let date = |name: &str| -> Result<Option<u64>, DcpError> {
        match vc.get(name).and_then(Value::as_str) {
            None => Ok(None),
            Some(s) => chrono::DateTime::parse_from_rfc3339(s)
                .map(|d| Some(d.timestamp().max(0) as u64))
                .map_err(|e| DcpError::InvalidCredential(format!("`{name}`: {e}"))),
        }
    };
    if let Some(expiration) = date("expirationDate")?
        && expiration <= now
    {
        return Err(DcpError::CredentialExpired);
    }
    let not_before = [
        payload.get("nbf").and_then(Value::as_u64),
        date("issuanceDate")?,
        date("validFrom")?,
    ];
    if not_before
        .into_iter()
        .flatten()
        .any(|t| t > now + LEEWAY_SECS)
    {
        return Err(invalid("the credential is not valid yet"));
    }

    let types = match vc.get("type") {
        Some(Value::String(t)) => vec![t.clone()],
        Some(Value::Array(ts)) => ts
            .iter()
            .filter_map(Value::as_str)
            .map(String::from)
            .collect(),
        _ => vec![],
    };
    let id = vc
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| str_claim(payload, "jti"))
        .map(String::from);
    Ok(CredentialClaims {
        id,
        issuer: issuer.to_string(),
        types,
    })
}

impl Dcp {
    fn check_did(&self, did: &str) -> Result<(), DcpError> {
        self.outbound
            .check_did(did, self.config.insecure_http)
            .map_err(|e| DcpError::OutboundDestinationNotAllowed(e.to_string()))
    }

    async fn resolve(&self, did: &str) -> Result<DidDocument, DcpError> {
        self.check_did(did)?;
        resolve_did(&self.http, did, self.config.insecure_http)
            .await
            .map_err(DcpError::DidResolution)
    }

    /// Verifies an `Authorization` header carrying a Self-Issued ID Token,
    /// as `ds-identity-hub-rs`'s `verify_bearer_token` does: resolves the
    /// `iss` DID (its host checked against the allow-list first), verifies
    /// the signature with the key the `kid` names, which must be listed
    /// under `capabilityInvocation`, then `sub` = `iss`, `aud` names this
    /// connector, `exp`, `nbf`/`iat` (30 s leeway), and `jti`, which is
    /// required and must not have been seen before.
    pub async fn verify_self_issued_token(
        &self,
        authorization: Option<&str>,
    ) -> Result<VerifiedToken, DcpError> {
        let token = authorization
            .and_then(|h| h.strip_prefix("Bearer "))
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .ok_or(DcpError::MissingToken)?;
        let (_, header, payload) = decode_jws_unverified(token).map_err(DcpError::Malformed)?;
        if header.get("alg").and_then(Value::as_str) != Some("ES256") {
            return Err(DcpError::Malformed("alg must be ES256".to_string()));
        }
        let caller = str_claim(&payload, "iss")
            .ok_or_else(|| DcpError::Malformed("token has no iss".to_string()))?
            .to_string();
        let kid = str_claim(&header, "kid")
            .ok_or_else(|| DcpError::Malformed("token has no kid".to_string()))?;

        let document = self.resolve(&caller).await?;
        let key = find_verifying_key(&document, kid).map_err(DcpError::InvalidSignature)?;
        verify_jws_signature(token, &key).map_err(DcpError::InvalidSignature)?;
        if !document.capability_invocation.iter().any(|id| id == kid) {
            return Err(DcpError::KeyNotAuthorizedForInvocation(kid.to_string()));
        }
        if str_claim(&payload, "sub") != Some(caller.as_str()) {
            return Err(DcpError::IssuerSubjectMismatch);
        }
        if !audience_names(&payload, self.did()) {
            return Err(DcpError::WrongAudience);
        }
        check_times(&payload, now_secs())?;
        let jti = str_claim(&payload, "jti").ok_or(DcpError::MissingJti)?;
        if !self
            .seen_jti
            .lock()
            .expect("jti cache lock poisoned")
            .insert(jti)
        {
            return Err(DcpError::TokenReplayed);
        }
        Ok(VerifiedToken {
            participant_did: caller,
            access_token: str_claim(&payload, "token").map(String::from),
            claims: payload,
            document: Arc::new(document),
        })
    }

    /// Fetches and verifies the credentials `verified`'s participant holds
    /// for `required_scopes`: queries `{CredentialService}/presentations/query`
    /// (the endpoint from its DID document, on the allow-list, `https`
    /// unless `insecure_http`) with this connector's own Self-Issued ID
    /// Token (`aud` the participant, the participant's access token as its
    /// `token`), then checks every presentation (signed by a key of the
    /// participant's DID, `iss` the participant, `aud` this connector, not
    /// expired) and every credential in it (signed by a key of its
    /// issuer's DID, the issuer trusted, the subject the participant, not
    /// expired). Any credential that fails is a failure of the whole call.
    /// Finally every type the scopes name must be among them.
    pub async fn verify_credentials(
        &self,
        verified: &VerifiedToken,
        required_scopes: &[String],
    ) -> Result<Vec<VerifiedCredential>, DcpError> {
        let participant = verified.participant_did.as_str();
        let access_token = verified
            .access_token
            .as_deref()
            .ok_or(DcpError::NoAccessToken)?;
        let endpoint = service_endpoint_url(&verified.document, "CredentialService")
            .map_err(DcpError::CredentialService)?;
        let url = format!("{}/presentations/query", endpoint.trim_end_matches('/'));
        if !self.config.insecure_http && !url.starts_with("https://") {
            return Err(DcpError::OutboundDestinationNotAllowed(format!(
                "Credential Service endpoint '{endpoint}' is not https"
            )));
        }
        self.outbound
            .check_url(&url)
            .map_err(|e| DcpError::OutboundDestinationNotAllowed(e.to_string()))?;

        let bearer = self
            .identity
            .mint_self_issued_token(participant, Some(access_token));
        let query = json!({
            "@context": [DCP_CONTEXT],
            "type": "PresentationQueryMessage",
            "scope": required_scopes,
        });
        let mut response = self
            .http
            .post(&url)
            .bearer_auth(bearer)
            .json(&query)
            .send()
            .await
            .map_err(|e| DcpError::CredentialService(e.to_string()))?;
        if !response.status().is_success() {
            return Err(DcpError::CredentialService(format!(
                "{url} answered {}",
                response.status()
            )));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| DcpError::CredentialService(e.to_string()))?
        {
            body.extend_from_slice(&chunk);
            if body.len() > MAX_PRESENTATION_RESPONSE_BYTES {
                return Err(DcpError::CredentialService(
                    "the presentation response is too large".to_string(),
                ));
            }
        }
        let message: PresentationResponseMessage = serde_json::from_slice(&body).map_err(|e| {
            DcpError::InvalidPresentation(format!("not a PresentationResponseMessage: {e}"))
        })?;
        if message.presentation.is_empty() {
            return Err(DcpError::InvalidPresentation(
                "the response holds no presentation".to_string(),
            ));
        }

        let now = now_secs();
        let mut credentials = Vec::new();
        for vp in &message.presentation {
            for vc in self.verify_presentation(verified, vp, now)? {
                credentials.push(self.verify_credential(participant, &vc, now).await?);
            }
        }
        for scope in required_scopes {
            let Some(required) = credential_type_of_scope(scope) else {
                continue;
            };
            if !credentials.iter().any(|c| c.types.contains(&required)) {
                return Err(DcpError::MissingCredentialType(required));
            }
        }
        Ok(credentials)
    }

    /// Checks one JWT VP and returns the JWT VCs it holds.
    fn verify_presentation(
        &self,
        verified: &VerifiedToken,
        vp: &str,
        now: u64,
    ) -> Result<Vec<String>, DcpError> {
        let invalid = |m: String| DcpError::InvalidPresentation(m);
        let (_, header, payload) = decode_jws_unverified(vp).map_err(invalid)?;
        let kid = str_claim(&header, "kid").ok_or_else(|| invalid("no kid".to_string()))?;
        let key = find_verifying_key(&verified.document, kid).map_err(invalid)?;
        verify_jws_signature(vp, &key).map_err(invalid)?;
        if str_claim(&payload, "iss") != Some(verified.participant_did.as_str()) {
            return Err(invalid(
                "the presentation's iss is not the participant".to_string(),
            ));
        }
        if !audience_names(&payload, self.did()) {
            return Err(invalid(
                "the presentation's aud does not name this connector".to_string(),
            ));
        }
        check_times(&payload, now).map_err(|e| invalid(e.to_string()))?;
        let body = payload
            .get("vp")
            .ok_or_else(|| invalid("no `vp` claim".to_string()))?;
        if let Some(holder) = body.get("holder").and_then(Value::as_str)
            && holder != verified.participant_did
        {
            return Err(invalid(
                "the presentation's holder is not the participant".to_string(),
            ));
        }
        let vcs = match body.get("verifiableCredential") {
            None => return Ok(vec![]),
            Some(Value::Array(vcs)) => vcs.clone(),
            Some(single) => vec![single.clone()],
        };
        vcs.into_iter()
            .map(|vc| match vc {
                Value::String(jws) => Ok(jws),
                _ => Err(invalid("only JWT credentials are supported".to_string())),
            })
            .collect()
    }

    /// Checks one JWT VC: trusted issuer (before anything is fetched), its
    /// signature by a key of the issuer's DID, then
    /// [`check_credential_claims`].
    async fn verify_credential(
        &self,
        participant: &str,
        jws: &str,
        now: u64,
    ) -> Result<VerifiedCredential, DcpError> {
        let (_, header, payload) =
            decode_jws_unverified(jws).map_err(DcpError::InvalidCredential)?;
        let claims = check_credential_claims(&payload, participant, now)?;
        if !self.config.trusted_issuers.contains(&claims.issuer) {
            return Err(DcpError::UntrustedIssuer(claims.issuer));
        }
        let kid = str_claim(&header, "kid")
            .ok_or_else(|| DcpError::InvalidCredential("no kid".to_string()))?;
        let document = self.resolve(&claims.issuer).await?;
        let key = find_verifying_key(&document, kid).map_err(DcpError::InvalidCredential)?;
        verify_jws_signature(jws, &key).map_err(DcpError::InvalidCredential)?;
        Ok(VerifiedCredential {
            id: claims.id,
            issuer: claims.issuer,
            types: claims.types,
            claims: payload,
        })
    }
}
