//! Process configuration, read from environment variables with sane
//! defaults so the binary runs unconfigured out of the box.

use std::path::PathBuf;

use control_plane_dcp::{DEFAULT_REQUIRED_SCOPE, DcpConfig};

/// Runtime configuration for the control plane HTTP server.
#[derive(Debug, Clone)]
pub struct Config {
    /// Port the single HTTP listener binds on. `0` lets the OS assign an
    /// ephemeral port (used by tests).
    pub port: u16,
    /// This connector's agent id, as reported in the well-known metadata
    /// response is not itself part of `VersionResponse` (the spec's
    /// `dataspacetck.dsp.connector.agent.id` is a TCK-side config knob
    /// describing the CUT to the TCK, not a value the CUT serves) — kept
    /// here anyway since a real connector would use it as the
    /// `participantId` on catalogs/agreements it issues.
    pub agent_id: String,
    /// The connector's externally reachable base URL, e.g.
    /// `http://localhost:8080` — no trailing slash. The well-known metadata
    /// endpoint is served unversioned at `<base_url>/.well-known/dspace-version`.
    pub base_url: String,
    /// The path segment appended to `base_url` for every versioned DSP
    /// endpoint (catalog/negotiations/transfers), e.g. `/dsp/2025-1` — no
    /// trailing slash.
    pub dsp_path: String,
    /// Directory of the on-disk RDF store holding this connector's settings
    /// graph and runtime state (`control-plane-graph`). `None` keeps
    /// everything in memory, lost on exit. Read from `GRAPH_STORE_PATH`,
    /// the same variable Contreforts' own `GraphConfig` uses.
    pub store_path: Option<PathBuf>,
    /// DCP authentication of the counterparty (`control-plane-dcp`).
    /// `None` leaves every DSP endpoint unauthenticated, as before DCP:
    /// the `Authorization` header is ignored and nothing is bound to a
    /// participant. The Docker DSP TCK runs this way, since it cannot mint
    /// DCP tokens.
    pub dcp: Option<DcpConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: 8080,
            agent_id: "urn:connector:ds-control-plane-rs".to_string(),
            base_url: "http://localhost:8080".to_string(),
            dsp_path: "/dsp/2025-1".to_string(),
            store_path: None,
            dcp: None,
        }
    }
}

impl Config {
    /// Reads `PORT`, `AGENT_ID`, `BASE_URL`, `DSP_PATH`, `GRAPH_STORE_PATH`
    /// and the `DCP_*` variables ([`dcp_from_lookup`]) from the
    /// environment, falling back to [`Config::default`] for anything unset
    /// (an empty `GRAPH_STORE_PATH` counts as unset). An unusable `DCP_*`
    /// value is an error, never a silent fallback to no authentication.
    pub fn from_env() -> anyhow::Result<Self> {
        let default = Self::default();
        Ok(Self {
            port: std::env::var("PORT")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(default.port),
            agent_id: std::env::var("AGENT_ID").unwrap_or(default.agent_id),
            base_url: std::env::var("BASE_URL").unwrap_or(default.base_url),
            dsp_path: std::env::var("DSP_PATH").unwrap_or(default.dsp_path),
            store_path: std::env::var_os("GRAPH_STORE_PATH")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            dcp: dcp_from_lookup(|name| std::env::var(name).ok())?,
        })
    }

    /// `base_url` + `dsp_path` — the root every catalog/negotiation/transfer
    /// endpoint is mounted under, and the value that should be configured
    /// as the TCK's `dataspacetck.dsp.connector.http.url`.
    pub fn dsp_base_url(&self) -> String {
        format!("{}{}", self.base_url, self.dsp_path)
    }
}

/// The DCP configuration from `DCP_*` variables, read through `get`:
///
/// - `DCP_DID`: this connector's `did:web` DID. Unset or empty turns DCP
///   off (`None`); any other `DCP_*` variable set without it is an error.
/// - `DCP_KEY_FILE`: its P-256 private key, a PKCS#8 or SEC1 PEM or an EC
///   JWK. Unset: a key is generated at startup (with a warning).
/// - `DCP_TRUSTED_ISSUERS`: issuer DIDs whose credentials are accepted,
///   comma-separated. Unset or empty trusts nobody.
/// - `DCP_REQUIRED_SCOPES`: the scopes a counterparty must hold credentials
///   for, separated by commas or spaces. Default
///   `org.eclipse.dspace.dcp.vc.type:MembershipCredential:read`.
/// - `DCP_ALLOWED_HOSTS`: hosts the verifier may contact to resolve
///   counterparty DIDs and query their Credential Services,
///   comma-separated (trusted issuers' hosts are added). Unset allows none.
/// - `DCP_INSECURE_HTTP`: `true` resolves `did:web` over `http` and accepts
///   `http` Credential Services. Tests and local setups only.
pub fn dcp_from_lookup(get: impl Fn(&str) -> Option<String>) -> anyhow::Result<Option<DcpConfig>> {
    const OTHERS: [&str; 5] = [
        "DCP_KEY_FILE",
        "DCP_TRUSTED_ISSUERS",
        "DCP_REQUIRED_SCOPES",
        "DCP_ALLOWED_HOSTS",
        "DCP_INSECURE_HTTP",
    ];
    let get = |name: &str| get(name).map(|v| v.trim().to_string());
    let Some(did) = get("DCP_DID").filter(|v| !v.is_empty()) else {
        if let Some(name) = OTHERS
            .iter()
            .find(|n| get(n).is_some_and(|v| !v.is_empty()))
        {
            anyhow::bail!("{name} is set but DCP_DID is not: set DCP_DID to turn DCP on");
        }
        return Ok(None);
    };
    let list = |name: &str, separators: &[char]| -> Vec<String> {
        get(name)
            .unwrap_or_default()
            .split(separators)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(String::from)
            .collect()
    };
    let mut dcp = DcpConfig::new(did);
    dcp.key_file = get("DCP_KEY_FILE")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    dcp.trusted_issuers = list("DCP_TRUSTED_ISSUERS", &[',']);
    dcp.required_scopes = match get("DCP_REQUIRED_SCOPES") {
        Some(_) => list("DCP_REQUIRED_SCOPES", &[',', ' ', '\t', '\n']),
        None => vec![DEFAULT_REQUIRED_SCOPE.to_string()],
    };
    dcp.allowed_hosts = list("DCP_ALLOWED_HOSTS", &[',']);
    dcp.insecure_http = match get("DCP_INSECURE_HTTP")
        .unwrap_or_default()
        .to_lowercase()
        .as_str()
    {
        "" | "false" | "0" | "no" => false,
        "true" | "1" | "yes" => true,
        other => anyhow::bail!("DCP_INSECURE_HTTP must be true or false, not '{other}'"),
    };
    Ok(Some(dcp))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup(vars: &[(&str, &str)]) -> anyhow::Result<Option<DcpConfig>> {
        let vars: Vec<(String, String)> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        dcp_from_lookup(|name| vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()))
    }

    #[test]
    fn without_a_did_dcp_is_off() {
        assert!(lookup(&[]).unwrap().is_none());
        assert!(lookup(&[("DCP_DID", " ")]).unwrap().is_none());
        assert!(lookup(&[("DCP_TRUSTED_ISSUERS", "did:web:i")]).is_err());
    }

    #[test]
    fn the_dcp_variables_are_read() {
        let dcp = lookup(&[
            ("DCP_DID", "did:web:cp.example"),
            ("DCP_KEY_FILE", "/run/secrets/key.pem"),
            ("DCP_TRUSTED_ISSUERS", "did:web:a, did:web:b,"),
            ("DCP_ALLOWED_HOSTS", "consumer.example,other.example"),
            ("DCP_INSECURE_HTTP", "true"),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(dcp.did, "did:web:cp.example");
        assert_eq!(dcp.key_file, Some(PathBuf::from("/run/secrets/key.pem")));
        assert_eq!(dcp.trusted_issuers, ["did:web:a", "did:web:b"]);
        assert_eq!(dcp.required_scopes, [DEFAULT_REQUIRED_SCOPE]);
        assert_eq!(dcp.allowed_hosts, ["consumer.example", "other.example"]);
        assert!(dcp.insecure_http);

        let dcp = lookup(&[
            ("DCP_DID", "did:web:cp.example"),
            (
                "DCP_REQUIRED_SCOPES",
                "org.eclipse.dspace.dcp.vc.type:A:read org.eclipse.dspace.dcp.vc.type:B:read",
            ),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(dcp.required_scopes.len(), 2);
        assert!(dcp.trusted_issuers.is_empty());
        assert!(dcp.allowed_hosts.is_empty());
        assert!(!dcp.insecure_http);
        assert_eq!(dcp.key_file, None);

        assert!(lookup(&[("DCP_DID", "did:web:x"), ("DCP_INSECURE_HTTP", "maybe")]).is_err());
    }
}
