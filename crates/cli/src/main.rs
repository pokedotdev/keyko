//! tierkey: encrypted secrets in git with SOPS + age, shared with your team. Each person opens them
//! with their own key or with level keys from a key server on the tailnet. No subcommand prints a
//! secret value.

mod commands;
mod config;
mod dns;
mod dokploy;
mod dotenv;
mod http;
mod keys;
mod sops;

use clap::{Parser, Subcommand};

pub type Result<T> = std::result::Result<T, String>;

pub fn fail<T>(message: impl Into<String>) -> Result<T> {
    Err(message.into())
}

pub fn warn(message: &str) {
    eprintln!("tierkey: {message}");
}

pub fn info(message: &str) {
    eprintln!("tierkey: {message}");
}

/// Secrets in git with SOPS + age. A target is `dev`, `<env>/<file>` (`production/api`), `auto`
/// (the `auto` file of the current branch's environment) or a path to a .env file under secrets/.
///
/// Keys: SOPS_AGE_KEY or SOPS_AGE_KEY_FILE when set (CI, Cloudflare Pages); else your own key
/// (~/.config/tierkey/key.txt) for the levels [people] gives you, and the level keys the key
/// server gives you for the rest, kept in memory; else the break-glass key file. Under Claude
/// Code (CLAUDECODE=1) only the dev level is ever opened.
#[derive(Parser)]
#[command(version, about, long_about)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Start a project: secrets/config.toml, with you as its first person.
    Init {
        /// Your name in [people] (default: from git's user.name).
        #[arg(long)]
        name: Option<String>,
        /// The project's name (default: the directory's).
        #[arg(long)]
        project: Option<String>,
    },
    /// Create your own key (once per machine) and print the command that adds you to a project.
    Join {
        /// Your name in [people] (default: from git's user.name).
        name: Option<String>,
    },
    /// The people who open files with their own key: list, add, remove.
    People {
        #[command(subcommand)]
        cmd: Option<PeopleCmd>,
    },
    /// Run a command with a file's secrets in its environment, and nowhere else.
    Run {
        target: String,
        /// When the file does not exist, run the command without it.
        #[arg(long)]
        optional: bool,
        #[arg(
            trailing_var_arg = true,
            allow_hyphen_values = true,
            required = true,
            value_name = "CMD"
        )]
        cmd: Vec<String>,
    },
    /// The environment a branch deploys to (the current branch by default).
    Env { branch: Option<String> },
    /// Key names of a file, without decrypting.
    Names { target: String },
    /// Set a value: prompted (hidden), read from stdin, or random.
    Set {
        target: String,
        name: String,
        /// A random hex value of this many bytes (at least 16).
        #[arg(long, value_name = "BYTES")]
        random: Option<usize>,
    },
    /// Remove a key.
    Unset { target: String, name: String },
    /// Open the decrypted file in $EDITOR (people only).
    Edit { target: String },
    /// Every file encrypted for its recipients, every environment mapped. Needs no key.
    Check {
        /// Also compare key names with Dokploy.
        #[arg(long)]
        remote: bool,
    },
    /// The age public keys that can open each file.
    Recipients { target: Option<String> },
    /// Regenerate secrets/.sops.yaml and re-encrypt every file for its recipients.
    Rekey,
    /// Rekey without a removed key, with new data keys, and list what it could read.
    Revoke { public_key: String },
    /// Re-sign a file after resolving a git conflict by hand.
    MergeFix { target: String },
    /// Save a file as its Dokploy application's environment.
    Sync {
        target: String,
        /// Show which names would change; save nothing.
        #[arg(long)]
        dry_run: bool,
        /// Do not ask before saving.
        #[arg(long)]
        yes: bool,
        /// Redeploy the application after saving.
        #[arg(long)]
        deploy: bool,
    },
    /// Rotate one key at a time in a `name=key,...` list.
    #[command(subcommand)]
    Pair(PairCmd),
    /// Create a break-glass key file, or with --level a new level key.
    Keygen {
        /// Create this level: its public key goes into secrets/config.toml, its private key to
        /// the key server's Dokploy service or --out.
        #[arg(long)]
        level: Option<String>,
        /// Write the private key to this new file (mode 0600) instead.
        #[arg(long)]
        out: Option<std::path::PathBuf>,
        /// Redeploy the key server after saving a level key to it.
        #[arg(long)]
        deploy: bool,
        #[arg(long)]
        yes: bool,
    },
    /// Replace a level's key: new key pair, files re-encrypted with new data keys.
    RotateLevel {
        level: String,
        /// Write the new private key to this new file (mode 0600) instead of Dokploy.
        #[arg(long)]
        out: Option<std::path::PathBuf>,
        /// Redeploy the key server after saving the new key to it.
        #[arg(long)]
        deploy: bool,
        #[arg(long)]
        yes: bool,
    },
    /// Who the key server sees you as, and which levels it gives you.
    Whoami,
}

#[derive(Subcommand)]
enum PeopleCmd {
    /// Each person, their levels and key (the default).
    List,
    /// Add someone, give them more levels, or replace their key; re-encrypts their files.
    Add {
        name: String,
        /// Their age public key (from `tierkey join`). Optional when they are already listed.
        key: Option<String>,
        /// Levels to give them, comma-separated (default: dev, for someone new).
        #[arg(long, alias = "level", value_delimiter = ',')]
        levels: Vec<String>,
    },
    /// Remove someone, or only some of their levels; gives their files new data keys.
    Remove {
        name: String,
        /// Only take these levels, comma-separated.
        #[arg(long, alias = "level", value_delimiter = ',')]
        levels: Vec<String>,
    },
}

#[derive(Subcommand)]
enum PairCmd {
    /// The names in the list.
    List { target: String, list: String },
    /// Add a new random key, and set it in each `<target>:VAR` too.
    Add {
        target: String,
        list: String,
        name: String,
        #[arg(value_name = "TARGET:VAR")]
        also: Vec<String>,
    },
    /// Remove a key; the last one cannot be removed.
    Remove {
        target: String,
        list: String,
        name: String,
    },
}

fn main() {
    let cli = Cli::parse();
    if let Err(message) = commands::dispatch(cli.command) {
        eprintln!("tierkey: {message}");
        std::process::exit(1);
    }
}
