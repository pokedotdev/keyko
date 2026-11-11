//! `secrets/config.toml`: the project, its levels, people and machine keys, which level each file
//! belongs to, branch-to-environment globs and Dokploy applications.
//!
//! A level is opened by its shared key (in `[levels]`, served by the key server), by the people
//! given it in `[people]`, or both. A level that only people open needs no key server.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use tierkey_core::{glob_match, glob_to_regex, is_public_key, valid_name};

use crate::{Result, fail};

pub const CONFIG_FILE: &str = "config.toml";
pub const SOPS_FILE: &str = ".sops.yaml";

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The project's name on the key server.
    pub project: String,
    /// The key server, e.g. `https://tierkey.example.ts.net`. Without one, people use the
    /// break-glass key file.
    #[serde(default)]
    pub server: Option<String>,
    /// The file `run auto` picks, `{env}` replaced by the branch's environment.
    #[serde(default = "default_auto")]
    pub auto: String,
    /// Maintainers' break-glass public keys: they open every file.
    #[serde(default)]
    pub break_glass: Vec<String>,
    /// Level name to its shared public key. The private keys live on the key server. Levels
    /// that only people open are not listed here.
    #[serde(default)]
    pub levels: BTreeMap<String, String>,
    /// People who open files with their own age key: name to their key and levels.
    #[serde(default)]
    pub people: BTreeMap<String, Person>,
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

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Person {
    /// Their age public key; the private key stays on their machine.
    pub key: String,
    /// The levels whose files are encrypted for them.
    pub levels: Vec<String>,
    /// The day (`YYYY-MM-DD`) a maintainer invited them with `tierkey people invite`. While set,
    /// `key` is the invite's key, and `tierkey join` with the invite code replaces it with theirs.
    #[serde(default)]
    pub invited: Option<String>,
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
        if self.levels.is_empty() && self.people.is_empty() {
            return fail(at("[levels] and [people] are both empty".into()));
        }
        for (name, key) in &self.levels {
            if !valid_name(name) {
                return fail(at(format!("invalid level name {name:?}")));
            }
            // An empty key is a level not created yet (`tierkey keygen --level`).
            if !key.is_empty() && !is_public_key(key) {
                return fail(at(format!("level {name} has no age public key")));
            }
        }
        let used: std::collections::BTreeSet<&str> =
            self.files.iter().map(|r| r.level.as_str()).collect();
        for (name, person) in &self.people {
            if !valid_name(name) {
                return fail(at(format!("invalid person name {name:?}")));
            }
            if !is_public_key(&person.key) {
                return fail(at(format!("{name} has no age public key")));
            }
            if let Some((other, _)) = self
                .people
                .iter()
                .find(|(other, p)| *other < name && p.key == person.key)
            {
                return fail(at(format!("{name} and {other} have the same key")));
            }
            if let Some(day) = &person.invited
                && days_from_date(day).is_none()
            {
                return fail(at(format!(
                    "{name} has invited = {day:?}, which is not a date like 2026-01-31"
                )));
            }
            if person.levels.is_empty() {
                return fail(at(format!(
                    "{name} has no levels; remove them with tierkey people remove {name}"
                )));
            }
            for level in &person.levels {
                if !self.levels.contains_key(level) && !used.contains(level.as_str()) {
                    return fail(at(format!(
                        "{name} has level {level}, which no [[files]] rule uses and [levels] does not list"
                    )));
                }
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
            if !self.levels.contains_key(&rule.level) && self.holders(&rule.level).is_empty() {
                return fail(at(format!(
                    "{} uses level {level}, which has no shared key in [levels] and nobody in [people]; \
                     give it to someone with tierkey people add NAME KEY --levels {level}",
                    rule.path,
                    level = rule.level
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

    /// Every level: those with a shared key and those only people open.
    pub fn all_levels(&self) -> std::collections::BTreeSet<String> {
        self.levels
            .keys()
            .chain(self.files.iter().map(|r| &r.level))
            .cloned()
            .collect()
    }

    /// Whether the level has a shared key on the key server (or is waiting for one).
    pub fn is_shared(&self, level: &str) -> bool {
        self.levels.contains_key(level)
    }

    /// The people given a level, by name.
    pub fn holders(&self, level: &str) -> Vec<&str> {
        self.people
            .iter()
            .filter(|(_, p)| p.levels.iter().any(|l| l == level))
            .map(|(n, _)| n.as_str())
            .collect()
    }

    /// The person whose key this is.
    pub fn person_of(&self, key: &str) -> Option<(&str, &Person)> {
        self.people
            .iter()
            .find(|(_, p)| p.key == key)
            .map(|(n, p)| (n.as_str(), p))
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
        if self.levels.get(&rule.level).is_some_and(|k| k.is_empty()) {
            return fail(format!(
                "level {} has no key yet; run tierkey keygen --level {}",
                rule.level, rule.level
            ));
        }
        Ok(())
    }

    fn rule_recipients(&self, rule: &FileRule) -> Vec<(String, String)> {
        let mut out = Vec::new();
        if let Some(key) = self.levels.get(&rule.level) {
            out.push((key.clone(), format!("level {}", rule.level)));
        }
        for name in self.holders(&rule.level) {
            out.push((self.people[name].key.clone(), format!("person {name}")));
        }
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
        if let Some((name, _)) = self.person_of(key) {
            return Some(format!("person {name}"));
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

    /// `.sops.yaml`, generated so plain `sops` agrees with tierkey.
    pub fn sops_yaml(&self) -> Result<String> {
        let mut out = String::from(
            "# Generated by tierkey from secrets/config.toml; do not edit.\n\
             # Regenerate and re-encrypt every file with: tierkey rekey\n\
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

/// Days since 1970-01-01 of a `YYYY-MM-DD` date.
pub fn days_from_date(date: &str) -> Option<i64> {
    let mut parts = date.split('-');
    let (y, m, d) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || y.len() != 4 || m.len() != 2 || d.len() != 2 {
        return None;
    }
    let (y, m, d): (i64, i64, i64) = (y.parse().ok()?, m.parse().ok()?, d.parse().ok()?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // Howard Hinnant's days_from_civil.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146097 + doe - 719468)
}

/// Days since 1970-01-01, today (UTC).
pub fn today() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| (d.as_secs() / 86400) as i64)
        .unwrap_or(0)
}

/// `YYYY-MM-DD` of a day since 1970-01-01.
pub fn date_of(days: i64) -> String {
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}")
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
            "no secrets/{CONFIG_FILE} in {} or above it; start one with: tierkey init",
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

    /// Every file under secrets/ that is neither a .env file nor tierkey' own.
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
        (0..4).map(|_| tierkey_core::generate_key().1).collect()
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

    #[test]
    fn people_open_their_levels() {
        let k = keys();
        let text = format!(
            r#"
project = "demo"
break_glass = ["{bg}"]

[levels]
dev = "{dev}"

[people]
bob = {{ key = "{bob}", levels = ["production"] }}
alice = {{ key = "{alice}", levels = ["dev", "production"] }}

[[files]]
path = "dev.env"
level = "dev"

[[files]]
path = "*"
level = "production"
"#,
            bg = k[0],
            dev = k[1],
            alice = k[2],
            bob = k[3]
        );
        let c = Config::parse(&text).unwrap();
        let keys_of = |rel: &str| -> Vec<String> {
            c.recipients_for(rel)
                .unwrap()
                .into_iter()
                .map(|(k, _)| k)
                .collect()
        };
        // Level key, then people by name, then break-glass.
        assert_eq!(
            keys_of("dev.env"),
            [k[1].clone(), k[2].clone(), k[0].clone()]
        );
        assert_eq!(
            keys_of("production/api.env"),
            [k[2].clone(), k[3].clone(), k[0].clone()]
        );
        assert!(c.is_shared("dev") && !c.is_shared("production"));
        assert_eq!(c.holders("production"), ["alice", "bob"]);
        assert_eq!(c.label_of(&k[3]).unwrap(), "person bob");
        assert!(
            c.sops_yaml()
                .unwrap()
                .contains(&format!("- {} # person alice", k[2]))
        );
    }

    #[test]
    fn converts_dates() {
        assert_eq!(days_from_date("1970-01-01"), Some(0));
        assert_eq!(days_from_date("2026-11-11"), Some(20768));
        assert_eq!(date_of(20768), "2026-11-11");
        assert_eq!(date_of(days_from_date("2024-02-29").unwrap()), "2024-02-29");
        for bad in [
            "2026-13-01",
            "2026-1-01",
            "26-01-01",
            "2026-01-01-1",
            "soon",
        ] {
            assert_eq!(days_from_date(bad), None, "{bad}");
        }
    }

    #[test]
    fn rejects_bad_people() {
        let k = keys();
        let rule = "[[files]]\npath = \"*\"\nlevel = \"dev\"\n";
        let bad = [
            // Nobody opens a level with no shared key.
            format!(
                "project = \"a\"\n[people]\nal = {{ key = \"{}\", levels = [\"dev\"] }}\n[[files]]\npath = \"*\"\nlevel = \"prod\"",
                k[0]
            ),
            // A level no rule uses.
            format!(
                "project = \"a\"\n[people]\nal = {{ key = \"{}\", levels = [\"dev\", \"typo\"] }}\n{rule}",
                k[0]
            ),
            format!(
                "project = \"a\"\n[people]\nal = {{ key = \"nope\", levels = [\"dev\"] }}\n{rule}"
            ),
            format!(
                "project = \"a\"\n[people]\nal = {{ key = \"{}\", levels = [] }}\n{rule}",
                k[0]
            ),
            format!(
                "project = \"a\"\n[people]\nAl = {{ key = \"{}\", levels = [\"dev\"] }}\n{rule}",
                k[0]
            ),
            format!(
                "project = \"a\"\n[people]\nal = {{ key = \"{0}\", levels = [\"dev\"] }}\nbo = {{ key = \"{0}\", levels = [\"dev\"] }}\n{rule}",
                k[0]
            ),
            format!("project = \"a\"\n{rule}"),
        ];
        for text in bad {
            assert!(Config::parse(&text).is_err(), "{text}");
        }
        let good = format!(
            "project = \"a\"\n[people]\nal = {{ key = \"{}\", levels = [\"dev\"] }}\n{rule}",
            k[0]
        );
        assert!(Config::parse(&good).is_ok());
    }
}
