//! Compiled pattern cache and its limits.
//!
//! [`ResultCache`] is `processScriptCache` (`src/ts/process/scripts.ts`), a `ByteBudgetLru` keyed by
//! `generateScriptCacheKey`; it persists across runs like the TS module state and is bounded by its own limits, not by
//! a run's `Budget`. [`PatternCache`] keeps compiled patterns, which only saves work.

use crate::engine::{CompileError, CompileLimits, CompiledPattern, DEFAULT_COMPILE_LIMITS};
use risunest_ir::budget::Budget;
use risunest_ir::stage::StageError;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// `scriptResultCacheBytes` and `scriptResultCacheEntries` of `getRuntimePerformanceBudgets()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResultLimits { pub max_bytes: usize, pub max_entries: usize }

impl ResultLimits {
    pub const NORMAL: Self = Self { max_bytes: 8 * 1024 * 1024, max_entries: 1000 };
    pub const LOW_SPEC: Self = Self { max_bytes: 2 * 1024 * 1024, max_entries: 250 };
}

struct Entry<V> { value: V, tick: u64 }

/// A least-recently-used map ordered by access ticks.
struct Lru<K: ?Sized, V> { entries: HashMap<Arc<K>, Entry<V>>, order: BTreeMap<u64, Arc<K>>, tick: u64 }

impl<K: Eq + std::hash::Hash + ?Sized, V> Lru<K, V> {
    fn new() -> Self { Self { entries: HashMap::new(), order: BTreeMap::new(), tick: 0 } }
    fn next_tick(&mut self) -> u64 { self.tick += 1; self.tick }

    fn get(&mut self, key: &K) -> Option<&V> {
        let tick = self.next_tick();
        let (stored, entry) = self.entries.get_key_value(key)?;
        let stored = stored.clone();
        let previous = entry.tick;
        self.order.remove(&previous);
        self.order.insert(tick, stored);
        let entry = self.entries.get_mut(key).expect("entry");
        entry.tick = tick;
        Some(&entry.value)
    }

    fn remove(&mut self, key: &K) -> Option<V> {
        let entry = self.entries.remove(key)?;
        self.order.remove(&entry.tick);
        Some(entry.value)
    }

    fn insert(&mut self, key: Arc<K>, value: V) {
        let tick = self.next_tick();
        self.order.insert(tick, key.clone());
        self.entries.insert(key, Entry { value, tick });
    }

    fn pop_oldest(&mut self) -> Option<(Arc<K>, V)> {
        let (_, key) = self.order.pop_first()?;
        let entry = self.entries.remove(&key).expect("ordered entry");
        Some((key, entry.value))
    }

    fn contains(&self, key: &K) -> bool { self.entries.contains_key(key) }
    fn len(&self) -> usize { self.entries.len() }
}

/// `ByteBudgetLru<string, CachedScriptResult>` measured as `2 * (key.length + data.length + reads)`. The reads term
/// counts chat variable names CBS read; the runner records none, so it is 0.
pub struct ResultCache { limits: ResultLimits, entries: Lru<[u16], Vec<u16>>, retained: usize }

fn measure(key: &[u16], data: &[u16]) -> usize { 2 * (key.len() + data.len()) }

impl ResultCache {
    pub fn new(limits: ResultLimits) -> Self { Self { limits, entries: Lru::new(), retained: 0 } }
    pub fn limits(&self) -> ResultLimits { self.limits }
    pub fn len(&self) -> usize { self.entries.len() }
    pub fn is_empty(&self) -> bool { self.entries.len() == 0 }
    pub fn retained_bytes(&self) -> usize { self.retained }

    /// `get`: a hit becomes the most recent entry.
    pub fn get(&mut self, key: &[u16]) -> Option<&[u16]> { self.entries.get(key).map(Vec::as_slice) }

    /// `set`: replaces an existing entry, refuses one larger than the budget, then evicts the oldest entries.
    pub fn set(&mut self, key: &[u16], data: Vec<u16>) -> bool {
        let bytes = measure(key, &data);
        if let Some(existing) = self.entries.remove(key) { self.retained -= measure(key, &existing); }
        if self.limits.max_bytes == 0 || bytes > self.limits.max_bytes { return false; }
        self.entries.insert(Arc::from(key), data);
        self.retained += bytes;
        while self.retained > self.limits.max_bytes || self.entries.len() > self.limits.max_entries {
            let Some((oldest, value)) = self.entries.pop_oldest() else { break };
            self.retained -= measure(&oldest, &value);
        }
        self.entries.contains(key)
    }

    /// `resetScriptCache`.
    pub fn clear(&mut self) { self.entries = Lru::new(); self.retained = 0; }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PatternLimits {
    pub max_entries: usize,
    /// Twice the source units plus the flags and the compiled size, summed over the entries.
    pub max_bytes: usize,
    pub compile: CompileLimits,
}

impl Default for PatternLimits {
    fn default() -> Self { Self { max_entries: 512, max_bytes: 64 << 20, compile: DEFAULT_COMPILE_LIMITS } }
}

pub type Compiled = Result<Arc<CompiledPattern>, CompileError>;

struct CachedPattern { compiled: Compiled, bytes: usize }

/// Compiled patterns by `(source, flags)`, including the ones the constructor rejects, measured by their source and
/// compiled sizes.
pub struct PatternCache { limits: PatternLimits, entries: Lru<(Vec<u16>, String), CachedPattern>, bytes: usize }

impl PatternCache {
    pub fn new(limits: PatternLimits) -> Self { Self { limits, entries: Lru::new(), bytes: 0 } }
    pub fn len(&self) -> usize { self.entries.len() }
    pub fn is_empty(&self) -> bool { self.entries.len() == 0 }
    pub fn retained_bytes(&self) -> usize { self.bytes }

    /// The compiled pattern and whether it came from the cache. A miss charges the source to `budget` as work.
    pub fn get_or_compile(&mut self, pattern: &[u16], flags: &str, budget: &Budget) -> Result<(Compiled, bool), StageError> {
        let key = (pattern.to_vec(), flags.to_string());
        if let Some(cached) = self.entries.get(&key) { return Ok((cached.compiled.clone(), true)); }
        budget.work(pattern.len() + flags.len())?;
        let compiled = CompiledPattern::compile_bounded(pattern, flags, self.limits.compile).map(Arc::new);
        let bytes = 2 * pattern.len() + flags.len() + compiled.as_ref().map_or(0, |compiled| compiled.compiled_bytes());
        if bytes <= self.limits.max_bytes {
            self.entries.insert(Arc::new(key), CachedPattern { compiled: compiled.clone(), bytes });
            self.bytes += bytes;
            while self.bytes > self.limits.max_bytes || self.entries.len() > self.limits.max_entries {
                let Some((_, oldest)) = self.entries.pop_oldest() else { break };
                self.bytes -= oldest.bytes;
            }
        }
        Ok((compiled, false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn units(value: &str) -> Vec<u16> { value.encode_utf16().collect() }

    #[test]
    fn result_cache_follows_byte_budget_lru() {
        let mut cache = ResultCache::new(ResultLimits { max_bytes: 40, max_entries: 2 });
        assert!(cache.set(&units("a"), units("1")));
        assert!(cache.set(&units("b"), units("2")));
        assert!(cache.get(&units("a")).is_some());
        assert!(cache.set(&units("c"), units("3")));
        assert!(cache.get(&units("b")).is_none(), "the least recent entry is evicted");
        assert_eq!((cache.len(), cache.retained_bytes()), (2, 8));
        assert!(!cache.set(&units("a"), units("01234567890123456789")), "an entry over the budget is refused");
        assert!(cache.get(&units("a")).is_none(), "and the replaced entry is gone");
        assert!(cache.set(&units("d"), units("012345678901234567")));
        assert_eq!((cache.len(), cache.retained_bytes()), (1, 38));
    }

    #[test]
    fn pattern_cache_bounds_entries_and_keeps_rejections() {
        let budget = crate::engine::tests::budget();
        let mut cache = PatternCache::new(PatternLimits { max_entries: 2, max_bytes: 1 << 20, compile: DEFAULT_COMPILE_LIMITS });
        let lookup = |cache: &mut PatternCache, pattern: &str| cache.get_or_compile(&units(pattern), "g", &budget).unwrap();
        assert!(!lookup(&mut cache, "(").1);
        assert!(lookup(&mut cache, "(").1);
        assert!(matches!(lookup(&mut cache, "(").0, Err(CompileError::Syntax)));
        let _ = lookup(&mut cache, "a");
        let _ = lookup(&mut cache, "b");
        assert!(!lookup(&mut cache, "(").1);
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn pattern_cache_counts_compiled_bytes_and_refuses_past_the_compile_limits() {
        let budget = crate::engine::tests::budget();
        let words = (0..400).map(|index| format!("w{index}")).collect::<Vec<_>>().join("|");
        let compiled = CompiledPattern::compile(&units(&words), "g").unwrap().compiled_bytes();
        let source = 2 * units(&words).len() + 1;
        assert!(compiled > source, "{compiled} {source}");
        let mut cache = PatternCache::new(PatternLimits { max_entries: 8, max_bytes: 3 * (compiled + source), compile: DEFAULT_COMPILE_LIMITS });
        let _ = cache.get_or_compile(&units(&words), "g", &budget).unwrap();
        assert_eq!(cache.retained_bytes(), compiled + source);
        let _ = cache.get_or_compile(&units(&format!("{words}|x")), "g", &budget).unwrap();
        let _ = cache.get_or_compile(&units(&format!("{words}|y")), "g", &budget).unwrap();
        assert_eq!(cache.len(), 2, "the compiled sizes evict the oldest pattern");
        let limits = CompileLimits { max_pattern_units: 8, max_alternation_depth: 399, max_compiled_bytes: compiled - 1 };
        assert_eq!(CompiledPattern::compile_bounded(&units("aaaaaaaaa"), "g", limits).unwrap_err(), CompileError::Unsupported("regex-pattern-units"));
        let limits = CompileLimits { max_pattern_units: 1 << 20, ..limits };
        assert_eq!(CompiledPattern::compile_bounded(&units(&words), "g", limits).unwrap_err(), CompileError::Unsupported("regex-compiled-bytes"));
        assert!(CompiledPattern::compile_bounded(&units("a"), "g", limits).is_ok());
        let limits = CompileLimits { max_alternation_depth: 398, ..DEFAULT_COMPILE_LIMITS };
        assert_eq!(CompiledPattern::compile_bounded(&units(&words), "g", limits).unwrap_err(), CompileError::Unsupported("regex-alternation-depth"));
        // Separators inside an enclosing group's chain add to its depth; classes, escapes and closed groups do not.
        let depth = |pattern: &str| crate::engine::alternation_depth(&units(pattern));
        assert_eq!((depth("a|b|(c|d|e)|f"), depth("[|||]\\||(?:a)"), depth("(a|b)(c|d)|e"), depth("a(?:b|(?:c|d))")), (4, 1, 1, 2));
    }
}
