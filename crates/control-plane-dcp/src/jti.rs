//! A capped `jti` replay cache, ported from `ds-identity-hub-rs`'s
//! `SeenJtiCache`. Tokens live minutes, so a fixed-capacity, oldest-evicted
//! set is enough in practice; a jti evicted early could be replayed only
//! while its token is still unexpired and only after [`DEFAULT_JTI_CAPACITY`]
//! newer tokens arrived.

use std::collections::{HashSet, VecDeque};

/// How many `jti` values the verifier remembers.
pub const DEFAULT_JTI_CAPACITY: usize = 16 * 1024;

pub struct SeenJtiCache {
    seen: HashSet<String>,
    order: VecDeque<String>,
    capacity: usize,
}

impl SeenJtiCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            seen: HashSet::new(),
            order: VecDeque::new(),
            capacity,
        }
    }

    /// The number of `jti` values currently remembered.
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }

    /// Records `jti`. `false` if it was already remembered (a replay).
    /// A fresh insert evicts the oldest entries past `capacity`, never the
    /// one just inserted.
    pub fn insert(&mut self, jti: &str) -> bool {
        if !self.seen.insert(jti.to_string()) {
            return false;
        }
        self.order.push_back(jti.to_string());
        while self.seen.len() > self.capacity {
            match self.order.pop_front() {
                Some(oldest) => {
                    self.seen.remove(&oldest);
                }
                None => break,
            }
        }
        true
    }
}
