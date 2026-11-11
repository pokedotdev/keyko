//! `secrets/config.toml`: the project, its levels and machine keys, which level each file belongs
//! to, branch-to-environment globs and Dokploy applications.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use litekeys_core::{glob_match, glob_to_regex, is_public_key, valid_name};
use serde::Deserialize;

use crate::{Result, fail};

pub const CONFIG_FILE: &str = "config.toml";
pub const SOPS_FILE: &str = ".sops.yaml";

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The project's name on the key server.
    pub project: String,
    /// The key server, e.g. `https://litekeys.example.ts.net`. Without one, people use the
    /// break-glass key file.
    #[serde(default)]
    pub server: Option<String>,
    /// The file `run auto` picks, `{env}` replaced by the branch's environment.
    #[serde(default = "default_auto")]
    pub auto: String,
    /// Maintainers' break-glass public keys: they open every file.
    #[serde(default)]
    pub break_glass: Vec<String>,
    /// Level name to its public key. The private keys live on the key server.
    pub levels: BTreeMap<String, String>,
    /// Machine name (Cloudflare Pages, CI) to its public key.
    #[serde(default)]
    pub machines: BTreeMap<String, String>,
    /// Which level each file under secrets/ belongs to; the first matching rule wins.
    #[serde(default)]
    pub files: Vec<FileRule>,
    /// Branch globs to environments; the first match wins.
    #[serde(default)]
    pub branches: Vec<BranchRule>,
    #[serde(default)]
    pub dokploy: Option<Dokploy>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileRule {
    /// Glob on the path under secrets/, e.g. `production/*.env`. `*` matches slashes too.
    pub path: String,
    pub level: String,
    /// Machine keys that also open these files.
    #[serde(default)]
    pub machines: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BranchRule {
    pub branch: String,
    pub env: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dokploy {
    /// The panel, e.g. `http://my-vps.example.ts.net:3000`. DOKPLOY_URL overrides it.
    #[serde(default)]
    pub url: Option<String>,
    /// The file under secrets/ that holds DOKPLOY_API_KEY (and optionally DOKPLOY_URL).
    #[serde(default = "default_credentials")]
    pub credentials: String,
    /// File under secrets/ to the Dokploy application its environment is saved to.
    #[serde(default)]
    pub apps: BTreeMap<String, String>,
    /// Where the key server runs, so `keygen --level` and `rotate-level` can save level keys.
    #[serde(default)]
    pub key_server: Option<KeyServerApp>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyServerApp {
    /// `compose` (the documented setup, with a Tailscale sidecar) or `application`.
    #[serde(default = "default_kind")]
    pub kind: AppKind,
    pub id: String,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AppKind {
    Compose,
    Application,
}

fn default_auto() -> String {
    "{env}/pages.env".into()
}

fn default_credentials() -> String {
    "ops.env".into()
}

fn default_kind() -> AppKind {
    AppKind::Compose
}

impl Config {
    pub fn parse(text: &str) -> Result<Config> {
        let config: Config =
            toml::from_str(text).map_err(|e| format!("secrets/{CONFIG_FILE}: {e}"))?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        let at = |what: String| format!("secrets/{CONFIG_FILE}: {what}");
        if !valid_name(&self.project) {
            return fail(at(format!("invalid project name {:?}", self.project)));
        }
        if self.levels.is_empty() {
            return fail(at("[levels] is empty".into()));
        }
        for (name, key) in &self.levels {
            if !valid_name(name) {
                return fail(at(format!("invalid level name {name:?}")));
            }
            // An empty key is a level not created yet (`litekeys keygen --level`).
            if !key.is_empty() && !is_public_key(key) {
                return fail(at(format!("level {name} has no age public key")));
            }
        }
        for (name, key) in &self.machines {
            if !is_public_key(key) {
                return fail(at(format!("machine {name} has no age public key")));
            }
        }
        for key in &self.break_glass {
            if !is_public_key(key) {
                return fail(at(format!("break_glass has an invalid public key {key}")));
            }
        }
        for rule in &self.files {
            if !self.levels.contains_key(&rule.level) {
                return fail(at(format!(
                    "{} uses an unknown level {}",
                    rule.path, rule.level
                )));
            }
            for machine in &rule.machines {
                if !self.machines.contains_key(machine) {
                    return fail(at(format!(
                        "{} uses an unknown machine {machine}",
                        rule.path
                    )));
                }
            }
        }
        if !self.auto.contains("{env}") || !self.auto.ends_with(".env") {
            return fail(at("auto must contain {env} and end with .env".into()));
        }
        Ok(())
    }

    /// The rule a path under secrets/ falls under.
    pub fn rule_for(&self, rel: &str) -> Option<&FileRule> {
        self.files.iter().find(|r| glob_match(&r.path, rel))
    }

    pub fn level_of(&self, rel: &str) -> Result<&str> {
        match self.rule_for(rel) {
            Some(rule) => Ok(&rule.level),
            None => fail(format!(
                "secrets/{rel} matches no [[files]] rule in secrets/{CONFIG_FILE}"
            )),
        }
    }

    /// The public keys a file must be encrypted for, labelled, in a stable order.
    pub fn recipients_for(&self, rel: &str) -> Result<Vec<(String, String)>> {
        let Some(rule) = self.rule_for(rel) else {
            return fail(format!(
                "secrets/{rel} matches no [[files]] rule in secrets/{CONFIG_FILE}"
            ));
        };
        self.level_ready(rule)?;
        Ok(self.rule_recipients(rule))
    }

    fn level_ready(&self, rule: &FileRule) -> Result<()> {
        if self.levels[&rule.level].is_empty() {
            return fail(format!(
                "level {} has no key yet; run litekeys keygen --level {}",
                rule.level, rule.level
            ));
        }
        Ok(())
    }

    fn rule_recipients(&self, rule: &FileRule) -> Vec<(String, String)> {
        let mut out = vec![(
            self.levels[&rule.level].clone(),
            format!("level {}", rule.level),
        )];
        for machine in &rule.machines {
            out.push((self.machines[machine].clone(), format!("machine {machine}")));
        }
        for key in &self.break_glass {
            out.push((key.clone(), "break-glass".into()));
        }
        let mut seen = std::collections::HashSet::new();
        out.retain(|(key, _)| seen.insert(key.clone()));
        out
    }

    /// What a public key is in this config, if anything.
    pub fn label_of(&self, key: &str) -> Option<String> {
        if let Some((name, _)) = self.levels.iter().find(|(_, k)| !k.is_empty() && *k == key) {
            return Some(format!("level {name}"));
        }
        if let Some((name, _)) = self.machines.iter().find(|(_, k)| *k == key) {
            return Some(format!("machine {name}"));
        }
        self.break_glass
            .iter()
            .any(|k| k == key)
            .then(|| "break-glass".into())
    }

    /// The environment a branch deploys to.
    pub fn env_for(&self, branch: &str) -> Result<&str> {
        match self.branches.iter().find(|r| glob_match(&r.branch, branch)) {
            Some(rule) => Ok(&rule.env),
            None => fail(format!(
                "no [[branches]] rule in secrets/{CONFIG_FILE} matches the branch {branch}"
            )),
        }
    }

    /// `.sops.yaml`, generated so plain `sops` agrees with litekeys.
    pub fn sops_yaml(&self) -> Result<String> {
        let mut out = String::from(
            "# Generated by litekeys from secrets/config.toml; do not edit.\n\
             # Regenerate and re-encrypt every file with: litekeys rekey\n\
             # The first rule whose path_regex matches the file wins.\n\
             creation_rules:\n",
        );
        for rule in &self.files {
            self.level_ready(rule)?;
            out.push_str(&format!("  # {} -> level {}\n", rule.path, rule.level));
            out.push_str(&format!(
                "  - path_regex: '{}'\n",
                glob_to_regex(&rule.path).replace('\'', "''")
            ));
            out.push_str("    key_groups:\n      - age:\n");
            for (key, label) in self.rule_recipients(rule) {
                out.push_str(&format!("          - {key} # {label}\n"));
            }
        }
        Ok(out)
    }
}

/// A project: its root, its secrets/ directory and its config.
#[derive(Debug, Clone)]
pub struct Project {
    pub root: PathBuf,
    pub dir: PathBuf,
    pub config: Config,
}

impl Project {
    /// Finds the nearest secrets/config.toml at or above `start`.
    pub fn find(start: &Path) -> Result<Project> {
        let mut at = Some(start);
        while let Some(dir) = at {
            let candidate = dir.join("secrets").join(CONFIG_FILE);
            if candidate.is_file() {
                let text = std::fs::read_to_string(&candidate)
                    .map_err(|e| format!("cannot read {}: {e}", candidate.display()))?;
                return Ok(Project {
                    root: dir.to_path_buf(),
                    dir: dir.join("secrets"),
                    config: Config::parse(&text)?,
                });
            }
            at = dir.parent();
        }
        fail(format!(
            "no secrets/{CONFIG_FILE} in {} or above it",
            start.display()
        ))
    }

    pub fn config_path(&self) -> PathBuf {
        self.dir.join(CONFIG_FILE)
    }

    pub fn sops_path(&self) -> PathBuf {
        self.dir.join(SOPS_FILE)
    }

    /// `secrets/production/api.env` for display.
    pub fn show(&self, file: &Path) -> String {
        file.strip_prefix(&self.root)
            .unwrap_or(file)
            .display()
            .to_string()
    }

    /// The path under secrets/, with forward slashes.
    pub fn rel(&self, file: &Path) -> String {
        file.strip_prefix(&self.dir)
            .unwrap_or(file)
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/")
    }

    /// Every .env file under secrets/, sorted.
    pub fn all_files(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        walk(&self.dir, &mut |path| {
            if path.extension().is_some_and(|e| e == "env") {
                out.push(path.to_path_buf());
            }
        });
        out.sort();
        out
    }

    /// Every file under secrets/ that is neither a .env file nor litekeys' own.
    pub fn stray_files(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        walk(&self.dir, &mut |path| {
            let own = path == self.config_path() || path == self.sops_path();
            if !own && path.extension().is_none_or(|e| e != "env") {
                out.push(path.to_path_buf());
            }
        });
        out.sort();
        out
    }

    /// Writes `.sops.yaml` if it differs from what the config generates. True when it changed.
    pub fn write_sops_yaml(&self) -> Result<bool> {
        let want = self.config.sops_yaml()?;
        let path = self.sops_path();
        if std::fs::read_to_string(&path).ok().as_deref() == Some(want.as_str()) {
            return Ok(false);
        }
        std::fs::write(&path, want).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        Ok(true)
    }
}

pub fn walk(dir: &Path, f: &mut dyn FnMut(&Path)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(t) if t.is_dir() => walk(&path, f),
            Ok(t) if t.is_file() => f(&path),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> Vec<String> {
        (0..4).map(|_| litekeys_core::generate_key().1).collect()
    }

    fn sample() -> (Config, Vec<String>) {
        let k = keys();
        let text = format!(
            r#"
project = "demo"
break_glass = ["{bg}"]

[levels]
dev = "{dev}"
production = "{prod}"

[machines]
pages = "{pages}"

[[files]]
path = "dev.env"
level = "dev"

[[files]]
path = "production/*"
level = "production"

[[files]]
path = "*"
level = "dev"
machines = ["pages"]

[[branches]]
branch = "main"
env = "production"

[[branches]]
branch = "*"
env = "preview"
"#,
            bg = k[0],
            dev = k[1],
            prod = k[2],
            pages = k[3]
        );
        (Config::parse(&text).unwrap(), k)
    }

    #[test]
    fn first_rule_wins() {
        let (c, k) = sample();
        assert_eq!(c.level_of("dev.env").unwrap(), "dev");
        assert_eq!(c.level_of("production/api.env").unwrap(), "production");
        assert_eq!(c.level_of("beta/pages.env").unwrap(), "dev");
        let r = c.recipients_for("beta/pages.env").unwrap();
        let keys: Vec<_> = r.iter().map(|(k, _)| k.clone()).collect();
        assert_eq!(keys, vec![k[1].clone(), k[3].clone(), k[0].clone()]);
        assert_eq!(c.env_for("main").unwrap(), "production");
        assert_eq!(c.env_for("feature/x").unwrap(), "preview");
        assert_eq!(c.label_of(&k[3]).unwrap(), "machine pages");
    }

    #[test]
    fn generates_sops_yaml() {
        let (c, k) = sample();
        let y = c.sops_yaml().unwrap();
        assert!(y.contains("path_regex: '^production/.*$'"));
        assert!(y.contains(&format!("- {} # level production", k[2])));
        assert!(y.find("^dev\\.env$").unwrap() < y.find("^production").unwrap());
    }

    #[test]
    fn rejects_bad_configs() {
        let k = keys();
        let bad = [
            format!("project = \"Bad\"\n[levels]\ndev = \"{}\"", k[0]),
            "project = \"a\"\n[levels]\ndev = \"nope\"".to_string(),
            format!(
                "project = \"a\"\n[levels]\ndev = \"{}\"\n[[files]]\npath = \"*\"\nlevel = \"prod\"",
                k[0]
            ),
            format!("project = \"a\"\nunknown = 1\n[levels]\ndev = \"{}\"", k[0]),
        ];
        for text in bad {
            assert!(Config::parse(&text).is_err(), "{text}");
        }
    }
}
