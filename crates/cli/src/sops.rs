//! Runs the `sops` binary, downloading a pinned release where it is not installed.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use sha2::{Digest, Sha256};

use crate::keys::KeyEnv;
use crate::{Result, fail};

pub const SOPS_VERSION: &str = "3.13.3";

/// SHA-256 of each pinned sops release binary.
const SOPS_SUMS: [(&str, &str); 4] = [
    (
        "linux.amd64",
        "e5bec3346a873ae91d871550f3e698c1aad962aff462a080e40f25fde17fef6b",
    ),
    (
        "linux.arm64",
        "53b0abacd38ef1b12a66d6c100956691b9cefce018d91f81e73ddf7438b94d77",
    ),
    (
        "darwin.amd64",
        "42162d5cef10b74fcf80a045a70e658d7ce6e63d6ea1be6f347e44015714468d",
    ),
    (
        "darwin.arm64",
        "b97c0d434aab577dc40310e8d22ff9e45eef4c80638ab978daae9b4681c59286",
    ),
];

pub struct Sops {
    bin: PathBuf,
    config: PathBuf,
    /// An empty directory sops gets as its config home, so it reads no key but the ones chosen.
    empty_home: PathBuf,
}

impl Sops {
    /// `KEYKO_SOPS`, else `sops` on PATH, else the pinned release cached in `<root>/.cache/`.
    pub fn locate(root: &Path, config: PathBuf) -> Result<Sops> {
        let bin = match std::env::var_os("KEYKO_SOPS") {
            Some(bin) => PathBuf::from(bin),
            None => match which("sops") {
                Some(bin) => bin,
                None => download(root)?,
            },
        };
        let empty_home = std::env::temp_dir().join(format!("keyko-empty-{}", std::process::id()));
        Ok(Sops {
            bin,
            config,
            empty_home,
        })
    }

    fn command(&self, key: &KeyEnv) -> Command {
        let mut cmd = Command::new(&self.bin);
        cmd.arg("--config").arg(&self.config);
        // sops would also read keys.txt under the user's config directory; point it elsewhere so
        // it uses exactly the identity keyko picked.
        let isolate = |cmd: &mut Command| {
            cmd.env("XDG_CONFIG_HOME", &self.empty_home)
                .env_remove("SOPS_AGE_KEY_CMD")
                .env_remove("SOPS_AGE_SSH_PRIVATE_KEY_FILE");
        };
        match key {
            KeyEnv::Inherit => {}
            KeyEnv::Keys(keys) => {
                isolate(&mut cmd);
                cmd.env("SOPS_AGE_KEY", keys)
                    .env_remove("SOPS_AGE_KEY_FILE");
            }
            KeyEnv::None => {
                isolate(&mut cmd);
                cmd.env_remove("SOPS_AGE_KEY")
                    .env_remove("SOPS_AGE_KEY_FILE");
            }
        }
        cmd
    }

    /// Runs sops with `input` on stdin and returns its stdout. Its stderr goes to ours.
    pub fn output(&self, key: &KeyEnv, args: &[&str], input: Option<&[u8]>) -> Result<Vec<u8>> {
        let mut cmd = self.command(key);
        cmd.args(args)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("cannot run {}: {e}", self.bin.display()))?;
        if let Some(input) = input {
            let mut stdin = child.stdin.take().expect("piped stdin");
            // A write error shows up as sops failing, below.
            let _ = stdin.write_all(input);
        }
        let out = child
            .wait_with_output()
            .map_err(|e| format!("sops failed: {e}"))?;
        if !out.status.success() {
            return fail(format!("sops {} failed", args.first().unwrap_or(&"")));
        }
        Ok(out.stdout)
    }

    /// Runs sops attached to the terminal (`edit`).
    pub fn interactive(&self, key: &KeyEnv, args: &[&str]) -> Result<()> {
        let status = self
            .command(key)
            .args(args)
            .status()
            .map_err(|e| format!("cannot run {}: {e}", self.bin.display()))?;
        if !status.success() {
            return fail(format!("sops {} failed", args.first().unwrap_or(&"")));
        }
        Ok(())
    }

    /// Decrypts a file to dotenv text, in memory.
    pub fn decrypt(&self, key: &KeyEnv, file: &Path) -> Result<String> {
        let out = self.output(
            key,
            &["decrypt", "--output-type", "dotenv", &path_str(file)?],
            None,
        )?;
        String::from_utf8(out).map_err(|_| "sops returned text that is not UTF-8".to_string())
    }

    /// Encrypts dotenv text for the creation rule of `file`, returning the encrypted text.
    pub fn encrypt(&self, key: &KeyEnv, file: &Path, plain: &[u8]) -> Result<Vec<u8>> {
        let args = [
            "encrypt",
            "--filename-override",
            &path_str(file)?,
            "--input-type",
            "dotenv",
            "--output-type",
            "dotenv",
            "/dev/stdin",
        ];
        self.output(key, &args, Some(plain))
    }
}

impl Drop for Sops {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.empty_home);
    }
}

pub fn path_str(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| format!("{} is not UTF-8", path.display()))
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| {
            use std::os::unix::fs::PermissionsExt;
            p.metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

fn platform() -> Result<&'static str> {
    let os = match std::env::consts::OS {
        "linux" => "linux",
        "macos" => "darwin",
        other => {
            return fail(format!(
                "sops is not installed and there is no release for {other}"
            ));
        }
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => {
            return fail(format!(
                "sops is not installed and there is no release for {other}"
            ));
        }
    };
    Ok(match (os, arch) {
        ("linux", "amd64") => "linux.amd64",
        ("linux", "arm64") => "linux.arm64",
        ("darwin", "amd64") => "darwin.amd64",
        _ => "darwin.arm64",
    })
}

/// Downloads the pinned sops release into `<root>/.cache/` and checks its SHA-256.
fn download(root: &Path) -> Result<PathBuf> {
    let platform = platform()?;
    let sum = SOPS_SUMS
        .iter()
        .find(|(p, _)| *p == platform)
        .map(|(_, s)| *s)
        .expect("pinned");
    let dir = root.join(".cache");
    let bin = dir.join(format!("sops-{SOPS_VERSION}"));
    if bin.is_file() {
        return Ok(bin);
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let url = format!(
        "https://github.com/getsops/sops/releases/download/v{SOPS_VERSION}/sops-v{SOPS_VERSION}.{platform}"
    );
    eprintln!("keyko: sops is not installed; downloading sops {SOPS_VERSION}");
    let mut response = crate::http::agent()
        .get(&url)
        .call()
        .map_err(|e| format!("cannot download sops: {e}"))?;
    if response.status() != 200 {
        return fail(format!("cannot download sops: HTTP {}", response.status()));
    }
    let body = response
        .body_mut()
        .with_config()
        .limit(200 * 1024 * 1024)
        .read_to_vec()
        .map_err(|e| format!("cannot download sops: {e}"))?;
    if hex::encode(Sha256::digest(&body)) != sum {
        return fail("the downloaded sops does not match its pinned checksum");
    }
    let tmp = dir.join(format!("sops-{SOPS_VERSION}.download"));
    std::fs::write(&tmp, &body).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
        .and_then(|_| std::fs::rename(&tmp, &bin))
        .map_err(|e| format!("cannot install {}: {e}", bin.display()))?;
    Ok(bin)
}
