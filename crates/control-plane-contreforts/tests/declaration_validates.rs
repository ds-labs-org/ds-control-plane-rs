//! Checks this connector's `declaration.ttl` against Contreforts' own
//! meta-shapes and lints, the same check `ds-identity-hub-rs` and
//! `ds-sql-dps-rs` run on theirs.

const DECLARATION_TTL: &str = include_str!("../src/declaration.ttl");

#[test]
fn declaration_validates_against_contreforts_meta_shapes() {
    match contreforts_declaration::validate(DECLARATION_TTL) {
        Ok(_declaration) => {}
        Err(violations) => panic!("declaration.ttl failed validation:\n{violations}"),
    }
}

/// The kinds the connector answers to are exactly the kinds its
/// declaration names, so a driver resolving a pulled document's kind
/// through the declaration finds a class for every one of them.
#[test]
fn declaration_names_every_kind_the_connector_serves() {
    for kind in control_plane_contreforts::KINDS {
        assert!(
            DECLARATION_TTL.contains(&format!("contreforts:entityKind \"{kind}\"")),
            "declaration.ttl does not declare entity kind {kind}"
        );
    }
}
