//! The subcommands. Values only move through memory and pipes to sops, the target command,
//! Dokploy or the key server; nothing here prints one.

use std::collections::BTreeSet;
use std::io::{BufRead, IsTerminal, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use tierkey_core::{generate_key, is_public_key, key_env_var, valid_name};

use crate::config::{AppKind, Config, Project};
use crate::dokploy::Dokploy;
use crate::keys::{KeyEnv, Keys};
use crate::sops::{Sops, path_str};
use crate::{Cmd, PairCmd, Result, dotenv, fail, info};

pub fn dispatch(cmd: Cmd) -> Result<()> {
    let cwd = std::env::current_dir().map_err(|e| format!("no current directory: {e}"))?;
    let cwd = cwd.canonicalize().unwrap_or(cwd);
    let ctx = Ctx {
        project: Project::find(&cwd)?,
    };
    match cmd {
        Cmd::Run {
            target,
            optional,
            cmd,
        } => ctx.run(&target, optional, &cmd),
        Cmd::Env { branch } => {
            let branch = match branch {
                Some(b) => b,
                None => current_branch(&ctx.project.root)?,
            };
            println!("{}", ctx.project.config.env_for(&branch)?);
            Ok(())
        }
        Cmd::Names { target } => {
            let file = ctx.existing(&target)?;
            for name in dotenv::names(&read(&file)?) {
                println!("{name}");
            }
            Ok(())
        }
        Cmd::Set {
            target,
            name,
            random,
        } => {
            let file = ctx.resolve(&target)?;
            let value = read_value(&name, random)?;
            let keys = Keys::new(&ctx.project);
            ctx.set_value(&keys, &file, &name, &value)
        }
        Cmd::Unset { target, name } => ctx.unset(&target, &name),
        Cmd::Edit { target } => ctx.edit(&target),
        Cmd::Check { remote } => {
            ctx.check()?;
            if remote { ctx.check_remote() } else { Ok(()) }
        }
        Cmd::Recipients { target } => ctx.recipients(target.as_deref()),
        Cmd::Rekey => ctx.rekey(),
        Cmd::Revoke { public_key } => ctx.revoke(&public_key),
        Cmd::MergeFix { target } => ctx.merge_fix(&target),
        Cmd::Sync {
            target,
            dry_run,
            yes,
            deploy,
        } => ctx.sync(&target, dry_run, yes, deploy),
        Cmd::Pair(pair) => ctx.pair(pair),
        Cmd::Keygen {
            level: None, out, ..
        } => ctx.keygen_break_glass(out),
        Cmd::Keygen {
            level: Some(level),
            out,
            deploy,
            yes,
        } => ctx.keygen_level(&level, out, deploy, yes),
        Cmd::RotateLevel {
            level,
            out,
            deploy,
            yes,
        } => ctx.rotate_level(&level, out, deploy, yes),
        Cmd::Whoami => ctx.whoami(),
    }
}

struct Ctx {
    project: Project,
}

fn read(file: &Path) -> Result<String> {
    std::fs::read_to_string(file).map_err(|e| format!("cannot read {}: {e}", file.display()))
}

fn write(file: &Path, text: &str) -> Result<()> {
    std::fs::write(file, text).map_err(|e| format!("cannot write {}: {e}", file.display()))
}

/// Ends text with exactly one newline.
fn one_newline(text: &str) -> String {
    format!("{}\n", text.trim_end_matches('\n'))
}

/// The branch being built or checked out: Cloudflare Pages, GitHub Actions, else git.
fn current_branch(root: &Path) -> Result<String> {
    for var in ["CF_PAGES_BRANCH", "GITHUB_HEAD_REF", "GITHUB_REF_NAME"] {
        if let Ok(v) = std::env::var(var)
            && !v.is_empty()
        {
            return Ok(v);
        }
    }
    // symbolic-ref also names a branch with no commits yet; rev-parse covers a detached HEAD.
    for args in [
        &["symbolic-ref", "--short", "HEAD"][..],
        &["rev-parse", "--abbrev-ref", "HEAD"][..],
    ] {
        let out = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .stderr(std::process::Stdio::null())
            .output();
        if let Ok(out) = out
            && out.status.success()
        {
            return Ok(String::from_utf8_lossy(&out.stdout).trim().to_string());
        }
    }
    fail("cannot tell the current branch")
}

fn random_hex(bytes: usize) -> Result<String> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).map_err(|e| format!("no randomness: {e}"))?;
    Ok(hex::encode(buf))
}

/// A value from --random, a hidden prompt, or stdin.
fn read_value(name: &str, random: Option<usize>) -> Result<String> {
    let value = if let Some(bytes) = random {
        if bytes < 16 {
            return fail("--random needs at least 16 bytes");
        }
        random_hex(bytes)?
    } else if std::io::stdin().is_terminal() {
        rpassword::prompt_password(format!("Value for {name} (hidden): "))
            .map_err(|e| format!("cannot read the value: {e}"))?
    } else {
        let mut value = String::new();
        std::io::stdin()
            .read_to_string(&mut value)
            .map_err(|e| format!("cannot read stdin: {e}"))?;
        let trimmed = value.strip_suffix('\n').unwrap_or(&value);
        trimmed.strip_suffix('\r').unwrap_or(trimmed).to_string()
    };
    if value.is_empty() {
        return fail("empty value; use unset to remove a key");
    }
    if value.contains('\n') {
        return fail("values cannot span lines");
    }
    Ok(value)
}

/// Asks on the terminal; anything but y/yes is no.
fn confirm(question: &str) -> Result<bool> {
    let tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .map_err(|_| format!("{question} needs a terminal to answer; pass --yes"))?;
    let mut writer = tty.try_clone().map_err(|e| e.to_string())?;
    write!(writer, "{question} [y/N] ").map_err(|e| e.to_string())?;
    writer.flush().ok();
    let mut answer = String::new();
    std::io::BufReader::new(tty)
        .read_line(&mut answer)
        .map_err(|e| e.to_string())?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes" | "Yes"))
}

/// Writes a private key to a new file only its owner can read.
fn write_private(file: &Path, comment: &str, private: &str) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(parent) = file.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(file)
        .map_err(|e| format!("cannot create {}: {e}", file.display()))?;
    writeln!(f, "{comment}\n{private}").map_err(|e| format!("cannot write {}: {e}", file.display()))
}

impl Ctx {
    fn config(&self) -> &Config {
        &self.project.config
    }

    fn sops(&self) -> Result<Sops> {
        Sops::locate(&self.project.root, self.project.sops_path())
    }

    /// `dev`, `production/api`, `auto` or a path, to a .env file under secrets/.
    fn resolve(&self, arg: &str) -> Result<PathBuf> {
        let dir = &self.project.dir;
        let file = if arg == "auto" {
            let env = self
                .config()
                .env_for(&current_branch(&self.project.root)?)?
                .to_string();
            dir.join(self.config().auto.replace("{env}", &env))
        } else if Path::new(arg).is_file() {
            Path::new(arg)
                .canonicalize()
                .map_err(|e| format!("{arg}: {e}"))?
        } else {
            dir.join(format!("{}.env", arg.strip_suffix(".env").unwrap_or(arg)))
        };
        let inside =
            file.starts_with(dir) && !self.project.rel(&file).split('/').any(|c| c == "..");
        if !inside || file.extension().is_none_or(|e| e != "env") {
            return fail(format!("{arg} is not a .env file under secrets/"));
        }
        Ok(file)
    }

    fn existing(&self, arg: &str) -> Result<PathBuf> {
        let file = self.resolve(arg)?;
        if !file.is_file() {
            return fail(format!("{} does not exist", self.project.show(&file)));
        }
        Ok(file)
    }

    fn levels_of<'f>(
        &self,
        files: impl IntoIterator<Item = &'f PathBuf>,
    ) -> Result<BTreeSet<String>> {
        files
            .into_iter()
            .map(|f| {
                self.config()
                    .level_of(&self.project.rel(f))
                    .map(str::to_string)
            })
            .collect()
    }

    fn key_for(&self, keys: &Keys, file: &Path) -> Result<KeyEnv> {
        keys.for_levels(&self.levels_of([&file.to_path_buf()])?)
    }

    fn run(&self, target: &str, optional: bool, cmd: &[String]) -> Result<()> {
        let file = self.resolve(target)?;
        let mut command = Command::new(&cmd[0]);
        command.args(&cmd[1..]);
        if !file.is_file() {
            if !optional {
                return fail(format!("{} does not exist", self.project.show(&file)));
            }
            // An environment that has not moved its secrets to git keeps its platform's variables.
            info(&format!(
                "no {}, running without it",
                self.project.show(&file)
            ));
        } else {
            let keys = Keys::new(&self.project);
            let key = self.key_for(&keys, &file)?;
            let plain = self
                .sops()?
                .decrypt(&key, &file)
                .map_err(|_| format!("could not decrypt {}", self.project.show(&file)))?;
            command.envs(dotenv::pairs(&plain));
        }
        // The command replaces this process: its arguments pass through untouched, and the values
        // exist only in its environment.
        let err = command.exec();
        fail(format!("cannot run {}: {err}", cmd[0]))
    }

    fn set_value(&self, keys: &Keys, file: &Path, name: &str, value: &str) -> Result<()> {
        if !dotenv::valid_var(name) || name.starts_with("sops_") {
            return fail(format!("invalid name: {name}"));
        }
        let sops = self.sops()?;
        if file.is_file() {
            let key = self.key_for(keys, file)?;
            let json = serde_json::to_string(value).expect("a string encodes");
            sops.output(
                &key,
                &[
                    "set",
                    "--value-stdin",
                    &path_str(file)?,
                    &format!("[\"{name}\"]"),
                ],
                Some(json.as_bytes()),
            )?;
        } else {
            self.config().level_of(&self.project.rel(file))?;
            self.project.write_sops_yaml()?;
            if let Some(parent) = file.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
            }
            // Encrypting needs only public keys.
            let key = keys.for_levels(&BTreeSet::new())?;
            let out = sops.encrypt(&key, file, format!("{name}={value}\n").as_bytes())?;
            write(file, &one_newline(&String::from_utf8_lossy(&out)))?;
        }
        info(&format!("set {name} in {}", self.project.show(file)));
        Ok(())
    }

    fn unset(&self, target: &str, name: &str) -> Result<()> {
        let file = self.existing(target)?;
        if !dotenv::valid_var(name) {
            return fail(format!("invalid name: {name}"));
        }
        let keys = Keys::new(&self.project);
        let key = self.key_for(&keys, &file)?;
        self.sops()?.output(
            &key,
            &["unset", &path_str(&file)?, &format!("[\"{name}\"]")],
            None,
        )?;
        info(&format!("removed {name} from {}", self.project.show(&file)));
        Ok(())
    }

    fn edit(&self, target: &str) -> Result<()> {
        if crate::keys::is_agent() {
            return fail("agents cannot open decrypted files; use set");
        }
        let file = self.existing(target)?;
        let keys = Keys::new(&self.project);
        let key = self.key_for(&keys, &file)?;
        self.sops()?.interactive(&key, &["edit", &path_str(&file)?])
    }

    fn check(&self) -> Result<()> {
        let mut failed = false;
        let mut problem = |m: String| {
            eprintln!("tierkey: {m}");
            failed = true;
        };
        let p = &self.project;
        for file in p.all_files() {
            let (rel, show) = (p.rel(&file), p.show(&file));
            let text = read(&file)?;
            let pairs = dotenv::pairs(&text);
            if !pairs.iter().any(|(n, _)| *n == "sops_mac") {
                problem(format!("{show} is not encrypted with sops"));
                continue;
            }
            if pairs
                .iter()
                .any(|(n, v)| !n.starts_with("sops_") && !v.starts_with("ENC["))
            {
                problem(format!("{show} has a value that is not encrypted"));
            }
            match self.config().recipients_for(&rel) {
                Err(e) => problem(e),
                Ok(want) => {
                    let want: BTreeSet<String> = want.into_iter().map(|(k, _)| k).collect();
                    let have: BTreeSet<String> = dotenv::recipients(&text).into_iter().collect();
                    if want != have {
                        problem(format!(
                            "{show} is not encrypted for its recipients in secrets/config.toml; run tierkey rekey"
                        ));
                    }
                }
            }
        }
        // Nothing else may live under secrets/: a stray file there is probably a decrypted copy.
        for file in p.stray_files() {
            problem(format!("{} is not an encrypted .env file", p.show(&file)));
        }
        match self.config().sops_yaml() {
            Err(e) => problem(e),
            Ok(want) if std::fs::read_to_string(p.sops_path()).ok().as_ref() != Some(&want) => {
                problem(
                    "secrets/.sops.yaml does not match secrets/config.toml; run tierkey rekey"
                        .into(),
                )
            }
            Ok(_) => {}
        }
        // Every environment directory must be reachable from a branch.
        if let Ok(entries) = std::fs::read_dir(&p.dir) {
            let mut dirs: Vec<String> = entries
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect();
            dirs.sort();
            for env in dirs {
                if !self.config().branches.iter().any(|b| b.env == env) {
                    problem(format!(
                        "secrets/{env}/ is not in [[branches]] in secrets/config.toml"
                    ));
                }
            }
        }
        if failed {
            return fail("check failed");
        }
        info("ok");
        Ok(())
    }

    /// DOKPLOY_API_KEY and DOKPLOY_URL from the environment, else from the credentials file.
    fn dokploy(&self, keys: &Keys) -> Result<Dokploy> {
        let settings = self.config().dokploy.clone();
        let env = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        let (key, mut url) = (env("DOKPLOY_API_KEY"), env("DOKPLOY_URL"));
        let key = match key {
            Some(key) => key,
            None => {
                let credentials = settings
                    .as_ref()
                    .map(|d| d.credentials.clone())
                    .unwrap_or_else(|| "ops.env".into());
                let file = self.project.dir.join(&credentials);
                if !file.is_file() {
                    return fail(format!(
                        "set DOKPLOY_API_KEY or add it to secrets/{credentials}"
                    ));
                }
                let key_env = self.key_for(keys, &file)?;
                let plain = self
                    .sops()?
                    .decrypt(&key_env, &file)
                    .map_err(|_| format!("could not decrypt secrets/{credentials}"))?;
                if url.is_none() {
                    url = dotenv::get_var(&plain, "DOKPLOY_URL").map(str::to_string);
                }
                match dotenv::get_var(&plain, "DOKPLOY_API_KEY") {
                    Some(key) => key.to_string(),
                    None => return fail(format!("secrets/{credentials} has no DOKPLOY_API_KEY")),
                }
            }
        };
        let Some(url) = url.or_else(|| settings.and_then(|d| d.url)) else {
            return fail("set DOKPLOY_URL or [dokploy] url in secrets/config.toml");
        };
        Ok(Dokploy::new(&url, key))
    }

    fn app_of(&self, file: &Path) -> Result<String> {
        let rel = self.project.rel(file);
        match self
            .config()
            .dokploy
            .as_ref()
            .and_then(|d| d.apps.get(&rel))
        {
            Some(id) => Ok(id.clone()),
            None => fail(format!(
                "secrets/{rel} has no Dokploy application in [dokploy.apps] of secrets/config.toml"
            )),
        }
    }

    fn sync(&self, target: &str, dry_run: bool, yes: bool, deploy: bool) -> Result<()> {
        let file = self.existing(target)?;
        let show = self.project.show(&file);
        let id = self.app_of(&file)?;
        let keys = Keys::new(&self.project);
        let key = self.key_for(&keys, &file)?;
        let wanted = self
            .sops()?
            .decrypt(&key, &file)
            .map_err(|_| format!("could not decrypt {show}"))?;
        let wanted = wanted.trim_end_matches('\n');
        let dokploy = self.dokploy(&keys)?;
        let app = dokploy
            .app(AppKind::Application, &id)
            .map_err(|e| format!("could not read Dokploy application {id}: {e}"))?;
        let changes = dotenv::changes(wanted, &app.env());
        let name = app.name();
        if changes.is_empty() {
            info(&format!("Dokploy {name} already matches {show}"));
        } else {
            eprintln!("Dokploy {name}, from {show}:");
            for change in &changes {
                eprintln!("  {change}");
            }
            if dry_run {
                return Ok(());
            }
            if !yes && !confirm(&format!("Save these to Dokploy {name}?"))? {
                return fail("nothing saved");
            }
            dokploy
                .save_env(&app, wanted)
                .map_err(|e| format!("Dokploy refused the environment: {e}"))?;
            info(&format!("saved to Dokploy {name}"));
        }
        if deploy && !dry_run {
            dokploy
                .deploy(&app)
                .map_err(|e| format!("Dokploy did not start the deploy: {e}"))?;
            info(&format!("deploying {name}"));
        } else if !changes.is_empty() && !dry_run {
            info("the app keeps its old values until it is redeployed (--deploy)");
        }
        Ok(())
    }

    /// Compares key names only with every application in [dokploy.apps].
    fn check_remote(&self) -> Result<()> {
        let keys = Keys::new(&self.project);
        let dokploy = self.dokploy(&keys)?;
        let apps = self
            .config()
            .dokploy
            .as_ref()
            .map(|d| d.apps.clone())
            .unwrap_or_default();
        let mut failed = false;
        for (path, id) in apps {
            let file = self.project.dir.join(&path);
            if !file.is_file() {
                eprintln!("tierkey: [dokploy.apps] lists {path}, which does not exist");
                failed = true;
                continue;
            }
            let app = dokploy
                .app(AppKind::Application, &id)
                .map_err(|e| format!("could not read Dokploy application {id}: {e}"))?;
            // Names only: values on both sides are dropped before comparing.
            let git: String = dotenv::names(&read(&file)?)
                .iter()
                .map(|n| format!("{n}=\n"))
                .collect();
            let changes = dotenv::changes(&git, &dotenv::blank_values(&app.env()));
            if !changes.is_empty() {
                eprintln!("tierkey: secrets/{path} and Dokploy {} differ:", app.name());
                for change in changes {
                    let change = change
                        .replace("added ", "only in git: ")
                        .replace("removed ", "only in Dokploy: ");
                    eprintln!("  {change}");
                }
                failed = true;
            }
        }
        if failed {
            return fail("check --remote failed");
        }
        info("Dokploy has the same names");
        Ok(())
    }

    /// `name=key` lists, such as an API's accepted keys: add the new key next to the old one,
    /// move its users over, then remove the old one, with no moment of downtime.
    fn pair(&self, cmd: PairCmd) -> Result<()> {
        let (target, list) = match &cmd {
            PairCmd::List { target, list }
            | PairCmd::Add { target, list, .. }
            | PairCmd::Remove { target, list, .. } => (target.clone(), list.clone()),
        };
        let list = &list;
        if !dotenv::valid_var(list) {
            return fail(format!("invalid name: {list}"));
        }
        let file = self.existing(&target)?;
        let keys = Keys::new(&self.project);
        let key = self.key_for(&keys, &file)?;
        let plain = self
            .sops()?
            .decrypt(&key, &file)
            .map_err(|_| format!("could not decrypt {}", self.project.show(&file)))?;
        let current = dotenv::get_var(&plain, list).unwrap_or("").to_string();
        drop(plain);
        let entries: Vec<&str> = current.split(',').filter(|e| !e.is_empty()).collect();
        let name_of = |e: &str| e.split('=').next().unwrap_or("").to_string();
        match cmd {
            PairCmd::List { .. } => {
                for entry in entries {
                    println!("{}", name_of(entry));
                }
            }
            PairCmd::Add { name, also, .. } => {
                let ok = !name.is_empty()
                    && name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c));
                if !ok {
                    return fail(format!("invalid key name: {name}"));
                }
                if entries.iter().any(|e| name_of(e) == name) {
                    return fail(format!("{list} already has {name}"));
                }
                let mut targets = Vec::new();
                for t in &also {
                    let Some((target, var)) = t.rsplit_once(':') else {
                        return fail(format!("{t} is not <target>:VAR"));
                    };
                    targets.push((self.resolve(target)?, var.to_string()));
                }
                let new = random_hex(32)?;
                let mut value = current.clone();
                if !value.is_empty() {
                    value.push(',');
                }
                value.push_str(&format!("{name}={new}"));
                self.set_value(&keys, &file, list, &value)?;
                for (target, var) in targets {
                    self.set_value(&keys, &target, &var, &new)?;
                }
            }
            PairCmd::Remove { name, .. } => {
                let kept: Vec<&str> = entries
                    .iter()
                    .copied()
                    .filter(|e| name_of(e) != name)
                    .collect();
                if kept.len() == entries.len() {
                    return fail(format!("{list} has no {name}"));
                }
                if kept.is_empty() {
                    return fail(format!(
                        "{name} is the last key in {list}; add its replacement first"
                    ));
                }
                self.set_value(&keys, &file, list, &kept.join(","))?;
            }
        }
        Ok(())
    }

    fn recipients(&self, target: Option<&str>) -> Result<()> {
        let line = |key: &str| match self.config().label_of(key) {
            Some(label) => format!("{key}  ({label})"),
            None => format!("{key}  (not in secrets/config.toml)"),
        };
        if let Some(target) = target {
            let file = self.existing(target)?;
            for key in dotenv::recipients(&read(&file)?) {
                println!("{}", line(&key));
            }
            return Ok(());
        }
        for file in self.project.all_files() {
            println!("{}:", self.project.show(&file));
            for key in dotenv::recipients(&read(&file)?) {
                println!("  {}", line(&key));
            }
        }
        Ok(())
    }

    /// `sops updatekeys` for each file, with keys that open all of them.
    fn update_keys(&self, files: &[PathBuf], key: &KeyEnv) -> Result<()> {
        let sops = self.sops()?;
        for file in files {
            sops.output(key, &["updatekeys", "--yes", &path_str(file)?], None)?;
        }
        Ok(())
    }

    /// A new data key for each file, so old copies' keys cannot open future values.
    fn rotate_data_keys(&self, files: &[PathBuf], key: &KeyEnv) -> Result<()> {
        let sops = self.sops()?;
        for file in files {
            sops.output(key, &["rotate", "--in-place", &path_str(file)?], None)?;
        }
        Ok(())
    }

    fn rekey(&self) -> Result<()> {
        if self.project.write_sops_yaml()? {
            info("updated secrets/.sops.yaml");
        }
        let files = self.project.all_files();
        let keys = Keys::new(&self.project);
        let key = keys.for_rewrite(&self.levels_of(&files)?)?;
        self.update_keys(&files, &key)?;
        info(&format!("rekeyed {} files", files.len()));
        Ok(())
    }

    fn revoke(&self, public_key: &str) -> Result<()> {
        if !is_public_key(public_key) {
            return fail(format!("{public_key} is not an age public key"));
        }
        if self.config().label_of(public_key).is_some() {
            return fail(format!(
                "remove {public_key} from secrets/config.toml first"
            ));
        }
        let mut affected = Vec::new();
        for file in self.project.all_files() {
            if dotenv::recipients(&read(&file)?)
                .iter()
                .any(|k| k == public_key)
            {
                affected.push(file);
            }
        }
        if affected.is_empty() {
            info(&format!("no file is encrypted for {public_key}"));
            return Ok(());
        }
        self.rekey()?;
        let keys = Keys::new(&self.project);
        let key = keys.for_rewrite(&self.levels_of(&affected)?)?;
        self.rotate_data_keys(&affected, &key)?;
        println!(
            "The key could open these values, which stay readable in git history. Rotate them:"
        );
        self.print_names(&affected)
    }

    fn print_names(&self, files: &[PathBuf]) -> Result<()> {
        for file in files {
            println!("{}:", self.project.show(file));
            for name in dotenv::names(&read(file)?) {
                println!("  {name}");
            }
        }
        Ok(())
    }

    fn merge_fix(&self, target: &str) -> Result<()> {
        let file = self.existing(target)?;
        let text = read(&file)?;
        if text.lines().any(|l| {
            ["<<<<<<<", "=======", ">>>>>>>"]
                .iter()
                .any(|m| l.starts_with(m))
        }) {
            return fail("resolve the conflict markers first");
        }
        // Keep exactly one copy of each sops_ metadata line, whichever side it came from.
        let mut seen = BTreeSet::new();
        let cleaned: Vec<&str> = text
            .lines()
            .filter(|l| match l.split_once('=') {
                Some((name, _)) if name.starts_with("sops_") => seen.insert(name.to_string()),
                _ => true,
            })
            .collect();
        write(&file, &one_newline(&cleaned.join("\n")))?;
        self.project.write_sops_yaml()?;
        let keys = Keys::new(&self.project);
        let key = keys.for_rewrite(&self.levels_of([&file])?)?;
        let sops = self.sops()?;
        // Plaintext only moves through memory, never to disk.
        let plain = sops.output(&key, &["decrypt", "--ignore-mac", &path_str(&file)?], None)?;
        let out = sops.encrypt(&key, &file, &plain)?;
        write(&file, &one_newline(&String::from_utf8_lossy(&out)))?;
        info(&format!("re-signed {}", self.project.show(&file)));
        Ok(())
    }

    fn keygen_break_glass(&self, out: Option<PathBuf>) -> Result<()> {
        let file = out.unwrap_or_else(|| Keys::new(&self.project).break_glass_file());
        if file.exists() {
            return fail(format!("{} already exists", file.display()));
        }
        if let Some(parent) = file.parent() {
            use std::os::unix::fs::PermissionsExt;
            if !parent.exists() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
                std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).ok();
            }
        }
        let (private, public) = generate_key();
        write_private(
            &file,
            &format!("# tierkey break-glass key\n# public key: {public}"),
            &private,
        )?;
        println!(
            "Created {}. Its public key, for break_glass in secrets/config.toml:",
            file.display()
        );
        println!("{public}");
        Ok(())
    }

    /// Sends a new level private key where the key server reads it, before anything else
    /// depends on it. The server keeps older keys after it, comma-separated, so branches still
    /// encrypted for them open until they are removed.
    fn deliver(
        &self,
        keys: &Keys,
        level: &str,
        private: &str,
        public: &str,
        out: Option<PathBuf>,
        deploy: bool,
    ) -> Result<()> {
        let project = &self.config().project;
        let var = key_env_var(project, level);
        if let Some(out) = out {
            write_private(
                &out,
                &format!(
                    "# tierkey level key {project}/{level}\n# public key: {public}\n# The key server reads it from {var}."
                ),
                private,
            )?;
            info(&format!(
                "wrote the private key of {project}/{level} to {}; add its last line to {var} on the key server (in front of any older key, comma-separated), then delete the file",
                out.display()
            ));
            return Ok(());
        }
        let Some(server) = self
            .config()
            .dokploy
            .as_ref()
            .and_then(|d| d.key_server.clone())
        else {
            return fail(
                "say where the private key goes: --out FILE, or [dokploy.key_server] in secrets/config.toml",
            );
        };
        let dokploy = self.dokploy(keys)?;
        let app = dokploy.app(server.kind, &server.id)?;
        let env = app.env();
        let value = match dotenv::get_var(&env, &var).filter(|v| !v.is_empty()) {
            Some(old) => format!("{private},{old}"),
            None => private.to_string(),
        };
        dokploy.save_env(&app, &dotenv::set_var(&env, &var, &value))?;
        info(&format!(
            "saved the key of {project}/{level} to Dokploy {} as {var}",
            app.name()
        ));
        if deploy {
            dokploy.deploy(&app)?;
            info(&format!("deploying {}", app.name()));
        } else {
            info("the key server loads it once it is redeployed (--deploy)");
        }
        Ok(())
    }

    fn set_level_public_key(&self, level: &str, public: &str) -> Result<Project> {
        let path = self.project.config_path();
        let mut doc: toml_edit::DocumentMut = read(&path)?
            .parse()
            .map_err(|e| format!("secrets/config.toml: {e}"))?;
        doc["levels"][level] = toml_edit::value(public);
        let text = doc.to_string();
        Config::parse(&text)?;
        write(&path, &text)?;
        let project = Project::find(&self.project.root)?;
        // While other levels still have no key (a migration), .sops.yaml waits for them.
        if project.config.sops_yaml().is_ok() {
            project.write_sops_yaml()?;
        }
        Ok(project)
    }

    fn refuse_agent(&self, what: &str) -> Result<()> {
        if crate::keys::is_agent() {
            return fail(format!("agents cannot {what}; leave it for a maintainer"));
        }
        Ok(())
    }

    fn destination(&self, out: &Option<PathBuf>) -> String {
        match (
            out,
            self.config()
                .dokploy
                .as_ref()
                .and_then(|d| d.key_server.as_ref()),
        ) {
            (Some(out), _) => out.display().to_string(),
            (None, Some(server)) => format!("the key server's Dokploy service {}", server.id),
            (None, None) => "nowhere (pass --out FILE)".into(),
        }
    }

    fn keygen_level(
        &self,
        level: &str,
        out: Option<PathBuf>,
        deploy: bool,
        yes: bool,
    ) -> Result<()> {
        self.refuse_agent("create level keys")?;
        if !valid_name(level) {
            return fail(format!("invalid level name: {level}"));
        }
        if self
            .config()
            .levels
            .get(level)
            .is_some_and(|k| !k.is_empty())
        {
            return fail(format!("level {level} exists; use rotate-level {level}"));
        }
        let question = format!(
            "Create level {level} of {} and save its key to {}?",
            self.config().project,
            self.destination(&out)
        );
        if !yes && !confirm(&question)? {
            return fail("nothing changed");
        }
        let (private, public) = generate_key();
        let keys = Keys::new(&self.project);
        self.deliver(&keys, level, &private, &public, out, deploy)?;
        self.set_level_public_key(level, &public)?;
        info(&format!(
            "added level {level} ({public}) to secrets/config.toml; point [[files]] rules at it, then run tierkey rekey"
        ));
        Ok(())
    }

    fn rotate_level(
        &self,
        level: &str,
        out: Option<PathBuf>,
        deploy: bool,
        yes: bool,
    ) -> Result<()> {
        self.refuse_agent("rotate level keys")?;
        if self.config().levels.get(level).is_none_or(|k| k.is_empty()) {
            return fail(format!(
                "secrets/config.toml has no key for level {level}; create it with keygen --level {level}"
            ));
        }
        let mut files = Vec::new();
        for file in self.project.all_files() {
            if self
                .config()
                .rule_for(&self.project.rel(&file))
                .is_some_and(|r| r.level == level)
            {
                files.push(file);
            }
        }
        // The current key, to re-encrypt the files after the switch.
        let keys = Keys::new(&self.project);
        let old = if files.is_empty() {
            KeyEnv::None
        } else {
            keys.for_rewrite(&BTreeSet::from([level.to_string()]))?
        };
        let question = format!(
            "Rotate level {level} of {}: re-encrypt {} files with a new key saved to {}?",
            self.config().project,
            files.len(),
            self.destination(&out)
        );
        if !yes && !confirm(&question)? {
            return fail("nothing changed");
        }
        let (private, public) = generate_key();
        self.deliver(&keys, level, &private, &public, out, deploy)?;
        let rotated = Ctx {
            project: self.set_level_public_key(level, &public)?,
        };
        rotated.project.write_sops_yaml()?;
        // New data keys while the old level key still opens the files, then the new recipients.
        rotated.rotate_data_keys(&files, &old)?;
        rotated.update_keys(&files, &old)?;
        info(&format!("level {level} is now {public}; commit secrets/"));
        info(&format!(
            "once no branch needs the old key, remove it from {} on the key server",
            key_env_var(&self.config().project, level)
        ));
        if !files.is_empty() {
            println!("Level {level} could open these values; rotate them if someone lost access:");
            rotated.print_names(&files)?;
        }
        Ok(())
    }

    fn whoami(&self) -> Result<()> {
        let keys = Keys::new(&self.project);
        let access = keys.access()?;
        println!("login: {}", access.login);
        println!("project: {}", access.project);
        println!(
            "levels: {}",
            if access.levels.is_empty() {
                "none".into()
            } else {
                access.levels.join(", ")
            }
        );
        if keys.agent {
            println!(
                "(under Claude Code tierkey only requests {})",
                tierkey_core::AGENT_LEVEL
            );
        }
        Ok(())
    }
}
