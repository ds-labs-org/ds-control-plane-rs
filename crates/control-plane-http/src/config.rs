//! Process configuration, read from environment variables with sane
//! defaults so the binary runs unconfigured out of the box.

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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: 8080,
            agent_id: "urn:connector:ds-control-plane-rs".to_string(),
            base_url: "http://localhost:8080".to_string(),
            dsp_path: "/dsp/2025-1".to_string(),
        }
    }
}

impl Config {
    /// Reads `PORT`, `AGENT_ID`, `BASE_URL`, `DSP_PATH` from the
    /// environment, falling back to [`Config::default`] for anything unset.
    pub fn from_env() -> Self {
        let default = Self::default();
        Self {
            port: std::env::var("PORT")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(default.port),
            agent_id: std::env::var("AGENT_ID").unwrap_or(default.agent_id),
            base_url: std::env::var("BASE_URL").unwrap_or(default.base_url),
            dsp_path: std::env::var("DSP_PATH").unwrap_or(default.dsp_path),
        }
    }

    /// `base_url` + `dsp_path` — the root every catalog/negotiation/transfer
    /// endpoint is mounted under, and the value that should be configured
    /// as the TCK's `dataspacetck.dsp.connector.http.url`.
    pub fn dsp_base_url(&self) -> String {
        format!("{}{}", self.base_url, self.dsp_path)
    }
}
