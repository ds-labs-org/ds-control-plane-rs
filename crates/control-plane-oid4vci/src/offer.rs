//! Parsing of the Credential Offer as it reaches a wallet (spec §4.1):
//! an `openid-credential-offer://` URI (or any URL) carrying exactly one of
//! `credential_offer` (the offer by value, URL-encoded JSON) or
//! `credential_offer_uri` (a URL to fetch it from), or a bare JSON object.

use url::Url;

use crate::error::Oid4vciError;
use crate::types::CredentialOffer;

/// A Credential Offer before resolution: either already in hand, or a
/// reference the wallet must dereference with an HTTP GET.
#[derive(Debug, Clone, PartialEq)]
pub enum OfferSource {
    ByValue(CredentialOffer),
    ByReference(String),
}

impl OfferSource {
    /// Parses `input` as either a Credential Offer URI or a raw JSON offer
    /// object.
    pub fn parse(input: &str) -> Result<Self, Oid4vciError> {
        let input = input.trim();
        if input.starts_with('{') {
            let offer = serde_json::from_str(input)
                .map_err(|e| Oid4vciError::InvalidOffer(format!("offer JSON: {e}")))?;
            return Ok(Self::ByValue(offer));
        }

        let url =
            Url::parse(input).map_err(|e| Oid4vciError::InvalidOffer(format!("offer URI: {e}")))?;
        let mut by_value = None;
        let mut by_reference = None;
        for (k, v) in url.query_pairs() {
            match k.as_ref() {
                "credential_offer" => by_value = Some(v.into_owned()),
                "credential_offer_uri" => by_reference = Some(v.into_owned()),
                _ => {}
            }
        }

        match (by_value, by_reference) {
            (Some(json), None) => {
                let offer = serde_json::from_str(&json).map_err(|e| {
                    Oid4vciError::InvalidOffer(format!("credential_offer parameter: {e}"))
                })?;
                Ok(Self::ByValue(offer))
            }
            (None, Some(uri)) => Ok(Self::ByReference(uri)),
            (Some(_), Some(_)) => Err(Oid4vciError::InvalidOffer(
                "offer URI carries both credential_offer and credential_offer_uri".into(),
            )),
            (None, None) => Err(Oid4vciError::InvalidOffer(
                "offer URI carries neither credential_offer nor credential_offer_uri".into(),
            )),
        }
    }
}

/// Builds the well-known URL for `issuer` by inserting `well_known` between
/// the host and the path component, as both OID4VCI 1.0 §12.2.2 (issuer
/// metadata) and RFC 8414 §3 (authorization server metadata) require.
/// `https://issuer.example/tenant` → `https://issuer.example/.well-known/<x>/tenant`.
pub fn well_known_url(issuer: &str, well_known: &str) -> Result<String, Oid4vciError> {
    let mut url = Url::parse(issuer)
        .map_err(|e| Oid4vciError::InvalidMetadata(format!("identifier '{issuer}': {e}")))?;
    let path = url.path().trim_end_matches('/').to_string();
    url.set_path(&format!("/.well-known/{well_known}{path}"));
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_offer_by_value() {
        let json = r#"{"credential_issuer":"https://issuer.example","credential_configuration_ids":["MembershipCredential"],"grants":{"urn:ietf:params:oauth:grant-type:pre-authorized_code":{"pre-authorized_code":"abc","tx_code":{"length":4,"input_mode":"numeric"}}}}"#;
        let mut uri = Url::parse("openid-credential-offer://").unwrap();
        uri.query_pairs_mut().append_pair("credential_offer", json);

        let OfferSource::ByValue(offer) = OfferSource::parse(uri.as_str()).unwrap() else {
            panic!("expected by-value offer");
        };
        assert_eq!(offer.credential_issuer, "https://issuer.example");
        let grant = offer.pre_authorized_code().unwrap();
        assert_eq!(grant.pre_authorized_code, "abc");
        assert_eq!(grant.tx_code.as_ref().unwrap().length, Some(4));
    }

    #[test]
    fn parses_offer_by_reference() {
        let src = OfferSource::parse(
            "openid-credential-offer://?credential_offer_uri=https%3A%2F%2Fissuer.example%2Foffers%2F1",
        )
        .unwrap();
        assert_eq!(
            src,
            OfferSource::ByReference("https://issuer.example/offers/1".into())
        );
    }

    #[test]
    fn parses_bare_json_offer() {
        let src = OfferSource::parse(
            r#"{"credential_issuer":"https://i.example","credential_configuration_ids":["x"]}"#,
        )
        .unwrap();
        assert!(matches!(src, OfferSource::ByValue(_)));
    }

    #[test]
    fn rejects_offer_uri_with_both_or_neither_parameter() {
        assert!(OfferSource::parse("openid-credential-offer://?foo=bar").is_err());
        assert!(
            OfferSource::parse(
                "openid-credential-offer://?credential_offer=%7B%7D&credential_offer_uri=x"
            )
            .is_err()
        );
    }

    #[test]
    fn well_known_is_inserted_before_the_path() {
        assert_eq!(
            well_known_url("https://issuer.example", "openid-credential-issuer").unwrap(),
            "https://issuer.example/.well-known/openid-credential-issuer"
        );
        assert_eq!(
            well_known_url(
                "https://issuer.example/tenant/1/",
                "oauth-authorization-server"
            )
            .unwrap(),
            "https://issuer.example/.well-known/oauth-authorization-server/tenant/1"
        );
    }
}
