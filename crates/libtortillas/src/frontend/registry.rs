use std::{hash::Hash, sync::Arc};

use dashmap::DashMap;

/// Guard-free facade over sharded keyed scope ownership.
///
/// Registry guards never escape this type: callers receive cloned `Arc`s or
/// owned vectors, so actor communication and async work cannot accidentally
/// retain a DashMap shard lock.
#[derive(Debug)]
pub(crate) struct ScopeRegistry<K: Eq + Hash, V> {
   values: DashMap<K, Arc<V>>,
}

impl<K, V> ScopeRegistry<K, V>
where
   K: Clone + Eq + Hash,
{
   pub(crate) fn new() -> Self {
      Self {
         values: DashMap::new(),
      }
   }

   pub(crate) fn insert(&self, key: K, value: &Arc<V>) {
      self.values.insert(key, Arc::clone(value));
   }

   pub(crate) fn get_or_insert_with(&self, key: K, create: impl FnOnce() -> V) -> Arc<V> {
      if let Some(value) = self.get(&key) {
         return value;
      }

      // Construct before entering the shard so arbitrary initialization never
      // runs while a DashMap lock is held. A racing insertion may make this
      // allocation unused, which is preferable to extending the lock lifetime.
      let candidate = Arc::new(create());
      Arc::clone(self.values.entry(key).or_insert(candidate).value())
   }

   pub(crate) fn get(&self, key: &K) -> Option<Arc<V>> {
      self.values.get(key).map(|value| Arc::clone(value.value()))
   }

   pub(crate) fn remove(&self, key: &K) -> Option<Arc<V>> {
      self.values.remove(key).map(|(_, value)| value)
   }

   pub(crate) fn values(&self) -> Vec<Arc<V>> {
      self
         .values
         .iter()
         .map(|value| Arc::clone(value.value()))
         .collect()
   }

   pub(crate) fn remove_all(&self) -> Vec<Arc<V>> {
      let keys = self
         .values
         .iter()
         .map(|entry| entry.key().clone())
         .collect::<Vec<_>>();
      keys
         .into_iter()
         .filter_map(|key| self.remove(&key))
         .collect()
   }
}
