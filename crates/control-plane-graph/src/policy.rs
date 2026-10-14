//! ODRL policies (`Offer`, `Agreement`) as RDF: a policy node typed
//! `odrl:Offer`/`odrl:Agreement`, its rules as `odrl:permission`/
//! `odrl:prohibition`/`odrl:obligation` children, each rule's constraints
//! as `odrl:constraint` children, all carrying `dscp:order`.
//!
//! A policy's `@id` is stored as the literal `dscp:policyId` on a blank
//! node rather than used as the node's IRI: an offer id arrives from the
//! counterparty, so two negotiations quoting the same id must not end up
//! sharing (and merging) one node. `rightOperand` is any JSON value on the
//! wire, so it is stored as an `rdf:JSON` literal and round-trips exactly.

use control_plane_core::common::{Agreement, Constraint, Offer, Rule};
use oxigraph::model::{BlankNode, Literal, NamedOrBlankNode, NamedOrBlankNodeRef};

use crate::store::{GraphError, Reader, Writer, iri};
use crate::vocab::{dscp, dspace, odrl, rdf};

const RULE_KINDS: [&str; 3] = ["permission", "prohibition", "obligation"];

fn write_rules(w: &mut Writer, policy: &BlankNode, kind: &str, rules: &[Rule]) {
    for (i, rule) in rules.iter().enumerate() {
        let r = w.add_child(policy.clone(), odrl(kind), i);
        w.add_type(r.clone(), odrl(&capitalize(kind)));
        w.add_str(r.clone(), odrl("action"), &rule.action);
        for (j, c) in rule.constraint.iter().enumerate() {
            let node = w.add_child(r.clone(), odrl("constraint"), j);
            w.add_str(node.clone(), odrl("leftOperand"), &c.left_operand);
            w.add_str(node.clone(), odrl("operator"), &c.operator);
            w.add(
                node,
                odrl("rightOperand"),
                Literal::new_typed_literal(c.right_operand.to_string(), iri(rdf("JSON"))),
            );
        }
    }
}

fn read_rules(
    r: &Reader<'_>,
    policy: NamedOrBlankNodeRef<'_>,
    kind: &str,
) -> Result<Vec<Rule>, GraphError> {
    let mut rules = Vec::new();
    for rule in r.ordered_children(policy, &odrl(kind))? {
        let mut constraint = Vec::new();
        for c in r.ordered_children(rule.as_ref(), &odrl("constraint"))? {
            let right = r.str(c.as_ref(), &odrl("rightOperand"))?;
            constraint.push(Constraint {
                left_operand: r.str(c.as_ref(), &odrl("leftOperand"))?,
                operator: r.str(c.as_ref(), &odrl("operator"))?,
                right_operand: serde_json::from_str(&right)?,
            });
        }
        rules.push(Rule {
            action: r.str(rule.as_ref(), &odrl("action"))?,
            constraint,
        });
    }
    Ok(rules)
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Writes `offer` as a new child of `parent` via `predicate`.
pub(crate) fn write_offer(
    w: &mut Writer,
    parent: impl Into<NamedOrBlankNode>,
    predicate: String,
    index: usize,
    offer: &Offer,
) {
    let p = w.add_child(parent, predicate, index);
    w.add_type(p.clone(), odrl("Offer"));
    w.add_str(p.clone(), dscp("policyId"), &offer.id);
    w.add_opt_str(p.clone(), odrl("target"), offer.target.as_deref());
    for (kind, rules) in
        RULE_KINDS
            .iter()
            .zip([&offer.permission, &offer.prohibition, &offer.obligation])
    {
        write_rules(w, &p, kind, rules);
    }
}

pub(crate) fn read_offer(
    r: &Reader<'_>,
    node: NamedOrBlankNodeRef<'_>,
) -> Result<Offer, GraphError> {
    Ok(Offer {
        id: r.str(node, &dscp("policyId"))?,
        type_: "Offer".to_string(),
        target: r.opt_str(node, &odrl("target"))?,
        permission: read_rules(r, node, "permission")?,
        prohibition: read_rules(r, node, "prohibition")?,
        obligation: read_rules(r, node, "obligation")?,
    })
}

pub(crate) fn write_agreement(
    w: &mut Writer,
    parent: impl Into<NamedOrBlankNode>,
    predicate: String,
    agreement: &Agreement,
) {
    let p = w.add_child(parent, predicate, 0);
    w.add_type(p.clone(), odrl("Agreement"));
    w.add_str(p.clone(), dscp("policyId"), &agreement.id);
    w.add_str(p.clone(), odrl("target"), &agreement.target);
    w.add_date_time(p.clone(), dspace("timestamp"), agreement.timestamp);
    w.add_str(p.clone(), odrl("assigner"), &agreement.assigner);
    w.add_str(p.clone(), odrl("assignee"), &agreement.assignee);
    for (kind, rules) in RULE_KINDS.iter().zip([
        &agreement.permission,
        &agreement.prohibition,
        &agreement.obligation,
    ]) {
        write_rules(w, &p, kind, rules);
    }
}

pub(crate) fn read_agreement(
    r: &Reader<'_>,
    node: NamedOrBlankNodeRef<'_>,
) -> Result<Agreement, GraphError> {
    Ok(Agreement {
        id: r.str(node, &dscp("policyId"))?,
        type_: "Agreement".to_string(),
        target: r.str(node, &odrl("target"))?,
        timestamp: r.date_time(node, &dspace("timestamp"))?,
        assigner: r.str(node, &odrl("assigner"))?,
        assignee: r.str(node, &odrl("assignee"))?,
        permission: read_rules(r, node, "permission")?,
        prohibition: read_rules(r, node, "prohibition")?,
        obligation: read_rules(r, node, "obligation")?,
    })
}
