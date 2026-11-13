//! Secrets: tokens and keys agents need, kept out of `indra.toml`.
//!
//! - **Encrypted at rest.** One file, `vault.json`, encrypted with
//!   XChaCha20-Poly1305. The key is `INDRA_SECRETS_KEY` (64 hex characters)
//!   when set, so it can live outside the disk (a service manager's
//!   credentials, a CI secret); otherwise a random key in `vault.key`, created
//!   with owner-only permissions next to the vault.
//! - **Referenced, not copied.** A profile's environment says
//!   `JIRA_TOKEN = "secret:jira"`; the value is looked up only when an agent
//!   starts, and only if the secret's scope allows that profile and node.
//! - **Write-only.** Nothing here returns a value to the console or the API;
//!   listing shows names, descriptions, scopes and use.
//! - **Redacted.** Each process gets the values it was given, so its feed and
//!   log can replace them with `[secret:name]`.
//! - **Audited.** Every delivery is appended to `audit.log`.

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub type Result<T> = std::result::Result<T, String>;

/// An agent's environment.
pub type Env = BTreeMap<String, String>;
/// (value, secret name) pairs a process redacts from its output.
pub type Redactions = Vec<(String, String)>;

/// The prefix that makes an environment value a secret reference.
pub const PREFIX: &str = "secret:";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    value: String,
    #[serde(default)]
    description: String,
    /// Profiles that may receive it (empty: any).
    #[serde(default)]
    profiles: Vec<String>,
    /// Nodes it may be sent to (empty: any).
    #[serde(default)]
    nodes: Vec<String>,
    created: u64,
    updated: u64,
    #[serde(default)]
    last_used: Option<u64>,
    #[serde(default)]
    uses: u64,
}

/// What the console may know about a secret: everything but its value.
#[derive(Debug, Clone, Serialize)]
pub struct Info {
    pub name: String,
    pub description: String,
    pub profiles: Vec<String>,
    pub nodes: Vec<String>,
    pub created: u64,
    pub updated: u64,
    pub last_used: Option<u64>,
    pub uses: u64,
}

/// A change to a secret; `None` leaves that part as it is.
#[derive(Debug, Default, Deserialize)]
pub struct Change {
    pub value: Option<String>,
    pub description: Option<String>,
    pub profiles: Option<Vec<String>>,
    pub nodes: Option<Vec<String>>,
}

#[derive(Serialize, Deserialize)]
struct Sealed {
    version: u32,
    nonce: String,
    data: String,
}

pub struct Vault {
    dir: PathBuf,
    lock: Mutex<()>,
}

/// Secret names: what `secret:<name>` can say.
pub fn validate_name(n: &str) -> Result<()> {
    let ok = !n.is_empty()
        && n.len() <= 64
        && !n.starts_with(['-', '.'])
        && n.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c));
    if ok {
        Ok(())
    } else {
        Err(format!(
            "invalid secret name {:?}: use letters, digits, '_', '.' and '-'",
            n
        ))
    }
}

/// The secret an environment value refers to, if it is a reference.
pub fn reference(value: &str) -> Option<&str> {
    value.strip_prefix(PREFIX)
}

/// Replace secret values in `text` with `[secret:name]`. Very short values
/// are left alone: replacing "1" everywhere would only make noise.
pub fn redact(text: &str, values: &[(String, String)]) -> String {
    let mut out = text.to_string();
    for (value, name) in values {
        if value.len() >= 4 && out.contains(value.as_str()) {
            out = out.replace(value.as_str(), &format!("[secret:{}]", name));
        }
    }
    out
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{:02x}", x)).collect()
}

fn unhex(s: &str) -> Result<Vec<u8>> {
    let s = s.trim();
    if s.len() % 2 != 0 || !s.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err("not hex".into());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

fn now() -> u64 {
    crate::deck::runs::now()
}

/// Write a file only its owner can read, replacing it atomically.
fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    create_private_dir(dir)?;
    let tmp = dir.join(format!(
        ".{}.{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("f"),
        std::process::id()
    ));
    {
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
        let mut f = o
            .open(&tmp)
            .map_err(|e| format!("cannot write {}: {}", tmp.display(), e))?;
        f.write_all(data)
            .and_then(|_| f.sync_all())
            .map_err(|e| format!("cannot write {}: {}", tmp.display(), e))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cannot replace {}: {}", path.display(), e)
    })
}

fn create_private_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {}", dir.display(), e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

impl Vault {
    pub fn open(dir: PathBuf) -> Vault {
        Vault {
            dir,
            lock: Mutex::new(()),
        }
    }

    fn vault_path(&self) -> PathBuf {
        self.dir.join("vault.json")
    }

    /// The key: `INDRA_SECRETS_KEY`, else `vault.key` (made on first write).
    fn key(&self, create: bool) -> Result<Option<[u8; 32]>> {
        let parse = |s: &str, from: &str| -> Result<[u8; 32]> {
            let b = unhex(s).map_err(|_| format!("{} must be 64 hex characters", from))?;
            b.try_into()
                .map_err(|_| format!("{} must be 64 hex characters", from))
        };
        if let Ok(k) = std::env::var("INDRA_SECRETS_KEY") {
            if !k.trim().is_empty() {
                return parse(&k, "INDRA_SECRETS_KEY").map(Some);
            }
        }
        let path = self.dir.join("vault.key");
        match std::fs::read_to_string(&path) {
            Ok(s) => parse(&s, &path.display().to_string()).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if !create {
                    return Ok(None);
                }
                let mut k = [0u8; 32];
                getrandom::fill(&mut k).map_err(|e| format!("no OS randomness: {}", e))?;
                write_private(&path, format!("{}\n", hex(&k)).as_bytes())?;
                Ok(Some(k))
            }
            Err(e) => Err(format!("cannot read {}: {}", path.display(), e)),
        }
    }

    fn load(&self) -> Result<BTreeMap<String, Entry>> {
        let text = match std::fs::read_to_string(self.vault_path()) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(e) => return Err(format!("cannot read the secret store: {}", e)),
        };
        let sealed: Sealed = serde_json::from_str(&text)
            .map_err(|e| format!("the secret store is damaged: {}", e))?;
        let key = self
            .key(false)?
            .ok_or("the secret store's key is missing (vault.key, or INDRA_SECRETS_KEY)")?;
        let nonce = unhex(&sealed.nonce).map_err(|_| "the secret store is damaged")?;
        if nonce.len() != 24 {
            return Err("the secret store is damaged".into());
        }
        let data = unhex(&sealed.data).map_err(|_| "the secret store is damaged")?;
        let plain = XChaCha20Poly1305::new((&key).into())
            .decrypt(XNonce::from_slice(&nonce), data.as_ref())
            .map_err(|_| "cannot open the secret store: wrong key, or the file was changed")?;
        serde_json::from_slice(&plain).map_err(|e| format!("the secret store is damaged: {}", e))
    }

    fn save(&self, entries: &BTreeMap<String, Entry>) -> Result<()> {
        let key = self.key(true)?.ok_or("no key")?;
        let mut nonce = [0u8; 24];
        getrandom::fill(&mut nonce).map_err(|e| format!("no OS randomness: {}", e))?;
        let plain = serde_json::to_vec(entries).map_err(|e| e.to_string())?;
        let data = XChaCha20Poly1305::new((&key).into())
            .encrypt(XNonce::from_slice(&nonce), plain.as_ref())
            .map_err(|_| "encryption failed")?;
        let sealed = Sealed {
            version: 1,
            nonce: hex(&nonce),
            data: hex(&data),
        };
        write_private(
            &self.vault_path(),
            serde_json::to_string(&sealed)
                .map_err(|e| e.to_string())?
                .as_bytes(),
        )
    }

    pub fn list(&self) -> Result<Vec<Info>> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        Ok(self
            .load()?
            .into_iter()
            .map(|(name, e)| Info {
                name,
                description: e.description,
                profiles: e.profiles,
                nodes: e.nodes,
                created: e.created,
                updated: e.updated,
                last_used: e.last_used,
                uses: e.uses,
            })
            .collect())
    }

    /// Create or change a secret. A new secret needs a value.
    pub fn set(&self, name: &str, c: Change) -> Result<()> {
        validate_name(name)?;
        if let Some(v) = &c.value {
            if v.is_empty() || v.len() > 64 * 1024 || v.contains('\0') {
                return Err("a secret value must be 1 byte to 64 KB, without NUL".into());
            }
        }
        for p in c.profiles.iter().flatten() {
            // A profile, or a connector as "connector:<name>".
            let name = p.strip_prefix("connector:").unwrap_or(p);
            crate::deck::nodes::validate_name(name).map_err(|e| format!("profile: {}", e))?;
        }
        for n in c.nodes.iter().flatten() {
            crate::deck::nodes::validate_name(n).map_err(|e| format!("node: {}", e))?;
        }
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut all = self.load()?;
        let t = now();
        let e = match all.get_mut(name) {
            Some(e) => e,
            None => {
                let value = c.value.clone().ok_or_else(|| {
                    format!("secret '{}' does not exist yet: give it a value", name)
                })?;
                all.insert(
                    name.to_string(),
                    Entry {
                        value,
                        description: String::new(),
                        profiles: vec![],
                        nodes: vec![],
                        created: t,
                        updated: t,
                        last_used: None,
                        uses: 0,
                    },
                );
                all.get_mut(name).unwrap()
            }
        };
        if let Some(v) = c.value {
            e.value = v;
        }
        if let Some(d) = c.description {
            e.description = d;
        }
        if let Some(p) = c.profiles {
            e.profiles = p;
        }
        if let Some(n) = c.nodes {
            e.nodes = n;
        }
        e.updated = t;
        self.save(&all)
    }

    pub fn remove(&self, name: &str) -> Result<bool> {
        validate_name(name)?;
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut all = self.load()?;
        let gone = all.remove(name).is_some();
        if gone {
            self.save(&all)?;
        }
        Ok(gone)
    }

    /// Resolve the `secret:` references in an agent's environment for a
    /// launch with `profile` on `node`. Returns the environment with values,
    /// and (value, name) pairs for redaction. Fails, before anything starts,
    /// on a missing secret or one whose scope does not allow this launch.
    pub fn resolve(
        &self,
        env: &BTreeMap<String, String>,
        profile: Option<&str>,
        node: &str,
    ) -> Result<(Env, Redactions)> {
        if !env.values().any(|v| reference(v).is_some()) {
            return Ok((env.clone(), Vec::new()));
        }
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let all = self.load()?;
        let mut out = env.clone();
        let mut used = Vec::new();
        for (k, v) in env {
            let Some(name) = reference(v) else { continue };
            validate_name(name)?;
            let e = all.get(name).ok_or_else(|| {
                format!(
                    "secret '{}' (for {}) is not set: add it in Settings > Secrets or with `indra secret set {}`",
                    name, k, name
                )
            })?;
            if !e.profiles.is_empty()
                && !profile
                    .map(|p| e.profiles.iter().any(|x| x == p))
                    .unwrap_or(false)
            {
                return Err(format!(
                    "secret '{}' may only be used by profile(s) {}",
                    name,
                    e.profiles.join(", ")
                ));
            }
            if !e.nodes.is_empty() && !e.nodes.iter().any(|x| x == node) {
                return Err(format!(
                    "secret '{}' may not be sent to node '{}' (allowed: {})",
                    name,
                    node,
                    e.nodes.join(", ")
                ));
            }
            out.insert(k.clone(), e.value.clone());
            used.push((e.value.clone(), name.to_string()));
        }
        Ok((out, used))
    }

    /// Record that secrets were delivered to a process: use counts and the audit log.
    pub fn record_use(&self, names: &[String], profile: Option<&str>, node: &str, process: u64) {
        if names.is_empty() {
            return;
        }
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        if let Ok(mut all) = self.load() {
            let t = now();
            for n in names {
                if let Some(e) = all.get_mut(n) {
                    e.uses += 1;
                    e.last_used = Some(t);
                }
            }
            let _ = self.save(&all);
        }
        let line = serde_json::json!({
            "ts": now(), "secrets": names, "profile": profile, "node": node, "process": process,
        });
        let path = self.dir.join("audit.log");
        let mut o = std::fs::OpenOptions::new();
        o.create(true).append(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
        if let Ok(mut f) = o.open(&path) {
            let _ = writeln!(f, "{}", line);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vault(name: &str) -> (Vault, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("indra-secrets-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&dir);
        (Vault::open(dir.clone()), dir)
    }

    #[test]
    fn values_are_encrypted_scoped_and_never_listed() {
        let (v, dir) = vault("basic");
        v.set(
            "jira",
            Change {
                value: Some("tok-SECRET-123".into()),
                description: Some("Jira API".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let raw = std::fs::read_to_string(dir.join("vault.json")).unwrap();
        assert!(!raw.contains("tok-SECRET-123"), "encrypted at rest");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("vault.key"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
        let list = v.list().unwrap();
        assert_eq!(list[0].name, "jira");
        assert!(!serde_json::to_string(&list).unwrap().contains("tok-SECRET"));

        let env: BTreeMap<String, String> = [
            ("JIRA_TOKEN".to_string(), "secret:jira".to_string()),
            ("PLAIN".to_string(), "x".to_string()),
        ]
        .into_iter()
        .collect();
        let (out, used) = v.resolve(&env, Some("worker"), "local").unwrap();
        assert_eq!(out["JIRA_TOKEN"], "tok-SECRET-123");
        assert_eq!(out["PLAIN"], "x");
        assert_eq!(redact("got tok-SECRET-123!", &used), "got [secret:jira]!");

        // Scope: only the worker profile, only on local.
        v.set(
            "jira",
            Change {
                profiles: Some(vec!["worker".into()]),
                nodes: Some(vec!["local".into()]),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(v.resolve(&env, Some("worker"), "local").is_ok());
        assert!(v
            .resolve(&env, Some("planner"), "local")
            .unwrap_err()
            .contains("only be used by"));
        assert!(v.resolve(&env, None, "local").is_err());
        assert!(v
            .resolve(&env, Some("worker"), "box")
            .unwrap_err()
            .contains("may not be sent"));
        // The value survived the metadata change.
        assert_eq!(
            v.resolve(&env, Some("worker"), "local").unwrap().0["JIRA_TOKEN"],
            "tok-SECRET-123"
        );

        let missing: BTreeMap<String, String> = [("X".to_string(), "secret:nope".to_string())]
            .into_iter()
            .collect();
        assert!(v
            .resolve(&missing, None, "local")
            .unwrap_err()
            .contains("is not set"));
        assert!(
            v.set("new", Change::default()).is_err(),
            "a new secret needs a value"
        );
        assert!(v
            .set(
                "../x",
                Change {
                    value: Some("v".into()),
                    ..Default::default()
                }
            )
            .is_err());

        v.record_use(&["jira".into()], Some("worker"), "local", 7);
        assert_eq!(v.list().unwrap()[0].uses, 1);
        assert!(std::fs::read_to_string(dir.join("audit.log"))
            .unwrap()
            .contains("\"process\":7"));
        assert!(v.remove("jira").unwrap());
        assert!(v.list().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_wrong_key_or_a_changed_file_is_refused() {
        let (v, dir) = vault("tamper");
        v.set(
            "a",
            Change {
                value: Some("value-1".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let raw = std::fs::read_to_string(dir.join("vault.json")).unwrap();
        let mut sealed: Sealed = serde_json::from_str(&raw).unwrap();
        let last = sealed.data.pop().unwrap();
        sealed.data.push(if last == '0' { '1' } else { '0' });
        std::fs::write(
            dir.join("vault.json"),
            serde_json::to_string(&sealed).unwrap(),
        )
        .unwrap();
        assert!(v
            .list()
            .unwrap_err()
            .contains("wrong key, or the file was changed"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
