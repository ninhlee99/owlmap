//! Incremental, resumable runs. Every Claude call is keyed by a SHA-256 of
//! exactly what would be sent — model, system prompt and user prompt — so a
//! change to any file, prompt, model or language invalidates only the calls it
//! affects.
//!
//! The cache lives next to the docs as `.owlmap-cache.json`. While a run is in
//! progress it is saved every few calls (keeping the previous run's entries
//! too), so an interrupted run loses at most a handful of calls. A finished
//! run rewrites it with only the entries it used, so stale entries never pile up.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const FILE_NAME: &str = ".owlmap-cache.json";
/// Bump when the shape of cached values changes.
const VERSION: u32 = 1;
/// Save to disk after this many new entries.
const FLUSH_EVERY: usize = 5;

#[derive(Default, Serialize, Deserialize)]
struct OnDisk {
    version: u32,
    entries: BTreeMap<String, Value>,
}

pub struct CacheStore {
    dir: Option<PathBuf>,
    prev: BTreeMap<String, Value>,
    used: Mutex<BTreeMap<String, Value>>,
    unsaved: AtomicUsize,
}

impl CacheStore {
    /// Opens the cache in `dir`. A missing, unreadable or outdated file — or
    /// `fresh` — starts empty; the file is still written as the run goes.
    pub fn open(dir: &Path, fresh: bool) -> Self {
        let prev = if fresh {
            BTreeMap::new()
        } else {
            fs::read(dir.join(FILE_NAME))
                .ok()
                .and_then(|b| serde_json::from_slice::<OnDisk>(&b).ok())
                .filter(|c| c.version == VERSION)
                .map(|c| c.entries)
                .unwrap_or_default()
        };
        Self { dir: Some(dir.to_path_buf()), prev, used: Mutex::default(), unsaved: AtomicUsize::new(0) }
    }

    /// A cache that never touches disk.
    pub fn in_memory() -> Self {
        Self { dir: None, prev: BTreeMap::new(), used: Mutex::default(), unsaved: AtomicUsize::new(0) }
    }

    pub fn len_previous(&self) -> usize {
        self.prev.len()
    }

    /// Whether `key` would be a hit, without counting it as used.
    pub fn contains(&self, key: &str) -> bool {
        self.used.lock().unwrap().contains_key(key) || self.prev.contains_key(key)
    }

    /// Looks up `key`; a hit from the previous run is kept for the next one.
    pub fn get(&self, key: &str) -> Option<Value> {
        let mut used = self.used.lock().unwrap();
        if let Some(v) = used.get(key) {
            return Some(v.clone());
        }
        let v = self.prev.get(key)?.clone();
        used.insert(key.to_string(), v.clone());
        Some(v)
    }

    /// Peeks at a value without marking it used (for estimates).
    pub fn peek(&self, key: &str) -> Option<Value> {
        self.used.lock().unwrap().get(key).cloned().or_else(|| self.prev.get(key).cloned())
    }

    pub fn put(&self, key: String, value: Value) {
        self.used.lock().unwrap().insert(key, value);
        if self.unsaved.fetch_add(1, Ordering::SeqCst) + 1 >= FLUSH_EVERY {
            let _ = self.save_progress();
        }
    }

    /// Saves everything known so far — previous entries included — so an
    /// interrupted run can resume.
    pub fn save_progress(&self) -> Result<()> {
        let used = self.used.lock().unwrap();
        let mut all = self.prev.clone();
        all.extend(used.iter().map(|(k, v)| (k.clone(), v.clone())));
        self.unsaved.store(0, Ordering::SeqCst);
        self.write(all)
    }

    /// Saves only the entries this run used.
    pub fn finish(&self) -> Result<()> {
        let used = self.used.lock().unwrap().clone();
        self.write(used)
    }

    fn write(&self, entries: BTreeMap<String, Value>) -> Result<()> {
        let Some(dir) = &self.dir else { return Ok(()) };
        fs::create_dir_all(dir)?;
        let tmp = dir.join(format!("{FILE_NAME}.tmp"));
        fs::write(&tmp, serde_json::to_vec(&OnDisk { version: VERSION, entries })?)?;
        fs::rename(tmp, dir.join(FILE_NAME))?; // atomic: never leave a half-written cache
        Ok(())
    }
}

/// Collision-safe key over several parts (length-prefixed so "ab"+"c" ≠ "a"+"bc").
pub fn key(parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update((p.len() as u64).to_le_bytes());
        h.update(p.as_bytes());
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}
