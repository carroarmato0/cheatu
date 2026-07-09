//! Embed the build version: the git tag when HEAD sits exactly on one,
//! otherwise the short commit hash, otherwise (tarball builds without .git)
//! the crate version.

use std::process::Command;

fn main() {
    let version = git(&["describe", "--tags", "--exact-match", "HEAD"])
        .or_else(|| git(&["rev-parse", "--short", "HEAD"]))
        .unwrap_or_else(|| format!("v{}", std::env::var("CARGO_PKG_VERSION").unwrap()));
    println!("cargo:rustc-env=CHEATU_VERSION={version}");

    // Re-embed when HEAD moves (new commit, checkout, or new tag).
    for p in [
        "../../.git/HEAD",
        "../../.git/refs",
        "../../.git/packed-refs",
    ] {
        if std::path::Path::new(p).exists() {
            println!("cargo:rerun-if-changed={p}");
        }
    }
}

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!s.is_empty()).then_some(s)
}
