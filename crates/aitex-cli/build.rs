use std::path::{Path, PathBuf};
use std::process::Command;

fn git_output(manifest_dir: &Path, arguments: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(manifest_dir)
        .args(arguments)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?;
    let value = value.trim();
    (!value.is_empty() && !value.chars().any(char::is_control)).then(|| value.to_string())
}

fn watch_git_path(manifest_dir: &Path, name: &str) {
    let Some(path) = git_output(manifest_dir, &["rev-parse", "--git-path", name]) else {
        return;
    };
    let path = PathBuf::from(path);
    let path = if path.is_absolute() {
        path
    } else {
        manifest_dir.join(path)
    };
    println!("cargo:rerun-if-changed={}", path.display());
}

fn main() {
    println!("cargo:rerun-if-env-changed=AITEX_BUILD_GIT_DESCRIBE");

    let manifest_dir = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo supplies CARGO_MANIFEST_DIR"),
    );
    watch_git_path(&manifest_dir, "HEAD");
    watch_git_path(&manifest_dir, "packed-refs");
    watch_git_path(&manifest_dir, "refs/tags");
    if let Some(reference) = git_output(&manifest_dir, &["symbolic-ref", "-q", "HEAD"]) {
        watch_git_path(&manifest_dir, &reference);
    }

    let description = std::env::var("AITEX_BUILD_GIT_DESCRIBE")
        .ok()
        .filter(|value| !value.is_empty() && !value.chars().any(char::is_control))
        .or_else(|| git_output(&manifest_dir, &["describe", "--tags", "--always"]));
    let package_tag = format!(
        "v{}",
        std::env::var("CARGO_PKG_VERSION").expect("Cargo supplies CARGO_PKG_VERSION")
    );
    if let Some(description) = description.filter(|value| value != &package_tag) {
        println!("cargo:rustc-env=AITEX_GIT_DESCRIBE={description}");
    }
}
