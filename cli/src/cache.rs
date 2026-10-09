//! Incremental runs. Every Claude call is keyed by a SHA-256 of exactly what
//! would be sent — model, system prompt and user prompt — so a change to any
//! file, prompt, model or language invalidates only the calls it affects.
//!
//! The cache lives next to the docs as `.owlmap-cache.json`. Each run rewrites
//! it with only the entries it used, so stale entries never pile up.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const FILE_NAME: &str = ".owlmap-cache.json";
/// Bump when the shape of cached values changes.
const VERSION: u32 = 1;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Cache {
    version: u32,
    pub entries: BTreeMap<String, Value>,
}

impl Cache {
    /// A missing, unreadable or outdated cache is simply empty.
    pub fn load(dir: &Path) -> Self {
        fs::read(dir.join(FILE_NAME))
            .ok()
            .and_then(|b| serde_json::from_slice::<Cache>(&b).ok())
            .filter(|c| c.version == VERSION)
            .unwrap_or_default()
    }

    pub fn save(dir: &Path, entries: &BTreeMap<String, Value>) -> Result<()> {
        let c = Cache { version: VERSION, entries: entries.clone() };
        fs::write(dir.join(FILE_NAME), serde_json::to_vec(&c)?)?;
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
