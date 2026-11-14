//! Exercise Cargo's rebuild detection with disposable, dependency-free packages.

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

fn run(command: &mut Command) -> String {
    // Git hooks export repository context (notably GIT_INDEX_FILE). Fixture
    // commands and their build scripts must use only the disposable repository.
    for (name, _) in std::env::vars_os() {
        if name.to_str().is_some_and(|name| name.starts_with("GIT_")) {
            command.env_remove(name);
        }
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{command:?}\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn git(root: &Path, args: &[&str]) -> String {
    run(Command::new("git").arg("-C").arg(root).args(args))
}

fn commit(root: &Path, paths: &[&str]) {
    run(Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["add", "--"])
        .args(paths));
    git(
        root,
        &[
            "-c",
            "user.name=Build Test",
            "-c",
            "user.email=build@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
            "commit",
            "-qm",
            "Fixture",
        ],
    );
}

fn package(root: &Path) {
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"version-fixture\"\nversion = \"1.2.3\"\nedition = \"2024\"\n",
    )
    .unwrap();
    std::fs::write(root.join("build.rs"), include_str!("../build.rs")).unwrap();
    std::fs::write(
        root.join("src/main.rs"),
        "fn main() { println!(\"{}\", env!(\"BATMON_BUILD_VERSION\")); }\n",
    )
    .unwrap();
    std::fs::write(root.join(".gitignore"), "/target\n").unwrap();
}

fn build(root: &Path) -> String {
    run(Command::new(env!("CARGO"))
        .current_dir(root)
        .env("CARGO_TARGET_DIR", root.join("target"))
        // This fixture is executed locally at the host's default output path.
        .env_remove("CARGO_BUILD_TARGET")
        .args(["build", "--offline", "--quiet"]));
    run(&mut Command::new(root.join("target/debug/version-fixture")))
}

#[test]
fn fixture_builds_ignore_inherited_cargo_targets() {
    let rustc = run(Command::new("rustc").arg("-vV"));
    let host = rustc
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .unwrap();
    for target in [host, "batmon-fixture-invalid-target"] {
        // Invoke a single existing fixture test in a child process so the
        // target setting is inherited without changing global environment.
        let output = run(Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "source_without_git_metadata_does_not_inherit_a_parent_repository",
            ])
            .env("CARGO_BUILD_TARGET", target));
        assert!(output.contains("1 passed"), "{output}");
    }
}

fn build_script(root: &Path) -> Command {
    let script = root.join("build-script");
    run(Command::new("rustc")
        .arg("--edition=2024")
        .arg(root.join("build.rs"))
        .arg("-o")
        .arg(&script));
    let mut command = Command::new(script);
    command
        .env("OUT_DIR", root)
        .env("CARGO_MANIFEST_DIR", root)
        .env("CARGO_PKG_VERSION", "1.2.3");
    command
}

#[test]
fn rebuilds_follow_commits_and_tracked_changes() {
    let fixture = TempDir::new().unwrap();
    let root = fixture.path();
    package(root);
    // Generate Cargo.lock before committing so a normal build starts clean.
    assert_eq!(build(root), "1.2.3 (unknown)");
    git(root, &["init", "-q"]);
    commit(
        root,
        &[
            "Cargo.toml",
            "Cargo.lock",
            "build.rs",
            "src/main.rs",
            ".gitignore",
        ],
    );
    let first = git(root, &["rev-parse", "--short=7", "HEAD"]);
    assert_eq!(build(root), format!("1.2.3 ({first})"));

    std::fs::write(root.join("untracked.txt"), "ignored by dirty detection\n").unwrap();
    assert_eq!(build(root), format!("1.2.3 ({first})"));
    std::fs::remove_file(root.join("untracked.txt")).unwrap();

    // A non-Rust edit must update the version, including a cached clean build.
    let ignore = std::fs::read_to_string(root.join(".gitignore")).unwrap();
    std::fs::write(root.join(".gitignore"), format!("{ignore}# edited\n")).unwrap();
    assert_eq!(build(root), format!("1.2.3 ({first}, dirty)"));
    std::fs::write(root.join(".gitignore"), &ignore).unwrap();
    assert_eq!(build(root), format!("1.2.3 ({first})"));

    std::fs::write(root.join("tracked.txt"), "staged addition\n").unwrap();
    git(root, &["add", "--", "tracked.txt"]);
    assert_eq!(build(root), format!("1.2.3 ({first}, dirty)"));
    commit(root, &["tracked.txt"]);
    let second = git(root, &["rev-parse", "--short=7", "HEAD"]);
    assert_ne!(first, second);
    assert_eq!(build(root), format!("1.2.3 ({second})"));

    git(root, &["checkout", "-q", "--detach", &first]);
    assert_eq!(build(root), format!("1.2.3 ({first})"));
}

#[test]
fn linked_worktree_uses_its_own_commit_and_status() {
    let fixture = TempDir::new().unwrap();
    let root = fixture.path().join("source");
    package(&root);
    git(&root, &["init", "-q"]);
    commit(
        &root,
        &["Cargo.toml", "build.rs", "src/main.rs", ".gitignore"],
    );
    let worktree = fixture.path().join("worktree");
    git(
        &root,
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            worktree.to_str().unwrap(),
        ],
    );
    // The lockfile is untracked, so it does not make the fixture dirty.
    let hash = git(&worktree, &["rev-parse", "--short=7", "HEAD"]);
    assert_eq!(build(&worktree), format!("1.2.3 ({hash})"));
    std::fs::write(worktree.join(".gitignore"), "/target\n# changed\n").unwrap();
    assert_eq!(build(&worktree), format!("1.2.3 ({hash}, dirty)"));
}

#[test]
fn source_without_git_metadata_does_not_inherit_a_parent_repository() {
    let fixture = TempDir::new().unwrap();
    git(fixture.path(), &["init", "-q"]);
    std::fs::write(fixture.path().join("parent.txt"), "unrelated repository\n").unwrap();
    commit(fixture.path(), &["parent.txt"]);
    let archive = fixture.path().join("archive");
    package(&archive);
    assert_eq!(build(&archive), "1.2.3 (unknown)");
}

#[test]
fn missing_git_executable_falls_back_without_failing_the_build_script() {
    let fixture = TempDir::new().unwrap();
    let root = fixture.path();
    package(root);
    git(root, &["init", "-q"]);
    commit(
        root,
        &["Cargo.toml", "build.rs", "src/main.rs", ".gitignore"],
    );
    let output = run(build_script(root).env("PATH", ""));
    assert!(output.contains("cargo::rustc-env=BATMON_BUILD_VERSION=1.2.3 (unknown)"));
}

#[test]
fn foreign_git_context_cannot_change_the_builds_commit_or_dirty_status() {
    let fixture = TempDir::new().unwrap();
    let root = fixture.path().join("source");
    let foreign = fixture.path().join("foreign");
    for directory in [&root, &foreign] {
        package(directory);
        git(directory, &["init", "-q"]);
    }
    std::fs::write(
        foreign.join(".gitignore"),
        "/target\n# foreign repository\n",
    )
    .unwrap();
    for directory in [&root, &foreign] {
        commit(
            directory,
            &["Cargo.toml", "build.rs", "src/main.rs", ".gitignore"],
        );
    }
    let hash = git(&root, &["rev-parse", "--short=7", "HEAD"]);
    assert_ne!(hash, git(&foreign, &["rev-parse", "--short=7", "HEAD"]));
    std::fs::write(
        foreign.join(".gitignore"),
        "/target\n# dirty foreign repository\n",
    )
    .unwrap();

    let foreign_git = foreign.join(".git");
    let mut command = build_script(&root);
    for (variable, path) in [
        ("GIT_DIR", foreign_git.clone()),
        ("GIT_WORK_TREE", foreign.clone()),
        ("GIT_INDEX_FILE", foreign_git.join("index")),
        ("GIT_COMMON_DIR", foreign_git.clone()),
        ("GIT_OBJECT_DIRECTORY", foreign_git.join("objects")),
        (
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            foreign_git.join("objects"),
        ),
    ] {
        command.env(variable, path);
    }
    for dirty in [false, true] {
        if dirty {
            git(&foreign, &["restore", "--", ".gitignore"]);
            std::fs::write(root.join(".gitignore"), "/target\n# dirty source\n").unwrap();
        }
        // Bypass run(): it would remove the injected context under Git hooks.
        let output = command.output().unwrap();
        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        let suffix = if dirty { ", dirty" } else { "" };
        assert!(
            stdout.contains(&format!(
                "cargo::rustc-env=BATMON_BUILD_VERSION=1.2.3 ({hash}{suffix})"
            )),
            "{stdout}"
        );
    }
}
