//! Pieces the tierkey CLI and key server share: names, the HTTP API, globs and age keys.

use std::collections::BTreeMap;

use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};

/// The app capability the server reads from `Tailscale-App-Capabilities` unless configured.
pub const DEFAULT_CAPABILITY: &str = "github.com/pokedotdev/tierkey/cap/tierkey";

/// The level Claude Code and other agents are limited to.
pub const AGENT_LEVEL: &str = "dev";

/// Project and level names: lowercase letters and digits in dash-separated words.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.split('-').all(|word| {
            !word.is_empty()
                && word
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

/// The server's environment variable that holds a level's private key:
/// `TIERKEY_KEY__<PROJECT>__<LEVEL>`, uppercase, dashes as underscores. Valid names never hold
/// two dashes in a row, so `__` always separates the parts.
pub fn key_env_var(project: &str, level: &str) -> String {
    let part = |s: &str| s.to_ascii_uppercase().replace('-', "_");
    format!("TIERKEY_KEY__{}__{}", part(project), part(level))
}

/// Reverses [`key_env_var`].
pub fn parse_key_env_var(var: &str) -> Option<(String, String)> {
    let rest = var.strip_prefix("TIERKEY_KEY__")?;
    let (project, level) = rest.split_once("__")?;
    let part = |s: &str| s.to_ascii_lowercase().replace('_', "-");
    let (project, level) = (part(project), part(level));
    (valid_name(&project) && valid_name(&level)).then_some((project, level))
}

/// `GET /v1/keys/<project>?levels=a,b`: the requested level keys the caller may have.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KeysResponse {
    pub project: String,
    pub login: String,
    /// Level name to its age private key (`AGE-SECRET-KEY-1...`).
    pub keys: BTreeMap<String, String>,
}

/// `GET /v1/access/<project>`: which levels the caller may fetch, without any key.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccessResponse {
    pub project: String,
    pub login: String,
    pub levels: Vec<String>,
}

/// An error body: `{"error": "..."}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: String,
}

/// Shell-style glob, as bash's `[[ $text == $pattern ]]`: `*` matches any run of characters,
/// slashes included, `?` one character, `[a-z]` / `[!a-z]` a class, `\x` a literal `x`.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    glob_at(&p, &t)
}

fn glob_at(p: &[char], t: &[char]) -> bool {
    let (mut pi, mut ti) = (0, 0);
    // Where to resume after the last `*`: (pattern index after it, text index it matched up to).
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() {
            match p[pi] {
                '*' => {
                    star = Some((pi + 1, ti));
                    pi += 1;
                    continue;
                }
                '?' => {
                    pi += 1;
                    ti += 1;
                    continue;
                }
                '[' => {
                    if let Some((matched, len)) = class(&p[pi..], t[ti])
                        && matched
                    {
                        pi += len;
                        ti += 1;
                        continue;
                    } else if class(&p[pi..], t[ti]).is_none() && t[ti] == '[' {
                        pi += 1;
                        ti += 1;
                        continue;
                    }
                }
                '\\' if pi + 1 < p.len() => {
                    if p[pi + 1] == t[ti] {
                        pi += 2;
                        ti += 1;
                        continue;
                    }
                }
                c if c == t[ti] => {
                    pi += 1;
                    ti += 1;
                    continue;
                }
                _ => {}
            }
        }
        match star {
            Some((sp, st)) => {
                pi = sp;
                ti = st + 1;
                star = Some((sp, st + 1));
            }
            None => return false,
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

/// Matches `c` against the class at the start of `p` (which begins with `[`). Returns whether it
/// matched and the class's length, or None when `p` holds no closed class.
fn class(p: &[char], c: char) -> Option<(bool, usize)> {
    let mut i = 1;
    let negate = matches!(p.get(i), Some('!' | '^'));
    if negate {
        i += 1;
    }
    let mut matched = false;
    let mut first = true;
    while i < p.len() {
        if p[i] == ']' && !first {
            return Some((matched != negate, i + 1));
        }
        first = false;
        let lo = p[i];
        if i + 2 < p.len() && p[i + 1] == '-' && p[i + 2] != ']' {
            if lo <= c && c <= p[i + 2] {
                matched = true;
            }
            i += 3;
        } else {
            if lo == c {
                matched = true;
            }
            i += 1;
        }
    }
    None
}

/// The same glob as an anchored regex, for `path_regex` in a generated `.sops.yaml`.
pub fn glob_to_regex(pattern: &str) -> String {
    let p: Vec<char> = pattern.chars().collect();
    let mut out = String::from("^");
    let mut i = 0;
    while i < p.len() {
        match p[i] {
            '*' => out.push_str(".*"),
            '?' => out.push('.'),
            '[' => match class(&p[i..], '\0') {
                Some((_, len)) => {
                    let inner: String = p[i + 1..i + len - 1].iter().collect();
                    let inner = match inner.strip_prefix('!') {
                        Some(rest) => format!("^{rest}"),
                        None => inner,
                    };
                    out.push('[');
                    out.push_str(&inner.replace('\\', "\\\\"));
                    out.push(']');
                    i += len;
                    continue;
                }
                None => out.push_str("\\["),
            },
            '\\' if i + 1 < p.len() => {
                i += 1;
                push_literal(&mut out, p[i]);
            }
            c => push_literal(&mut out, c),
        }
        i += 1;
    }
    out.push('$');
    out
}

fn push_literal(out: &mut String, c: char) {
    if ".+()|{}^$[]\\".contains(c) {
        out.push('\\');
    }
    out.push(c);
}

/// A new age X25519 key pair: (private `AGE-SECRET-KEY-1...`, public `age1...`).
pub fn generate_key() -> (String, String) {
    let identity = age::x25519::Identity::generate();
    let public = identity.to_public().to_string();
    (identity.to_string().expose_secret().to_string(), public)
}

/// The public key of an age private key.
pub fn public_key_of(private: &str) -> Option<String> {
    private
        .trim()
        .parse::<age::x25519::Identity>()
        .ok()
        .map(|i| i.to_public().to_string())
}

/// Whether a string is an age X25519 public key.
pub fn is_public_key(key: &str) -> bool {
    key.parse::<age::x25519::Recipient>().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        for ok in ["dev", "myapp", "pages-preview", "a1"] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in ["", "Dev", "a--b", "-a", "a-", "a_b", "a/b", "a.b"] {
            assert!(!valid_name(bad), "{bad}");
        }
    }

    #[test]
    fn env_vars_round_trip() {
        assert_eq!(
            key_env_var("mine-utils", "dev"),
            "TIERKEY_KEY__MINE_UTILS__DEV"
        );
        assert_eq!(
            parse_key_env_var("TIERKEY_KEY__MINE_UTILS__DEV"),
            Some(("mine-utils".into(), "dev".into()))
        );
        assert_eq!(parse_key_env_var("TIERKEY_KEY__X"), None);
        assert_eq!(parse_key_env_var("OTHER"), None);
    }

    #[test]
    fn globs_behave_like_bash() {
        let cases = [
            ("main", "main", true),
            ("main", "mainx", false),
            ("bs/*", "bs/boss-fight", true),
            ("bs/*", "bs/a/b", true),
            ("bs/*", "bs", false),
            ("release-*", "release-1.2", true),
            ("*", "", true),
            ("*", "anything/at/all", true),
            ("a?c", "abc", true),
            ("a?c", "ac", false),
            ("[ab]x", "bx", true),
            ("[!ab]x", "bx", false),
            ("[a-c]*", "cat", true),
            ("*.env", "production/api.env", true),
            ("production/*.env", "production/api.env", true),
            ("production/*.env", "beta/pages.env", false),
            ("\\*", "*", true),
            ("\\*", "a", false),
            ("*a*b", "xxaxxb", true),
            ("*a*b", "xxaxxbc", false),
        ];
        for (pattern, text, want) in cases {
            assert_eq!(glob_match(pattern, text), want, "{pattern} vs {text}");
        }
    }

    #[test]
    fn globs_become_regexes() {
        assert_eq!(glob_to_regex("dev.env"), "^dev\\.env$");
        assert_eq!(glob_to_regex("production/*.env"), "^production/.*\\.env$");
        assert_eq!(glob_to_regex("[!a]?"), "^[^a].$");
    }

    #[test]
    fn keys() {
        let (private, public) = generate_key();
        assert!(private.starts_with("AGE-SECRET-KEY-1"));
        assert!(is_public_key(&public));
        assert_eq!(public_key_of(&private).as_deref(), Some(public.as_str()));
        assert_eq!(public_key_of("nope"), None);
    }
}
