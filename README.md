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
  — real DSP message shapes, a single in-memory seed dataset, no filtering.
- **Contract negotiation**: the full Provider-role HTTP surface
  (`negotiations/request`, `.../request`, `.../events`,
  `.../agreement/verification`, `.../termination`) and a
  `ContractNegotiation` state machine matching the spec's state
  descriptions. No automatic agreement/finalization logic — advancing a
  negotiation past `REQUESTED` currently requires an external actor driving
  the same HTTP surface a real counter-party would.
- **Transfer process**: the full Provider-role HTTP surface (`transfers/request`,
  `.../start`, `.../completion`, `.../termination`, `.../suspension`) and a
  `TransferProcess` state machine matching the spec's state descriptions.
  No cross-check against a real Agreement store, no data plane wiring.
- **Policy**: an always-allow stub — no ODRL constraint evaluation.
- **Consumer role**: out of scope for this pass (no consumer HTTP surface,
  no outbound negotiation/transfer initiation).
- **OID4VCI holder**: the connector can obtain key-bound Verifiable
  Credentials for itself from an
  [OpenID for Verifiable Credential Issuance 1.0](https://openid.net/specs/openid-4-verifiable-credential-issuance-1_0.html)
  issuer, using the Pre-Authorized Code flow, through an API-key-gated
  management API. See "OID4VCI holder" below.

## Layout

```
crates/
  control-plane-core/   Domain types: DCAT/ODRL-flavored catalog vocabulary,
                        the ContractNegotiation and TransferProcess state
                        machines, and their DSP wire message shapes.
  control-plane-http/   axum HTTP server exposing the real DSP endpoint
                        paths, an in-memory store, the OID4VCI holder
                        management API, and the `ds-control-plane-rs` binary.
  control-plane-oid4vci/ OID4VCI 1.0 holder client: offer parsing, issuer
                        and authorization server metadata, pre-authorized
                        code token exchange, nonce + ES256 key proofs,
                        credential, deferred and notification endpoints.
    tests/dsp_tck.rs    Runs the real, official Eclipse Dataspace TCK
                        (`eclipsedataspacetck/dsp-tck-runtime`) against an
                        in-process instance of this server.
```

## Building and testing

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
`/dsp/2025-1`).

### OID4VCI holder

Off unless `MANAGEMENT_API_KEY` is set; every route then requires it in
`X-Api-Key` (these routes make outbound requests to issuer URLs taken from
the offer, so they are never exposed unauthenticated).
`OID4VCI_HOLDER_JWK` takes the holder's private P-256 key as an EC JWK;
without it a key is generated at startup and lost on restart.

```bash
MANAGEMENT_API_KEY=secret cargo run -p control-plane-http

# Accept an offer (offer URI, or the Credential Offer JSON object itself)
curl -H 'X-Api-Key: secret' -H 'Content-Type: application/json' \
  -d '{"offer": "openid-credential-offer://?credential_offer_uri=...", "tx_code": "1234"}' \
  http://localhost:8080/management/oid4vci/offers

curl -H 'X-Api-Key: secret' http://localhost:8080/management/oid4vci/credentials
```

| Route | Purpose |
|---|---|
| `GET /management/oid4vci/holder` | Holder public JWK and `did:jwk` |
| `POST /management/oid4vci/offers` | Accept a Credential Offer (`offer`, optional `tx_code`, optional `credential_configuration_ids`) |
| `GET /management/oid4vci/credentials[/{id}]` | Credentials received |
| `GET /management/oid4vci/deferred` | Pending deferred issuances |
| `POST /management/oid4vci/deferred/{id}` | Poll one deferred issuance (`202` pending, `200` issued) |

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
