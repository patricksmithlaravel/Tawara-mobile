//! The rules this repository is held to (Tawara-wallet's docs/DECISIONS.md
//! D32 item 9 and D33), checked on the manifests and the lockfile as they are
//! committed, and on the features cargo resolves for the Android targets.
//!
//! The application comes from Tawara-wallet, pinned by full commit hash, and
//! is never patched, replaced or overridden here. Every package the owner's
//! fork ships is one copy, at one commit, and no crate name comes from two
//! sources. Android's activity backend is winit's NativeActivity and nothing
//! else. The release profile that keeps key material scrubbable is restated,
//! and the mobile crate reaches the wallet only through `tawara-app`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

const TAWARA_GIT: &str = "https://github.com/patricksmithlaravel/Tawara-wallet";
const ICED_GIT: &str = "https://github.com/patricksmithlaravel/iced_mobile";

/// The two Android targets the android job builds (platform/android/tawara.sh,
/// run by .github/workflows/mobile.yml).
const ANDROID_TARGETS: [&str; 2] = ["aarch64-linux-android", "x86_64-linux-android"];

/// Every dependency the mobile crate may name: the application and the
/// platform glue. Anything else, the library and `tawara-wallet-core` above
/// all, is reached through `tawara-app` or not at all. `winit` is not one of
/// them. It comes from the owner's fork through `iced_winit`: a crates.io line
/// of its own would be a second copy, and a direct line to the fork would be a
/// second line to keep in step with iced (D32 item 9 (b)).
const MOBILE_DEPENDENCIES: [&str; 8] = [
    "tawara-app",
    "iced_winit",
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

/// Every package in Cargo.lock whose name `pick` accepts, with its source
/// (empty for a package with none, which is a path package).
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

/// The features cargo resolves for `package` when it builds for `target`.
/// Cargo.lock records no features, so `cargo tree` is the one place they show.
/// `--locked` makes it refuse a lockfile that does not match the manifests,
/// rather than quietly rewriting the lockfile to match.
fn resolved_features(target: &str, package: &str) -> BTreeSet<String> {
    let output = Command::new(env!("CARGO"))
        .current_dir(root())
        .args([
            "tree",
            "--locked",
            "--color",
            "never",
            "--target",
            target,
            "--invert",
            package,
            "--depth",
            "0",
            "--prefix",
            "none",
            "--format",
            "{p} | {f}",
        ])
        .output()
        .unwrap_or_else(|e| panic!("cannot run cargo tree: {e}"));
    assert!(
        output.status.success(),
        "cargo tree cannot resolve {package} for {target}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "the build for {target} holds {} copies of {package}; it must hold one",
        lines.len()
    );
    let line = lines[0];
    let (_, features) = line
        .split_once(" | ")
        .unwrap_or_else(|| panic!("cargo tree printed `{line}`, not a package and its features"));
    features
        .split(',')
        .filter(|f| !f.is_empty())
        .map(str::to_owned)
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

/// Every package Cargo.lock takes from the owner's fork is at the commit the
/// workspace pins for `iced_winit`, and is the only package of its name in the
/// lock, so the build holds one copy of each (D32 item 9 (e)). The fork ships
/// iced and the packages under it, winit and dpi among them, so the walk is
/// over the fork's URL and not over names starting `iced`.
#[test]
fn iced_is_one_copy_from_the_fork() {
    let manifest = read_toml(&root().join("Cargo.toml"));
    let rev = pinned(&manifest, "iced_winit", ICED_GIT);
    let lock = read_toml(&root().join("Cargo.lock"));
    let packages = locked(&lock, |_| true);
    let fork = format!("git+{ICED_GIT}");
    let from_fork: Vec<(&str, &str)> = packages
        .iter()
        .copied()
        .filter(|(_, source)| source.starts_with(&fork))
        .collect();
    for name in ["iced", "winit"] {
        assert!(
            from_fork.iter().any(|(n, _)| *n == name),
            "Cargo.lock holds no {name} from the fork; a check over nothing passes"
        );
    }
    let expected = format!("git+{ICED_GIT}?rev={rev}#{rev}");
    for (name, source) in from_fork {
        assert_eq!(
            source, expected,
            "Cargo.lock resolved {name} from the fork at another commit than the one \
             iced_winit pins"
        );
        let copies = packages.iter().filter(|(n, _)| *n == name).count();
        assert_eq!(
            copies, 1,
            "Cargo.lock holds {copies} packages named {name}; the build holds one copy of each"
        );
    }
}

/// No crate name comes from two sources in Cargo.lock (D32 item 9 (e)). The
/// test above holds the fork's own packages; this one holds every other crate,
/// so a second copy of any crate, from crates.io or from another git commit,
/// fails. Two versions of one name from one source, as `bitflags` has, are not
/// a second source and pass.
#[test]
fn no_crate_name_comes_from_two_sources() {
    let lock = read_toml(&root().join("Cargo.lock"));
    let mut sources: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for (name, source) in locked(&lock, |_| true) {
        sources.entry(name).or_default().insert(source);
    }
    assert!(
        !sources.is_empty(),
        "Cargo.lock lists no packages; a check over nothing passes"
    );
    for (name, from) in &sources {
        assert_eq!(
            from.len(),
            1,
            "Cargo.lock takes {name} from {} sources ({from:?}); a crate name must come from one",
            from.len()
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

/// The kinds of dependency table a manifest can hold, each of which can also
/// sit under `[target.'cfg(...)']`. A crate the mobile crate names in any of
/// them is in its build, for its tests or for its build script, so the reach
/// rule covers all three.
const DEPENDENCY_KINDS: [&str; 3] = ["dependencies", "dev-dependencies", "build-dependencies"];

/// What the mobile crate may name in a table of `kind`: the list above, and
/// `toml` for this test alone.
fn allowed(kind: &str, package: &str) -> bool {
    MOBILE_DEPENDENCIES.contains(&package) || (kind == "dev-dependencies" && package == "toml")
}

/// Every dependency `manifest` names that the reach rule does not allow, as
/// `[section] key (package name)`. A line is judged by the package it takes, which is its
/// `package` value when it has one and its key otherwise, so a renamed line
/// such as `iced_winit = { package = "winit", ... }` is judged as winit.
fn strays_in_manifest(manifest: &toml::Table) -> Vec<String> {
    let mut tables: Vec<(String, &str, &toml::Table)> = Vec::new();
    for kind in DEPENDENCY_KINDS {
        if let Some(t) = manifest.get(kind).and_then(|d| d.as_table()) {
            tables.push((kind.to_owned(), kind, t));
        }
    }
    if let Some(targets) = manifest.get("target").and_then(|t| t.as_table()) {
        for (cfg, t) in targets {
            for kind in DEPENDENCY_KINDS {
                if let Some(d) = t.get(kind).and_then(|d| d.as_table()) {
                    tables.push((format!("target.{cfg}.{kind}"), kind, d));
                }
            }
        }
    }
    assert!(
        !tables.is_empty(),
        "the mobile crate names no dependency; a check over nothing passes"
    );
    let mut strays = Vec::new();
    for (section, kind, deps) in tables {
        for (key, dep) in deps {
            let package = dep
                .get("package")
                .and_then(|v| v.as_str())
                .unwrap_or(key.as_str());
            if !allowed(kind, key) || !allowed(kind, package) {
                strays.push(format!("[{section}] {key} (package {package})"));
            }
        }
    }
    strays
}

/// Every package Cargo.lock records as a dependency of `tawara-mobile` that
/// the reach rule does not allow. The lock names packages, not keys, and lists
/// those of every table and every target, so a line that is renamed, or that
/// takes a renamed line from `[workspace.dependencies]`, shows here under the
/// package it takes. `toml` is allowed because the lock does not say which
/// table it came from; the manifest walk above holds it to the tests.
fn strays_in_lock(lock: &toml::Table) -> Vec<String> {
    let mobile: Vec<&toml::Table> = lock["package"]
        .as_array()
        .expect("Cargo.lock lists packages")
        .iter()
        .filter_map(|p| p.as_table())
        .filter(|p| p.get("name").and_then(|v| v.as_str()) == Some("tawara-mobile"))
        .collect();
    assert_eq!(mobile.len(), 1, "Cargo.lock holds one tawara-mobile");
    let dependencies: Vec<&str> = mobile[0]
        .get("dependencies")
        .and_then(|d| d.as_array())
        .into_iter()
        .flatten()
        .filter_map(|d| d.as_str())
        .collect();
    assert!(
        !dependencies.is_empty(),
        "Cargo.lock records no dependency of tawara-mobile; a check over nothing passes"
    );
    dependencies
        .into_iter()
        .filter(|d| {
            let name = d.split(' ').next().unwrap_or(d);
            !allowed("dev-dependencies", name)
        })
        .map(str::to_owned)
        .collect()
}

/// The mobile crate names the application and its platform glue and nothing
/// else, in any table, under any name. A winit line of its own, from crates.io
/// or from the fork, fails here even when the lock still holds one copy (D32
/// item 9 (b)).
#[test]
fn the_mobile_crate_reaches_the_wallet_only_through_the_app() {
    let manifest = read_toml(&root().join("crates/mobile/Cargo.toml"));
    let strays = strays_in_manifest(&manifest);
    assert!(
        strays.is_empty(),
        "crates/mobile names {strays:?}; it reaches the wallet only through tawara-app"
    );
    let lock = read_toml(&root().join("Cargo.lock"));
    let strays = strays_in_lock(&lock);
    assert!(
        strays.is_empty(),
        "Cargo.lock records {strays:?} as dependencies of tawara-mobile; it reaches the \
         wallet only through tawara-app"
    );
}

/// Lines a walk of [dependencies] alone, judging each line by its key, would
/// let through: winit under [dev-dependencies] or [build-dependencies], and
/// winit renamed to `iced_winit`. Cargo builds each of them, so each must be
/// refused, by the manifest walk and by the lock (D32 item 9 (b)).
#[test]
fn the_reach_rule_refuses_winit_in_any_table_or_under_any_name() {
    let fixtures = [
        (
            "winit under [dev-dependencies]",
            r#"
                [dependencies]
                tawara-app.workspace = true
                [dev-dependencies]
                toml.workspace = true
                [dev-dependencies.winit]
                git = "https://github.com/patricksmithlaravel/iced_mobile"
                rev = "d4ab3a057db96162b087a27dd0752a3b15abfa6b"
            "#,
        ),
        (
            "winit renamed to iced_winit",
            r#"
                [dependencies]
                tawara-app.workspace = true
                [target.'cfg(target_os = "android")'.dependencies.iced_winit]
                package = "winit"
                git = "https://github.com/patricksmithlaravel/iced_mobile"
                rev = "d4ab3a057db96162b087a27dd0752a3b15abfa6b"
                features = ["android-native-activity"]
            "#,
        ),
        (
            "winit under a target's [build-dependencies]",
            r#"
                [dependencies]
                tawara-app.workspace = true
                [target.'cfg(target_os = "ios")'.build-dependencies]
                winit = "0.30.13"
            "#,
        ),
        (
            "toml outside [dev-dependencies]",
            r#"
                [dependencies]
                tawara-app.workspace = true
                toml.workspace = true
            "#,
        ),
    ];
    for (case, text) in fixtures {
        let manifest: toml::Table = text.parse().expect("the fixture is TOML");
        assert!(
            !strays_in_manifest(&manifest).is_empty(),
            "the manifest walk lets through {case}"
        );
    }
    let lock: toml::Table = r#"
        [[package]]
        name = "tawara-mobile"
        version = "0.1.0"
        dependencies = [
         "iced_winit",
         "tawara-app",
         "toml",
         "winit",
        ]
    "#
    .parse()
    .expect("the fixture is TOML");
    assert_eq!(
        strays_in_lock(&lock),
        ["winit"],
        "the lock check lets through a winit that tawara-mobile depends on directly"
    );
}

/// Android's activity backend is winit's NativeActivity and nothing else (D32
/// item 9 (b)). android-activity refuses to build with neither backend or with
/// both, and a backend is switched on only by a feature, so the check reads the
/// line that chooses it and then the features cargo resolves for each Android
/// target, where a backend switched on by any other crate, or by a default,
/// would appear.
#[test]
fn android_activity_has_the_native_activity_backend_only() {
    let manifest = read_toml(&root().join("Cargo.toml"));
    let chosen: Vec<&str> = workspace_deps(&manifest)
        .get("iced_winit")
        .and_then(|d| d.get("features"))
        .and_then(|f| f.as_array())
        .into_iter()
        .flatten()
        .filter_map(|f| f.as_str())
        .collect();
    assert!(
        chosen.contains(&"android-native-activity") && !chosen.contains(&"android-game-activity"),
        "the iced_winit line must choose android-native-activity and not android-game-activity; \
         it names {chosen:?}"
    );
    for target in ANDROID_TARGETS {
        let features = resolved_features(target, "android-activity");
        assert!(
            features.contains("native-activity") && !features.contains("game-activity"),
            "android-activity is built for {target} with features {features:?}; it must have \
             the native-activity backend and not the game-activity one"
        );
    }
}
