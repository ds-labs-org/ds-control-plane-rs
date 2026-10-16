//! Runs the official Eclipse Dataspace TCK's DSP conformance suite (the
//! `eclipsedataspacetck/dsp-tck-runtime` container) against a real,
//! fully-booted instance of this control plane (`control_plane_http::build`)
//! — not a mock, and not a reimplementation of the TCK's own assertions.
//!
//! ## Scope: what this actually proves, and what it doesn't
//!
//! This control plane is a bootstrap, Provider role only, with an
//! always-allow stub policy: its provider agrees to every request and
//! finalizes once verified, but never counter-offers and never starts a
//! transfer on its own (see `../../../ARCHITECTURE.md`, "Scope"). The DSP
//! TCK's own config-driven filtering
//! (`dsp.tck.properties`'s `dataspacetck.test.package`) selects whole
//! verification packages, not individual test classes — `CN`/`CN_C` share
//! one package, as do `TP`/`TP_C`, so the Consumer-role tests cannot be
//! excluded by that filter alone.
//!
//! So rather than either faking a green run by asserting nothing
//! meaningful, or skipping the exercise entirely, this test asserts the
//! *exact* set of TCK test ids already known to fail for documented,
//! out-of-scope or not-yet-implemented reasons ([`EXPECTED_FAILURES`]):
//!
//! - a regression on any test not in that set also fails this test;
//! - the TCK unexpectedly reporting *fewer* failures than
//!   [`EXPECTED_FAILURES`] also fails this test — a sign the list (and
//!   `../../../ARCHITECTURE.md`'s scope claims) have gone stale, not a
//!   reason to celebrate a silent pass;
//! - an entirely new/different failure fails this test the same way a
//!   missing expected failure does — the assertion is an exact-set
//!   comparison, not a subset check.
//!
//! ## Running this
//!
//! Requires Docker (pulls and runs
//! `eclipsedataspacetck/dsp-tck-runtime:latest`). Not run by plain
//! `cargo test` or the "quality" CI job — `#[ignore]`d so neither needs
//! Docker:
//!
//! ```sh
//! cargo test -p control-plane-http --test dsp_tck -- --ignored --nocapture
//! ```
//!
//! Verified genuinely green against a real, locally running
//! `dsp-tck-runtime:latest` container (not written blind) — see
//! `../../../ARCHITECTURE.md`, "TCK conformance snapshot" for the exact run
//! this was checked against.

use std::collections::BTreeSet;
use std::path::{self, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use control_plane_core::common::Agreement;
use control_plane_core::negotiation::{ContractNegotiation, NegotiationState};
use control_plane_http::config::Config;
use futures::FutureExt;
use futures::future::BoxFuture;
use regex::Regex;
use testcontainers::core::logs::LogFrame;
use testcontainers::core::logs::consumer::LogConsumer;
use testcontainers::core::{ContainerPort, Host, IntoContainerPort, Mount, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{GenericImage, ImageExt};

/// Fixed (not OS-assigned) port: `dsp.tck.properties`'
/// `dataspacetck.dsp.connector.http.url`/`.http.base.url` are baked into a
/// static file bind-mounted into the TCK container, so it can't learn a
/// port picked at runtime. Distinct from this project's own documented
/// default (8080, see `../../../README.md`) so this test can't collide
/// with a `cargo run` left running on the same machine.
const SERVER_PORT: u16 = 28080;

/// The exact TCK test ids (`ClassTagPrefix:NN-NN`, as printed by the TCK
/// runtime's own reporting) expected to fail against this bootstrap, and
/// why. See this file's module doc comment for what asserting an exact set
/// (rather than "no failures") buys, and `dsp.tck.properties` for why the
/// TCK's own filters can't exclude the Consumer-role tests outright.
const EXPECTED_FAILURES: &[&str] = &[
    // Contract negotiation, provider role (`CN`): this provider agrees to
    // every request and finalizes once the consumer verifies (see
    // ../../../ARCHITECTURE.md, "Negotiation progression"), so the
    // scenarios built on that path pass (`CN:01-04`, `CN:02-02`,
    // `CN:02-03`, `CN:03-01`). These remaining ones need the provider to
    // send a counter-offer first or to terminate on its own, which a
    // provider that always agrees never does.
    "CN:01-01",
    "CN:01-02",
    "CN:01-03",
    "CN:02-01",
    "CN:02-04",
    "CN:02-05",
    "CN:02-06",
    "CN:02-07",
    "CN:03-02",
    "CN:03-03",
    "CN:03-04",
    // Contract negotiation, consumer role (`CN_C`): needs a whole
    // consumer-role HTTP surface this bootstrap does not implement.
    "CN_C:01-01",
    "CN_C:01-02",
    "CN_C:01-03",
    "CN_C:01-04",
    "CN_C:02-01",
    "CN_C:02-02",
    "CN_C:02-03",
    "CN_C:02-04",
    "CN_C:02-05",
    "CN_C:02-06",
    "CN_C:03-01",
    "CN_C:03-02",
    "CN_C:03-03",
    "CN_C:03-04",
    "CN_C:03-05",
    "CN_C:03-06",
    // Transfer process, provider role (`TP`) and consumer role (`TP_C`):
    // same root cause — these scenarios require the provider to
    // spontaneously push a `TransferStartMessage` (`TP:01-*`, `TP:02-*`,
    // `TP:03-03..06`) or require a consumer-role HTTP surface (`TP_C:*`)
    // this bootstrap does not implement (see ../../../ARCHITECTURE.md).
    "TP:01-01",
    "TP:01-02",
    "TP:01-03",
    "TP:01-04",
    "TP:01-05",
    "TP:02-01",
    "TP:02-02",
    "TP:02-03",
    "TP:03-03",
    "TP:03-04",
    "TP:03-05",
    "TP:03-06",
    "TP_C:01-01",
    "TP_C:01-02",
    "TP_C:01-03",
    "TP_C:01-04",
    "TP_C:01-05",
    "TP_C:02-01",
    "TP_C:02-02",
    "TP_C:02-03",
    "TP_C:02-05",
    "TP_C:03-01",
    "TP_C:03-02",
    "TP_C:03-03",
    "TP_C:03-04",
    "TP_C:03-05",
    "TP_C:03-06",
];

/// Must match every `TP_NN_NN_AGREEMENTID` in dsp.tck.properties.
const TCK_AGREEMENT_ID: &str = "urn:uuid:control-plane-rs-tck-agreement";

/// A finalized negotiation holding [`TCK_AGREEMENT_ID`].
fn tck_agreement_negotiation() -> ContractNegotiation {
    let mut n = ContractNegotiation::new(
        "urn:uuid:control-plane-rs-tck-negotiation",
        "urn:uuid:control-plane-rs-tck-consumer",
    );
    n.state = NegotiationState::Finalized;
    n.agreement = Some(Agreement {
        id: TCK_AGREEMENT_ID.to_string(),
        type_: "Agreement".to_string(),
        target: control_plane_http::state::SEED_DATASET_ID.to_string(),
        timestamp: chrono::Utc::now(),
        assigner: "urn:connector:ds-control-plane-rs".to_string(),
        assignee: "urn:uuid:control-plane-rs-tck-consumer".to_string(),
        permission: vec![],
        prohibition: vec![],
        obligation: vec![],
    });
    n
}

fn properties_path() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/dsp.tck.properties"
    ))
}

#[tokio::test]
#[ignore = "needs Docker; run explicitly with `cargo test -p control-plane-http --test dsp_tck -- --ignored --nocapture`"]
async fn dsp_tck_matches_documented_bootstrap_scope() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .try_init();

    let config = Config {
        port: SERVER_PORT,
        agent_id: "urn:connector:ds-control-plane-rs".to_string(),
        base_url: format!("http://host.docker.internal:{SERVER_PORT}"),
        dsp_path: "/dsp/2025-1".to_string(),
        store_path: None,
    };

    let app = control_plane_http::build(config)
        .await
        .expect("failed to build the control plane app");
    // The `TP` tests open transfers under `TP_NN_NN_AGREEMENTID` from
    // dsp.tck.properties, and a transfer needs an agreement this connector
    // has finalized, so one is stored before the run.
    app.graph
        .put_negotiation(&tck_agreement_negotiation())
        .expect("failed to store the TCK agreement");
    tokio::spawn(async move {
        let _ = control_plane_http::serve(app).await;
    });
    wait_for_port(SERVER_PORT).await;

    let reporter = TckReporter::default();
    let properties_path = properties_path();
    let properties_path = path::absolute(&properties_path)
        .expect("dsp.tck.properties path resolves to an absolute path");

    let _tck = GenericImage::new("eclipsedataspacetck/dsp-tck-runtime", "latest")
        .with_exposed_port(8083.tcp())
        .with_wait_for(WaitFor::message_on_stdout("Test run complete"))
        .with_mapped_port(8083, ContainerPort::Tcp(8083))
        // `testcontainers`' default startup timeout (60s) is far shorter
        // than a full TCK run against this bootstrap's scope (the whole
        // `dsp.verification` package, ~65 tests) takes end to end — and
        // the "ready" condition above (`WaitFor::message_on_stdout`) is
        // itself the run's own completion message, not an early "server is
        // up" signal. Without raising this, `.start()` fails with
        // `WaitContainerError::StartupTimeout` long before the TCK is done.
        .with_startup_timeout(Duration::from_secs(15 * 60))
        .with_mount(Mount::bind_mount(
            properties_path
                .to_str()
                .expect("dsp.tck.properties path is valid UTF-8"),
            "/etc/tck/config.properties",
        ))
        // The TCK container calls back into this host's own control plane
        // (`dataspacetck.dsp.connector.http.url` in dsp.tck.properties)
        // over this address, since the control plane runs on the host, not
        // in a container of its own.
        .with_host("host.docker.internal", Host::HostGateway)
        .with_log_consumer(reporter.clone())
        .start()
        .await
        .expect("failed to start the dsp-tck-runtime container");

    let actual: BTreeSet<String> = reporter.failures().into_iter().collect();
    let expected: BTreeSet<String> = EXPECTED_FAILURES.iter().map(|s| s.to_string()).collect();

    assert_eq!(
        actual, expected,
        "TCK failure set no longer matches EXPECTED_FAILURES. Either a real \
         regression, or this bootstrap now covers more than EXPECTED_FAILURES \
         documents (some entries no longer fail). Update EXPECTED_FAILURES and \
         ../../../ARCHITECTURE.md together with whichever is true — never just \
         to make this test pass again. Actual failures reported by the TCK this \
         run: {actual:?}"
    );
}

/// Polls the server's own port until it accepts a TCP connection, so the
/// TCK container is never started against a server that hasn't finished
/// binding yet.
async fn wait_for_port(port: u16) {
    let addr = format!("127.0.0.1:{port}");
    for _ in 0..200 {
        if tokio::net::TcpStream::connect(&addr).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("control plane on {addr} never came up");
}

/// Collects `FAILED: <TestId>` lines from the TCK container's own stdout.
/// The container process itself always exits 0 regardless of test
/// outcome — watching its logs is the only way to learn which tests
/// actually failed (same approach `ds-sql-dps-rs`'s `dps_tck.rs` uses for
/// the sibling Data Plane Signaling TCK).
#[derive(Clone, Default)]
struct TckReporter {
    failures: Arc<Mutex<Vec<String>>>,
}

static FAIL_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"FAILED: ([\w.]+:\d+-\d+)").expect("valid regex"));

impl LogConsumer for TckReporter {
    fn accept<'a>(&'a self, record: &'a LogFrame) -> BoxFuture<'a, ()> {
        let log = String::from_utf8_lossy(record.bytes());
        if let Some(caps) = FAIL_REGEX.captures(&log) {
            self.failures
                .lock()
                .expect("failures lock poisoned")
                .push(caps[1].to_string());
        }
        print!("{log}");
        futures::future::ready(()).boxed()
    }
}

impl TckReporter {
    fn failures(&self) -> Vec<String> {
        self.failures
            .lock()
            .expect("failures lock poisoned")
            .clone()
    }
}
