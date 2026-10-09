//! The human-latency ledger on disk, so it survives a daemon restart. Spend can
//! be rebuilt from transcripts; waits on a human cannot.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use colony_core::usage::{ProjectLatency, BUCKET_MS, LEDGER_KEEP_MS};
use serde::{Deserialize, Serialize};

const VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct File {
    version: u32,
    projects: BTreeMap<String, ProjectLatency>,
}

pub fn path() -> PathBuf {
    colony_source::colony_home().join("latency.json")
}

/// The saved ledger, minus buckets older than the ledger window. A missing,
/// corrupt, or newer-version file gives an empty ledger rather than an error.
pub fn load(path: &Path, now_ms: u64) -> BTreeMap<String, ProjectLatency> {
    let Some(file) = std::fs::read(path).ok().and_then(|b| serde_json::from_slice::<File>(&b).ok()) else {
        return BTreeMap::new();
    };
    if file.version != VERSION {
        return BTreeMap::new();
    }
    let oldest = now_ms.saturating_sub(LEDGER_KEEP_MS) / BUCKET_MS;
    let mut projects = file.projects;
    for p in projects.values_mut() {
        p.buckets.retain(|b, _| *b >= oldest);
    }
    projects.retain(|_, p| !p.buckets.is_empty());
    projects
}

/// Write to a temp file beside the target, then rename over it, so a crash
/// mid-write never leaves a torn file.
pub fn save(path: &Path, projects: &BTreeMap<String, ProjectLatency>) -> std::io::Result<()> {
    let body = serde_json::to_vec(&File { version: VERSION, projects: projects.clone() }).expect("serializes");
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("colony-latency-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn sample(bucket: u64) -> BTreeMap<String, ProjectLatency> {
        let mut p = ProjectLatency { name: "bingosync".into(), ..Default::default() };
        p.buckets.insert(bucket, vec![3_000, 9_000]);
        BTreeMap::from([("c:/x/bingosync".to_string(), p)])
    }

    #[test]
    fn round_trips() {
        let f = dir().join("latency.json");
        let now = 10 * LEDGER_KEEP_MS;
        let ledger = sample(now / BUCKET_MS);
        save(&f, &ledger).unwrap();
        assert_eq!(load(&f, now), ledger);
    }

    #[test]
    fn drops_buckets_older_than_the_window() {
        let f = dir().join("latency.json");
        let now = 10 * LEDGER_KEEP_MS;
        save(&f, &sample(now / BUCKET_MS - LEDGER_KEEP_MS / BUCKET_MS - 1)).unwrap();
        assert!(load(&f, now).is_empty());
    }

    #[test]
    fn missing_corrupt_or_future_files_start_empty() {
        let d = dir();
        assert!(load(&d.join("nope.json"), 1).is_empty());
        let bad = d.join("bad.json");
        std::fs::write(&bad, b"{ not json").unwrap();
        assert!(load(&bad, 1).is_empty());
        let future = d.join("future.json");
        std::fs::write(&future, br#"{"version":99,"projects":{}}"#).unwrap();
        assert!(load(&future, 1).is_empty());
    }
}
