//! Which age identity sops gets: `SOPS_AGE_KEY`/`SOPS_AGE_KEY_FILE` when set (CI, Cloudflare
//! Pages); else your own key for the levels `[people]` gives you, plus level keys fetched from the
//! key server into memory for the rest; else the break-glass key file. Commands that re-encrypt
//! get the break-glass key alongside.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use tierkey_core::{AGENT_LEVEL, AccessResponse, KeysResponse, public_key_of};

use crate::config::Project;
use crate::{Result, fail, warn};

/// What sops gets in its environment.
#[derive(Debug, Clone)]
pub enum KeyEnv {
    /// The caller set SOPS_AGE_KEY or SOPS_AGE_KEY_FILE.
    Inherit,
    /// Private keys for SOPS_AGE_KEY, one per line. Only ever in memory and sops's environment.
    Keys(String),
    /// No key: enough to encrypt a new file.
    None,
}

pub struct Keys<'a> {
    project: &'a Project,
    /// Claude Code (`CLAUDECODE=1`): only the agent level, never the break-glass key.
    pub agent: bool,
    /// Level keys fetched so far: each invocation asks the server at most once per level.
    fetched: RefCell<BTreeMap<String, String>>,
}

pub fn is_agent() -> bool {
    std::env::var_os("CLAUDECODE").is_some_and(|v| !v.is_empty())
}

fn env_set(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| !v.is_empty())
}

/// Your own key: TIERKEY_KEY_FILE, else `tierkey/key.txt` in the user's config directory
/// (`$XDG_CONFIG_HOME`, else `~/.config`). One key per person, for every project.
pub fn personal_key_file() -> PathBuf {
    if let Some(file) = std::env::var_os("TIERKEY_KEY_FILE").filter(|v| !v.is_empty()) {
        return PathBuf::from(file);
    }
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"));
    config.join("tierkey").join("key.txt")
}

/// The age identities in a key file.
#[derive(Clone)]
pub struct KeyFile {
    pub path: PathBuf,
    /// `AGE-SECRET-KEY-1...` lines.
    pub private: Vec<String>,
    /// Their public keys, in the same order.
    pub public: Vec<String>,
}

impl KeyFile {
    /// None when the file is missing or holds no key.
    pub fn read(path: &Path) -> Option<KeyFile> {
        let text = std::fs::read_to_string(path).ok()?;
        let private: Vec<String> = text
            .lines()
            .map(str::trim)
            .filter(|l| l.starts_with("AGE-SECRET-KEY-"))
            .map(str::to_string)
            .collect();
        let public: Vec<String> = private.iter().filter_map(|k| public_key_of(k)).collect();
        (!private.is_empty()).then(|| KeyFile {
            path: path.to_path_buf(),
            private,
            public,
        })
    }
}

/// The keys of the person running tierkey, as this project sees them.
pub struct Mine {
    /// Key files to hand to sops: your own, and `.age/key.txt` when it is a person's key.
    pub files: Vec<KeyFile>,
    /// Who you are in `[people]`, and with which key.
    pub person: Option<(String, String)>,
    /// Under an agent: your key file is a break-glass key, which agents never use.
    pub blocked_break_glass: bool,
}

impl Mine {
    pub fn public(&self) -> Option<&str> {
        match &self.person {
            Some((_, key)) => Some(key),
            None => self
                .files
                .first()
                .and_then(|f| f.public.first())
                .map(String::as_str),
        }
    }
}

/// A command line to repeat, for error messages: `tierkey run production/api -- ./deploy.sh`.
pub fn this_command() -> String {
    std::iter::once("tierkey".to_string())
        .chain(std::env::args().skip(1).map(|a| shell_quote(&a)))
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn shell_quote(arg: &str) -> String {
    let plain = !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-./:=,@%+".contains(c));
    if plain {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', "'\\''"))
    }
}

/// A name for someone in `[people]`: git's user.name, else $USER, as lowercase dashed words.
pub fn suggested_name() -> String {
    let git = Command::new("git")
        .args(["config", "user.name"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    git.into_iter()
        .chain(std::env::var("USER").ok())
        .find_map(|n| to_name(&n))
        .unwrap_or_else(|| "me".into())
}

/// Lowercase letters and digits in dash-separated words, or None when nothing is left.
pub fn to_name(text: &str) -> Option<String> {
    let mapped: String = text
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() {
                c
            } else {
                '-'
            }
        })
        .collect();
    let name = mapped
        .split('-')
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    (!name.is_empty()).then_some(name)
}

impl<'a> Keys<'a> {
    pub fn new(project: &'a Project) -> Keys<'a> {
        Keys {
            project,
            agent: is_agent(),
            fetched: RefCell::new(BTreeMap::new()),
        }
    }

    /// The key server's URL: TIERKEY_SERVER (empty or `off` disables it), else the config's.
    pub fn server(&self) -> Option<String> {
        match std::env::var("TIERKEY_SERVER") {
            Ok(v) if v.is_empty() || v == "off" => None,
            Ok(v) => Some(v),
            Err(_) => self.project.config.server.clone(),
        }
        .map(|s| s.trim_end_matches('/').to_string())
    }

    /// TIERKEY_BREAK_GLASS_KEY_FILE, else `.age/key.txt` next to the main checkout's git
    /// directory, so every worktree shares it.
    pub fn break_glass_file(&self) -> PathBuf {
        if let Some(file) = std::env::var_os("TIERKEY_BREAK_GLASS_KEY_FILE") {
            return PathBuf::from(file);
        }
        let common = Command::new("git")
            .arg("-C")
            .arg(&self.project.root)
            .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()));
        let base = common
            .as_deref()
            .and_then(|c| c.parent())
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| self.project.root.clone());
        base.join(".age").join("key.txt")
    }

    /// Your key file, and `.age/key.txt` when the config lists its key under `[people]` (as in
    /// projects that kept one key per person there).
    pub fn mine(&self) -> Mine {
        let config = &self.project.config;
        let is_break_glass = |f: &KeyFile| f.public.iter().any(|k| config.break_glass.contains(k));
        let person_in = |f: &KeyFile| {
            f.public
                .iter()
                .find_map(|k| config.person_of(k).map(|(n, _)| (n.to_string(), k.clone())))
        };
        let mut mine = Mine {
            files: Vec::new(),
            person: None,
            blocked_break_glass: false,
        };
        if let Some(file) = KeyFile::read(&personal_key_file()) {
            if self.agent && is_break_glass(&file) {
                mine.blocked_break_glass = true;
            } else {
                mine.person = person_in(&file);
                mine.files.push(file);
            }
        }
        if mine.person.is_none()
            && let Some(file) = KeyFile::read(&self.break_glass_file())
            && !is_break_glass(&file)
            && let Some(person) = person_in(&file)
        {
            mine.person = Some(person);
            mine.files.push(file);
        }
        mine
    }

    /// An identity for sops that opens files of every level in `levels`.
    pub fn for_levels(&self, levels: &BTreeSet<String>) -> Result<KeyEnv> {
        self.identity(levels, false)
    }

    /// Like `for_levels`, for commands that re-encrypt files (`rekey`, `revoke`, `rotate-level`,
    /// `merge-fix`, `people`): the break-glass key, when there is one, goes along with the
    /// other keys. Files that are not yet encrypted for their level, such as right after a
    /// migration adds levels, only open with it.
    pub fn for_rewrite(&self, levels: &BTreeSet<String>) -> Result<KeyEnv> {
        self.identity(levels, true)
    }

    fn identity(&self, levels: &BTreeSet<String>, with_break_glass: bool) -> Result<KeyEnv> {
        if env_set("SOPS_AGE_KEY") || env_set("SOPS_AGE_KEY_FILE") {
            return Ok(KeyEnv::Inherit);
        }
        if levels.is_empty() {
            return Ok(KeyEnv::None);
        }
        if self.agent
            && let Some(level) = levels.iter().find(|l| *l != AGENT_LEVEL)
        {
            return fail(format!(
                "agents can only open {AGENT_LEVEL}-level files, and this needs level {level}; \
                 ask the user to run it: {}",
                this_command()
            ));
        }
        let config = &self.project.config;
        let mine = self.mine();
        let mut ids: Vec<String> = mine.files.iter().flat_map(|f| f.private.clone()).collect();
        let person = mine
            .person
            .as_ref()
            .and_then(|(name, _)| config.people.get(name).map(|p| (name, p)));
        let missing: BTreeSet<String> = levels
            .iter()
            .filter(|l| person.is_none_or(|(_, p)| !p.levels.contains(l)))
            .cloned()
            .collect();
        let break_glass = self.break_glass_file();
        let has_break_glass = !self.agent
            && mine.files.iter().all(|f| f.path != break_glass)
            && break_glass.metadata().is_ok_and(|m| m.len() > 0);
        let mut use_break_glass = with_break_glass && has_break_glass;
        let (shared, personal): (BTreeSet<String>, BTreeSet<String>) =
            missing.into_iter().partition(|l| config.is_shared(l));
        if let Some(level) = personal.first() {
            if !has_break_glass {
                return fail(self.no_access(&mine, level));
            }
            use_break_glass = true;
        }
        if !shared.is_empty() {
            match self.server() {
                Some(server) => match self.fetch(&server, &shared) {
                    Ok(keys) => ids.push(keys),
                    Err(e) if has_break_glass => {
                        warn(&format!(
                            "{e}; using the break-glass key {}",
                            break_glass.display()
                        ));
                        use_break_glass = true;
                    }
                    Err(e) => return Err(e),
                },
                None if has_break_glass => use_break_glass = true,
                None if !config.holders(shared.first().unwrap()).is_empty() => {
                    return fail(self.no_access(&mine, shared.first().unwrap()));
                }
                None if self.agent => {
                    return fail(format!(
                        "agents need the key server or a personal key for level {AGENT_LEVEL}, \
                         and this project has no key server; run: tierkey join"
                    ));
                }
                None => {
                    return fail(format!(
                        "this project has no key server and there is no break-glass key at {}; \
                         to open files with your own key, run: tierkey join",
                        break_glass.display()
                    ));
                }
            }
        }
        if use_break_glass {
            match KeyFile::read(&break_glass) {
                Some(file) => ids.extend(file.private),
                None => warn(&format!("cannot read a key from {}", break_glass.display())),
            }
        }
        Ok(KeyEnv::Keys(ids.join("\n")))
    }

    /// Why you cannot open a level through `[people]`, and the command that fixes it.
    pub fn no_access(&self, mine: &Mine, level: &str) -> String {
        let project = &self.project.config.project;
        if mine.blocked_break_glass {
            return format!(
                "your key file {} holds a break-glass key, which agents never use; give the agent \
                 a personal key with level {level} (tierkey join, with TIERKEY_KEY_FILE set to a new file)",
                personal_key_file().display()
            );
        }
        match (&mine.person, mine.public()) {
            (Some((name, _)), _) => format!(
                "{name} does not have level {level} of {project}; ask a maintainer to run: \
                 tierkey people add {name} --levels {level}"
            ),
            (None, Some(public)) => format!(
                "your key is not in [people] of secrets/config.toml; ask a maintainer to run: \
                 tierkey people add {} {public} --levels {level}",
                suggested_name()
            ),
            (None, None) => format!(
                "level {level} of {project} opens with personal keys, and you have none at {}; run: tierkey join",
                personal_key_file().display()
            ),
        }
    }

    /// The level keys, joined for SOPS_AGE_KEY. Asks the server only for levels not fetched yet.
    fn fetch(&self, server: &str, levels: &BTreeSet<String>) -> Result<String> {
        let missing: Vec<&str> = {
            let fetched = self.fetched.borrow();
            levels
                .iter()
                .filter(|l| !fetched.contains_key(*l))
                .map(String::as_str)
                .collect()
        };
        if !missing.is_empty() {
            let project = &self.project.config.project;
            let url = format!("{server}/v1/keys/{project}?levels={}", missing.join(","));
            let body: KeysResponse = get_json(server, &url)?;
            for level in &missing {
                let Some(keys) = body.keys.get(*level) else {
                    return fail(format!(
                        "the key server does not give {} level {level} of {project}; ask a \
                         maintainer to add you to the group that grants it",
                        body.login
                    ));
                };
                let want = &self.project.config.levels[*level];
                let found = keys
                    .split(',')
                    .any(|k| public_key_of(k).as_deref() == Some(want));
                if !found {
                    warn(&format!(
                        "the key server has no key for the public key secrets/config.toml lists \
                         for level {level}; files may not open"
                    ));
                }
                self.fetched
                    .borrow_mut()
                    .insert(level.to_string(), keys.clone());
            }
        }
        let fetched = self.fetched.borrow();
        Ok(levels
            .iter()
            .flat_map(|l| fetched[l].split(','))
            .collect::<Vec<_>>()
            .join("\n"))
    }

    /// Who the key server sees and which levels it would give.
    pub fn access(&self) -> Result<AccessResponse> {
        let Some(server) = self.server() else {
            return fail("this project has no key server (server in secrets/config.toml)");
        };
        let url = format!("{server}/v1/access/{}", self.project.config.project);
        get_json(&server, &url)
    }
}

fn get_json<T: serde::de::DeserializeOwned>(server: &str, url: &str) -> Result<T> {
    let mut response = crate::http::agent().get(url).call().map_err(|e| {
        format!("cannot reach the key server at {server} ({e}); are you on the tailnet? (tailscale status)")
    })?;
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("cannot read the key server's answer: {e}"))?;
    if response.status() != 200 {
        return fail(format!(
            "the key server refused ({}): {}",
            response.status(),
            crate::http::error_text(&body)
        ));
    }
    serde_json::from_str(&body)
        .map_err(|e| format!("the key server sent an unexpected answer: {e}"))
}
