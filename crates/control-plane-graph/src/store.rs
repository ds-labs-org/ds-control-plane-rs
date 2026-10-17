//! The store itself and the small read/write helpers every record type
//! shares.
//!
//! Lookups never build SPARQL from request input: a `providerPid` or
//! dataset id arriving on an HTTP path is turned into a `NamedNode` (an
//! invalid IRI simply means "not found") and matched with
//! `quads_for_pattern`, so there is no query string for it to escape from.
//! Every other value a counterparty sends is stored as a plain literal.
//!
//! Writes replace a record wholesale: the record's node and every blank
//! node reachable from it are removed and the new triples inserted, in one
//! transaction, under a process-wide write lock so a read-modify-write
//! (`update_negotiation`, `update_transfer`) cannot interleave with
//! another one. Getters hold the same lock for reading: Oxigraph takes a
//! new snapshot on every `quads_for_pattern`, so without it a record read
//! during a rewrite could list blank children that are already gone.

use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use oxigraph::model::{
    BlankNode, GraphName, GraphNameRef, Literal, NamedNode, NamedOrBlankNode, NamedOrBlankNodeRef,
    Quad, Term,
};
use oxigraph::store::{StorageError, Store};

use crate::vocab::{dscp, rdf, xsd};

#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    #[error("invalid IRI: {0}")]
    Iri(#[from] oxigraph::model::IriParseError),
    #[error("store error: {0}")]
    Storage(#[from] StorageError),
    #[error("stored JSON value could not be read back: {0}")]
    Json(#[from] serde_json::Error),
    #[error("stored record {node} is missing required property <{property}>")]
    Missing { node: String, property: String },
    /// A write would overwrite a node that already holds a different kind
    /// of record (e.g. a dataset `@id` that names a data service or the
    /// settings node). Nothing was written.
    #[error("{node} cannot be written as a {kind}: {reason}")]
    Conflict {
        node: String,
        kind: String,
        reason: String,
    },
    #[error("stored record {node} has an unreadable <{property}>: {value}")]
    Invalid {
        node: String,
        property: String,
        value: String,
    },
}

/// The connector's RDF store: settings and runtime state in two named
/// graphs of one Oxigraph store. See `../../../ARCHITECTURE.md`,
/// "Contreforts: settings and runtime persistence".
pub struct ControlPlaneGraph {
    pub(crate) store: Store,
    /// Held for writing by every write and for reading by every public
    /// getter, so a getter decodes a whole record from one state of the
    /// store. Oxigraph takes a fresh snapshot per `quads_for_pattern`, and
    /// a record is read with many of them.
    lock: RwLock<()>,
}

impl ControlPlaneGraph {
    /// A fresh, empty store with no on-disk backing. Everything is lost when
    /// the process exits; used by tests and when no store path is
    /// configured.
    pub fn open_in_memory() -> Result<Self, GraphError> {
        Ok(Self {
            store: Store::new()?,
            lock: RwLock::new(()),
        })
    }

    /// Opens (creating it if needed) a RocksDB-backed store at `path`, so
    /// settings and runtime state survive a restart. Only one process may
    /// hold a given path open read-write at a time.
    #[cfg(feature = "rocksdb")]
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self, GraphError> {
        Ok(Self {
            store: Store::open(path)?,
            lock: RwLock::new(()),
        })
    }

    /// Flushes buffered writes to disk (a no-op for an in-memory store).
    #[cfg(feature = "rocksdb")]
    pub fn flush(&self) -> Result<(), GraphError> {
        Ok(self.store.flush()?)
    }

    /// Taken by every public write for its whole duration.
    pub(crate) fn write_guard(&self) -> RwLockWriteGuard<'_, ()> {
        self.lock.write().expect("graph lock poisoned")
    }

    /// Taken by every public getter for its whole duration, so its reads
    /// never interleave with a write. Never take it while holding either
    /// guard: `RwLock` is not reentrant.
    pub(crate) fn read_guard(&self) -> RwLockReadGuard<'_, ()> {
        self.lock.read().expect("graph lock poisoned")
    }

    /// The number of `class` instances in `graph`, without decoding any of
    /// them: cheap enough for startup logging and the seeding check, and
    /// never failed by one damaged record.
    pub(crate) fn count(&self, graph: &NamedNode, class: &str) -> Result<usize, GraphError> {
        let _guard = self.read_guard();
        Ok(self.reader(graph).instances(class)?.len())
    }

    /// Replaces the record rooted at `root` in `graph` with `quads`
    /// atomically: removes `root`'s own triples and every blank node
    /// reachable from it, then inserts `quads`.
    pub(crate) fn replace(
        &self,
        graph: &NamedNode,
        root: &NamedNode,
        quads: Vec<Quad>,
    ) -> Result<(), GraphError> {
        self.replace_all(graph, vec![(root.clone(), quads)])
    }

    /// [`Self::replace`] for several records in one transaction, so either
    /// every record is rewritten or none is. When a root appears more than
    /// once, its last entry wins.
    pub(crate) fn replace_all(
        &self,
        graph: &NamedNode,
        records: Vec<(NamedNode, Vec<Quad>)>,
    ) -> Result<(), GraphError> {
        let mut latest: Vec<(NamedNode, Vec<Quad>)> = Vec::new();
        for (root, quads) in records {
            latest.retain(|(r, _)| *r != root);
            latest.push((root, quads));
        }
        let mut old = Vec::new();
        for (root, _) in &latest {
            old.extend(self.tree(graph, root.as_ref().into())?);
        }
        let mut transaction = self.store.start_transaction()?;
        for quad in &old {
            transaction.remove(quad);
        }
        for (_, quads) in &latest {
            for quad in quads {
                transaction.insert(quad);
            }
        }
        transaction.commit()?;
        Ok(())
    }

    /// Once `graph` holds more than `max` instances of `class`, removes the
    /// least recently modified ones (by `dcterms:modified`, then IRI) and
    /// everything reachable from them, back down to `max`, in one
    /// transaction. Records for which `keep_longer` is true go only once
    /// every other record is gone. `written`, the record the caller has
    /// just stored, is never evicted, so a write cannot remove its own
    /// record. A record whose timestamp is unreadable counts as the
    /// oldest. Callers hold the write guard.
    pub(crate) fn evict_oldest(
        &self,
        graph: &NamedNode,
        class: &str,
        max: usize,
        written: NamedOrBlankNodeRef<'_>,
        keep_longer: impl Fn(&Reader<'_>, NamedOrBlankNodeRef<'_>) -> bool,
    ) -> Result<(), GraphError> {
        let r = self.reader(graph);
        let nodes = r.instances(class)?;
        let nodes_len = nodes.len();
        if nodes_len <= max {
            return Ok(());
        }
        let modified = crate::vocab::dcterms("modified");
        let mut aged = Vec::with_capacity(nodes.len());
        for node in nodes {
            if node.as_ref() == written {
                continue;
            }
            let at = match r.date_time(node.as_ref(), &modified) {
                Ok(at) => at,
                Err(e @ GraphError::Storage(_)) => return Err(e),
                Err(_) => chrono::DateTime::<chrono::Utc>::MIN_UTC,
            };
            aged.push((keep_longer(&r, node.as_ref()), at, node));
        }
        aged.sort_by(|(k, a, x), (l, b, y)| {
            k.cmp(l)
                .then_with(|| a.cmp(b))
                .then_with(|| x.to_string().cmp(&y.to_string()))
        });
        let excess = nodes_len - max;
        let mut old = Vec::new();
        for (_, _, node) in aged.into_iter().take(excess) {
            old.extend(self.tree(graph, node.as_ref())?);
        }
        let mut transaction = self.store.start_transaction()?;
        for quad in &old {
            transaction.remove(quad);
        }
        transaction.commit()?;
        Ok(())
    }

    /// Every quad in `graph` whose subject is `node` or a blank node
    /// reachable from it.
    fn tree(
        &self,
        graph: &NamedNode,
        node: NamedOrBlankNodeRef<'_>,
    ) -> Result<Vec<Quad>, GraphError> {
        let mut out = Vec::new();
        let mut pending = vec![NamedOrBlankNode::from(node)];
        while let Some(subject) = pending.pop() {
            for quad in self.store.quads_for_pattern(
                Some(subject.as_ref()),
                None,
                None,
                Some(graph.as_ref().into()),
            ) {
                let quad = quad?;
                if let Term::BlankNode(b) = &quad.object {
                    pending.push(b.clone().into());
                }
                out.push(quad);
            }
        }
        Ok(out)
    }

    pub(crate) fn reader<'a>(&'a self, graph: &'a NamedNode) -> Reader<'a> {
        Reader {
            store: &self.store,
            graph: graph.as_ref().into(),
        }
    }
}

/// Accumulates the quads for one record in one named graph.
pub(crate) struct Writer {
    graph: GraphName,
    pub(crate) quads: Vec<Quad>,
}

impl Writer {
    pub(crate) fn new(graph: &NamedNode) -> Self {
        Self {
            graph: graph.clone().into(),
            quads: Vec::new(),
        }
    }

    pub(crate) fn add(
        &mut self,
        subject: impl Into<NamedOrBlankNode>,
        predicate: String,
        object: impl Into<Term>,
    ) {
        self.quads.push(Quad::new(
            subject,
            iri(predicate),
            object,
            self.graph.clone(),
        ));
    }

    pub(crate) fn add_type(&mut self, subject: impl Into<NamedOrBlankNode>, class: String) {
        self.add(subject, rdf("type"), iri(class));
    }

    pub(crate) fn add_str(
        &mut self,
        subject: impl Into<NamedOrBlankNode>,
        predicate: String,
        value: &str,
    ) {
        self.add(subject, predicate, Literal::new_simple_literal(value));
    }

    pub(crate) fn add_opt_str(
        &mut self,
        subject: impl Into<NamedOrBlankNode>,
        predicate: String,
        value: Option<&str>,
    ) {
        if let Some(value) = value {
            self.add_str(subject, predicate, value);
        }
    }

    pub(crate) fn add_date_time(
        &mut self,
        subject: impl Into<NamedOrBlankNode>,
        predicate: String,
        value: chrono::DateTime<chrono::Utc>,
    ) {
        self.add(
            subject,
            predicate,
            Literal::new_typed_literal(value.to_rfc3339(), iri(xsd("dateTime"))),
        );
    }

    /// A fresh blank child of `parent`, linked by `predicate` and carrying
    /// `dscp:order index` so [`Reader::ordered_children`] can restore the
    /// original array order.
    pub(crate) fn add_child(
        &mut self,
        parent: impl Into<NamedOrBlankNode>,
        predicate: String,
        index: usize,
    ) -> BlankNode {
        let child = BlankNode::default();
        self.add(parent, predicate, child.clone());
        self.add(
            child.clone(),
            dscp("order"),
            Literal::new_typed_literal(index.to_string(), iri(xsd("integer"))),
        );
        child
    }
}

/// Reads properties of nodes in one named graph.
pub(crate) struct Reader<'a> {
    store: &'a Store,
    graph: GraphNameRef<'a>,
}

impl Reader<'_> {
    pub(crate) fn objects(
        &self,
        subject: NamedOrBlankNodeRef<'_>,
        predicate: &str,
    ) -> Result<Vec<Term>, GraphError> {
        let predicate = iri(predicate.to_string());
        let mut out = Vec::new();
        for quad in self.store.quads_for_pattern(
            Some(subject),
            Some(predicate.as_ref()),
            None,
            Some(self.graph),
        ) {
            out.push(quad?.object);
        }
        Ok(out)
    }

    /// Subjects in this graph with `rdf:type class`.
    pub(crate) fn instances(&self, class: &str) -> Result<Vec<NamedOrBlankNode>, GraphError> {
        let predicate = iri(rdf("type"));
        let class = iri(class.to_string());
        let mut out = Vec::new();
        for quad in self.store.quads_for_pattern(
            None,
            Some(predicate.as_ref()),
            Some(class.as_ref().into()),
            Some(self.graph),
        ) {
            out.push(quad?.subject);
        }
        Ok(out)
    }

    /// Every `rdf:type` of `subject` that is a named class.
    pub(crate) fn types(
        &self,
        subject: NamedOrBlankNodeRef<'_>,
    ) -> Result<Vec<String>, GraphError> {
        Ok(self
            .objects(subject, &rdf("type"))?
            .into_iter()
            .filter_map(|t| match t {
                Term::NamedNode(n) => Some(n.into_string()),
                _ => None,
            })
            .collect())
    }

    pub(crate) fn has_type(
        &self,
        subject: NamedOrBlankNodeRef<'_>,
        class: &str,
    ) -> Result<bool, GraphError> {
        Ok(self
            .objects(subject, &rdf("type"))?
            .iter()
            .any(|t| matches!(t, Term::NamedNode(n) if n.as_str() == class)))
    }

    /// The first value of `predicate` on `subject`, as a string: a
    /// literal's lexical value or a named node's IRI.
    pub(crate) fn opt_str(
        &self,
        subject: NamedOrBlankNodeRef<'_>,
        predicate: &str,
    ) -> Result<Option<String>, GraphError> {
        Ok(self
            .objects(subject, predicate)?
            .into_iter()
            .find_map(|t| match t {
                Term::Literal(l) => Some(l.value().to_string()),
                Term::NamedNode(n) => Some(n.into_string()),
                _ => None,
            }))
    }

    pub(crate) fn str(
        &self,
        subject: NamedOrBlankNodeRef<'_>,
        predicate: &str,
    ) -> Result<String, GraphError> {
        self.opt_str(subject, predicate)?
            .ok_or_else(|| GraphError::Missing {
                node: subject.to_string(),
                property: predicate.to_string(),
            })
    }

    pub(crate) fn date_time(
        &self,
        subject: NamedOrBlankNodeRef<'_>,
        predicate: &str,
    ) -> Result<chrono::DateTime<chrono::Utc>, GraphError> {
        let value = self.str(subject, predicate)?;
        chrono::DateTime::parse_from_rfc3339(&value)
            .map(|d| d.with_timezone(&chrono::Utc))
            .map_err(|_| GraphError::Invalid {
                node: subject.to_string(),
                property: predicate.to_string(),
                value,
            })
    }

    /// The subjects carrying `predicate` with the plain string `value`.
    pub(crate) fn subjects_with_str(
        &self,
        predicate: &str,
        value: &str,
    ) -> Result<Vec<NamedOrBlankNode>, GraphError> {
        let predicate = iri(predicate.to_string());
        let value = Literal::new_simple_literal(value);
        let mut out = Vec::new();
        for quad in self.store.quads_for_pattern(
            None,
            Some(predicate.as_ref()),
            Some(value.as_ref().into()),
            Some(self.graph),
        ) {
            out.push(quad?.subject);
        }
        Ok(out)
    }

    /// The subjects linking to `object` with `predicate`.
    pub(crate) fn subjects_linking_to(
        &self,
        predicate: &str,
        object: NamedOrBlankNodeRef<'_>,
    ) -> Result<Vec<NamedOrBlankNode>, GraphError> {
        let predicate = iri(predicate.to_string());
        let mut out = Vec::new();
        for quad in self.store.quads_for_pattern(
            None,
            Some(predicate.as_ref()),
            Some(object.into()),
            Some(self.graph),
        ) {
            out.push(quad?.subject);
        }
        Ok(out)
    }

    /// The node-valued objects of `predicate` on `subject`, sorted by their
    /// `dscp:order`.
    pub(crate) fn ordered_children(
        &self,
        subject: NamedOrBlankNodeRef<'_>,
        predicate: &str,
    ) -> Result<Vec<NamedOrBlankNode>, GraphError> {
        let mut children = Vec::new();
        for term in self.objects(subject, predicate)? {
            let node: NamedOrBlankNode = match term {
                Term::BlankNode(b) => b.into(),
                Term::NamedNode(n) => n.into(),
                _ => continue,
            };
            let order = self
                .opt_str(node.as_ref(), &dscp("order"))?
                .and_then(|o| o.parse::<u64>().ok())
                .unwrap_or(u64::MAX);
            children.push((order, node));
        }
        children.sort_by_key(|(order, _)| *order);
        Ok(children.into_iter().map(|(_, node)| node).collect())
    }
}

/// Decodes every node of a list read with `read`, skipping (and logging)
/// the ones that fail to decode instead of failing the whole list: one
/// damaged record must not hide every other one, nor stop the server from
/// starting. Storage errors still fail the list. Single-record lookups
/// keep reporting the damage.
pub(crate) fn readable<T>(
    kind: &str,
    nodes: &[NamedNode],
    mut read: impl FnMut(&NamedNode) -> Result<T, GraphError>,
) -> Result<Vec<T>, GraphError> {
    let mut out = Vec::with_capacity(nodes.len());
    for node in nodes {
        match read(node) {
            Ok(value) => out.push(value),
            Err(e @ GraphError::Storage(_)) => return Err(e),
            Err(error) => {
                tracing::warn!(%node, kind, %error, "skipping unreadable stored record");
            }
        }
    }
    Ok(out)
}

/// A `NamedNode` for one of this crate's own vocabulary IRIs, which are
/// all built from the constants in [`crate::vocab`] and known valid.
pub(crate) fn iri(value: String) -> NamedNode {
    NamedNode::new_unchecked(value)
}

/// Request input that is meant to be an IRI, e.g. a `providerPid` from an
/// HTTP path. `None` when it is not a valid IRI, which callers treat as
/// "no such record".
pub(crate) fn lookup_iri(value: &str) -> Option<NamedNode> {
    NamedNode::new(value).ok()
}
