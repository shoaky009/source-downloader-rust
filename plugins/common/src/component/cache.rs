use moka::sync::Cache;
use std::hash::Hash;
use std::time::Duration;

const CACHE_CAPACITY: u64 = 500;
const CACHE_IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

pub(super) fn new_cache<K, V>() -> Cache<K, V>
where
    K: Eq + Hash + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    Cache::builder().max_capacity(CACHE_CAPACITY).time_to_idle(CACHE_IDLE_TIMEOUT).build()
}
