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
  serving one fixed, in-memory seed dataset with a real DCAT/ODRL-shaped
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

- **OID4VCI holder** (`crates/control-plane-oid4vci`,
  `crates/control-plane-http/src/oid4vci.rs`): the connector acts as an
  [OID4VCI 1.0](https://openid.net/specs/openid-4-verifiable-credential-issuance-1_0.html)
  wallet for itself. Covered: Credential Offer by value and by reference;
  issuer metadata at the 1.0 well-known location (inserted before the
  path) with a fallback to the appended pre-1.0 location, and a check that
  it names the offering issuer; RFC 8414 authorization server metadata
  with an OpenID discovery fallback; Pre-Authorized Code token exchange
  with `tx_code`; `credential_identifiers` from the token response used in
  place of the configuration id, as the spec requires; Nonce Endpoint plus
  `openid4vci-proof+jwt` ES256 key proofs carrying the holder's `jwk`, with
  one retry on `invalid_nonce`; 1.0 request/response shapes (`proofs`,
  `credentials[]`); deferred issuance; best-effort `credential_accepted`
  notification. Exercised by `crates/control-plane-oid4vci/tests/mock_issuer.rs`
  against an in-process issuer that verifies the proof signature, `typ`,
  `aud` and `nonce`.

  Not covered: the Authorization Code flow (needs a user-agent redirect a
  headless control plane does not have), DPoP-bound tokens, client
  authentication / wallet attestation, encrypted credential responses,
  signed issuer metadata, and any validation of the issued credential
  itself (it is stored as received). Received credentials and the holder
  key live in memory unless `OID4VCI_HOLDER_JWK` pins the key.

  Why an in-repo client instead of a library: surveyed on 2026-10-14,
  `affinidi-openid4vci` still sends draft-13 request shapes (`format` +
  singular `proof`) despite advertising 1.0; spruceid's `oid4vci-rs` is
  1.0-correct but pulls in `ssi`, `isomdl` and an `oauth2` alpha and is
  only on crates.io as a third-party fork; `reallyme-openid4vci-wallet` is
  1.0-correct but two weeks old; impierce and credibil/vercre releases are
  pre-1.0 drafts.

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
- **Storage**: entirely in-memory (`std::sync::Mutex<HashMap<...>>`),
  reset on every process restart.
- **Catalog seeding**: exactly one fixed dataset id
  (`urn:uuid:control-plane-rs-seed-dataset`), always present regardless of
  what a caller asks for. `crates/control-plane-http/tests/dsp.tck.properties`
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
