//! Build number = number of git commits (always grows, same locally and in CI).
//! Override with BUILD_NUMBER=…; falls back to 0 outside a git checkout.
use std::process::Command;

fn main() {
    let n = std::env::var("BUILD_NUMBER").ok().filter(|s| !s.is_empty()).or_else(|| {
        let out = Command::new("git").args(["rev-list", "--count", "HEAD"]).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    });
    println!("cargo:rustc-env=BUILD_NUMBER={}", n.unwrap_or_else(|| "0".into()));
    println!("cargo:rerun-if-env-changed=BUILD_NUMBER");
    // Rebuild when a commit is made (HEAD moves or the branch ref changes).
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/heads");
    println!("cargo:rerun-if-changed=.git/packed-refs");
}
