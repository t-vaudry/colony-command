//! Paired remote devices and their credentials.
//!
//! A device pairs by redeeming a short code the desktop app shows, and gets
//! its own credential back. Only a hash of the credential is kept
//! (`~/.colony/devices.json`), so the file can't be used to connect. Unlike
//! the loopback token in `daemon.json`, credentials are not regenerated when
//! the daemon starts: they survive restarts and the update flow until the
//! user revokes them.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::broadcast;

/// How long a pairing code works.
pub const CODE_TTL_MS: u64 = 5 * 60 * 1000;
/// This many wrong codes within `FAIL_WINDOW_MS` lock pairing, and drop the codes shown.
pub const MAX_FAILS: usize = 5;
pub const FAIL_WINDOW_MS: u64 = 60 * 1000;
const MAX_NAME: usize = 40;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Stored {
    id: String,
    name: String,
    /// SHA-256 of the credential, hex.
    hash: String,
    created_ms: u64,
}

/// What the desktop app lists.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub created_ms: u64,
    pub last_seen_ms: Option<u64>,
}

#[derive(Debug, PartialEq)]
pub enum PairError {
    /// Wrong, used or expired.
    BadCode,
    /// Too many wrong tries; try again after the window.
    Locked,
}

#[derive(Default)]
struct Inner {
    devices: Vec<Stored>,
    /// code -> expiry (unix ms)
    codes: HashMap<String, u64>,
    fails: Vec<u64>,
    seen: HashMap<String, u64>,
}

pub struct Devices {
    path: Option<PathBuf>,
    inner: Mutex<Inner>,
    /// Ids of devices as they are revoked, so open sockets can close at once.
    pub revoked: broadcast::Sender<String>,
}

impl Default for Devices {
    fn default() -> Self {
        Devices { path: None, inner: Mutex::default(), revoked: broadcast::channel(16).0 }
    }
}

fn hash(credential: &str) -> String {
    Sha256::digest(credential.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

fn random_bytes() -> [u8; 32] {
    let mut out = [0u8; 32];
    out[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    out[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    out
}

/// Digits only, so it can be typed; spaces and dashes in what a person types are ignored.
fn normalize(code: &str) -> String {
    code.chars().filter(char::is_ascii_digit).collect()
}

impl Devices {
    /// Reads the saved devices; a missing or unreadable file means none.
    pub fn load(path: PathBuf) -> Devices {
        let devices = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .and_then(|v| serde_json::from_value::<Vec<Stored>>(v.get("devices")?.clone()).ok())
            .unwrap_or_default();
        Devices { path: Some(path), inner: Mutex::new(Inner { devices, ..Inner::default() }), revoked: broadcast::channel(16).0 }
    }

    fn save(&self, inner: &Inner) {
        let Some(path) = &self.path else { return };
        let tmp = path.with_extension("json.tmp");
        let body = serde_json::to_vec_pretty(&serde_json::json!({ "devices": inner.devices })).unwrap();
        let _ = std::fs::create_dir_all(path.parent().unwrap_or(path));
        if std::fs::write(&tmp, body).and_then(|_| std::fs::rename(&tmp, path)).is_err() {
            crate::log("could not save paired devices");
        }
    }

    /// A fresh single-use pairing code (8 digits) and when it expires.
    pub fn new_code(&self, now: u64) -> (String, u64) {
        let mut inner = self.inner.lock().unwrap();
        inner.codes.retain(|_, exp| *exp > now);
        let n = u64::from_le_bytes(random_bytes()[..8].try_into().unwrap()) % 100_000_000;
        let code = format!("{n:08}");
        let expires = now + CODE_TTL_MS;
        inner.codes.insert(code.clone(), expires);
        (code, expires)
    }

    /// Exchange a code for a device credential. The credential is returned
    /// once and never stored in the clear.
    pub fn redeem(&self, code: &str, name: &str, now: u64) -> Result<(DeviceInfo, String), PairError> {
        let mut inner = self.inner.lock().unwrap();
        inner.fails.retain(|t| now.saturating_sub(*t) < FAIL_WINDOW_MS);
        if inner.fails.len() >= MAX_FAILS {
            return Err(PairError::Locked);
        }
        let code = normalize(code);
        let valid = inner.codes.remove(&code).is_some_and(|exp| exp > now);
        if !valid {
            inner.fails.push(now);
            if inner.fails.len() >= MAX_FAILS {
                // Someone is guessing: nothing shown on the desktop stays usable.
                inner.codes.clear();
            }
            return Err(PairError::BadCode);
        }
        let credential = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random_bytes());
        let name: String = name.trim().chars().take(MAX_NAME).collect();
        let stored = Stored {
            id: uuid::Uuid::new_v4().simple().to_string(),
            name: if name.is_empty() { "Device".into() } else { name },
            hash: hash(&credential),
            created_ms: now,
        };
        let info = DeviceInfo { id: stored.id.clone(), name: stored.name.clone(), created_ms: now, last_seen_ms: None };
        inner.devices.push(stored);
        self.save(&inner);
        Ok((info, credential))
    }

    /// The device a credential belongs to, if it is still paired.
    pub fn authenticate(&self, credential: &str, now: u64) -> Option<String> {
        let h = hash(credential);
        let mut inner = self.inner.lock().unwrap();
        let id = inner.devices.iter().find(|d| d.hash == h)?.id.clone();
        inner.seen.insert(id.clone(), now);
        Some(id)
    }

    pub fn is_paired(&self, id: &str) -> bool {
        self.inner.lock().unwrap().devices.iter().any(|d| d.id == id)
    }

    pub fn list(&self) -> Vec<DeviceInfo> {
        let inner = self.inner.lock().unwrap();
        inner
            .devices
            .iter()
            .map(|d| DeviceInfo { id: d.id.clone(), name: d.name.clone(), created_ms: d.created_ms, last_seen_ms: inner.seen.get(&d.id).copied() })
            .collect()
    }

    /// Remove a device. Its credential stops working at once, and any open
    /// connection of it is told to close.
    pub fn revoke(&self, id: &str) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let before = inner.devices.len();
        inner.devices.retain(|d| d.id != id);
        inner.seen.remove(id);
        let removed = inner.devices.len() != before;
        if removed {
            self.save(&inner);
            let _ = self.revoked.send(id.to_string());
        }
        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        std::env::temp_dir().join(format!("colony-devices-test-{}", uuid::Uuid::new_v4().simple())).join("devices.json")
    }

    #[test]
    fn a_code_pairs_once() {
        let d = Devices::default();
        let (code, _) = d.new_code(1000);
        let (info, cred) = d.redeem(&code, "Tablet", 2000).unwrap();
        assert_eq!(info.name, "Tablet");
        assert_eq!(d.authenticate(&cred, 3000), Some(info.id));
        assert_eq!(d.redeem(&code, "Again", 3000), Err(PairError::BadCode), "single use");
        assert_eq!(d.authenticate("nope", 3000), None);
    }

    #[test]
    fn a_code_typed_with_spaces_works_and_expired_ones_do_not() {
        let d = Devices::default();
        let (code, expires) = d.new_code(0);
        let spaced = format!("{} {}", &code[..4], &code[4..]);
        assert!(d.redeem(&spaced, "x", 10).is_ok());
        let (code, _) = d.new_code(0);
        assert_eq!(d.redeem(&code, "x", expires + 1), Err(PairError::BadCode));
    }

    #[test]
    fn repeated_wrong_codes_lock_pairing_and_drop_the_shown_code() {
        let d = Devices::default();
        let (code, _) = d.new_code(0);
        for i in 0..MAX_FAILS as u64 {
            assert_eq!(d.redeem("00000000", "x", i), Err(PairError::BadCode));
        }
        // Even the right code is refused while locked, and it was dropped anyway.
        assert_eq!(d.redeem(&code, "x", 10), Err(PairError::Locked));
        // After the window the lock lifts, but the old code is gone.
        assert_eq!(d.redeem(&code, "x", FAIL_WINDOW_MS + 100), Err(PairError::BadCode));
        let (fresh, _) = d.new_code(FAIL_WINDOW_MS + 200);
        assert!(d.redeem(&fresh, "x", FAIL_WINDOW_MS + 300).is_ok());
    }

    #[test]
    fn a_revoked_credential_stops_working_at_once() {
        let d = Devices::default();
        let mut rx = d.revoked.subscribe();
        let (code, _) = d.new_code(0);
        let (info, cred) = d.redeem(&code, "Tablet", 1).unwrap();
        assert!(d.revoke(&info.id));
        assert_eq!(d.authenticate(&cred, 2), None);
        assert_eq!(rx.try_recv().unwrap(), info.id);
        assert!(!d.revoke(&info.id));
        assert!(d.list().is_empty());
    }

    #[test]
    fn devices_survive_a_restart_and_only_a_hash_is_kept() {
        let path = temp();
        let d = Devices::load(path.clone());
        let (code, _) = d.new_code(0);
        let (info, cred) = d.redeem(&code, "Tablet", 1).unwrap();
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(!on_disk.contains(&cred), "the credential itself is never written");
        let again = Devices::load(path);
        assert_eq!(again.authenticate(&cred, 2), Some(info.id.clone()));
        assert_eq!(again.list()[0].name, "Tablet");
        // Revoking persists too.
        again.revoke(&info.id);
        assert_eq!(Devices::load(again.path.clone().unwrap()).authenticate(&cred, 3), None);
    }

    #[test]
    fn names_are_trimmed_and_capped() {
        let d = Devices::default();
        let (code, _) = d.new_code(0);
        let (info, _) = d.redeem(&code, &format!("  {}  ", "n".repeat(100)), 1).unwrap();
        assert_eq!(info.name.len(), MAX_NAME);
        let (code, _) = d.new_code(0);
        assert_eq!(d.redeem(&code, "   ", 1).unwrap().0.name, "Device");
    }
}
