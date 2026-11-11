//! Dotenv text as sops reads and writes it: `NAME=value` lines, sops metadata in `sops_*` keys.

use std::collections::BTreeMap;

/// Whether `name` is a valid variable name.
pub fn valid_var(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `NAME=value` lines, in order, metadata included.
pub fn pairs(text: &str) -> Vec<(&str, &str)> {
    text.lines()
        .filter_map(|line| line.split_once('='))
        .filter(|(name, _)| valid_var(name))
        .collect()
}

/// Key names, without sops metadata. Reads plaintext names only, so it needs no key.
pub fn names(text: &str) -> Vec<String> {
    pairs(text)
        .into_iter()
        .map(|(n, _)| n)
        .filter(|n| !n.starts_with("sops_"))
        .map(str::to_string)
        .collect()
}

/// The age recipients an encrypted file lists.
pub fn recipients(text: &str) -> Vec<String> {
    pairs(text)
        .into_iter()
        .filter(|(n, _)| {
            n.strip_prefix("sops_age__list_")
                .and_then(|r| r.strip_suffix("__map_recipient"))
                .is_some_and(|i| !i.is_empty() && i.bytes().all(|b| b.is_ascii_digit()))
        })
        .map(|(_, v)| v.to_string())
        .collect()
}

/// `added NAME`, `removed NAME` and `changed NAME` from `have` to `want`, sorted by name. Never
/// includes a value.
pub fn changes(want: &str, have: &str) -> Vec<String> {
    let want: BTreeMap<&str, &str> = pairs(want).into_iter().collect();
    let have: BTreeMap<&str, &str> = pairs(have).into_iter().collect();
    let mut names: Vec<&str> = want.keys().chain(have.keys()).copied().collect();
    names.sort();
    names.dedup();
    names
        .into_iter()
        .filter_map(|n| match (want.get(n), have.get(n)) {
            (Some(_), None) => Some(format!("added {n}")),
            (None, Some(_)) => Some(format!("removed {n}")),
            (Some(a), Some(b)) if a != b => Some(format!("changed {n}")),
            _ => None,
        })
        .collect()
}

/// The same text with only names left: `NAME=`.
pub fn blank_values(text: &str) -> String {
    pairs(text)
        .into_iter()
        .map(|(n, _)| format!("{n}=\n"))
        .collect()
}

/// Sets `name` in an env text, replacing its line or appending one.
pub fn set_var(text: &str, name: &str, value: &str) -> String {
    let mut out = Vec::new();
    let mut done = false;
    for line in text.lines() {
        if line.split_once('=').is_some_and(|(n, _)| n == name) {
            if !done {
                out.push(format!("{name}={value}"));
                done = true;
            }
        } else {
            out.push(line.to_string());
        }
    }
    if !done {
        out.push(format!("{name}={value}"));
    }
    out.join("\n")
}

/// The value of `name` in an env text.
pub fn get_var<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    pairs(text)
        .into_iter()
        .find(|(n, _)| *n == name)
        .map(|(_, v)| v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_skip_metadata_and_junk() {
        let text = "A=1\n# c\n\nsops_mac=x\nB_2=ENC[...]\n1X=bad\nC=a=b\n";
        assert_eq!(names(text), ["A", "B_2", "C"]);
        assert_eq!(get_var(text, "C"), Some("a=b"));
    }

    #[test]
    fn reads_recipients() {
        let text = "A=1\nsops_age__list_0__map_recipient=age1a\nsops_age__list_1__map_recipient=age1b\nsops_age__list_0__map_enc=x\n";
        assert_eq!(recipients(text), ["age1a", "age1b"]);
    }

    #[test]
    fn changes_by_name_only() {
        let want = "DATABASE_URL=postgres://z\nTOKEN=new\nEXTRA=merged";
        let have = "DATABASE_URL=postgres://z\nOLD=1\n\nTOKEN=stale";
        assert_eq!(
            changes(want, have),
            ["added EXTRA", "removed OLD", "changed TOKEN"]
        );
        assert!(changes(want, want).is_empty());
        assert_eq!(
            changes(&blank_values(want), &blank_values(have)),
            ["added EXTRA", "removed OLD"]
        );
    }

    #[test]
    fn sets_vars() {
        assert_eq!(set_var("A=1\nB=2", "B", "3"), "A=1\nB=3");
        assert_eq!(set_var("A=1", "B", "3"), "A=1\nB=3");
        assert_eq!(set_var("", "B", "3"), "B=3");
    }
}
