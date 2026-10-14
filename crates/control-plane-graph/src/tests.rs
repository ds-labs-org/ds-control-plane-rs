use control_plane_core::catalog::{DataService, Dataset, Distribution};
use control_plane_core::common::{Agreement, Constraint, Offer, Rule};
use control_plane_core::negotiation::{ContractNegotiation, NegotiationState};
use control_plane_core::transfer::{TransferProcess, TransferState};

use super::*;

fn settings() -> ConnectorSettings {
    ConnectorSettings {
        agent_id: "urn:connector:test".to_string(),
        base_url: "http://localhost:8080".to_string(),
        dsp_path: "/dsp/2025-1".to_string(),
        catalog_id: "urn:uuid:test-catalog".to_string(),
        data_service_id: "urn:uuid:test-service".to_string(),
    }
}

fn constraint(left: &str, op: &str, right: serde_json::Value) -> Constraint {
    Constraint {
        left_operand: left.to_string(),
        operator: op.to_string(),
        right_operand: right,
    }
}

/// Two offers, several rules of each kind, constraints deliberately not in
/// sorted order, and non-string `rightOperand`s, so order and JSON
/// round-tripping are both actually exercised.
fn offer(id: &str) -> Offer {
    Offer {
        id: id.to_string(),
        type_: "Offer".to_string(),
        target: None,
        permission: vec![
            Rule {
                action: "use".to_string(),
                constraint: vec![
                    constraint("spatial", "eq", serde_json::json!("http://example.org/EU")),
                    constraint("count", "lteq", serde_json::json!(10)),
                    constraint("purpose", "isAnyOf", serde_json::json!(["a", "b"])),
                ],
            },
            Rule {
                action: "distribute".to_string(),
                constraint: vec![],
            },
        ],
        prohibition: vec![Rule {
            action: "sell".to_string(),
            constraint: vec![],
        }],
        obligation: vec![],
    }
}

fn dataset(id: &str, s: &ConnectorSettings) -> Dataset {
    Dataset {
        id: id.to_string(),
        type_: "Dataset".to_string(),
        has_policy: vec![
            offer(&format!("{id}#offer-1")),
            offer(&format!("{id}#offer-0")),
        ],
        distribution: vec![
            Distribution {
                type_: "Distribution".to_string(),
                format: "HttpData-PULL".to_string(),
                access_service: s.data_service(),
            },
            Distribution {
                type_: "Distribution".to_string(),
                format: "AmazonS3-PUSH".to_string(),
                access_service: s.data_service(),
            },
        ],
    }
}

#[test]
fn settings_round_trip_and_are_absent_until_written() {
    let graph = ControlPlaneGraph::open_in_memory().unwrap();
    assert_eq!(graph.settings().unwrap(), None);
    graph.put_settings(&settings()).unwrap();
    assert_eq!(graph.settings().unwrap(), Some(settings()));

    let mut changed = settings();
    changed.agent_id = "urn:connector:renamed".to_string();
    graph.put_settings(&changed).unwrap();
    assert_eq!(graph.settings().unwrap(), Some(changed));
}

#[test]
fn dataset_round_trips_with_policy_and_distribution_order() {
    let graph = ControlPlaneGraph::open_in_memory().unwrap();
    let s = settings();
    graph.put_settings(&s).unwrap();
    let ds = dataset("urn:uuid:dataset-a", &s);
    graph.put_dataset(&ds).unwrap();

    let back = graph
        .dataset("urn:uuid:dataset-a")
        .unwrap()
        .expect("stored");
    assert_eq!(
        serde_json::to_value(&back).unwrap(),
        serde_json::to_value(&ds).unwrap()
    );
}

#[test]
fn moving_the_base_url_updates_every_distribution_endpoint() {
    let graph = ControlPlaneGraph::open_in_memory().unwrap();
    let mut s = settings();
    graph.put_settings(&s).unwrap();
    graph
        .put_dataset(&dataset("urn:uuid:dataset-a", &s))
        .unwrap();

    s.base_url = "https://connector.example".to_string();
    graph.put_settings(&s).unwrap();

    let back = graph.dataset("urn:uuid:dataset-a").unwrap().unwrap();
    for d in back.distribution {
        assert_eq!(
            d.access_service.endpoint_url,
            "https://connector.example/dsp/2025-1"
        );
    }
}

#[test]
fn replacing_a_dataset_leaves_no_stale_triples() {
    let graph = ControlPlaneGraph::open_in_memory().unwrap();
    let s = settings();
    graph.put_settings(&s).unwrap();
    let ds = dataset("urn:uuid:dataset-a", &s);
    graph.put_dataset(&ds).unwrap();
    let size = graph.store.len().unwrap();
    graph.put_dataset(&ds).unwrap();
    assert_eq!(graph.store.len().unwrap(), size);

    assert!(graph.remove_dataset("urn:uuid:dataset-a").unwrap());
    assert!(graph.datasets().unwrap().is_empty());
    assert!(!graph.remove_dataset("urn:uuid:dataset-a").unwrap());
}

#[test]
fn datasets_lists_every_dataset_sorted_by_id() {
    let graph = ControlPlaneGraph::open_in_memory().unwrap();
    let s = settings();
    graph.put_dataset(&dataset("urn:uuid:b", &s)).unwrap();
    graph.put_dataset(&dataset("urn:uuid:a", &s)).unwrap();
    let ids: Vec<String> = graph
        .datasets()
        .unwrap()
        .into_iter()
        .map(|d| d.id)
        .collect();
    assert_eq!(ids, vec!["urn:uuid:a", "urn:uuid:b"]);
}

#[test]
fn lookups_with_non_iri_or_wrong_kind_ids_find_nothing() {
    let graph = ControlPlaneGraph::open_in_memory().unwrap();
    let s = settings();
    graph.put_settings(&s).unwrap();
    graph
        .put_dataset(&dataset("urn:uuid:dataset-a", &s))
        .unwrap();
    graph
        .put_negotiation(&ContractNegotiation::new("urn:uuid:n1", "c1"))
        .unwrap();

    assert!(graph.dataset("not an iri > {").unwrap().is_none());
    assert!(graph.negotiation("} INSERT DATA {").unwrap().is_none());
    // A real node of another kind is not mistaken for the one asked for.
    assert!(graph.dataset("urn:uuid:n1").unwrap().is_none());
    assert!(graph.negotiation("urn:uuid:dataset-a").unwrap().is_none());
    assert!(graph.transfer("urn:uuid:n1").unwrap().is_none());
}

fn negotiation() -> ContractNegotiation {
    let mut n = ContractNegotiation::new("urn:uuid:n1", "urn:uuid:c1");
    let mut o = offer("urn:uuid:offer");
    o.target = Some("urn:uuid:dataset-a".to_string());
    n.dataset_id = o.target.clone();
    n.offer = Some(o);
    n.agreement = Some(Agreement {
        id: "urn:uuid:agreement".to_string(),
        type_: "Agreement".to_string(),
        target: "urn:uuid:dataset-a".to_string(),
        timestamp: chrono::DateTime::parse_from_rfc3339("2026-10-14T12:00:00Z")
            .unwrap()
            .into(),
        assigner: "urn:connector:provider".to_string(),
        assignee: "urn:connector:consumer".to_string(),
        permission: vec![Rule {
            action: "use".to_string(),
            constraint: vec![],
        }],
        prohibition: vec![],
        obligation: vec![],
    });
    n
}

#[test]
fn negotiation_round_trips_with_offer_and_agreement() {
    let graph = ControlPlaneGraph::open_in_memory().unwrap();
    let n = negotiation();
    graph.put_negotiation(&n).unwrap();
    let back = graph.negotiation("urn:uuid:n1").unwrap().expect("stored");
    assert_eq!(back.provider_pid, n.provider_pid);
    assert_eq!(back.consumer_pid, n.consumer_pid);
    assert_eq!(back.state, n.state);
    assert_eq!(back.dataset_id, n.dataset_id);
    assert_eq!(back.offer, n.offer);
    assert_eq!(back.agreement, n.agreement);
}

#[test]
fn state_is_stored_as_the_dsp_state_iri() {
    let graph = ControlPlaneGraph::open_in_memory().unwrap();
    graph.put_negotiation(&negotiation()).unwrap();
    let state =
        oxigraph::model::NamedNode::new("https://w3id.org/dspace/2025/1/REQUESTED").unwrap();
    let hits = graph
        .store
        .quads_for_pattern(None, None, Some(state.as_ref().into()), None)
        .count();
    assert_eq!(hits, 1);
}

#[test]
fn update_writes_back_only_on_ok() {
    let graph = ControlPlaneGraph::open_in_memory().unwrap();
    graph.put_negotiation(&negotiation()).unwrap();

    let rejected = graph
        .update_negotiation("urn:uuid:n1", |n| n.transition(NegotiationState::Finalized))
        .unwrap()
        .expect("exists");
    assert!(rejected.is_err());
    assert_eq!(
        graph.negotiation("urn:uuid:n1").unwrap().unwrap().state,
        NegotiationState::Requested
    );

    graph
        .update_negotiation("urn:uuid:n1", |n| n.transition(NegotiationState::Agreed))
        .unwrap()
        .expect("exists")
        .expect("valid transition");
    let back = graph.negotiation("urn:uuid:n1").unwrap().unwrap();
    assert_eq!(back.state, NegotiationState::Agreed);
    assert_eq!(
        back.offer,
        negotiation().offer,
        "untouched fields survive an update"
    );

    assert!(
        graph
            .update_negotiation("urn:uuid:missing", |_| Ok::<(), ()>(()))
            .unwrap()
            .is_none()
    );
}

#[test]
fn transfer_round_trips_and_updates() {
    let graph = ControlPlaneGraph::open_in_memory().unwrap();
    let t = TransferProcess::new(
        "urn:uuid:t1",
        "urn:uuid:c1",
        "urn:uuid:agreement",
        "HttpData-PULL",
    );
    graph.put_transfer(&t).unwrap();
    graph
        .update_transfer("urn:uuid:t1", |t| t.transition(TransferState::Started))
        .unwrap()
        .unwrap()
        .unwrap();
    let back = graph.transfer("urn:uuid:t1").unwrap().unwrap();
    assert_eq!(back.state, TransferState::Started);
    assert_eq!(back.agreement_id, "urn:uuid:agreement");
    assert_eq!(back.format, "HttpData-PULL");
    assert_eq!(graph.transfers().unwrap().len(), 1);
    assert!(graph.negotiations().unwrap().is_empty());
}

#[test]
fn records_carry_a_modified_timestamp() {
    let graph = ControlPlaneGraph::open_in_memory().unwrap();
    let before = chrono::Utc::now();
    graph.put_negotiation(&negotiation()).unwrap();
    let records = graph.negotiations().unwrap();
    assert_eq!(records.len(), 1);
    assert!(records[0].modified >= before - chrono::Duration::seconds(1));
}

#[cfg(feature = "rocksdb")]
#[test]
fn everything_survives_reopening_an_on_disk_store() {
    let dir = tempfile::tempdir().unwrap();
    let s = settings();
    {
        let graph = ControlPlaneGraph::open(dir.path()).unwrap();
        graph.put_settings(&s).unwrap();
        graph
            .put_dataset(&dataset("urn:uuid:dataset-a", &s))
            .unwrap();
        graph.put_negotiation(&negotiation()).unwrap();
        graph
            .put_transfer(&TransferProcess::new(
                "urn:uuid:t1",
                "c1",
                "a1",
                "HttpData-PULL",
            ))
            .unwrap();
        graph.flush().unwrap();
    }
    let graph = ControlPlaneGraph::open(dir.path()).unwrap();
    assert_eq!(graph.settings().unwrap(), Some(s));
    assert!(graph.dataset("urn:uuid:dataset-a").unwrap().is_some());
    assert_eq!(
        graph.negotiation("urn:uuid:n1").unwrap().unwrap().agreement,
        negotiation().agreement
    );
    assert!(graph.transfer("urn:uuid:t1").unwrap().is_some());
}

#[test]
fn data_service_is_readable_by_id() {
    let graph = ControlPlaneGraph::open_in_memory().unwrap();
    graph.put_settings(&settings()).unwrap();
    let service: DataService = graph
        .data_service("urn:uuid:test-service")
        .unwrap()
        .unwrap();
    assert_eq!(service.endpoint_url, "http://localhost:8080/dsp/2025-1");
    assert!(graph.data_service("urn:uuid:nope").unwrap().is_none());
}

/// A store with settings and two datasets, `urn:ds:a` and `urn:ds:b`.
fn seeded() -> (ControlPlaneGraph, ConnectorSettings) {
    let s = settings();
    let graph = ControlPlaneGraph::open_in_memory().unwrap();
    graph.put_settings(&s).unwrap();
    graph.put_dataset(&dataset("urn:ds:a", &s)).unwrap();
    graph.put_dataset(&dataset("urn:ds:b", &s)).unwrap();
    (graph, s)
}

/// `put` must be rejected and leave settings and every dataset exactly as
/// they were, including after the startup `put_settings` runs again.
fn assert_rejected_without_damage(
    graph: &ControlPlaneGraph,
    s: &ConnectorSettings,
    put: Result<(), GraphError>,
) {
    let before = (Some(s.clone()), {
        let fresh = ControlPlaneGraph::open_in_memory().unwrap();
        fresh.put_settings(s).unwrap();
        fresh.put_dataset(&dataset("urn:ds:a", s)).unwrap();
        fresh.put_dataset(&dataset("urn:ds:b", s)).unwrap();
        serde_json::to_value(fresh.datasets().unwrap()).unwrap()
    });
    let now = |g: &ControlPlaneGraph| {
        (
            g.settings().unwrap(),
            serde_json::to_value(g.datasets().unwrap()).unwrap(),
        )
    };
    assert!(put.is_err(), "colliding write was accepted");
    assert_eq!(now(graph), before);
    graph.put_settings(s).unwrap();
    assert_eq!(now(graph), before);
}

#[test]
fn access_service_id_equal_to_another_dataset_is_rejected() {
    let (graph, s) = seeded();
    let mut c = dataset("urn:ds:c", &s);
    c.distribution[1].access_service.id = "urn:ds:a".to_string();
    let put = graph.put_dataset(&c);
    assert_rejected_without_damage(&graph, &s, put);
}

#[test]
fn dataset_or_access_service_id_equal_to_the_settings_node_is_rejected() {
    let (graph, s) = seeded();
    let put = graph.put_dataset(&dataset(crate::vocab::SETTINGS_NODE, &s));
    assert_rejected_without_damage(&graph, &s, put);

    let mut c = dataset("urn:ds:c", &s);
    c.distribution[0].access_service.id = crate::vocab::SETTINGS_NODE.to_string();
    let put = graph.put_dataset(&c);
    assert_rejected_without_damage(&graph, &s, put);
}

#[test]
fn dataset_id_equal_to_a_data_service_is_rejected() {
    let (graph, s) = seeded();
    let put = graph.put_dataset(&dataset(&s.data_service_id, &s));
    assert_rejected_without_damage(&graph, &s, put);
}

#[test]
fn access_service_id_equal_to_its_own_dataset_is_rejected() {
    let (graph, s) = seeded();
    let mut c = dataset("urn:ds:c", &s);
    c.distribution[0].access_service.id = "urn:ds:c".to_string();
    let put = graph.put_dataset(&c);
    assert_rejected_without_damage(&graph, &s, put);
}

#[test]
fn settings_whose_service_id_is_a_dataset_are_rejected() {
    let (graph, s) = seeded();
    let moved = ConnectorSettings {
        data_service_id: "urn:ds:a".to_string(),
        ..s.clone()
    };
    let put = graph.put_settings(&moved);
    assert_rejected_without_damage(&graph, &s, put);
}

/// Readers must never see a record half-rewritten: `replace` swaps every
/// blank node of an offer for a fresh one, so a read that lists the old
/// children and then looks up their properties after the swap would fail
/// with `GraphError::Missing`.
#[test]
fn reads_concurrent_with_rewrites_see_whole_records() {
    let graph = ControlPlaneGraph::open_in_memory().unwrap();
    let s = settings();
    graph.put_settings(&s).unwrap();
    graph.put_negotiation(&negotiation()).unwrap();
    graph.put_dataset(&dataset("urn:uuid:d1", &s)).unwrap();
    let done = std::sync::atomic::AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            for _ in 0..300 {
                graph
                    .update_negotiation("urn:uuid:n1", |_| Ok::<(), ()>(()))
                    .unwrap()
                    .unwrap()
                    .unwrap();
                graph.put_dataset(&dataset("urn:uuid:d1", &s)).unwrap();
            }
            done.store(true, std::sync::atomic::Ordering::Release);
        });
        let mut failures = Vec::new();
        while !done.load(std::sync::atomic::Ordering::Acquire) {
            match graph.negotiation("urn:uuid:n1") {
                Ok(Some(_)) => {}
                other => failures.push(format!("negotiation: {other:?}")),
            }
            if let Err(e) = graph.negotiations() {
                failures.push(format!("negotiations: {e}"));
            }
            match graph.dataset("urn:uuid:d1") {
                Ok(Some(_)) => {}
                other => failures.push(format!("dataset: {other:?}")),
            }
            if let Err(e) = graph.datasets() {
                failures.push(format!("datasets: {e}"));
            }
        }
        assert!(
            failures.is_empty(),
            "{} torn reads, first: {}",
            failures.len(),
            failures[0]
        );
    });
}
