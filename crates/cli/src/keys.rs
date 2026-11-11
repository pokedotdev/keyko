//! Which age identity sops gets: `SOPS_AGE_KEY`/`SOPS_AGE_KEY_FILE` when set (CI, Cloudflare
//! Pages), else level keys fetched from the key server into memory, else the break-glass key file.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::Command;

use litekeys_core::{AGENT_LEVEL, AccessResponse, KeysResponse, public_key_of};

use crate::config::Project;
use crate::{Result, fail, warn};

/// What sops gets in its environment.
#[derive(Debug, Clone)]
pub enum KeyEnv {
    /// The caller set SOPS_AGE_KEY or SOPS_AGE_KEY_FILE.
    Inherit,
    /// Private keys for SOPS_AGE_KEY, one per line. Only ever in memory and sops's environment.
    Keys(String),
    /// A key file for SOPS_AGE_KEY_FILE.
    File(PathBuf),
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

impl<'a> Keys<'a> {
    pub fn new(project: &'a Project) -> Keys<'a> {
        Keys {
            project,
            agent: is_agent(),
            fetched: RefCell::new(BTreeMap::new()),
        }
    }

    /// The key server's URL: LITEKEYS_SERVER (empty or `off` disables it), else the config's.
    pub fn server(&self) -> Option<String> {
        match std::env::var("LITEKEYS_SERVER") {
            Ok(v) if v.is_empty() || v == "off" => None,
            Ok(v) => Some(v),
            Err(_) => self.project.config.server.clone(),
        }
        .map(|s| s.trim_end_matches('/').to_string())
    }

    /// LITEKEYS_BREAK_GLASS_KEY_FILE, else `.age/key.txt` next to the main checkout's git
    /// directory, so every worktree shares it.
    pub fn break_glass_file(&self) -> PathBuf {
        if let Some(file) = std::env::var_os("LITEKEYS_BREAK_GLASS_KEY_FILE") {
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

    /// An identity for sops that opens files of every level in `levels`.
    pub fn for_levels(&self, levels: &BTreeSet<String>) -> Result<KeyEnv> {
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
                 leave the command for a maintainer"
            ));
        }
        let break_glass = self.break_glass_file();
        let has_break_glass = !self.agent && break_glass.metadata().is_ok_and(|m| m.len() > 0);
        match self.server() {
            Some(server) => match self.fetch(&server, levels) {
                Ok(keys) => Ok(KeyEnv::Keys(keys)),
                Err(e) if has_break_glass => {
                    warn(&format!(
                        "{e}; using the break-glass key {}",
                        break_glass.display()
                    ));
                    Ok(KeyEnv::File(break_glass))
                }
                Err(e) => Err(e),
            },
            None if has_break_glass => Ok(KeyEnv::File(break_glass)),
            None if self.agent => fail("agents need the key server, and this project has none"),
            None => fail(format!(
                "this project has no key server and there is no break-glass key at {}",
                break_glass.display()
            )),
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
