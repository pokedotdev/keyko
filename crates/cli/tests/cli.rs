//! Runs the litekeys binary against throwaway git repositories with throwaway age keys, a fake
//! key server and a fake Dokploy. Needs `sops` on PATH (the Nix dev shell has it); without it the
//! tests are skipped, unless LITEKEYS_REQUIRE_SOPS=1 (CI) makes that a failure.

mod fake;

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};

use fake::{Fake, Request, serve};
use litekeys_core::generate_key;
use serde_json::{Value, json};

fn has_sops() -> bool {
    let found = Command::new("sops")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok();
    if !found && std::env::var_os("LITEKEYS_REQUIRE_SOPS").is_some() {
        panic!("sops is not installed and LITEKEYS_REQUIRE_SOPS is set");
    }
    if !found {
        eprintln!("skipping: sops is not installed");
    }
    found
}

struct Key {
    private: String,
    public: String,
}

fn key() -> Key {
    let (private, public) = generate_key();
    Key { private, public }
}

struct Repo {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    levels: BTreeMap<&'static str, Key>,
    pages: Key,
    break_glass: Key,
    server: Fake,
    /// Levels the fake key server gives.
    granted: Arc<Mutex<BTreeSet<String>>>,
    /// Level -> keys the fake key server holds.
    held: Arc<Mutex<BTreeMap<String, String>>>,
}

struct Run {
    status: i32,
    stdout: String,
    stderr: String,
}

impl Run {
    fn ok(&self) -> bool {
        self.status == 0
    }
    fn all(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }
}

impl From<Output> for Run {
    fn from(o: Output) -> Run {
        Run {
            status: o.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&o.stdout).into(),
            stderr: String::from_utf8_lossy(&o.stderr).into(),
        }
    }
}

fn key_server(
    granted: Arc<Mutex<BTreeSet<String>>>,
    held: Arc<Mutex<BTreeMap<String, String>>>,
) -> Fake {
    serve(Box::new(move |req: &Request| {
        let granted = granted.lock().unwrap().clone();
        let held = held.lock().unwrap().clone();
        let (path, query) = req.path.split_once('?').unwrap_or((&req.path, ""));
        let given: Vec<&String> = granted.iter().filter(|l| held.contains_key(*l)).collect();
        match path {
            "/v1/access/demo" => (
                200,
                json!({"project": "demo", "login": "alice@example.com", "levels": given})
                    .to_string(),
            ),
            "/v1/keys/demo" => {
                let requested: Vec<&str> = query
                    .strip_prefix("levels=")
                    .unwrap_or("")
                    .split(',')
                    .collect();
                let keys: BTreeMap<&str, &String> = requested
                    .iter()
                    .filter(|l| granted.contains(**l))
                    .filter_map(|l| held.get(*l).map(|k| (*l, k)))
                    .collect();
                if keys.is_empty() {
                    return (
                        403,
                        json!({"error": "alice@example.com may not have that"}).to_string(),
                    );
                }
                (
                    200,
                    json!({"project": "demo", "login": "alice@example.com", "keys": keys})
                        .to_string(),
                )
            }
            _ => (404, "{}".into()),
        }
    }))
}

impl Repo {
    fn new() -> Repo {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        std::fs::create_dir_all(root.join("secrets")).unwrap();
        let levels = BTreeMap::from([("dev", key()), ("preview", key()), ("production", key())]);
        let (pages, break_glass) = (key(), key());
        let granted = Arc::new(Mutex::new(BTreeSet::from([
            "dev".to_string(),
            "preview".to_string(),
            "production".to_string(),
        ])));
        let held = Arc::new(Mutex::new(
            levels
                .iter()
                .map(|(l, k)| (l.to_string(), k.private.clone()))
                .collect(),
        ));
        let server = key_server(granted.clone(), held.clone());
        let repo = Repo {
            _tmp: tmp,
            root,
            levels,
            pages,
            break_glass,
            server,
            granted,
            held,
        };
        repo.write_config("");
        repo
    }

    fn config(&self, extra: &str) -> String {
        format!(
            r#"# A test project.
project = "demo"
break_glass = ["{bg}"]

[levels]
dev = "{dev}"
preview = "{preview}"
production = "{production}"

[machines]
pages = "{pages}"

[[files]]
path = "dev.env"
level = "dev"

[[files]]
path = "ops.env"
level = "production"

[[files]]
path = "production/*"
level = "production"

[[files]]
path = "*"
level = "preview"
machines = ["pages"]

[[branches]]
branch = "main"
env = "production"

[[branches]]
branch = "bs/*"
env = "beta"

[[branches]]
branch = "*"
env = "preview"
{extra}"#,
            bg = self.break_glass.public,
            dev = self.levels["dev"].public,
            preview = self.levels["preview"].public,
            production = self.levels["production"].public,
            pages = self.pages.public,
        )
    }

    fn write_config(&self, extra: &str) {
        std::fs::write(self.root.join("secrets/config.toml"), self.config(extra)).unwrap();
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.path(rel)).unwrap()
    }

    fn command(&self, args: &[&str], env: &[(&str, &str)]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_litekeys"));
        cmd.args(args).current_dir(&self.root);
        for (k, _) in std::env::vars() {
            let strip = ["SOPS_", "LITEKEYS_", "GITHUB_", "DOKPLOY_", "CF_PAGES"]
                .iter()
                .any(|p| k.starts_with(p))
                || k == "CLAUDECODE";
            if strip {
                cmd.env_remove(k);
            }
        }
        cmd.env("LITEKEYS_SERVER", &self.server.url);
        cmd.env(
            "LITEKEYS_BREAK_GLASS_KEY_FILE",
            self.root.join(".age/none.txt"),
        );
        cmd.envs(env.iter().copied());
        cmd
    }

    fn run_env(&self, args: &[&str], input: &str, env: &[(&str, &str)]) -> Run {
        let mut child = self
            .command(args, env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap().into()
    }

    fn run(&self, args: &[&str]) -> Run {
        self.run_env(args, "", &[])
    }

    fn set(&self, target: &str, name: &str, value: &str) {
        let r = self.run_env(&["set", target, name], value, &[]);
        assert!(r.ok(), "set {target} {name}: {}", r.stderr);
    }

    /// A variable's value as the file's command sees it.
    fn value(&self, target: &str, name: &str, env: &[(&str, &str)]) -> Run {
        self.run_env(
            &[
                "run",
                target,
                "--",
                "sh",
                "-c",
                &format!("printf %s \"${name}\""),
            ],
            "",
            env,
        )
    }

    fn requests(&self) -> Vec<String> {
        self.server
            .requests
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.path.clone())
            .collect()
    }
}

fn git(root: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .args(args)
            .current_dir(root)
            .status()
            .unwrap()
            .success()
    );
}

#[test]
fn maps_branches_to_environments_first_match_first() {
    let repo = Repo::new();
    assert_eq!(repo.run(&["env", "main"]).stdout.trim(), "production");
    assert_eq!(repo.run(&["env", "bs/boss-fight"]).stdout.trim(), "beta");
    assert_eq!(repo.run(&["env", "fix/typo"]).stdout.trim(), "preview");
    assert_eq!(
        repo.run_env(&["env"], "", &[("CF_PAGES_BRANCH", "bs/x")])
            .stdout
            .trim(),
        "beta"
    );
    assert_eq!(repo.run(&["env"]).stdout.trim(), "production");
}

#[test]
fn sets_runs_and_never_prints_values() {
    if !has_sops() {
        return;
    }
    let repo = Repo::new();
    let created = repo.run_env(
        &["set", "production/api", "DATABASE_URL"],
        "postgres://x y\n",
        &[],
    );
    assert!(created.ok(), "{}", created.stderr);
    assert!(
        repo.run(&["set", "production/api", "TOKEN", "--random", "32"])
            .ok()
    );
    assert!(
        !repo
            .run(&["set", "production/api", "TOKEN", "--random", "8"])
            .ok()
    );
    let changed = repo.run_env(
        &["set", "production/api", "DATABASE_URL"],
        "postgres://z",
        &[],
    );
    assert!(changed.ok(), "{}", changed.stderr);
    assert!(
        !repo
            .run_env(&["set", "production/api", "X"], "a\nb\n", &[])
            .ok()
    );
    assert!(
        !repo
            .run_env(&["set", "production/api", "sops_x"], "a", &[])
            .ok()
    );
    for r in [&created, &changed] {
        assert!(!r.all().contains("postgres://"));
    }
    let file = repo.read("secrets/production/api.env");
    assert!(!file.contains("postgres://"));
    assert!(file.lines().any(|l| l.starts_with("DATABASE_URL=ENC[")));
    assert_eq!(
        repo.run(&["names", "production/api"]).stdout,
        "DATABASE_URL\nTOKEN\n"
    );
    assert!(
        repo.read("secrets/.sops.yaml")
            .contains("Generated by litekeys")
    );

    let script = r#"test "$DATABASE_URL" = "postgres://z" && test ${#TOKEN} -eq 64 && echo "$1""#;
    let r = repo.run(&[
        "run",
        "production/api",
        "--",
        "sh",
        "-c",
        script,
        "sh",
        "a b",
    ]);
    assert!(r.ok(), "{}", r.stderr);
    assert_eq!(r.stdout, "a b\n");
    // The fetched level key reaches sops only, not the command.
    let r = repo.run(&[
        "run",
        "production/api",
        "--",
        "sh",
        "-c",
        "test -z \"$SOPS_AGE_KEY\"",
    ]);
    assert!(r.ok(), "{}", r.stderr);
    assert!(
        repo.requests()
            .iter()
            .all(|p| p.starts_with("/v1/keys/demo?levels=production"))
    );
    assert!(repo.run(&["unset", "production/api", "TOKEN"]).ok());
    assert_eq!(
        repo.run(&["names", "production/api"]).stdout,
        "DATABASE_URL\n"
    );
}

#[test]
fn asks_only_for_dev_under_claude_code() {
    if !has_sops() {
        return;
    }
    let repo = Repo::new();
    repo.set("dev", "GITHUB_PACKAGES_TOKEN", "ghp_test");
    repo.set("production/api", "SECRET", "s");
    repo.server.requests.lock().unwrap().clear();
    let agent = [("CLAUDECODE", "1")];
    assert_eq!(
        repo.value("dev", "GITHUB_PACKAGES_TOKEN", &agent).stdout,
        "ghp_test"
    );
    let denied = repo.run_env(&["run", "production/api", "--", "true"], "", &agent);
    assert!(!denied.ok());
    assert!(
        denied
            .stderr
            .contains("agents can only open dev-level files")
    );
    assert!(!repo.run_env(&["edit", "dev"], "", &agent).ok());
    assert!(
        !repo
            .run_env(&["rotate-level", "dev", "--yes", "--out", "x"], "", &agent)
            .ok()
    );
    assert_eq!(repo.requests(), ["/v1/keys/demo?levels=dev"]);
    // Nor does it fall back to the break-glass key.
    std::fs::create_dir_all(repo.path(".age")).unwrap();
    std::fs::write(repo.path(".age/bg.txt"), &repo.break_glass.private).unwrap();
    let bg = repo.path(".age/bg.txt");
    let env = [
        ("CLAUDECODE", "1"),
        ("LITEKEYS_BREAK_GLASS_KEY_FILE", bg.to_str().unwrap()),
        ("LITEKEYS_SERVER", "http://127.0.0.1:9"),
    ];
    assert!(!repo.run_env(&["run", "dev", "--", "true"], "", &env).ok());
}

#[test]
fn reads_sops_age_key_from_the_environment_as_pages_and_ci_do() {
    if !has_sops() {
        return;
    }
    let repo = Repo::new();
    repo.set("beta/pages", "CHANNEL", "production");
    repo.set("production/api", "SECRET", "s");
    let pages = [
        ("SOPS_AGE_KEY", repo.pages.private.as_str()),
        ("LITEKEYS_SERVER", "off"),
    ];
    assert_eq!(
        repo.value("beta/pages", "CHANNEL", &pages).stdout,
        "production"
    );
    assert!(
        !repo
            .run_env(&["run", "production/api", "--", "true"], "", &pages)
            .ok()
    );
    // auto picks the Pages file of the branch's environment.
    let env = [pages[0], pages[1], ("CF_PAGES_BRANCH", "bs/thing")];
    assert_eq!(repo.value("auto", "CHANNEL", &env).stdout, "production");
    // Without the file only --optional runs the command, with the platform's variables.
    let main = [("CF_PAGES_BRANCH", "main"), ("KEEP", "platform")];
    assert!(!repo.run_env(&["run", "auto", "--", "true"], "", &main).ok());
    let r = repo.run_env(
        &["run", "auto", "--optional", "--", "sh", "-c", "echo $KEEP"],
        "",
        &main,
    );
    assert!(r.ok(), "{}", r.stderr);
    assert_eq!(r.stdout, "platform\n");
    assert!(repo.requests().is_empty() || repo.requests().iter().all(|p| !p.contains("main")));
}

#[test]
fn uses_the_key_server_then_the_break_glass_key() {
    if !has_sops() {
        return;
    }
    let repo = Repo::new();
    repo.set("production/api", "SECRET", "s");
    repo.granted.lock().unwrap().remove("production");
    let denied = repo.run(&["run", "production/api", "--", "true"]);
    assert!(!denied.ok());
    assert!(denied.stderr.contains("may not have"), "{}", denied.stderr);
    let who = repo.run(&["whoami"]);
    assert_eq!(
        who.stdout,
        "login: alice@example.com\nproject: demo\nlevels: dev, preview\n"
    );
    std::fs::create_dir_all(repo.path(".age")).unwrap();
    std::fs::write(
        repo.path(".age/key.txt"),
        format!("# bg\n{}\n", repo.break_glass.private),
    )
    .unwrap();
    // The default break-glass file is .age/key.txt in the main checkout.
    let mut cmd = repo.command(
        &[
            "run",
            "production/api",
            "--",
            "sh",
            "-c",
            "printf %s $SECRET",
        ],
        &[("LITEKEYS_SERVER", "http://127.0.0.1:9")],
    );
    cmd.env_remove("LITEKEYS_BREAK_GLASS_KEY_FILE");
    let r: Run = cmd.output().unwrap().into();
    assert!(r.ok(), "{}", r.stderr);
    assert_eq!(r.stdout, "s");
    assert!(r.stderr.contains("cannot reach the key server"));
    // No key server at all: straight to the break-glass key.
    let mut cmd = repo.command(
        &["run", "production/api", "--", "true"],
        &[("LITEKEYS_SERVER", "off")],
    );
    cmd.env_remove("LITEKEYS_BREAK_GLASS_KEY_FILE");
    assert!(cmd.status().unwrap().success());
    // Nothing litekeys fetched was left on disk.
    let level_key = &repo.levels["production"].private;
    let mut leaked = Vec::new();
    walk(&repo.root, &mut |p| {
        if std::fs::read_to_string(p).is_ok_and(|t| t.contains(level_key.as_str())) {
            leaked.push(p.to_path_buf());
        }
    });
    assert!(leaked.is_empty(), "{leaked:?}");
}

fn walk(dir: &Path, f: &mut dyn FnMut(&Path)) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let p = entry.path();
        if p.is_dir() {
            walk(&p, f);
        } else {
            f(&p);
        }
    }
}

#[test]
fn checks_encryption_recipients_and_environments() {
    if !has_sops() {
        return;
    }
    let repo = Repo::new();
    repo.set("dev", "A", "1");
    repo.set("beta/pages", "B", "2");
    let r = repo.run(&["check"]);
    assert!(r.ok(), "{}", r.stderr);
    // check needs no key and no server.
    let offline = repo.run_env(&["check"], "", &[("LITEKEYS_SERVER", "http://127.0.0.1:9")]);
    assert!(offline.ok());

    std::fs::write(repo.path("secrets/beta/leak.env"), "A=plain\n").unwrap();
    assert!(
        repo.run(&["check"])
            .stderr
            .contains("secrets/beta/leak.env is not encrypted")
    );
    std::fs::remove_file(repo.path("secrets/beta/leak.env")).unwrap();

    std::fs::write(repo.path("secrets/notes.txt"), "x").unwrap();
    assert!(
        repo.run(&["check"])
            .stderr
            .contains("secrets/notes.txt is not an encrypted .env file")
    );
    std::fs::remove_file(repo.path("secrets/notes.txt")).unwrap();

    repo.set("ghost/pages", "A", "b");
    assert!(
        repo.run(&["check"])
            .stderr
            .contains("secrets/ghost/ is not in [[branches]]")
    );
    std::fs::remove_dir_all(repo.path("secrets/ghost")).unwrap();

    // A new machine key: the files and .sops.yaml are stale until rekey.
    let ci = key();
    let config = repo
        .config("")
        .replace("machines = [\"pages\"]", "machines = [\"pages\", \"ci\"]")
        .replace(
            &format!("pages = \"{}\"", repo.pages.public),
            &format!("pages = \"{}\"\nci = \"{}\"", repo.pages.public, ci.public),
        );
    std::fs::write(repo.path("secrets/config.toml"), config).unwrap();
    let stale = repo.run(&["check"]);
    assert!(
        stale
            .stderr
            .contains("secrets/beta/pages.env is not encrypted for its recipients"),
        "{}",
        stale.stderr
    );
    assert!(stale.stderr.contains(".sops.yaml does not match"));
    assert!(!stale.stderr.contains("dev.env is not"));
    let rekey = repo.run(&["rekey"]);
    assert!(rekey.ok(), "{}", rekey.stderr);
    assert!(repo.run(&["check"]).ok());
    let ci_env = [("SOPS_AGE_KEY", ci.private.as_str())];
    assert_eq!(repo.value("beta/pages", "B", &ci_env).stdout, "2");
    let recipients = repo.run(&["recipients", "beta/pages"]).stdout;
    assert!(recipients.contains(&format!("{}  (machine ci)", ci.public)));
    assert!(recipients.contains("(level preview)") && recipients.contains("(break-glass)"));
}

#[test]
fn revokes_a_key_with_new_data_keys_and_lists_what_it_could_read() {
    if !has_sops() {
        return;
    }
    let repo = Repo::new();
    repo.set("beta/pages", "DATABASE_URL", "x");
    repo.set("dev", "A", "1");
    let old = repo.read("secrets/beta/pages.env");
    let refused = repo.run(&["revoke", &repo.pages.public]);
    assert!(!refused.ok());
    assert!(refused.stderr.contains("remove"));
    repo.write_config("");
    let without = repo
        .config("")
        .replace("machines = [\"pages\"]", "")
        .replace(&format!("pages = \"{}\"", repo.pages.public), "");
    std::fs::write(repo.path("secrets/config.toml"), without).unwrap();
    let r = repo.run(&["revoke", &repo.pages.public]);
    assert!(r.ok(), "{}", r.stderr);
    assert_eq!(
        r.stdout,
        "The key could open these values, which stay readable in git history. Rotate them:\nsecrets/beta/pages.env:\n  DATABASE_URL\n"
    );
    assert!(
        !repo
            .run(&["recipients"])
            .stdout
            .contains(&repo.pages.public)
    );
    let pages = [("SOPS_AGE_KEY", repo.pages.private.as_str())];
    assert!(
        !repo
            .run_env(&["run", "beta/pages", "--", "true"], "", &pages)
            .ok()
    );
    let data_key = |t: &str| {
        t.lines()
            .find(|l| l.starts_with("sops_age__list_0__map_enc"))
            .map(str::to_string)
    };
    assert_ne!(
        data_key(&old),
        data_key(&repo.read("secrets/beta/pages.env"))
    );
    assert!(repo.run(&["check"]).ok());
}

#[test]
fn re_signs_a_file_merged_by_hand() {
    if !has_sops() {
        return;
    }
    let repo = Repo::new();
    repo.set("production/api", "A", "1");
    repo.set("production/api", "TOKEN", "t");
    let original = repo.read("secrets/production/api.env");
    repo.set("production/api", "EXTRA", "merged");
    let extra = repo
        .read("secrets/production/api.env")
        .lines()
        .find(|l| l.starts_with("EXTRA="))
        .unwrap()
        .to_string();
    // A hand merge: the other side's line next to this side's metadata, so the MAC no longer fits.
    std::fs::write(
        repo.path("secrets/production/api.env"),
        original.replacen("TOKEN=", &format!("{extra}\nTOKEN="), 1),
    )
    .unwrap();
    assert!(!repo.run(&["run", "production/api", "--", "true"]).ok());
    std::fs::write(
        repo.path("secrets/production/api.env"),
        format!("<<<<<<< HEAD\n{original}"),
    )
    .unwrap();
    assert!(
        repo.run(&["merge-fix", "production/api"])
            .stderr
            .contains("conflict markers")
    );
    std::fs::write(
        repo.path("secrets/production/api.env"),
        original.replacen("TOKEN=", &format!("{extra}\nTOKEN="), 1),
    )
    .unwrap();
    let r = repo.run(&["merge-fix", "production/api"]);
    assert!(r.ok(), "{}", r.stderr);
    assert_eq!(repo.value("production/api", "EXTRA", &[]).stdout, "merged");
}

#[test]
fn rotates_a_key_in_a_name_list_without_printing_it() {
    if !has_sops() {
        return;
    }
    let repo = Repo::new();
    repo.set("production/api", "API_PUBLISH_KEYS", "pages=old");
    let add = repo.run(&[
        "pair",
        "add",
        "production/api",
        "API_PUBLISH_KEYS",
        "pages2",
        "beta/pages:BS_PUBLISH_API_KEY",
    ]);
    assert!(add.ok(), "{}", add.stderr);
    assert_eq!(
        repo.run(&["pair", "list", "production/api", "API_PUBLISH_KEYS"])
            .stdout,
        "pages\npages2\n"
    );
    assert!(
        !repo
            .run(&[
                "pair",
                "add",
                "production/api",
                "API_PUBLISH_KEYS",
                "pages2"
            ])
            .ok()
    );
    let list = repo.value("production/api", "API_PUBLISH_KEYS", &[]).stdout;
    let key = list.strip_prefix("pages=old,pages2=").unwrap().to_string();
    assert_eq!(key.len(), 64);
    assert!(!add.all().contains(&key));
    assert_eq!(
        repo.value("beta/pages", "BS_PUBLISH_API_KEY", &[]).stdout,
        key
    );
    assert!(
        repo.run(&[
            "pair",
            "remove",
            "production/api",
            "API_PUBLISH_KEYS",
            "pages"
        ])
        .ok()
    );
    assert_eq!(
        repo.run(&["pair", "list", "production/api", "API_PUBLISH_KEYS"])
            .stdout,
        "pages2\n"
    );
    assert!(
        repo.run(&[
            "pair",
            "remove",
            "production/api",
            "API_PUBLISH_KEYS",
            "pages2"
        ])
        .stderr
        .contains("last key")
    );
    assert!(
        repo.run(&[
            "pair",
            "remove",
            "production/api",
            "API_PUBLISH_KEYS",
            "nope"
        ])
        .stderr
        .contains("has no nope")
    );
}

/// A fake Dokploy with one application and the key server's compose service.
struct FakeDokploy {
    fake: Fake,
    compose_env: Arc<Mutex<String>>,
}

fn dokploy(app_env: &str, compose_env: &str) -> FakeDokploy {
    let app = json!({
        "applicationId": "app1",
        "name": "api",
        "env": app_env,
        "buildArgs": "A=1",
        "buildSecrets": null,
        "createEnvFile": true
    });
    let compose_env = Arc::new(Mutex::new(compose_env.to_string()));
    let state = compose_env.clone();
    let fake = serve(Box::new(move |req: &Request| {
        if req.headers.get("x-api-key").map(String::as_str) != Some("dok") {
            return (401, "{}".into());
        }
        match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/api/application.one?applicationId=app1") => (200, app.to_string()),
            ("GET", "/api/compose.one?composeId=keys1") => (
                200,
                json!({"composeId": "keys1", "name": "litekeys", "env": *state.lock().unwrap()})
                    .to_string(),
            ),
            ("POST", "/api/compose.update") => {
                let body: Value = serde_json::from_str(&req.body).unwrap();
                *state.lock().unwrap() = body["env"].as_str().unwrap().to_string();
                (200, "true".into())
            }
            ("POST", _) => (200, "true".into()),
            _ => (404, "{}".into()),
        }
    }));
    FakeDokploy { fake, compose_env }
}

fn posts(d: &FakeDokploy) -> Vec<(String, Value)> {
    d.fake
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.method == "POST")
        .map(|r| (r.path.clone(), serde_json::from_str(&r.body).unwrap()))
        .collect()
}

const DOKPLOY: &str = r#"
[dokploy]
url = "http://unused.invalid"

[dokploy.apps]
"production/api.env" = "app1"

[dokploy.key_server]
kind = "compose"
id = "keys1"
"#;

#[test]
fn syncs_to_dokploy_by_name_and_keeps_build_settings() {
    if !has_sops() {
        return;
    }
    let repo = Repo::new();
    repo.write_config(DOKPLOY);
    repo.set("production/api", "DATABASE_URL", "postgres://z");
    repo.set("production/api", "TOKEN", "new");
    repo.set("production/api", "EXTRA", "merged");
    repo.set("beta/pages", "X", "1");
    let d = dokploy("DATABASE_URL=postgres://z\nOLD=1\n\nTOKEN=stale", "");
    let env = [
        ("DOKPLOY_API_KEY", "dok"),
        ("DOKPLOY_URL", d.fake.url.as_str()),
    ];

    let r = repo.run_env(&["check", "--remote"], "", &env);
    assert!(!r.ok());
    assert!(r.stderr.contains("only in git: EXTRA"), "{}", r.stderr);
    assert!(r.stderr.contains("only in Dokploy: OLD"));
    assert!(!r.stderr.contains("TOKEN") && !r.stderr.contains("postgres"));

    let r = repo.run_env(&["sync", "production/api", "--dry-run"], "", &env);
    assert!(r.ok(), "{}", r.stderr);
    for change in ["added EXTRA", "removed OLD", "changed TOKEN"] {
        assert!(r.stderr.contains(change), "{}", r.stderr);
    }
    assert!(
        !r.stderr.contains("DATABASE_URL")
            && !r.stderr.contains("postgres")
            && !r.stderr.contains("stale")
    );
    assert!(posts(&d).is_empty());

    let r = repo.run_env(&["sync", "production/api", "--yes", "--deploy"], "", &env);
    assert!(r.ok(), "{}", r.stderr);
    assert!(!r.all().contains("postgres") && !r.all().contains("stale"));
    let posts = posts(&d);
    assert_eq!(
        posts.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(),
        [
            "/api/application.saveEnvironment",
            "/api/application.deploy"
        ]
    );
    let saved = &posts[0].1;
    assert_eq!(saved["applicationId"], "app1");
    assert_eq!(saved["buildArgs"], "A=1");
    assert_eq!(saved["createEnvFile"], true);
    let saved_env = saved["env"].as_str().unwrap();
    assert!(
        saved_env.contains("DATABASE_URL=postgres://z")
            && saved_env.contains("EXTRA=merged")
            && !saved_env.contains("OLD=")
    );

    let r = repo.run_env(&["sync", "beta/pages", "--dry-run"], "", &env);
    assert!(r.stderr.contains("has no Dokploy application"));

    // Without DOKPLOY_API_KEY the credentials come from secrets/ops.env.
    repo.set("ops", "DOKPLOY_API_KEY", "dok");
    repo.set("ops", "DOKPLOY_URL", &d.fake.url);
    let r = repo.run(&["sync", "production/api", "--dry-run"]);
    assert!(r.ok(), "{}", r.stderr);
}

#[test]
fn rotates_a_level_through_the_key_servers_dokploy_service() {
    if !has_sops() {
        return;
    }
    let repo = Repo::new();
    repo.write_config(DOKPLOY);
    repo.set("beta/pages", "R2_TOKEN", "r2");
    repo.set("preview/pages", "OTHER", "o");
    repo.set("production/api", "SECRET", "s");
    let old = repo.levels["preview"].private.clone();
    let d = dokploy(
        "",
        &format!("LITEKEYS_LISTEN=127.0.0.1:8787\nLITEKEYS_KEY__DEMO__PREVIEW={old}"),
    );
    let env = [
        ("DOKPLOY_API_KEY", "dok"),
        ("DOKPLOY_URL", d.fake.url.as_str()),
    ];
    let before = repo.read("secrets/production/api.env");

    let r = repo.run_env(&["rotate-level", "preview", "--yes", "--deploy"], "", &env);
    assert!(r.ok(), "{}", r.stderr);
    assert_eq!(
        r.stdout,
        "Level preview could open these values; rotate them if someone lost access:\nsecrets/beta/pages.env:\n  R2_TOKEN\nsecrets/preview/pages.env:\n  OTHER\n"
    );
    assert!(!r.all().contains("AGE-SECRET-KEY"));
    // The new key goes first, the old one stays after it for branches not rekeyed yet.
    let server_env = d.compose_env.lock().unwrap().clone();
    let value = server_env
        .lines()
        .find_map(|l| l.strip_prefix("LITEKEYS_KEY__DEMO__PREVIEW="))
        .unwrap()
        .to_string();
    let (new, kept) = value.split_once(',').unwrap();
    assert_eq!(kept, old);
    assert!(server_env.contains("LITEKEYS_LISTEN=127.0.0.1:8787"));
    assert!(posts(&d).iter().any(|(p, _)| p == "/api/compose.deploy"));
    let new_public = litekeys_core::public_key_of(new).unwrap();
    assert!(
        repo.read("secrets/config.toml")
            .contains(&format!("preview = \"{new_public}\""))
    );
    assert!(
        repo.read("secrets/config.toml")
            .starts_with("# A test project.")
    );
    assert!(repo.read("secrets/.sops.yaml").contains(&new_public));
    assert_eq!(repo.read("secrets/production/api.env"), before);
    assert!(repo.run(&["check"]).ok());
    let new_env = [("SOPS_AGE_KEY", new)];
    assert_eq!(repo.value("beta/pages", "R2_TOKEN", &new_env).stdout, "r2");
    let old_env = [("SOPS_AGE_KEY", old.as_str())];
    assert!(
        !repo
            .run_env(&["run", "beta/pages", "--", "true"], "", &old_env)
            .ok()
    );
    // Teammates get the new key from the server.
    repo.held
        .lock()
        .unwrap()
        .insert("preview".into(), value.clone());
    assert_eq!(repo.value("preview/pages", "OTHER", &[]).stdout, "o");
}

#[test]
fn creates_break_glass_and_level_keys() {
    let repo = Repo::new();
    let file = repo.path(".age/key.txt");
    let r = repo.run_env(
        &["keygen"],
        "",
        &[("LITEKEYS_BREAK_GLASS_KEY_FILE", file.to_str().unwrap())],
    );
    assert!(r.ok(), "{}", r.stderr);
    let text = std::fs::read_to_string(&file).unwrap();
    let private = text
        .lines()
        .find(|l| l.starts_with("AGE-SECRET-KEY-"))
        .unwrap();
    assert!(
        r.stdout
            .contains(&litekeys_core::public_key_of(private).unwrap())
    );
    assert!(!r.all().contains(private));
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(
        !repo
            .run_env(
                &["keygen"],
                "",
                &[("LITEKEYS_BREAK_GLASS_KEY_FILE", file.to_str().unwrap())]
            )
            .ok()
    );

    assert!(
        !repo
            .run(&["keygen", "--level", "dev", "--yes", "--out", "x.txt"])
            .ok()
    );
    let out = repo.path("ops-key.txt");
    let r = repo.run(&[
        "keygen",
        "--level",
        "ops",
        "--yes",
        "--out",
        out.to_str().unwrap(),
    ]);
    assert!(r.ok(), "{}", r.stderr);
    assert!(r.stderr.contains("LITEKEYS_KEY__DEMO__OPS"));
    let private = std::fs::read_to_string(&out)
        .unwrap()
        .lines()
        .last()
        .unwrap()
        .to_string();
    let public = litekeys_core::public_key_of(&private).unwrap();
    assert!(
        repo.read("secrets/config.toml")
            .contains(&format!("ops = \"{public}\""))
    );
    assert!(!r.all().contains(&private));

    // A level without a key yet, as in a migration: check says so, keygen --level fills it in.
    let config = repo.read("secrets/config.toml");
    let dev = format!("dev = \"{}\"", repo.levels["dev"].public);
    std::fs::write(
        repo.path("secrets/config.toml"),
        config.replace(&dev, "dev = \"\""),
    )
    .unwrap();
    let r = repo.run(&["check"]);
    assert!(
        r.stderr.contains("level dev has no key yet"),
        "{}",
        r.stderr
    );
    assert!(
        !repo
            .run(&["rotate-level", "dev", "--yes", "--out", "y.txt"])
            .ok()
    );
    let out = repo.path("dev-key.txt");
    let r = repo.run(&[
        "keygen",
        "--level",
        "dev",
        "--yes",
        "--out",
        out.to_str().unwrap(),
    ]);
    assert!(r.ok(), "{}", r.stderr);
    assert!(!repo.read("secrets/config.toml").contains("dev = \"\""));
    let r = repo.run(&["rekey"]);
    assert!(r.ok(), "{}", r.stderr);
    assert!(repo.run(&["check"]).ok());
}
