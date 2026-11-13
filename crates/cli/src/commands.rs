//! The subcommands. Values only move through memory and pipes to sops, the target command,
//! Dokploy or the key server; nothing here prints one.

use std::collections::BTreeSet;
use std::io::{BufRead, IsTerminal, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use keyko_core::{generate_key, is_public_key, key_env_var, valid_name};

use crate::config::{AppKind, Config, Project};
use crate::dokploy::Dokploy;
use crate::keys::{
    KeyEnv, KeyFile, Keys, personal_key_file, shell_quote, suggested_name, this_command, to_name,
};
use crate::sops::{Sops, path_str};
use crate::{Cmd, PairCmd, PeopleCmd, Result, dotenv, fail, info};

pub fn dispatch(cmd: Cmd) -> Result<()> {
    let cwd = std::env::current_dir().map_err(|e| format!("no current directory: {e}"))?;
    let cwd = cwd.canonicalize().unwrap_or(cwd);
    let cmd = match cmd {
        Cmd::Init { name, project } => return init(&cwd, name, project),
        Cmd::Join { name, no_push } => return join(&cwd, name, no_push),
        other => other,
    };
    let ctx = Ctx {
        project: Project::find(&cwd)?,
    };
    match cmd {
        Cmd::Init { .. } | Cmd::Join { .. } => unreachable!("handled above"),
        Cmd::People { cmd } => ctx.people(cmd.unwrap_or(PeopleCmd::List)),
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

const NAME_RULE: &str =
    "lowercase letters and digits in dash-separated words, like alice or bob-smith";

fn person_name(name: Option<String>) -> Result<String> {
    match name {
        Some(name) if valid_name(&name) => Ok(name),
        Some(name) => fail(format!("invalid name {name:?}: {NAME_RULE}")),
        None => Ok(suggested_name()),
    }
}

/// Your key file, created on first use. True when it was just created.
fn ensure_personal_key() -> Result<(KeyFile, bool)> {
    let path = personal_key_file();
    if let Some(file) = KeyFile::read(&path) {
        return Ok((file, false));
    }
    if path.exists() {
        return fail(format!(
            "{} holds no age key; move it away, then run: keyko join",
            path.display()
        ));
    }
    if let Some(parent) = path.parent().filter(|p| !p.exists()) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).ok();
    }
    let (private, public) = generate_key();
    write_private(
        &path,
        &format!("# keyko personal key\n# public key: {public}"),
        &private,
    )?;
    info(&format!(
        "created your key {}; it is the only copy, so back it up, and never share or commit it",
        path.display()
    ));
    let file = KeyFile::read(&path).ok_or_else(|| format!("cannot read {}", path.display()))?;
    Ok((file, true))
}

/// The level someone new gets unless told otherwise.
fn default_level(config: &Config) -> String {
    let all = config.all_levels();
    if all.contains(keyko_core::AGENT_LEVEL) {
        keyko_core::AGENT_LEVEL.to_string()
    } else {
        all.into_iter().collect::<Vec<_>>().join(",")
    }
}

/// `keyko join`: with an invite code, your key in the place the invite holds for you; else
/// your key, and the command that adds you.
fn join(cwd: &Path, name: Option<String>, no_push: bool) -> Result<()> {
    let project = Project::find(cwd).ok();
    if let Some(p) = &project
        && let Some((person, _)) = Keys::new(p).mine().person
    {
        println!(
            "You are {person} in {}, with levels {}. Check with: keyko whoami",
            p.config.project,
            p.config.people[&person].levels.join(", ")
        );
        return Ok(());
    }
    if let Some(p) = project.clone()
        && let Some(code) = read_invite_code()?
    {
        return accept_invite(p, &code, no_push);
    }
    let name = person_name(name)?;
    let (file, _) = ensure_personal_key()?;
    let public = &file.public[0];
    println!("Your public key: {public}");
    match project {
        Some(p) => {
            println!(
                "Ask a maintainer of {} to run this (it holds no secret):\n\n  keyko people add {name} {public} --levels {}\n\nOnce they push it, git pull and check with: keyko whoami",
                p.config.project,
                default_level(&p.config)
            );
        }
        None => println!(
            "In a project with secrets/config.toml, keyko join prints the command that adds you to it."
        ),
    }
    Ok(())
}

/// An invite code: asked for (hidden) on a terminal, else read from stdin. None when empty.
fn read_invite_code() -> Result<Option<String>> {
    let code = if std::io::stdin().is_terminal() {
        rpassword::prompt_password("Invite code (hidden; press Enter if you have none): ")
            .map_err(|e| format!("cannot read the code: {e}"))?
    } else {
        let mut code = String::new();
        std::io::stdin()
            .read_to_string(&mut code)
            .map_err(|e| format!("cannot read stdin: {e}"))?;
        code
    };
    let code = code.trim();
    Ok((!code.is_empty()).then(|| code.to_string()))
}

/// The invited person whose invite key is `public`.
fn invited_as(project: &Project, public: &str) -> Option<String> {
    project
        .config
        .people
        .iter()
        .find(|(_, p)| p.key == public && p.invited.is_some())
        .map(|(name, _)| name.clone())
}

/// `keyko join` with a code from `people invite`: pulls if the invite is not here yet, opens
/// the invitee's files with the invite key, re-encrypts them for your own key with new data
/// keys, then commits secrets/ and pushes it.
fn accept_invite(project: Project, code: &str, no_push: bool) -> Result<()> {
    if crate::keys::is_agent() {
        return fail("agents cannot accept an invite; ask the user to run: keyko join");
    }
    let Some((private, public)) = parse_invite_code(code) else {
        return fail(format!(
            "that is not an invite code ({INVITE_PREFIX}...); ask whoever invited you to copy it again"
        ));
    };
    let git = Git::of(&project);
    let mut project = project;
    if invited_as(&project, &public).is_none()
        && let Some(git) = &git
    {
        git.pull().map_err(|e| {
            format!("the invite is not in this checkout, and git pull failed ({e}); pull, then run keyko join again")
        })?;
        project = Project::find(&project.root)?;
    }
    let Some(name) = invited_as(&project, &public) else {
        return fail(format!(
            "that code is not a pending invite of {}: it was used or cancelled, or not pushed yet; \
             ask for a new one with: keyko people invite NAME",
            project.config.project
        ));
    };
    if let Some(git) = &git {
        git.require_clean()?;
    }
    let (file, _) = ensure_personal_key()?;
    let mine = file.public[0].clone();
    let ctx = Ctx { project };
    let added = ctx
        .add_person(&name, Some(&mine), &[], None, Some(KeyEnv::Keys(private)))?
        .ok_or_else(|| "nothing changed".to_string())?;
    let shared = match &git {
        Some(git) => git.commit(&format!("secrets: {name} joins"), !no_push),
        None => Shared::NotGit,
    };
    let levels = added.updated.config().people[&name].levels.join(", ");
    info(&format!(
        "re-encrypted {} files for your key; {}",
        added.count,
        shared.describe()
    ));
    println!(
        "You are {name} in {}, with levels {levels}. Check with: keyko whoami",
        added.updated.config().project
    );
    Ok(())
}

/// `keyko init`: a project opened by people's own keys, with you holding every level.
fn init(cwd: &Path, name: Option<String>, project: Option<String>) -> Result<()> {
    if let Ok(p) = Project::find(cwd) {
        return fail(format!(
            "{} already exists; add people with: keyko people add NAME KEY --levels dev",
            p.config_path().display()
        ));
    }
    let root = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
        .unwrap_or_else(|| cwd.to_path_buf());
    let project = match project {
        Some(p) if valid_name(&p) => p,
        Some(p) => return fail(format!("invalid project name {p:?}: {NAME_RULE}")),
        None => root
            .file_name()
            .and_then(|n| to_name(&n.to_string_lossy()))
            .unwrap_or_else(|| "app".into()),
    };
    let name = person_name(name)?;
    let (file, _) = ensure_personal_key()?;
    let text = format!(
        r#"project = "{project}"

# People open files with their own age key. Someone new runs `keyko join` and sends you the
# command it prints: keyko people add NAME KEY --levels dev
[people]
{name} = {{ key = "{key}", levels = ["dev", "preview", "production"] }}

# Which level each file under secrets/ belongs to; the first matching rule wins. `*` matches
# slashes too.
[[files]]
path = "dev.env"
level = "dev"

[[files]]
path = "production/*"
level = "production"

[[files]]
path = "*"
level = "preview"

# Branch globs to environments (directories under secrets/); the first match wins.
[[branches]]
branch = "main"
env = "production"

[[branches]]
branch = "*"
env = "preview"
"#,
        key = file.public[0]
    );
    Config::parse(&text)?;
    let dir = root.join("secrets");
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    write(&dir.join(crate::config::CONFIG_FILE), &text)?;
    Project::find(&root)?.write_sops_yaml()?;
    println!(
        "Created secrets/config.toml for {project}, with you as {name} (levels dev, preview, production).\n\
         \n  keyko set dev NAME             # add a secret (prompted, hidden)\n  \
         keyko run dev -- <command>     # run with the file's values\n  \
         keyko check                    # before committing secrets/\n\n\
         To add someone, they run keyko join in the project and send you the command it prints."
    );
    Ok(())
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
            return fail(format!(
                "{} does not exist; keyko set {} NAME creates it",
                self.project.show(&file),
                shell_quote(arg)
            ));
        }
        Ok(file)
    }

    /// Decrypts a file in memory, or says why it would not open and what to run.
    fn decrypt(&self, keys: &Keys, file: &Path) -> Result<String> {
        let key = self.key_for(keys, file)?;
        self.sops()?
            .decrypt(&key, file)
            .map_err(|_| self.cannot_open(keys, file))
    }

    fn cannot_open(&self, keys: &Keys, file: &Path) -> String {
        let show = self.project.show(file);
        let rel = self.project.rel(file);
        let have = read(file)
            .map(|t| dotenv::recipients(&t))
            .unwrap_or_default();
        let level = self.config().level_of(&rel).unwrap_or_default();
        if let Some((name, key)) = keys.mine().person
            && self.config().people[&name]
                .levels
                .iter()
                .any(|l| l == level)
            && !have.contains(&key)
        {
            return format!(
                "{show} is not encrypted for {name} yet; someone who can open level {level} must run keyko rekey and push it, then git pull"
            );
        }
        format!(
            "could not decrypt {show}; if git merged it, run: keyko merge-fix {}",
            shell_quote(rel.trim_end_matches(".env"))
        )
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
                return fail(format!(
                    "{} does not exist; create it with: keyko set {} NAME (or run with --optional)",
                    self.project.show(&file),
                    shell_quote(target)
                ));
            }
            // An environment that has not moved its secrets to git keeps its platform's variables.
            info(&format!(
                "no {}, running without it",
                self.project.show(&file)
            ));
        } else {
            let keys = Keys::new(&self.project);
            let plain = self.decrypt(&keys, &file)?;
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
            return fail(format!(
                "agents cannot open decrypted files; change one value with: keyko set {} NAME",
                shell_quote(target)
            ));
        }
        let file = self.existing(target)?;
        let keys = Keys::new(&self.project);
        let key = self.key_for(&keys, &file)?;
        self.sops()?.interactive(&key, &["edit", &path_str(&file)?])
    }

    fn check(&self) -> Result<()> {
        let mut failed = false;
        let mut problem = |m: String| {
            eprintln!("keyko: {m}");
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
                            "{show} is not encrypted for its recipients in secrets/config.toml; run keyko rekey"
                        ));
                    }
                }
            }
        }
        // An invite code opens its levels until it is used; an old one is likely forgotten.
        for (name, person) in &self.config().people {
            if let Some(day) = person
                .invited
                .as_deref()
                .and_then(crate::config::days_from_date)
                && crate::config::today() - day > INVITE_DAYS
            {
                crate::warn(&format!(
                    "{name} was invited more than {INVITE_DAYS} days ago and has not joined; \
                     cancel with: keyko people remove {name}"
                ));
            }
        }
        // Nothing else may live under secrets/: a stray file there is probably a decrypted copy.
        for file in p.stray_files() {
            problem(format!("{} is not an encrypted .env file", p.show(&file)));
        }
        let have = std::fs::read_to_string(p.sops_path()).unwrap_or_default();
        match self.config().is_generated_sops_yaml(&have) {
            Err(e) => problem(e),
            Ok(false) => problem(
                "secrets/.sops.yaml does not match secrets/config.toml; run keyko rekey".into(),
            ),
            Ok(true) => {}
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
                let plain = self.decrypt(keys, &file)?;
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
        let wanted = self.decrypt(&keys, &file)?;
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
                eprintln!("keyko: [dokploy.apps] lists {path}, which does not exist");
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
                eprintln!("keyko: secrets/{path} and Dokploy {} differ:", app.name());
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
        let plain = self.decrypt(&keys, &file)?;
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
            &format!("# keyko break-glass key\n# public key: {public}"),
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
                    "# keyko level key {project}/{level}\n# public key: {public}\n# The key server reads it from {var}."
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
        // Older keys may still sit under the name from before the rename, TIERKEY_KEY__...
        let old_var = var.replacen("KEYKO_", "TIERKEY_", 1);
        let older = dotenv::get_var(&env, &var)
            .filter(|v| !v.is_empty())
            .or_else(|| dotenv::get_var(&env, &old_var).filter(|v| !v.is_empty()));
        let value = match older {
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

    /// The config's text after `change`, keeping its comments and layout.
    fn edited_config(&self, change: impl FnOnce(&mut toml_edit::DocumentMut)) -> Result<String> {
        let mut doc: toml_edit::DocumentMut = read(&self.project.config_path())?
            .parse()
            .map_err(|e| format!("secrets/config.toml: {e}"))?;
        change(&mut doc);
        Ok(doc.to_string())
    }

    fn set_level_public_key(&self, level: &str, public: &str) -> Result<Project> {
        let text = self.edited_config(|doc| {
            if !doc.contains_key("levels") {
                let mut table = toml_edit::Table::new();
                table.set_position(table_position(doc, "people"));
                doc.insert("levels", toml_edit::Item::Table(table));
            }
            doc["levels"][level] = toml_edit::value(public);
        })?;
        Config::parse(&text)?;
        write(&self.project.config_path(), &text)?;
        let project = Project::find(&self.project.root)?;
        // While other levels still have no key (a migration), .sops.yaml waits for them.
        if project.config.sops_yaml().is_ok() {
            project.write_sops_yaml()?;
        }
        Ok(project)
    }

    fn refuse_agent(&self, what: &str) -> Result<()> {
        if crate::keys::is_agent() {
            return fail(format!(
                "agents cannot {what}; ask the user to run it: {}",
                this_command()
            ));
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
            "added level {level} ({public}) to secrets/config.toml; point [[files]] rules at it, then run keyko rekey"
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
        if !self.config().is_shared(level) && self.config().all_levels().contains(level) {
            return fail(format!(
                "level {level} has no shared key: people open it with their own keys. To take it \
                 from someone, run: keyko people remove NAME --levels {level}"
            ));
        }
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
        let mine = keys.mine();
        match (&mine.person, mine.public()) {
            (Some((name, _)), _) => println!(
                "person: {name} (levels: {})",
                self.config().people[name].levels.join(", ")
            ),
            (None, Some(public)) if !self.config().people.is_empty() => println!(
                "person: none; your key is not in [people]. Ask a maintainer to run: keyko people add {} {public} --levels {}",
                suggested_name(),
                default_level(self.config())
            ),
            _ => {}
        }
        let Some(_) = keys.server() else {
            if mine.public().is_none() {
                return fail(format!(
                    "you have no key at {} and this project has no key server; run: keyko join",
                    personal_key_file().display()
                ));
            }
            return Ok(());
        };
        let access = match keys.access() {
            Ok(access) => access,
            Err(e) if mine.person.is_some() => {
                crate::warn(&e);
                return Ok(());
            }
            Err(e) => return Err(e),
        };
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
                "(under Claude Code keyko only requests {})",
                keyko_core::AGENT_LEVEL
            );
        }
        Ok(())
    }

    fn people(&self, cmd: PeopleCmd) -> Result<()> {
        match cmd {
            PeopleCmd::List => self.people_list(),
            PeopleCmd::Add { name, key, levels } => self.people_add(&name, key.as_deref(), &levels),
            PeopleCmd::Invite {
                name,
                levels,
                any_level,
                no_push,
            } => self.people_invite(&name, &levels, any_level, no_push),
            PeopleCmd::Remove { name, levels } => self.people_remove(&name, &levels),
        }
    }

    fn people_list(&self) -> Result<()> {
        let people = &self.config().people;
        if people.is_empty() {
            info(
                "nobody is in [people]; someone runs keyko join and sends you the command it prints",
            );
            return Ok(());
        }
        let me = Keys::new(&self.project).mine().person.map(|(n, _)| n);
        let width = people.keys().map(String::len).max().unwrap_or(0);
        for (name, person) in people {
            let you = if me.as_deref() == Some(name) {
                "  (you)"
            } else {
                ""
            };
            let invited = match &person.invited {
                Some(day) => format!("  (invited {day}, not joined yet)"),
                None => String::new(),
            };
            println!(
                "{name:<width$}  {}  {}{you}{invited}",
                person.key,
                person.levels.join(", ")
            );
        }
        Ok(())
    }

    /// Files of `levels` that are not encrypted for exactly their recipients.
    fn stale_files(&self, levels: &[String]) -> Result<Vec<PathBuf>> {
        let mut out = Vec::new();
        for file in self.project.all_files() {
            let rel = self.project.rel(&file);
            if !levels
                .iter()
                .any(|l| l == self.config().level_of(&rel).unwrap_or_default())
            {
                continue;
            }
            let want: BTreeSet<String> = self
                .config()
                .recipients_for(&rel)?
                .into_iter()
                .map(|(k, _)| k)
                .collect();
            let have: BTreeSet<String> = dotenv::recipients(&read(&file)?).into_iter().collect();
            if want != have {
                out.push(file);
            }
        }
        Ok(out)
    }

    /// The files encrypted for a public key.
    fn files_for_key(&self, key: &str) -> Result<Vec<PathBuf>> {
        let mut out = Vec::new();
        for file in self.project.all_files() {
            if dotenv::recipients(&read(&file)?).iter().any(|k| k == key) {
                out.push(file);
            }
        }
        Ok(out)
    }

    /// Writes a new config: `.sops.yaml` too. Returns the context for it.
    fn save_config(&self, text: &str) -> Result<Ctx> {
        write(&self.project.config_path(), text)?;
        let project = Project::find(&self.project.root)?;
        if project.config.sops_yaml().is_ok() {
            project.write_sops_yaml()?;
        }
        Ok(Ctx { project })
    }

    fn people_add(&self, name: &str, key: Option<&str>, levels: &[String]) -> Result<()> {
        self.refuse_agent("change who can open secrets")?;
        let Some(added) = self.add_person(name, key, levels, None, None)? else {
            return Ok(());
        };
        info(&format!(
            "{}; re-encrypted {} files. Commit and push secrets/; then {name} can run: keyko whoami",
            added.what, added.count
        ));
        added.print_rotate()
    }

    /// `keyko people invite`: someone new, under a key made for them that `keyko join`
    /// swaps for theirs. Commits secrets/ (and pushes it), then prints the code.
    fn people_invite(
        &self,
        name: &str,
        levels: &[String],
        any_level: bool,
        no_push: bool,
    ) -> Result<()> {
        self.refuse_agent("change who can open secrets")?;
        if !valid_name(name) {
            return fail(format!("invalid name {name:?}: {NAME_RULE}"));
        }
        if self.config().people.contains_key(name) {
            return fail(format!(
                "{name} is already in [people]; give them levels with: keyko people add {name} --levels L"
            ));
        }
        let all = self.config().all_levels();
        let levels: Vec<String> = if !levels.is_empty() {
            levels.to_vec()
        } else if all.contains(keyko_core::AGENT_LEVEL) {
            vec![keyko_core::AGENT_LEVEL.to_string()]
        } else {
            return fail(format!(
                "say which levels {name} gets: keyko people invite {name} --levels <{}>",
                all.iter().cloned().collect::<Vec<_>>().join("|")
            ));
        };
        if !any_level && let Some(level) = levels.iter().find(|l| *l != keyko_core::AGENT_LEVEL) {
            return fail(format!(
                "until {name} uses the code, anyone who has it opens the levels it gives, so an invite                  gives only {dev} unless you pass --any-level. Or, with nothing secret sent: {name} runs                  keyko join, and you run the command it prints (keyko people add {name} KEY --levels {level})",
                dev = keyko_core::AGENT_LEVEL
            ));
        }
        let git = Git::of(&self.project);
        if let Some(git) = &git {
            git.require_clean()?;
        }
        let (private, public) = generate_key();
        let today = crate::config::date_of(crate::config::today());
        let added = self
            .add_person(name, Some(&public), &levels, Some(&today), None)?
            .ok_or_else(|| format!("{name} was not added"))?;
        let shared = match &git {
            Some(git) => git.commit(
                &format!("secrets: invite {name} ({})", levels.join(", ")),
                !no_push,
            ),
            None => Shared::NotGit,
        };
        info(&format!(
            "invited {name} with levels {}; re-encrypted {} files; {}",
            levels.join(", "),
            added.count,
            shared.describe()
        ));
        println!(
            "Send {name} this one-time code privately:\n\n  {}\n\n\
             In their clone of the repository they run keyko join and paste it; that pushes their own key \
             in its place. Until then the code opens levels {}. To cancel: keyko people remove {name}",
            invite_code(&private),
            levels.join(", ")
        );
        Ok(())
    }

    /// Adds someone, gives them levels or replaces their key, and re-encrypts their files.
    /// `invited` marks someone new as invited; `key_env` overrides the keys that open the files.
    /// None when nothing changed.
    fn add_person(
        &self,
        name: &str,
        key: Option<&str>,
        levels: &[String],
        invited: Option<&str>,
        key_env: Option<KeyEnv>,
    ) -> Result<Option<Added>> {
        if !valid_name(name) {
            return fail(format!("invalid name {name:?}: {NAME_RULE}"));
        }
        if let Some(key) = key
            && !is_public_key(key)
        {
            return fail(format!(
                "{key} is not an age public key (age1...); they get theirs with: keyko join"
            ));
        }
        let all = self.config().all_levels();
        let all_list = all.iter().cloned().collect::<Vec<_>>().join(",");
        if let Some(level) = levels.iter().find(|l| !all.contains(*l)) {
            return fail(format!(
                "there is no level {level}; the levels are {all_list}. A new level starts with a [[files]] rule that uses it"
            ));
        }
        let old = self.config().people.get(name).cloned();
        let key = match (&old, key) {
            (None, None) => {
                return fail(format!(
                    "{name} is not in [people] yet; add them with the public key keyko join prints for them: keyko people add {name} age1... --levels {}",
                    if levels.is_empty() {
                        default_level(self.config())
                    } else {
                        levels.join(",")
                    }
                ));
            }
            (None, Some(key)) => key.to_string(),
            (Some(person), key) => key.unwrap_or(&person.key).to_string(),
        };
        let mut new_levels = old.as_ref().map(|p| p.levels.clone()).unwrap_or_default();
        let wanted = if old.is_none() && levels.is_empty() {
            if !all.contains(keyko_core::AGENT_LEVEL) {
                return fail(format!(
                    "say which levels {name} gets: keyko people add {name} {key} --levels <{}>",
                    all.iter().cloned().collect::<Vec<_>>().join("|")
                ));
            }
            vec![keyko_core::AGENT_LEVEL.to_string()]
        } else {
            levels.to_vec()
        };
        let mut added = Vec::new();
        for level in wanted {
            if !new_levels.contains(&level) {
                new_levels.push(level.clone());
                added.push(level);
            }
        }
        let replaced = old.as_ref().filter(|p| p.key != key).map(|p| p.key.clone());
        if old.is_some() && added.is_empty() && replaced.is_none() {
            info(&format!(
                "{name} already has levels {}; nothing changed",
                new_levels.join(", ")
            ));
            return Ok(None);
        }
        if let Some(label) = self.config().label_of(&key)
            && label != format!("person {name}")
        {
            return fail(format!("{key} is already {label} in secrets/config.toml"));
        }
        // A new key ends an invite: it is theirs now.
        let invited = match &old {
            None => invited.map(str::to_string),
            Some(p) if replaced.is_none() => p.invited.clone(),
            Some(_) => None,
        };
        let text =
            self.edited_config(|doc| set_person(doc, name, &key, &new_levels, invited.as_deref()))?;
        let updated = Ctx {
            project: Project {
                config: Config::parse(&text)?,
                ..self.project.clone()
            },
        };
        // The files of their levels that are not encrypted for them yet (or still for an old key).
        let stale = updated.stale_files(&new_levels)?;
        let rotate = match &replaced {
            Some(old_key) => self.files_for_key(old_key)?,
            None => Vec::new(),
        };
        let key_env = match key_env {
            Some(env) => env,
            None if stale.is_empty() => KeyEnv::None,
            None => Keys::new(&self.project)
                .for_rewrite(&self.levels_of(&stale)?)
                .map_err(|e| {
                    format!(
                        "nothing changed: re-encrypting {name}'s files needs keys you lack: {e}"
                    )
                })?,
        };
        let updated = self.save_config(&text)?;
        updated.rotate_data_keys(&rotate, &key_env)?;
        updated.update_keys(&stale, &key_env)?;
        let what = match (&old, &replaced) {
            (None, _) => format!("added {name} with levels {}", new_levels.join(", ")),
            (Some(_), Some(_)) => format!("replaced {name}'s key"),
            (Some(_), None) => format!("gave {name} levels {}", added.join(", ")),
        };
        Ok(Some(Added {
            updated,
            what,
            count: stale.len(),
            rotate,
        }))
    }

    fn people_remove(&self, name: &str, levels: &[String]) -> Result<()> {
        self.refuse_agent("change who can open secrets")?;
        let Some(person) = self.config().people.get(name).cloned() else {
            let names: Vec<&str> = self.config().people.keys().map(String::as_str).collect();
            return fail(format!(
                "{name} is not in [people] (people: {}); see them with: keyko people",
                if names.is_empty() {
                    "none".into()
                } else {
                    names.join(", ")
                }
            ));
        };
        if let Some(level) = levels.iter().find(|l| !person.levels.contains(*l)) {
            return fail(format!(
                "{name} does not have level {level}; their levels are {}",
                person.levels.join(", ")
            ));
        }
        let kept: Vec<String> = person
            .levels
            .iter()
            .filter(|l| !levels.is_empty() && !levels.contains(*l))
            .cloned()
            .collect();
        let used: BTreeSet<&str> = self
            .config()
            .files
            .iter()
            .map(|r| r.level.as_str())
            .collect();
        if let Some(level) = person.levels.iter().find(|l| {
            !kept.contains(l)
                && used.contains(l.as_str())
                && !self.config().is_shared(l)
                && self.config().holders(l) == [name]
        }) {
            return fail(format!(
                "{name} is the last person with level {level}, so nobody could open its files; \
                 give it to someone else first: keyko people add NAME --levels {level}"
            ));
        }
        let text = self.edited_config(|doc| {
            if kept.is_empty() {
                if let Some(people) = doc.get_mut("people").and_then(|p| p.as_table_like_mut()) {
                    people.remove(name);
                }
            } else {
                set_person(doc, name, &person.key, &kept, person.invited.as_deref());
            }
        })?;
        let new = Config::parse(&text).map_err(|e| format!("cannot remove {name}: {e}"))?;
        let updated = Ctx {
            project: Project {
                config: new,
                ..self.project.clone()
            },
        };
        // The files they could open and will not: new data keys, then without their key.
        let mut affected = Vec::new();
        for file in self.files_for_key(&person.key)? {
            let still = updated
                .config()
                .recipients_for(&self.project.rel(&file))?
                .iter()
                .any(|(k, _)| *k == person.key);
            if !still {
                affected.push(file);
            }
        }
        let keys = Keys::new(&self.project);
        let key_env = if affected.is_empty() {
            KeyEnv::None
        } else {
            keys.for_rewrite(&self.levels_of(&affected)?).map_err(|e| {
                format!("nothing changed: re-encrypting {name}'s files needs keys you lack: {e}")
            })?
        };
        let updated = self.save_config(&text)?;
        updated.rotate_data_keys(&affected, &key_env)?;
        updated.update_keys(&affected, &key_env)?;
        info(&format!(
            "{}; re-encrypted {} files with new data keys. Commit and push secrets/",
            if kept.is_empty() {
                format!("removed {name}")
            } else {
                format!("{name} keeps levels {}", kept.join(", "))
            },
            affected.len()
        ));
        if !affected.is_empty() {
            println!(
                "{name} could open these values, which stay readable in git history. Rotate them at their source if {name} should lose access:"
            );
            updated.print_names(&affected)?;
        }
        Ok(())
    }
}

/// What `add_person` did.
struct Added {
    updated: Ctx,
    what: String,
    /// Files re-encrypted.
    count: usize,
    /// Files an old key of theirs could open.
    rotate: Vec<PathBuf>,
}

impl Added {
    fn print_rotate(&self) -> Result<()> {
        if !self.rotate.is_empty() {
            println!(
                "The old key could open these values, which stay readable in git history. If it may have leaked, rotate them:"
            );
            self.updated.print_names(&self.rotate)?;
        }
        Ok(())
    }
}

/// Days after which `check` warns about an invite nobody used.
const INVITE_DAYS: i64 = 7;

const INVITE_PREFIX: &str = "KEYKO-INVITE-";
const AGE_PREFIX: &str = "AGE-SECRET-KEY-";

/// The code `people invite` prints: the invite's private key, renamed so it reads as one.
fn invite_code(private: &str) -> String {
    format!(
        "{INVITE_PREFIX}{}",
        private.strip_prefix(AGE_PREFIX).unwrap_or(private)
    )
}

/// The private key in an invite code, and its public key.
fn parse_invite_code(code: &str) -> Option<(String, String)> {
    let code = code.trim().to_ascii_uppercase();
    // TIERKEY-INVITE-: codes printed before the rename.
    let rest = code
        .strip_prefix(INVITE_PREFIX)
        .or_else(|| code.strip_prefix("TIERKEY-INVITE-"))?;
    let private = format!("{AGE_PREFIX}{rest}");
    let public = keyko_core::public_key_of(&private)?;
    Some((private, public))
}

/// What happened to a change in secrets/.
enum Shared {
    Pushed,
    Committed,
    PushFailed(String),
    CommitFailed(String),
    NotGit,
}

impl Shared {
    fn describe(&self) -> String {
        match self {
            Shared::Pushed => "committed and pushed secrets/".into(),
            Shared::Committed => "committed secrets/; push it when ready".into(),
            Shared::PushFailed(e) => format!(
                "committed secrets/, but could not push it ({e}); push it yourself, or open a pull request"
            ),
            Shared::CommitFailed(e) => {
                format!("could not commit secrets/ ({e}); commit and push it yourself")
            }
            Shared::NotGit => "not a git repository; share secrets/ yourself".into(),
        }
    }
}

/// The git work tree around a project, for the commands that commit secrets/ themselves.
struct Git {
    root: PathBuf,
    dir: PathBuf,
}

impl Git {
    /// None when the project is not in a git work tree.
    fn of(project: &Project) -> Option<Git> {
        let git = Git {
            root: project.root.clone(),
            dir: project.dir.clone(),
        };
        git.run(&["rev-parse", "--is-inside-work-tree"])
            .ok()
            .filter(|out| out.trim() == "true")
            .map(|_| git)
    }

    /// git's stdout, or its stderr as the error.
    fn run(&self, args: &[&str]) -> Result<String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| format!("cannot run git: {e}"))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into())
        } else {
            let err = String::from_utf8_lossy(&out.stderr);
            let err = err.trim();
            Err(if err.is_empty() {
                format!("git {} failed", args.join(" "))
            } else {
                err.lines().last().unwrap_or(err).to_string()
            })
        }
    }

    fn dir_arg(&self) -> String {
        self.dir.to_string_lossy().into_owned()
    }

    /// Refuses when secrets/ has changes of its own, which a commit of it would take along.
    fn require_clean(&self) -> Result<()> {
        let status = self.run(&["status", "--porcelain", "--", &self.dir_arg()])?;
        if !status.trim().is_empty() {
            return fail(
                "secrets/ has uncommitted changes, and this commits secrets/; commit or stash them first",
            );
        }
        Ok(())
    }

    /// Fast-forwards the current branch from its upstream.
    fn pull(&self) -> Result<()> {
        self.run(&["pull", "--ff-only", "-q"]).map(|_| ())
    }

    /// Commits everything under secrets/, and pushes it when asked.
    fn commit(&self, message: &str, push: bool) -> Shared {
        let dir = self.dir_arg();
        let committed = self
            .run(&["add", "-A", "--", &dir])
            .and_then(|_| self.run(&["commit", "-q", "-m", message, "--", &dir]));
        if let Err(e) = committed {
            return Shared::CommitFailed(e);
        }
        if !push {
            return Shared::Committed;
        }
        match self.run(&["push", "-q"]) {
            Ok(_) => Shared::Pushed,
            Err(e) => Shared::PushFailed(e),
        }
    }
}

/// Sets a person in `[people]` as a one-line inline table, adding the table after `[levels]`.
fn set_person(
    doc: &mut toml_edit::DocumentMut,
    name: &str,
    key: &str,
    levels: &[String],
    invited: Option<&str>,
) {
    if !doc.contains_key("people") {
        let mut table = toml_edit::Table::new();
        table.set_position(table_position(doc, "levels"));
        doc.insert("people", toml_edit::Item::Table(table));
    }
    let levels: toml_edit::Array = levels.iter().map(String::as_str).collect();
    let people = doc["people"].as_table_like_mut().expect("a table");
    match people.get_mut(name).and_then(|p| p.as_table_like_mut()) {
        Some(person) => {
            person.insert("key", toml_edit::value(key));
            person.insert("levels", toml_edit::value(levels));
            match invited {
                Some(day) => {
                    person.insert("invited", toml_edit::value(day));
                }
                None => {
                    person.remove("invited");
                }
            }
        }
        None => {
            let mut person = toml_edit::InlineTable::new();
            person.insert("key", key.into());
            person.insert("levels", levels.into());
            if let Some(day) = invited {
                person.insert("invited", day.into());
            }
            person.fmt();
            people.insert(name, toml_edit::value(person));
        }
    }
}

/// Where a new top-level table goes so it is written right after `after`, if that exists.
fn table_position(doc: &toml_edit::DocumentMut, after: &str) -> Option<isize> {
    doc.get(after)
        .and_then(|t| t.as_table())
        .and_then(|t| t.position())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_invite_codes_from_before_the_rename() {
        let (private, public) = generate_key();
        let code = invite_code(&private);
        assert_eq!(
            parse_invite_code(&code),
            Some((private.clone(), public.clone()))
        );
        let old = code.replacen(INVITE_PREFIX, "TIERKEY-INVITE-", 1);
        assert_eq!(parse_invite_code(&old), Some((private, public)));
    }
}
