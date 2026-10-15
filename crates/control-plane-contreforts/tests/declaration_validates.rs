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
/// through the declaration finds a class for every one of them, and the
/// declaration advertises no kind the connector would reject. Parsed as
/// RDF, so a kind mentioned only in a comment does not count.
#[test]
fn declaration_names_exactly_the_kinds_the_connector_serves() {
    use oxigraph::io::{RdfFormat, RdfParser};
    use oxigraph::model::Term;

    const ENTITY_KIND: &str = "https://contreforts.ds-labs.org/ontologies/declaration#entityKind";
    let declared: std::collections::BTreeSet<String> = RdfParser::from_format(RdfFormat::Turtle)
        .for_slice(DECLARATION_TTL.as_bytes())
        .map(|quad| quad.expect("declaration.ttl parses as Turtle"))
        .filter(|quad| quad.predicate.as_str() == ENTITY_KIND)
        .map(|quad| match quad.object {
            Term::Literal(l) => l.value().to_string(),
            other => panic!("entityKind must be a literal, got {other}"),
        })
        .collect();
    let served: std::collections::BTreeSet<String> = control_plane_contreforts::KINDS
        .iter()
        .map(|k| k.to_string())
        .collect();
    assert_eq!(declared, served);
}
