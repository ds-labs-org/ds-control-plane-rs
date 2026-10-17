# Dataspace Protocol Connector Control Plane

*(repo/product: `ds-control-plane-rs`)*

A from-scratch Rust implementation of a Dataspace Protocol (DSP) **connector
control plane**: the metadata, catalog, contract-negotiation, and
transfer-process surfaces of a dataspace [=Connector=], as defined by the
[Dataspace Protocol Specification](https://eclipse-dataspace-protocol-base.github.io/DataspaceProtocol)
(current TCK-supported version: `2025-1`).

> "A [=Transfer Process=] involves two logical constructs, a control plane
> and a data plane. Serving as a coordinating layer, services on the
> *control plane* receive messages and manage the local state of the
> [=Transfer Process=] (same as for the [=Catalog Protocol=] and the
> [=Contract Negotiation Protocol=]). On the *data plane*, the actual
> transfer of data takes place using a wire protocol."
> — [`transfer.process.protocol.md`, "Control and Data Planes"](https://raw.githubusercontent.com/eclipse-dataspace-protocol-base/DataspaceProtocol/main/specifications/transfer/transfer.process.protocol.md)

This project implements the **control plane** side of that split: the
message-passing state machines and their HTTP surface. It does not
implement a data plane (no wire-protocol data transfer of its own — see
sibling project `ds-sql-dps-rs` for a Data Plane Signaling driver) or the
identity/credential (DCP) side of a connector.

## Scope

Provider role only, for this bootstrap. See [`ARCHITECTURE.md`](ARCHITECTURE.md)
for the full scope statement (implemented vs. stubbed vs. out of scope) and
a real, date-stamped Eclipse Dataspace TCK conformance snapshot.

- **Metadata**: the `/.well-known/dspace-version` endpoint — implemented in
  full.
- **Catalog**: `POST {dsp}/catalog/request`, `GET {dsp}/catalog/datasets/:id`
  — real DSP message shapes, datasets served from an RDF settings graph
  (seeded with one dataset), no filtering.
- **Contract negotiation**: the full Provider-role HTTP surface
  (`negotiations/request`, `.../request`, `.../events`,
  `.../agreement/verification`, `.../termination`) and a
  `ContractNegotiation` state machine matching the spec's state
  descriptions. Given a `callbackAddress`, the provider agrees to the
  request on its own, sends the Agreement, and finalizes once the
  consumer verifies.
- **Transfer process**: the full Provider-role HTTP surface (`transfers/request`,
  `.../start`, `.../completion`, `.../termination`, `.../suspension`) and a
  `TransferProcess` state machine matching the spec's state descriptions.
  A transfer needs an agreement this connector has finalized. No data
  plane wiring.
- **Policy**: an always-allow stub — the provider agrees to every offer,
  with no ODRL constraint evaluation.
- **Consumer role**: out of scope for this pass (no consumer HTTP surface,
  no outbound negotiation/transfer initiation).

## Layout

```
crates/
  control-plane-core/   Domain types: DCAT/ODRL-flavored catalog vocabulary,
                        the ContractNegotiation and TransferProcess state
                        machines, and their DSP wire message shapes.
  control-plane-graph/  Oxigraph RDF store: the settings graph (connector
                        identity, catalog datasets and ODRL offers) and the
                        runtime graph (negotiations, transfer processes),
                        in memory or persisted with RocksDB.
  control-plane-contreforts/
                        Contreforts connector over that store, with its
                        SHACL-validated declaration.ttl.
  control-plane-http/   axum HTTP server exposing the real DSP endpoint
                        paths on top of the store, and the
                        `ds-control-plane-rs` binary.
    tests/dsp_tck.rs    Runs the real, official Eclipse Dataspace TCK
                        (`eclipsedataspacetck/dsp-tck-runtime`) against an
                        in-process instance of this server.
vendor/
  contreforts-core/     Git submodule (pinned), as in ds-identity-hub-rs.
```

## Building and testing

`vendor/contreforts-core` is a git submodule: clone with
`--recurse-submodules`, or run `git submodule update --init` first. The
default build compiles RocksDB from source (needs a C++ toolchain and
`clang`).

```bash
cargo build --workspace
cargo test --workspace
```

Running the server:

```bash
cargo run -p control-plane-http
# well-known metadata: http://localhost:8080/.well-known/dspace-version
# DSP endpoints:        http://localhost:8080/dsp/2025-1/...
```

Configuration is via environment variables (`crates/control-plane-http/src/config.rs`):
`PORT` (default `8080`), `AGENT_ID`, `BASE_URL`, `DSP_PATH` (default
`/dsp/2025-1`), and `GRAPH_STORE_PATH`: a directory for the on-disk RDF
store, so the catalog and every negotiation and transfer process survive a
restart. Unset, everything is kept in memory.

### Dataspace Protocol TCK conformance

`crates/control-plane-http/tests/dsp_tck.rs` runs the real, official
[Eclipse Dataspace TCK](https://github.com/eclipse-dataspacetck/dsp-tck)
(`eclipsedataspacetck/dsp-tck-runtime` Docker image) against a real,
in-process instance of this server — not a mock, and not a
reimplementation of the TCK's own assertions. It is `#[ignore]`d by default
(requires Docker) so plain `cargo test` and the fast CI job stay
Docker-free:

```bash
cargo test -p control-plane-http --test dsp_tck -- --ignored --nocapture
```

See [`ARCHITECTURE.md`](ARCHITECTURE.md) for the exact, real pass/fail
counts from an actual run, and why this bootstrap's documented scope
implies a specific, asserted set of expected TCK failures rather than a
100%-pass claim.

## License

Apache-2.0. See [LICENSE](LICENSE).
