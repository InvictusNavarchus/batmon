use std::env;
use std::path::Path;
use std::process::Command;

fn main() {
    // This path is deliberately never created: recheck Git on every build,
    // including commits, worktree switches and edits outside Rust source files.
    // Watching only HEAD/index would miss unstaged changes and their removal.
    let out_dir = env::var("OUT_DIR").expect("Cargo provides OUT_DIR");
    println!("cargo::rerun-if-changed={out_dir}/git-state");

    let root = env::var("CARGO_MANIFEST_DIR").expect("Cargo provides CARGO_MANIFEST_DIR");
    let version = env::var("CARGO_PKG_VERSION").expect("Cargo provides CARGO_PKG_VERSION");
    let identifier = git_identifier(Path::new(&root)).unwrap_or_else(|| "unknown".into());
    println!("cargo::rustc-env=BATMON_BUILD_VERSION={version} ({identifier})");
}

fn git_identifier(root: &Path) -> Option<String> {
    // An archive extracted inside another repository must not inherit its hash.
    // exists() also accepts the .git file used by linked worktrees.
    if !root.join(".git").exists() {
        return None;
    }
    let commit = git(root, &["rev-parse", "--short=7", "HEAD"])?;
    let status = git(root, &["status", "--porcelain", "--untracked-files=no"])?;
    if status.is_empty() {
        Some(commit)
    } else {
        Some(format!("{commit}, dirty"))
    }
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        // Status must not refresh the index or acquire locks during a build.
        .arg("--no-optional-locks")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8(output.stdout).ok()?.trim().to_owned())
}
