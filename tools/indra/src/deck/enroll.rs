//! Enrollment keys: one key many machines join with, for rolling indra out
//! with an MDM (Jamf, Intune, ...) or any script. Unlike the one-time codes of
//! Machines > Add a machine, a key works until it expires, reaches its use
//! limit or is revoked.
//!
//! The hub keeps only each key's SHA-256 (in `enroll-keys.json` in its data
//! folder), so the key itself is shown once, when it is made. Only admins
//! make, list or revoke keys.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Mutex;

/// What every key starts with, so one is easy to spot (and to scan for).
pub const PREFIX: &str = "IEK-";

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
struct Key {
    id: String,
    label: String,
    hash: String,
    created: u64,
    expires: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_uses: Option<u32>,
    #[serde(default)]
    uses: u32,
    #[serde(default)]
    revoked: bool,
    /// The machines that joined with it, newest last.
    #[serde(default)]
    machines: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_used: Option<u64>,
}

impl Key {
    fn state(&self, now: u64) -> &'static str {
        if self.revoked {
            "revoked"
        } else if now >= self.expires {
            "expired"
        } else if self.max_uses.map(|m| self.uses >= m).unwrap_or(false) {
            "used up"
        } else {
            "active"
        }
    }
}

pub struct Keys {
    path: PathBuf,
    lock: Mutex<()>,
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn hash(secret: &str) -> String {
    let d = Sha256::digest(secret.trim().to_ascii_uppercase().as_bytes());
    d.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Constant-time comparison of two hex digests.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

fn random(n: usize) -> Result<String, String> {
    const A: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut b = vec![0u8; n];
    getrandom::fill(&mut b).map_err(|e| format!("no OS randomness: {}", e))?;
    Ok(b.iter()
        .map(|x| A[(*x as usize) % A.len()] as char)
        .collect())
}

impl Keys {
    pub fn open(data_dir: &std::path::Path) -> Keys {
        Keys {
            path: data_dir.join("enroll-keys.json"),
            lock: Mutex::new(()),
        }
    }

    fn load(&self) -> Vec<Key> {
        std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    fn save(&self, keys: &[Key]) -> Result<(), String> {
        if let Some(d) = self.path.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        let text = serde_json::to_string_pretty(keys).map_err(|e| e.to_string())?;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        use std::io::Write;
        opts.open(&self.path)
            .and_then(|mut f| f.write_all(text.as_bytes()))
            .map_err(|e| format!("{}: {}", self.path.display(), e))
    }

    /// A new key, valid `days` (1 to 365) and at most `max_uses` times.
    /// Returns the key itself, which is not kept.
    pub fn create(
        &self,
        label: &str,
        days: u32,
        max_uses: Option<u32>,
    ) -> Result<(String, Value), String> {
        if !(1..=365).contains(&days) {
            return Err("a key is valid for 1 to 365 days".into());
        }
        if max_uses == Some(0) {
            return Err("at least one use".into());
        }
        let label = label.trim();
        if label.chars().count() > 80 {
            return Err("the label is longer than 80 characters".into());
        }
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let secret = format!("{}{}", PREFIX, random(26)?);
        let t = now();
        let k = Key {
            id: random(8)?.to_ascii_lowercase(),
            label: if label.is_empty() {
                "enrollment key".into()
            } else {
                label.into()
            },
            hash: hash(&secret),
            created: t,
            expires: t + days as u64 * 86_400,
            max_uses,
            ..Default::default()
        };
        let mut all = self.load();
        all.push(k.clone());
        self.save(&all)?;
        Ok((secret, view(&k, t)))
    }

    /// The keys, never their secrets.
    pub fn list(&self) -> Vec<Value> {
        let t = now();
        self.load().iter().rev().map(|k| view(k, t)).collect()
    }

    pub fn revoke(&self, id: &str) -> Result<(), String> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut all = self.load();
        let k = all.iter_mut().find(|k| k.id == id).ok_or("no such key")?;
        k.revoked = true;
        self.save(&all)
    }

    /// Is `secret` a key that can be used now? Its id if so.
    pub fn check(&self, secret: &str) -> Option<String> {
        if !secret.trim().to_ascii_uppercase().starts_with(PREFIX) {
            return None;
        }
        let h = hash(secret);
        let t = now();
        self.load()
            .into_iter()
            .find(|k| same(&k.hash, &h) && k.state(t) == "active")
            .map(|k| k.id)
    }

    /// Count a use by `machine`; false if the key stopped being usable meanwhile.
    pub fn record_use(&self, id: &str, machine: &str) -> bool {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut all = self.load();
        let t = now();
        let Some(k) = all
            .iter_mut()
            .find(|k| k.id == id && k.state(t) == "active")
        else {
            return false;
        };
        k.uses += 1;
        k.last_used = Some(t);
        k.machines.push(machine.to_string());
        if k.machines.len() > 200 {
            k.machines.remove(0);
        }
        self.save(&all).is_ok()
    }
}

fn view(k: &Key, now: u64) -> Value {
    json!({
        "id": k.id, "label": k.label, "created": k.created, "expires": k.expires,
        "max_uses": k.max_uses, "uses": k.uses, "state": k.state(now),
        "machines": k.machines, "last_used": k.last_used,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_work_until_used_up_or_revoked_and_are_kept_hashed() {
        let dir = std::env::temp_dir().join(format!("indra-enroll-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let keys = Keys::open(&dir);
        let (secret, v) = keys.create("laptops", 30, Some(2)).unwrap();
        assert!(secret.starts_with(PREFIX) && secret.len() == PREFIX.len() + 26);
        let id = v["id"].as_str().unwrap().to_string();
        // Only the hash is on disk.
        let disk = std::fs::read_to_string(dir.join("enroll-keys.json")).unwrap();
        assert!(!disk.contains(&secret[PREFIX.len()..]));
        // Any case, stray spaces.
        assert_eq!(
            keys.check(&format!(" {} ", secret.to_lowercase())),
            Some(id.clone())
        );
        assert_eq!(keys.check("IEK-WRONGWRONGWRONGWRONGWRONG"), None);
        assert_eq!(
            keys.check("ABCDE-FGHJK"),
            None,
            "a one-time code is not a key"
        );
        assert!(keys.record_use(&id, "a"));
        assert!(keys.record_use(&id, "b"));
        assert_eq!(keys.check(&secret), None, "used up");
        assert!(!keys.record_use(&id, "c"));
        let l = keys.list();
        assert_eq!(l[0]["state"], "used up");
        assert_eq!(l[0]["machines"], json!(["a", "b"]));
        assert!(l[0].get("hash").is_none(), "never listed");
        // Revoked keys stop working.
        let (s2, v2) = keys.create("", 1, None).unwrap();
        assert_eq!(v2["label"], "enrollment key");
        assert!(keys.check(&s2).is_some());
        keys.revoke(v2["id"].as_str().unwrap()).unwrap();
        assert_eq!(keys.check(&s2), None);
        assert!(keys.create("x", 0, None).is_err());
        assert!(keys.create("x", 400, None).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
