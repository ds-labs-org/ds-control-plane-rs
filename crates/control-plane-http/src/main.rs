//! `ds-control-plane-rs`: boots the DSP connector control plane HTTP
//! server. See `../../../README.md` and `../../../ARCHITECTURE.md`.

use control_plane_http::config::Config;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let config = Config::from_env();
    tracing::info!(
        port = config.port,
        base_url = %config.base_url,
        dsp_base_url = %config.dsp_base_url(),
        agent_id = %config.agent_id,
        "starting ds-control-plane-rs"
    );

    let app = control_plane_http::build(config).await?;
    tracing::info!(addr = %app.listener.local_addr()?, "listening");
    control_plane_http::serve(app).await
}
