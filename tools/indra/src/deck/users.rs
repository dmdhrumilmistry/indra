//! Console users, their roles, and sign-in sessions.
//!
//! Until the first user is added, the console works as before: the access
//! token `indra serve` prints is the key, and it can do everything. Once a
//! user exists, people sign in with a name and password, and each request is
//! allowed by role:
//!
//! - **viewer**: reads everything except settings, secrets and the terminal;
//! - **operator**: also starts, steers and stops agents and workflows, runs
//!   the board, triages, records, publishes and posts reviews;
//! - **admin**: also settings, secrets, users, the terminal, prompt edits,
//!   installs and enrolling machines.
//!
//! The access token stays an admin credential for tools (`indra attach`,
//! scripts) as `Authorization: Bearer`, but no longer signs anyone in from a
//! URL. Passwords are Argon2id hashes in `users.json` (owner-only) in the
//! data folder. Sessions live in memory: a restart signs everyone out.

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

pub type Result<T> = std::result::Result<T, String>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Viewer,
    Operator,
    Admin,
}

impl Role {
    pub fn parse(s: &str) -> Result<Role> {
        match s {
            "viewer" => Ok(Role::Viewer),
            "operator" => Ok(Role::Operator),
            "admin" => Ok(Role::Admin),
            _ => Err("role must be viewer, operator or admin".into()),
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::Operator => "operator",
            Role::Admin => "admin",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub name: String,
    pub role: Role,
    hash: String,
    #[serde(default)]
    pub created: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    users: Vec<User>,
}

/// Who a request is from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Who {
    pub user: String,
    pub role: Role,
}

struct Session {
    who: Who,
    created: u64,
    seen: u64,
}

/// A session ends after this long without a request, or this long in all.
const IDLE: u64 = 8 * 3600;
const MAX_AGE: u64 = 7 * 24 * 3600;
/// Wrong passwords for one name before it is locked for a while.
const MAX_FAILS: usize = 5;
const LOCK_SECS: u64 = 300;

pub struct Users {
    path: PathBuf,
    sessions: Mutex<HashMap<String, Session>>,
    fails: Mutex<HashMap<String, Vec<u64>>>,
    lock: Mutex<()>,
}

fn now() -> u64 {
    crate::deck::runs::now()
}

pub fn valid_name(n: &str) -> Result<()> {
    let ok = !n.is_empty()
        && n.len() <= 64
        && n.chars()
            .all(|c| c.is_ascii_alphanumeric() || "._@-".contains(c));
    if ok {
        Ok(())
    } else {
        Err("a user name is 1 to 64 letters, digits, '.', '_', '@' or '-'".into())
    }
}

fn check_password(p: &str) -> Result<()> {
    if p.chars().count() < 10 {
        return Err("a password needs at least 10 characters".into());
    }
    if p.len() > 512 {
        return Err("that password is too long".into());
    }
    Ok(())
}

fn hash(password: &str) -> Result<String> {
    let mut salt = [0u8; 16];
    getrandom::fill(&mut salt).map_err(|e| format!("no OS randomness: {}", e))?;
    let salt = SaltString::encode_b64(&salt).map_err(|e| e.to_string())?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| e.to_string())
}

fn verify(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash)
        .map(|h| {
            Argon2::default()
                .verify_password(password.as_bytes(), &h)
                .is_ok()
        })
        .unwrap_or(false)
}

impl Users {
    pub fn open(data_dir: &std::path::Path) -> Users {
        Users {
            path: data_dir.join("users.json"),
            sessions: Mutex::default(),
            fails: Mutex::default(),
            lock: Mutex::default(),
        }
    }

    fn load(&self) -> File {
        std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    fn save(&self, f: &File) -> Result<()> {
        if let Some(d) = self.path.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        let tmp = self.path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(f).map_err(|e| e.to_string())?;
        {
            let mut o = std::fs::OpenOptions::new();
            o.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                o.mode(0o600);
            }
            use std::io::Write;
            o.open(&tmp)
                .and_then(|mut f| f.write_all(text.as_bytes()))
                .map_err(|e| format!("{}: {}", tmp.display(), e))?;
        }
        std::fs::rename(&tmp, &self.path).map_err(|e| e.to_string())
    }

    /// Whether anyone has been added: until then the token is the key.
    pub fn any(&self) -> bool {
        !self.load().users.is_empty()
    }

    pub fn list(&self) -> Vec<(String, Role, u64)> {
        self.load()
            .users
            .into_iter()
            .map(|u| (u.name, u.role, u.created))
            .collect()
    }

    pub fn add(&self, name: &str, role: Role, password: &str) -> Result<()> {
        valid_name(name)?;
        check_password(password)?;
        let _g = self.lock.lock().unwrap();
        let mut f = self.load();
        if f.users.iter().any(|u| u.name == name) {
            return Err(format!("there is already a user named {}", name));
        }
        // The first user is an admin, or nobody could manage the rest.
        let role = if f.users.is_empty() {
            Role::Admin
        } else {
            role
        };
        f.users.push(User {
            name: name.into(),
            role,
            hash: hash(password)?,
            created: now(),
        });
        self.save(&f)
    }

    pub fn set_password(&self, name: &str, password: &str) -> Result<()> {
        check_password(password)?;
        let _g = self.lock.lock().unwrap();
        let mut f = self.load();
        let u = f
            .users
            .iter_mut()
            .find(|u| u.name == name)
            .ok_or_else(|| format!("no user named {}", name))?;
        u.hash = hash(password)?;
        self.save(&f)?;
        // Every session of that user ends.
        self.sessions
            .lock()
            .unwrap()
            .retain(|_, s| s.who.user != name);
        Ok(())
    }

    pub fn set_role(&self, name: &str, role: Role) -> Result<()> {
        let _g = self.lock.lock().unwrap();
        let mut f = self.load();
        let admins = f.users.iter().filter(|u| u.role == Role::Admin).count();
        let u = f
            .users
            .iter_mut()
            .find(|u| u.name == name)
            .ok_or_else(|| format!("no user named {}", name))?;
        if u.role == Role::Admin && role != Role::Admin && admins == 1 {
            return Err("that is the last admin; make someone else an admin first".into());
        }
        u.role = role;
        self.save(&f)?;
        for s in self.sessions.lock().unwrap().values_mut() {
            if s.who.user == name {
                s.who.role = role;
            }
        }
        Ok(())
    }

    pub fn remove(&self, name: &str) -> Result<()> {
        let _g = self.lock.lock().unwrap();
        let mut f = self.load();
        let Some(i) = f.users.iter().position(|u| u.name == name) else {
            return Err(format!("no user named {}", name));
        };
        let admins = f.users.iter().filter(|u| u.role == Role::Admin).count();
        if f.users[i].role == Role::Admin && admins == 1 && f.users.len() > 1 {
            return Err("that is the last admin; make someone else an admin first".into());
        }
        f.users.remove(i);
        self.save(&f)?;
        self.sessions
            .lock()
            .unwrap()
            .retain(|_, s| s.who.user != name);
        Ok(())
    }

    /// Check a name and password; a session token when they match. Too many
    /// wrong passwords for a name lock it for a while (and say so).
    pub fn sign_in(&self, name: &str, password: &str) -> Result<(String, Who)> {
        let t = now();
        {
            let mut fails = self.fails.lock().unwrap();
            let list = fails.entry(name.to_string()).or_default();
            list.retain(|x| t.saturating_sub(*x) < LOCK_SECS);
            if list.len() >= MAX_FAILS {
                return Err("too many wrong passwords; try again in a few minutes".into());
            }
        }
        let f = self.load();
        let user = f.users.iter().find(|u| u.name == name);
        // Hash even for an unknown name, so timing says nothing about it.
        let ok = match user {
            Some(u) => verify(password, &u.hash),
            None => {
                static DUMMY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
                let d = DUMMY.get_or_init(|| hash("not a real password").unwrap_or_default());
                let _ = verify(password, d);
                false
            }
        };
        let Some(u) = user.filter(|_| ok) else {
            self.fails
                .lock()
                .unwrap()
                .entry(name.to_string())
                .or_default()
                .push(t);
            return Err("wrong name or password".into());
        };
        self.fails.lock().unwrap().remove(name);
        let token = crate::deck::link::random_token(32);
        let who = Who {
            user: u.name.clone(),
            role: u.role,
        };
        self.sessions.lock().unwrap().insert(
            token.clone(),
            Session {
                who: who.clone(),
                created: t,
                seen: t,
            },
        );
        Ok((token, who))
    }

    /// The person behind a session token, while it is still valid.
    pub fn session(&self, token: &str) -> Option<Who> {
        let t = now();
        let mut s = self.sessions.lock().unwrap();
        s.retain(|_, x| t.saturating_sub(x.seen) < IDLE && t.saturating_sub(x.created) < MAX_AGE);
        let x = s.get_mut(token)?;
        x.seen = t;
        Some(x.who.clone())
    }

    pub fn sign_out(&self, token: &str) {
        self.sessions.lock().unwrap().remove(token);
    }
}

/// The least role a request needs.
pub fn needed(method: &str, path: &str) -> Role {
    let read = method == "GET" || method == "HEAD";
    let under = |p: &str| path == p || path.starts_with(&format!("{}/", p));
    // Your own session and password: anyone signed in.
    if under("/api/session") {
        return Role::Viewer;
    }
    // Settings, secrets, users and a shell on this machine: admins only,
    // even to look.
    if ["/api/config", "/api/secrets", "/api/users", "/api/term"]
        .iter()
        .any(|p| under(p))
    {
        return Role::Admin;
    }
    // Reading files where an agent works: not for viewers (a folder can
    // hold secrets the agent's output never showed).
    if under("/api/procs") && (path.ends_with("/files") || path.ends_with("/file")) {
        return Role::Operator;
    }
    // Enrollment keys: admins only, even to list.
    if under("/api/nodes/keys") {
        return Role::Admin;
    }
    // An engagement's audit log holds the commands its agents ran.
    if under("/api/engagements") && path.ends_with("/audit") {
        return Role::Operator;
    }
    // Creating and ending engagements (what may be tested, and when).
    if under("/api/engagements") && !read {
        return Role::Admin;
    }
    // Browsing a machine's folders, and changing where agents work.
    if under("/api/nodes") && (path.ends_with("/dirs") || path.ends_with("/folders")) {
        return Role::Admin;
    }
    if read {
        return Role::Viewer;
    }
    // Removing a machine.
    if method == "DELETE" && under("/api/nodes") {
        return Role::Admin;
    }
    // Changing what agents are told, installing code, enrolling machines.
    if [
        "/api/def",
        "/api/market",
        "/api/tools/skills",
        "/api/tools/site",
        "/api/nodes/enrol",
        "/api/ollama/default",
        "/api/workflow-files",
    ]
    .iter()
    .any(|p| under(p))
    {
        return Role::Admin;
    }
    Role::Operator
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_by_route() {
        assert_eq!(needed("GET", "/api/procs"), Role::Viewer);
        assert_eq!(needed("POST", "/api/procs"), Role::Operator);
        assert_eq!(needed("POST", "/api/workflows"), Role::Operator);
        assert_eq!(needed("PUT", "/api/workflow-files"), Role::Admin);
        assert_eq!(needed("POST", "/api/run/post"), Role::Operator);
        assert_eq!(needed("GET", "/api/engagements"), Role::Viewer);
        assert_eq!(needed("POST", "/api/engagements"), Role::Admin);
        assert_eq!(needed("POST", "/api/engagements/abcd1234/end"), Role::Admin);
        assert_eq!(
            needed("GET", "/api/engagements/abcd1234/audit"),
            Role::Operator
        );
        assert_eq!(needed("GET", "/api/config"), Role::Admin);
        assert_eq!(needed("GET", "/api/secrets"), Role::Admin);
        assert_eq!(needed("PUT", "/api/secrets/x"), Role::Admin);
        assert_eq!(needed("GET", "/api/term"), Role::Admin);
        assert_eq!(needed("GET", "/api/users"), Role::Admin);
        assert_eq!(needed("PUT", "/api/def"), Role::Admin);
        assert_eq!(needed("GET", "/api/def"), Role::Viewer);
        assert_eq!(needed("POST", "/api/market/install"), Role::Admin);
        assert_eq!(needed("GET", "/api/market"), Role::Viewer);
        assert_eq!(needed("POST", "/api/nodes/enrol"), Role::Admin);
        // Prefixes are whole segments.
        assert_eq!(needed("GET", "/api/configx"), Role::Viewer);
        assert_eq!(needed("PUT", "/api/session/password"), Role::Viewer);
        assert!(Role::Admin > Role::Operator && Role::Operator > Role::Viewer);
    }

    #[test]
    fn users_sessions_and_lockout() {
        let dir = std::env::temp_dir().join(format!("indra-users-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let u = Users::open(&dir);
        assert!(!u.any());
        assert!(u.add("alice", Role::Viewer, "short").is_err());
        assert!(u.add("bad name", Role::Viewer, "long enough pass").is_err());
        // The first user is an admin whatever was asked.
        u.add("alice", Role::Viewer, "correct horse battery")
            .unwrap();
        u.add("bob", Role::Operator, "another long secret").unwrap();
        assert!(u.add("bob", Role::Viewer, "another long secret").is_err());
        let l = u.list();
        assert_eq!(l[0].1, Role::Admin);
        assert_eq!(l[1].1, Role::Operator);
        let text = std::fs::read_to_string(dir.join("users.json")).unwrap();
        assert!(!text.contains("correct horse") && text.contains("$argon2id$"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let m = std::fs::metadata(dir.join("users.json"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(m & 0o777, 0o600);
        }

        let (tok, who) = u.sign_in("bob", "another long secret").unwrap();
        assert_eq!(who.role, Role::Operator);
        assert_eq!(u.session(&tok).unwrap().user, "bob");
        assert!(u.session("nope").is_none());
        // A role change applies to live sessions; a password change ends them.
        u.set_role("bob", Role::Viewer).unwrap();
        assert_eq!(u.session(&tok).unwrap().role, Role::Viewer);
        u.set_password("bob", "a brand new password").unwrap();
        assert!(u.session(&tok).is_none());
        let (tok, _) = u.sign_in("bob", "a brand new password").unwrap();
        u.sign_out(&tok);
        assert!(u.session(&tok).is_none());

        // The last admin stays an admin.
        assert!(u.set_role("alice", Role::Viewer).is_err());
        assert!(u.remove("alice").is_err());
        u.set_role("bob", Role::Admin).unwrap();
        u.set_role("alice", Role::Viewer).unwrap();

        // Five wrong passwords lock the name, even the right one then fails.
        for _ in 0..5 {
            assert!(u.sign_in("bob", "wrong wrong wrong").is_err());
        }
        assert!(u
            .sign_in("bob", "a brand new password")
            .unwrap_err()
            .contains("too many"));
        assert!(u.sign_in("nobody", "whatever whatever").is_err());
        u.remove("alice").unwrap();
        assert_eq!(u.list().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// ---- `indra user` ------------------------------------------------------------

#[derive(clap::Args, Debug)]
pub struct UserArgs {
    /// Config file (its data folder holds the users)
    #[arg(long, value_name = "FILE", global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    cmd: UserCmd,
}

#[derive(clap::Subcommand, Debug)]
enum UserCmd {
    /// Add a user (asks for the password; the first user is always an admin)
    Add {
        name: String,
        /// viewer, operator or admin
        #[arg(long, default_value = "operator")]
        role: String,
    },
    /// List users and their roles
    List,
    /// Set a user's password (asks for it)
    Passwd { name: String },
    /// Change a user's role
    Role { name: String, role: String },
    /// Remove a user
    Rm { name: String },
}

/// A password from the terminal without echo, twice; or one line of stdin
/// when it is not a terminal (scripts).
fn read_password(confirm: bool) -> std::result::Result<String, String> {
    use std::io::{BufRead, IsTerminal, Write};
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        let mut l = String::new();
        stdin.lock().read_line(&mut l).map_err(|e| e.to_string())?;
        return Ok(l.trim_end_matches(['\r', '\n']).to_string());
    }
    let ask = |prompt: &str| -> std::result::Result<String, String> {
        eprint!("{}", prompt);
        let _ = std::io::stderr().flush();
        #[cfg(unix)]
        let saved = unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(0, &mut t) == 0 {
                let old = t;
                t.c_lflag &= !libc::ECHO;
                libc::tcsetattr(0, libc::TCSANOW, &t);
                Some(old)
            } else {
                None
            }
        };
        let mut l = String::new();
        let r = stdin.lock().read_line(&mut l);
        #[cfg(unix)]
        if let Some(old) = saved {
            unsafe {
                libc::tcsetattr(0, libc::TCSANOW, &old);
            }
        }
        eprintln!();
        r.map_err(|e| e.to_string())?;
        Ok(l.trim_end_matches(['\r', '\n']).to_string())
    };
    let p = ask("Password: ")?;
    if confirm && ask("Again: ")? != p {
        return Err("the two passwords differ".into());
    }
    Ok(p)
}

pub fn run(a: UserArgs) -> std::result::Result<std::process::ExitCode, String> {
    let s = crate::deck::config::load(crate::deck::config::LoadOptions {
        config: a.config,
        targets: vec![],
        bind: None,
        read_only: true,
        cwd: std::env::current_dir().map_err(|e| e.to_string())?,
    })?;
    let u = Users::open(&s.data_dir);
    match a.cmd {
        UserCmd::Add { name, role } => {
            let role = Role::parse(&role)?;
            let first = !u.any();
            let p = read_password(true)?;
            u.add(&name, role, &p)?;
            println!(
                "added {} ({}){}",
                name,
                if first { "admin" } else { role.as_str() },
                if first {
                    ": the console now asks people to sign in (restart a running `indra serve` to show its sign-in URL)"
                } else {
                    ""
                }
            );
        }
        UserCmd::List => {
            if !u.any() {
                println!("no users: the console's access token is the key (`indra user add NAME` to add one)");
            }
            for (n, r, _) in u.list() {
                println!("{:<24} {}", n, r.as_str());
            }
        }
        UserCmd::Passwd { name } => {
            let p = read_password(true)?;
            u.set_password(&name, &p)?;
            println!("password set for {} (their sessions ended)", name);
        }
        UserCmd::Role { name, role } => {
            u.set_role(&name, Role::parse(&role)?)?;
            println!("{} is now {}", name, role);
        }
        UserCmd::Rm { name } => {
            u.remove(&name)?;
            println!("removed {}", name);
        }
    }
    Ok(std::process::ExitCode::SUCCESS)
}
