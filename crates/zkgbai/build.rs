//! Restrict the Windows DLL's export table to the engine entry points.
//! (MinGW-style linkers otherwise export every global symbol.)
//!
//! Also stamp the build with `git describe` (`ZKGBAI_BUILD`), so a run's
//! journal tells which code played.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=exports.def");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if os == "windows" && env == "gnu" {
        let def = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("exports.def");
        println!("cargo:rustc-cdylib-link-arg={}", def.display());
    }
    stamp();
}

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok().filter(|o| o.status.success())?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (!s.is_empty()).then_some(s)
}

fn stamp() {
    let build = git(&["describe", "--always", "--dirty", "--abbrev=8"]).unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=ZKGBAI_BUILD={build}");
    // Again on source changes (the dirty mark) and on commits. Only paths
    // that exist: a missing one would rerun this on every build.
    println!("cargo:rerun-if-changed=src");
    let mut watch = vec!["HEAD".to_owned(), "index".to_owned()];
    watch.extend(git(&["symbolic-ref", "-q", "HEAD"]));
    for p in watch {
        if let Some(path) = git(&["rev-parse", "--git-path", &p]).filter(|p| std::path::Path::new(p).exists()) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
}
