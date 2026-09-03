// SPDX-License-Identifier: AGPL-3.0-or-later
//! Records the git commit the bindings are compiled from so applications can
//! show which SDK build they embed. `MB_SDK_GIT_COMMIT` overrides discovery for
//! source archives and containers built without a repository.
use std::path::Path;
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    Some(text.trim().to_owned())
}

fn main() {
    println!("cargo:rerun-if-env-changed=MB_SDK_GIT_COMMIT");
    println!("cargo:rerun-if-env-changed=MB_SDK_GIT_DIRTY");
    let commit = std::env::var("MB_SDK_GIT_COMMIT")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .or_else(|| git(&["rev-parse", "HEAD"]))
        .unwrap_or_else(|| "unknown".to_owned());
    let dirty = match std::env::var("MB_SDK_GIT_DIRTY") {
        Ok(value) => value == "1" || value.eq_ignore_ascii_case("true"),
        Err(_) => git(&["status", "--porcelain", "--untracked-files=no"])
            .is_some_and(|status| !status.is_empty()),
    };
    if let Some(git_dir) = git(&["rev-parse", "--git-dir"]) {
        for name in ["HEAD", "index"] {
            let path = Path::new(&git_dir).join(name);
            if path.exists() {
                println!("cargo:rerun-if-changed={}", path.display());
            }
        }
    }
    println!("cargo:rustc-env=MB_SDK_GIT_COMMIT={commit}");
    println!(
        "cargo:rustc-env=MB_SDK_GIT_DIRTY={}",
        if dirty { "1" } else { "0" }
    );
}
