//! The rules this repository is held to (Tawara-wallet's docs/DECISIONS.md
//! D33), checked on the manifests and the lockfile as they are committed.
//!
//! The application comes from Tawara-wallet, pinned by full commit hash, and
//! is never patched, replaced or overridden here; iced is one copy, from the
//! owner's fork at one commit; the release profile that keeps key material
//! scrubbable is restated; and the mobile crate reaches the wallet only
//! through `tawara-app`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const TAWARA_GIT: &str = "https://github.com/patricksmithlaravel/Tawara-wallet";
const ICED_GIT: &str = "https://github.com/patricksmithlaravel/iced_mobile";

/// Every dependency the mobile crate may name: the application and the
/// platform glue. Anything else, the library and `tawara-wallet-core` above
/// all, is reached through `tawara-app` or not at all.
const MOBILE_DEPENDENCIES: [&str; 9] = [
    "tawara-app",
    "iced_winit",
    "winit",
    "jni",
    "ndk-context",
    "objc2",
    "block2",
    "objc2-foundation",
    "objc2-ui-kit",
];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the workspace root is two levels above crates/mobile")
}

fn read_toml(path: &Path) -> toml::Table {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    text.parse()
        .unwrap_or_else(|e| panic!("{} is not TOML: {e}", path.display()))
}

fn is_full_hash(rev: &str) -> bool {
    rev.len() == 40
        && rev
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn workspace_deps(manifest: &toml::Table) -> &toml::Table {
    manifest["workspace"]["dependencies"]
        .as_table()
        .expect("[workspace.dependencies] exists")
}

/// The line `key` in [workspace.dependencies]: a git source pinned by a full
/// hash and chosen by nothing else. Returns the hash.
fn pinned(manifest: &toml::Table, key: &str, git: &str) -> String {
    let dep = workspace_deps(manifest)
        .get(key)
        .and_then(|d| d.as_table())
        .unwrap_or_else(|| panic!("[workspace.dependencies] must name `{key}` as a table"));
    assert_eq!(
        dep.get("git").and_then(|v| v.as_str()),
        Some(git),
        "`{key}` must come from {git}"
    );
    let rev = dep.get("rev").and_then(|v| v.as_str()).unwrap_or("");
    assert!(
        is_full_hash(rev),
        "`{key}` must be pinned by a full 40-character lowercase commit hash, not {rev:?}"
    );
    let allowed = BTreeSet::from(["git", "rev", "default-features", "features"]);
    for k in dep.keys() {
        assert!(
            allowed.contains(k.as_str()),
            "`{key}` is chosen by its commit alone; it also names `{k}`"
        );
    }
    rev.to_owned()
}

/// Every package in Cargo.lock named `name` or starting with `prefix`, with
/// its source.
fn locked(lock: &toml::Table, pick: impl Fn(&str) -> bool) -> Vec<(&str, &str)> {
    lock["package"]
        .as_array()
        .expect("Cargo.lock lists packages")
        .iter()
        .filter_map(|p| p.as_table())
        .filter_map(|p| {
            let name = p.get("name")?.as_str()?;
            pick(name).then(|| (name, p.get("source").and_then(|v| v.as_str()).unwrap_or("")))
        })
        .collect()
}

#[test]
fn nothing_is_patched_replaced_or_overridden() {
    let manifest = read_toml(&root().join("Cargo.toml"));
    for section in ["patch", "replace"] {
        assert!(
            !manifest.contains_key(section),
            "Cargo.toml has a [{section}] section"
        );
    }
    for (key, dep) in workspace_deps(&manifest) {
        assert!(
            dep.get("path").is_none(),
            "`{key}` is a path dependency; everything comes from a registry or a pinned commit"
        );
    }
    assert!(
        !root().join(".cargo").exists(),
        "a .cargo directory could override sources or patch crates"
    );
}

#[test]
fn the_application_comes_from_tawara_wallet_by_full_hash() {
    let manifest = read_toml(&root().join("Cargo.toml"));
    let rev = pinned(&manifest, "tawara-app", TAWARA_GIT);
    let lock = read_toml(&root().join("Cargo.lock"));
    let tawara = locked(&lock, |n| n.starts_with("tawara-") && n != "tawara-mobile");
    assert!(
        tawara.iter().any(|(n, _)| *n == "tawara-app")
            && tawara.iter().any(|(n, _)| *n == "tawara-wallet-core"),
        "Cargo.lock holds tawara-app and tawara-wallet-core: {tawara:?}"
    );
    let expected = format!("git+{TAWARA_GIT}?rev={rev}#{rev}");
    for (name, source) in tawara {
        assert_eq!(
            source, expected,
            "Cargo.lock resolved {name} from somewhere other than the pinned commit"
        );
    }
}

#[test]
fn iced_is_one_copy_from_the_fork() {
    let manifest = read_toml(&root().join("Cargo.toml"));
    let rev = pinned(&manifest, "iced_winit", ICED_GIT);
    let lock = read_toml(&root().join("Cargo.lock"));
    let iced = locked(&lock, |n| n == "iced" || n.starts_with("iced_"));
    assert!(
        iced.iter().any(|(n, _)| *n == "iced"),
        "Cargo.lock holds no iced; the walk is over nothing"
    );
    let expected = format!("git+{ICED_GIT}?rev={rev}#{rev}");
    let mut names = BTreeSet::new();
    for (name, source) in iced {
        assert!(names.insert(name), "Cargo.lock holds two copies of {name}");
        assert_eq!(
            source, expected,
            "Cargo.lock resolved {name} from somewhere other than this repository's iced_winit \
             commit; it must be the commit Tawara-wallet pins for iced"
        );
    }
}

#[test]
fn the_release_profile_keeps_key_material_scrubbable() {
    let manifest = read_toml(&root().join("Cargo.toml"));
    let release = manifest
        .get("profile")
        .and_then(|p| p.get("release"))
        .and_then(|r| r.as_table())
        .expect("[profile.release] is restated");
    assert_eq!(
        release.get("panic").and_then(|v| v.as_str()),
        Some("unwind")
    );
    assert_eq!(
        release.get("overflow-checks").and_then(|v| v.as_bool()),
        Some(true)
    );
}

#[test]
fn the_mobile_crate_reaches_the_wallet_only_through_the_app() {
    let manifest = read_toml(&root().join("crates/mobile/Cargo.toml"));
    let mut tables: Vec<(String, &toml::Table)> = Vec::new();
    if let Some(t) = manifest.get("dependencies").and_then(|d| d.as_table()) {
        tables.push(("dependencies".into(), t));
    }
    if let Some(targets) = manifest.get("target").and_then(|t| t.as_table()) {
        for (cfg, t) in targets {
            if let Some(d) = t.get("dependencies").and_then(|d| d.as_table()) {
                tables.push((format!("target.{cfg}.dependencies"), d));
            }
        }
    }
    assert!(
        !tables.is_empty(),
        "the mobile crate names no dependency; a check over nothing passes"
    );
    for (section, deps) in tables {
        for key in deps.keys() {
            assert!(
                MOBILE_DEPENDENCIES.contains(&key.as_str()),
                "crates/mobile names `{key}` in [{section}]; it reaches the wallet only through \
                 tawara-app"
            );
        }
    }
}
