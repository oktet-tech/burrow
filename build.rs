//! Stamps the source revision into the binary as `BURROW_REVISION`: the tag
//! when HEAD is exactly on one, otherwise the short commit SHA, with
//! "-dirty" for uncommitted changes. Falls back to the crate version when
//! built outside a git checkout (e.g. from a source tarball).

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn main() {
    let revision = git(&["describe", "--tags", "--exact-match", "HEAD"])
        .or_else(|| git(&["rev-parse", "--short", "HEAD"]))
        .map(|rev| {
            let dirty = git(&["status", "--porcelain", "--untracked-files=no"]).is_some();
            if dirty { format!("{rev}-dirty") } else { rev }
        })
        .unwrap_or_else(|| format!("v{}", env!("CARGO_PKG_VERSION")));
    println!("cargo:rustc-env=BURROW_REVISION={revision}");

    // Re-stamp when HEAD moves, a branch or tag changes, the index does, or
    // sources are edited (dirty flag). Git paths come from git so this also
    // works in worktrees.
    println!("cargo:rerun-if-changed=src");
    if let Some(git_dir) = git(&["rev-parse", "--git-dir"]) {
        for path in ["HEAD", "index"] {
            println!("cargo:rerun-if-changed={git_dir}/{path}");
        }
    }
    if let Some(common_dir) = git(&["rev-parse", "--git-common-dir"]) {
        for path in ["refs", "packed-refs"] {
            println!("cargo:rerun-if-changed={common_dir}/{path}");
        }
    }
}
