# Architecture

- **Status:** Bootstrap, real endpoints wired and TCK-tested (see "TCK
  conformance snapshot" below). Not a full implementation.
- **Date:** 2026-09-19

This file records the scope of this project's bootstrap: what it
implements for real, what it stubs, and what it deliberately leaves out,
plus a real, date-stamped conformance run against the official Eclipse
Dataspace TCK.

## What Dataspace Protocol (DSP) is

DSP ([spec](https://eclipse-dataspace-protocol-base.github.io/DataspaceProtocol),
current TCK-supported version `2025-1`) is the wire protocol that lets
independent dataspace participants — Connectors — discover each other's
data offers, negotiate a usage policy, and drive a data transfer, without
either side needing to run the same software. It defines four protocol
surfaces, all covered by the [eclipse-dataspacetck/dsp-tck](https://github.com/eclipse-dataspacetck/dsp-tck)
conformance suite:

- **Metadata**: an unauthenticated `/.well-known/dspace-version` endpoint
  every Connector must expose, so a counter-party can discover which DSP
  version(s)/bindings it speaks (TCK group `MET`).
- **Catalog protocol**: how a Consumer requests a Provider's `Catalog`
  (datasets, their ODRL policies, and how to obtain them) (TCK group `CAT`).
- **Contract negotiation protocol**: the state machine by which a Consumer
  and Provider agree on a policy for a dataset, ending in a signed
  `Agreement` (TCK groups `CN` provider, `CN_C` consumer).
- **Transfer process protocol**: the state machine that starts, suspends,
  completes, or terminates the actual data movement coordinated by the
  control plane (the data plane itself is out of scope of DSP) (TCK groups
  `TP` provider, `TP_C` consumer).

This project implements a **connector control plane**: the message
handling and state machines behind those four surfaces. It is not a data
plane (see sibling project [`ds-sql-dps-rs`](https://github.com/ds-labs-org/ds-sql-dps-rs)
for a Data Plane Signaling driver) and not the identity/credential (DCP)
side of a connector.

## Scope

### Implemented for real

- **Metadata** (`crates/control-plane-http/src/metadata.rs`): the
  `/.well-known/dspace-version` endpoint, returning a real
  `VersionResponse` with one `2025-1`/`HTTPS` entry. This is the one
  surface expected to pass its whole TCK group outright — see "TCK
  conformance snapshot".
- **Wire message shapes** (`crates/control-plane-core`): real,
  spec-correct JSON envelopes for every message type in all four
  protocols — `@context`/`@type` plus the exact property names and
  nesting the spec's own `artifacts/src/main/resources/**/example/*.json`
  fixtures show (fetched from the
  [`eclipse-dataspace-protocol-base/DataspaceProtocol`](https://github.com/eclipse-dataspace-protocol-base/DataspaceProtocol)
  repo while building this project) — not placeholder JSON blobs invented
  from the prose alone.
- **`ContractNegotiation` and `TransferProcess` state machines**
  (`crates/control-plane-core/src/{negotiation,transfer}.rs`): real
  transition validation (`is_valid_transition`), rejecting an
  out-of-sequence state change rather than silently accepting it. See each
  module's doc comment for an honesty note: the spec's normative state
  diagrams are rendered PNG images in the spec repo, not machine-readable
  source, so the transition graphs here are reconstructed from the spec's
  *prose* description of each state and message type. If that
  reconstruction turns out to disagree with the diagram on some edge, that
  is a documented risk of this bootstrap, not a silent guess.
- **Catalog protocol HTTP surface** (`crates/control-plane-http/src/catalog.rs`):
  `POST {dsp}/catalog/request` and `GET {dsp}/catalog/datasets/:id`,
  serving the datasets in the settings graph (see "Contreforts: settings
  and runtime persistence"), each a real DCAT/ODRL-shaped
  `Dataset`/`Offer`/`Distribution`/`DataService`.
- **Contract negotiation HTTP surface, Provider role**
  (`crates/control-plane-http/src/negotiation.rs`): all six Provider path
  bindings (`GET .../:providerPid`, `POST .../request`,
  `POST .../:providerPid/request`, `.../events`,
  `.../agreement/verification`, `.../termination`), each driving the real
  state machine and returning a real `ContractNegotiationError` with an
  appropriate HTTP status (400 for an invalid transition, 404 for an
  unknown `providerPid`) rather than a bare status code.
- **Transfer process HTTP surface, Provider role**
  (`crates/control-plane-http/src/transfer.rs`): all six Provider path
  bindings (`GET .../:providerPid`, `POST .../request`, `.../start`,
  `.../completion`, `.../termination`, `.../suspension`), same real
  state-machine-plus-error-body treatment.

### Stubbed

- **Policy**: always-allow. `POST .../negotiations/request` never rejects
  an offer, and `POST .../transfers/request` never checks the referenced
  `agreementId` against a real, negotiated `Agreement` — there is no ODRL
  constraint evaluation at all.
- **Negotiation progression**: this control plane never *acts* as a
  Provider deciding to agree, counter-offer, or finalize on its own —
  moving a negotiation past `REQUESTED` requires an external actor driving
  the same Provider HTTP endpoints a real counter-party (or, in the TCK's
  case, the TCK itself acting as Consumer against `CN` tests that expect
  the Provider to be the one moving the state forward) would.
- **Catalog seeding**: a fresh settings graph is seeded with exactly one
  dataset id (`urn:uuid:control-plane-rs-seed-dataset`); more can be added
  only through the Contreforts connector (there is no management HTTP API
  yet). `crates/control-plane-http/tests/dsp.tck.properties`
  uses the TCK's own documented `CAT_<NN>_DATASETID` per-test override to
  point `CAT:01-01`/`CAT:01-02` at this fixed id, which is what lets the
  whole `CAT` group pass — see "TCK conformance snapshot". The TCK's
  analogous `CN_<NN>_DATASETID`/`CN_<NN>_OFFERID` overrides are not
  similarly leveraged, since `CN`'s failures are not about dataset-id
  matching (see "Negotiation progression" above).

### Out of scope for this pass

- **Consumer role** (`CN_C`, `TP_C` TCK groups): no outbound
  negotiation/transfer initiation, no consumer callback HTTP surface.
- **JSON-LD processing**: messages are read/written as plain, hand-built
  JSON matching the shape the real `2025-1` context would compact *to* —
  there is no JSON-LD expansion/compaction library in the loop. The TCK
  itself does perform real JSON-LD processing against the live context
  document, which is how e.g. a bare `"REQUESTED"` string in this
  project's response is correctly resolved by the TCK to
  `https://w3id.org/dspace/2025/1/REQUESTED` — see the conformance run
  below.
- **ODRL logical constraint groups** (`odrl:and`/`odrl:or`/`odrl:xone`/`odrl:andSequence`)
  — only the flat, atomic `leftOperand`/`operator`/`rightOperand` shape is
  modeled.
- **Pagination, filtering, compression** on the catalog endpoint.
- **Authorization**: the `Authorization` header is accepted but never
  checked.

## Contreforts: settings and runtime persistence

**2026-10-14:** the catalog and all negotiation/transfer-process state moved
off `Mutex<HashMap<...>>` and into an RDF store built on
[Contreforts](https://github.com/contreforts-ai), following the pattern
`ds-sql-dps-rs` and `ds-identity-hub-rs` already use, at the same pinned
upstream commit, so all three connectors are directly comparable.

**Vendoring:** `vendor/contreforts-core` is a git submodule pinned to
`95a49404a60024a5b432b17f7a7c299a3dac9c3f` (upstream publishes no tags),
and a workspace member that inherits `version`/`edition` from the root
`Cargo.toml`, as its manifest expects. Clone with
`git clone --recurse-submodules`, or run `git submodule update --init`
in an existing checkout; `.github/workflows/ci.yml` checks it out with
`submodules: true`.

**The store (`crates/control-plane-graph`):** one embedded Oxigraph store
with two named graphs, split the way Contreforts' own `CONFIG_GRAPH`
separates configuration from synced data:

- **Settings graph** (`urn:ds-control-plane-rs:graph:settings`): the
  connector's identity (`dspace:participantId`, base URL, DSP path, catalog
  id, its own `dcat:DataService` and `dcat:endpointURL`) and every catalog
  dataset as a `dcat:Dataset` with its ODRL offers (`odrl:hasPolicy`),
  `dcat:Distribution`s and a `dcterms:modified` timestamp (a dataset
  stored before datasets were timestamped reads as modified at the Unix
  epoch). Datasets and data services are named by their own
  `@id` IRIs, so the graph is SPARQL-addressable by the same ids the
  catalog serves. Distributions point at their data service by IRI, so a
  new base URL rewrites one `dcat:endpointURL` triple, not every dataset.
- **Runtime graph** (`urn:ds-control-plane-rs:graph:runtime`): one node per
  contract negotiation and transfer process, named by its `providerPid`,
  typed `dspace:ContractNegotiation`/`dspace:TransferProcess`, with its
  state as the real DSP state IRI (`dspace:REQUESTED`, ...), the offer and
  agreement as ODRL nodes, and a `dcterms:modified` timestamp.

Vocabulary is real DSP (`https://w3id.org/dspace/2025/1/`), DCAT, Dublin
Core and ODRL wherever a term exists, plus this project's own
`https://ds42.org/ontologies/ds-control-plane-rs#` (`dscp:`) for connector
settings, `dscp:order` (array order, which plain RDF loses; same reason as
the siblings' `ds:order`) and `dscp:policyId`. A policy's `@id` is a
literal on a blank node rather than the node's IRI because offer ids come
from the counterparty, and two negotiations quoting one id must not share
a node. `rightOperand` is stored as an `rdf:JSON` literal so any JSON value
round-trips exactly.

Request input never reaches a SPARQL string: a `providerPid` or dataset id
from an HTTP path becomes a `NamedNode` (an invalid IRI is simply "not
found") matched with `quads_for_pattern`, and everything else a
counterparty sends is a plain literal. Each write replaces a record's node
and the blank nodes under it in one transaction, under a write lock, so
`update_negotiation`/`update_transfer` are atomic read-modify-writes.
Getters hold the same lock (an `RwLock`) for reading: Oxigraph takes a new
snapshot on every `quads_for_pattern`, and a record is decoded with many of
them, so an unguarded read racing a rewrite could list blank children that
are already gone and fail with `GraphError::Missing`. Reads run alongside
each other but wait for an in-flight write. Since every graph call
blocks, HTTP handlers make them through `AppState::with_graph`, which runs
them on Tokio's blocking pool: a handler waiting on the lock or on a
RocksDB commit never parks a worker thread, so requests that don't touch
the store are still served while it is busy.
Datasets, data services and the settings node share the settings graph, so
`put_dataset` and `put_settings` first check that no IRI they are about to
replace is the settings node or already typed as another kind of record
(a dataset id naming a data service, an `accessService` naming a dataset or
its own dataset): such a write fails with `GraphError::Conflict` (a
`Config` error through Contreforts) and nothing is written. A data service
node is shared by every distribution that names it, so `put_dataset` only
adds data services that do not exist yet and never rewrites one: a pushed
`accessService` whose `endpointURL` differs from the stored node (including
the connector's own, which only `put_settings` writes) or from another
distribution in the same push is a `GraphError::Conflict` too. A dataset
and its new data services are written in one transaction.

**Persistence:** set `GRAPH_STORE_PATH` (the variable Contreforts' own
`GraphConfig` reads) to a directory and the store is RocksDB-backed:
the catalog, negotiations and transfer processes survive a restart
(`crates/control-plane-http/tests/persistence.rs` drives the real router,
restarts on the same directory and reads everything back). Unset, the
store is in memory as before. RocksDB is oxigraph's `rocksdb` feature,
a from-source C++ build, on by default through `control-plane-graph`'s
own `rocksdb` feature. Only one process can hold a store directory open:
while the server runs, a separate process cannot open `GRAPH_STORE_PATH`
(and an in-memory store cannot be shared at all). Connector writes to a
running server therefore go through the server's own handle, `App::graph`
(`ControlPlaneConnector::new(app.graph.clone())` in-process, as
`crates/control-plane-http/tests/connector.rs` does), and the catalog
serves them at once; a separate driver process can only write while the
server is stopped.

**Damaged records:** writes reject id collisions, but a record can still
come back unreadable (an older release, a hand edit). List reads
(`datasets()`, `negotiations()`, `transfers()`, and so the catalog and
the Contreforts pulls) skip such a record with a `tracing::warn` instead
of failing; a lookup of that one record still returns the error. Startup
only counts records by `rdf:type` (for its log line and the seeding
check), so it neither decodes every record nor fails on a damaged one.

**What is settings and what is deployment:** the connector's identity
(`AGENT_ID`, `BASE_URL`, `DSP_PATH`) describes the deployment, so it comes
from the environment and is rewritten into the settings graph on every
start. The catalog lives only in the settings graph: seeded once with the
default dataset when empty, then never overwritten by a restart.

**The connector (`crates/control-plane-contreforts`):** implements
`ContrefortsConnector` over the store with three kinds of its own,
declared in `src/declaration.ttl` with `contreforts:entityKind` so a
driver can resolve each back to its class:

| Kind | Graph | `pull`/`get` | `push`/`update` |
|---|---|---|---|
| `ds-control-plane-rs:dataset` | settings | yes, `since` filters on `dcterms:modified` | `push` adds a new catalog dataset (an existing id is a `Config` error); `update` replaces an existing one (an unknown id is `NotFound`) |
| `ds-control-plane-rs:contract-negotiation` | runtime | yes, `since` filters on `dcterms:modified` | unsupported |
| `ds-control-plane-rs:transfer-process` | runtime | yes, `since` filters on `dcterms:modified` | unsupported |

Datasets are writable because they are settings; negotiations and
transfers only change through the DSP protocol itself. The declaration is
checked against Contreforts' real SHACL meta-shapes and lints by
`tests/declaration_validates.rs`. As with the sibling projects, nothing
here has been run against a real Contreforts deployment
(`contreforts-product` is private).

**Bounded runtime graph:** anyone who can reach `negotiations/request` or
`transfers/request` adds a record, and with RocksDB it stays on disk, so
the runtime graph is capped the way `ds-identity-hub-rs` caps its
equivalent structures: `put_negotiation`/`put_transfer` keep at most
`MAX_NEGOTIATIONS`/`MAX_TRANSFERS` (2048 each) and, inside the same write
lock, evict the least recently modified records (by `dcterms:modified`)
with every blank node under them. Each record is bounded too: an offer
from a counterparty may carry at most `MAX_RULES_PER_OFFER` (64) rules,
`MAX_CONSTRAINTS_PER_RULE` (32) constraints per rule and a
`MAX_OPERAND_BYTES` (4 KiB) `rightOperand`, or the request is a 400 and
nothing is written; every route takes at most `MAX_BODY_BYTES` (256 KiB)
of body (`crates/control-plane-http/tests/request_limits.rs`). Still not
checked: that an offer's `target` or a transfer's `agreementId` names
something this connector actually holds (the stub policy accepts any).

## TCK conformance snapshot

**Real, not fabricated.** Run via `cargo test -p control-plane-http --test
dsp_tck -- --ignored --nocapture`, which boots a real, fully-booted
in-process instance of `control-plane-http` and runs the real, official
`eclipsedataspacetck/dsp-tck-runtime:latest` container against it —
verified genuinely green by actually executing it (both by hand against a
manually-run server while developing the expected-failure list below, and
through the exact `cargo test` invocation that ships in this repo and
gates CI), not written blind and not asserting a target that was never
executed.

- **Date:** 2026-09-19
- **TCK image:** `eclipsedataspacetck/dsp-tck-runtime:latest` (digest
  `sha256:02fc9fe4f5e7519284e235ac58ccda3a0fed85530c5e53d87bdb0893d5644bab`)
- **Scope run:** the TCK's whole `org.eclipse.dataspacetck.dsp.verification`
  package (`MET`, `CAT`, `CN`, `CN_C`, `TP`, `TP_C` all included) — the
  TCK's own config-driven filtering selects whole verification packages,
  and `CN`/`CN_C` share one package (as do `TP`/`TP_C`), so the
  Consumer-role groups cannot be excluded from the run itself. Instead,
  `crates/control-plane-http/tests/dsp_tck.rs` asserts the exact,
  documented set of failures this bootstrap's provider-only,
  no-auto-progression scope implies — see that file's `EXPECTED_FAILURES`
  for the full, commented list and why each entry fails.

**Result: 65 tests run, 8 passed, 57 failed — exactly the documented set.**

| Group | Total | Passed | Failed | Notes |
|-------|------:|-------:|-------:|-------|
| `MET` | 1 | 1 | 0 | Full pass, as expected — the metadata endpoint is fully implemented. |
| `CAT` | 3 | 3 | 0 | Full pass. `CAT:01-01`/`CAT:01-02` needed `dsp.tck.properties`'s `CAT_01_01_DATASETID`/`CAT_01_02_DATASETID` overrides pointed at this bootstrap's one real seed dataset id (the TCK's own documented mechanism for this — see its README, "Catalog protocol"); `CAT:01-03` (not-found) passes with the TCK's own random default id. |
| `CN` (provider) | 15 | 1 | 14 | Only `CN:02-02` (a purely reactive scenario: request, then a consumer-sent termination) passes. The other 14 all require this control plane to *spontaneously* push an Offer/Agreement/event/termination — this bootstrap's provider never acts on its own, only in response to inbound requests. |
| `CN_C` (consumer) | 16 | 0 | 16 | Out of scope: no consumer-role HTTP surface at all. |
| `TP` (provider) | 15 | 3 | 12 | `TP:03-01`, `TP:03-02` (reactive negative scenarios) and `TP:02-05` (a reactive termination) pass; the rest require the provider to spontaneously push a `TransferStartMessage`, which this bootstrap never does. |
| `TP_C` (consumer) | 15 | 0 | 15 | Out of scope: no consumer-role HTTP surface at all. |
| **Total** | **65** | **8** | **57** | |

Two things worth recording about how this snapshot was actually produced,
not just its numbers:

- **A real bug was found and fixed while producing it.** The standalone
  `GET {dsp}/catalog/datasets/:id` response was initially missing its
  top-level `@context` — a `Dataset` nested inside a `Catalog` correctly
  omits one, but a standalone `Dataset` document is itself a JSON-LD
  document and needs one, per the spec's own `catalog/example/dataset.json`
  fixture. This made `CAT:01-02` fail schema validation even once the
  right dataset id was configured. Fixed by
  `control_plane_core::catalog::DatasetDocument`, a thin wrapper adding
  `@context` around the plain, `@context`-free `Dataset` type — verified
  by re-running the real TCK, not just by re-reading the code.
- **A TCK documentation trap, worked around rather than treated as a bug in
  this project.** The TCK's own README documents
  `dataspacetck.dsp.default.wait` with an example of "5000 (5 seconds)",
  but the TCK's actual source
  (`DspSystemLauncher.DEFAULT_WAIT_SECONDS = 15`,
  `AbstractAsyncPipeline.thenWait`'s `latch.await(waitTime, SECONDS)`)
  treats the configured value as **seconds**, not milliseconds. Taken at
  the README's word, a scenario this bootstrap can never satisfy (the
  provider spontaneously pushing a message) would time out after ~83
  minutes instead of failing promptly — `dsp.tck.properties` sets it to
  `5` (seconds) once this was traced to the TCK's source.

See `crates/control-plane-http/tests/dsp_tck.rs` for the exact,
already-known failure set this asserts and why each entry fails, and how
to reproduce this run yourself (`cargo test -p control-plane-http --test
dsp_tck -- --ignored --nocapture`; takes a little over two minutes).

## Continuous integration

`.github/workflows/ci.yml` runs two jobs on every push/PR to `main`:

- **`quality`** — `cargo fmt --all -- --check`, `cargo clippy --workspace
  --all-targets -- -D warnings`, `cargo test --workspace` (which does not
  run `dsp_tck.rs`'s `#[ignore]`d test). No Docker needed; this is the
  fast, always-on gate.
- **`dsp-tck`** — the DSP TCK conformance test described above, on
  `ubuntu-latest` (Docker preinstalled). Gating, not `allow-failure` — an
  exact-match assertion against a documented, narrowed scope lets this
  still catch real regressions even though the TCK container itself
  reports real failures on every run.
