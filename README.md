# Tawara for Android and iOS

The phone apps of [Tawara](https://github.com/patricksmithlaravel/Tawara-wallet),
a graphical wallet for Mochimo v3, written in Rust with the
[iced](https://iced.rs) toolkit.

This repository holds only what is mobile: the Android and iOS entry
points, their platform glue and their platform files. The application
itself, every screen, the wallet's worker and, through it, the wallet
library with every refusal that protects a user, is `tawara-app` from
Tawara-wallet, the same crate the desktop runs. It is a dependency pinned by
full commit hash, never patched, vendored or copied here: a change the
phones need in a screen, in the wallet's logic or in text that protects the
user is made in Tawara-wallet and taken here by moving `rev` in
`Cargo.toml`. `crates/mobile/tests/policy.rs` holds the repository to that.

Tawara is not an official product of the Mochimo cryptocurrency.

## The record

The plan, the decisions and the screens live in Tawara-wallet's `docs/`:
[PLAN.md](https://github.com/patricksmithlaravel/Tawara-wallet/blob/main/docs/PLAN.md),
[DECISIONS.md](https://github.com/patricksmithlaravel/Tawara-wallet/blob/main/docs/DECISIONS.md)
and [SCREENS.md](https://github.com/patricksmithlaravel/Tawara-wallet/blob/main/docs/SCREENS.md).
Every `docs/...` and D-number cited in this repository's files refers to
them. D32 is the mobile shells; D33 is why they live here.

## Layout

```
crates/
  mobile/        the tawara-mobile crate: the Android library and the iOS binary
    tests/       the policy test
ci/
  notices.sh     writes THIRD-PARTY-NOTICES.txt, which CI compares with the committed file
platform/
  android/       manifest, backup rules, build, check and release scripts
  ios/           Info.plist, privacy manifest, build, check and release scripts
  build-number   the store build counter, and build-number.sh, which raises it
about.toml, THIRD-PARTY-NOTICES.hbs, THIRD-PARTY-NOTICES.txt   the notices (below)
.github/workflows/mobile.yml   the checks, the emulator and the simulators
```

## Building

The compiler is pinned in `rust-toolchain.toml`, as in Tawara-wallet.

```
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked          # the policy test
cargo deny check                         # needs cargo-deny 0.20.2 and the network
```

Android needs the Android SDK and NDK (`ANDROID_HOME`):

```
platform/android/tawara.sh build      # libtawara_mobile.so, arm64-v8a and x86_64
platform/android/tawara.sh apk        # the APK, signed with a throwaway debug key
platform/android/tawara.sh emulator   # boot an emulator, install, run the checks
```

`platform/android/release.sh` makes the Google Play bundle. It runs on the
owner's machine with the Android SDK pieces that docs/RELEASE.md section 1.6
names and the upload key, and CI never runs it (`platform/android/README.md`).

iOS needs a Mac with Xcode:

```
platform/ios/tawara.sh device      # build and link for a phone
platform/ios/tawara.sh simulator   # bundle, boot a simulator, run the checks
```

`platform/ios/release.sh` makes the App Store upload. It runs on the owner's Mac,
with Xcode 26.x and the owner's signing identities, and CI never runs it
(`platform/ios/README.md`).

The steps of each store build, the environment variables it reads and what the
owner supplies are in the two platform READMEs. The owner supplies the accounts,
the signing identities and profile, the upload key, the icons and the export
answer. CI signs and uploads nothing. D36, which sets that, is still proposed.

The build number is `CFBundleVersion` on iOS and `versionCode` on Android.
Each upload to either store takes the next one. `platform/build-number` holds
the last number uploaded, and `platform/build-number.sh bump N` raises it,
only to the number after it. A debug or CI build takes the development number,
1. A store build sets `TAWARA_BUILD_NUMBER` to the committed counter, and both
platform scripts refuse any other value. Where the counter lives awaits the
owner's decision (Tawara-wallet's `docs/RELEASE.md` section 5.2 names the
integer and no file).

CI runs all of these (`.github/workflows/mobile.yml`), with no third-party
actions (D13).

## Third-party notices

`THIRD-PARTY-NOTICES.txt` lists the crates the Android and iOS builds carry,
each with its licence text, and ends with the Mochimo licence from
`LICENSE.md`. It is generated with cargo-about 0.9.2, not written by hand:

```
cargo install --locked --features cli cargo-about --version 0.9.2
bash ci/notices.sh            # write THIRD-PARTY-NOTICES.txt
bash ci/notices.sh --check    # what CI runs: fail, with a diff, if it differs
```

`about.toml` names the licences the notices accept, the targets they
describe, and the licence files that name each crate's copyright holders where
cargo-about would not find them; some are fetched from the crate's repository,
so generating the file needs the network. `THIRD-PARTY-NOTICES.hbs` is their
layout. `ci/notices.sh` removes carriage returns and the standard MIT text's
template line for the holder, and appends `LICENSE.md`, because cargo-about
0.9.2 does not write that licence's text. The check job in
`.github/workflows/mobile.yml` regenerates the file and fails on a single
differing byte, and `crates/mobile/tests/policy.rs` holds `about.toml` to the
licences `deny.toml` admits and to the targets the platform scripts build. Why
the file is committed and checked, and not uploaded, is Tawara-wallet's
`docs/DECISIONS.md` D35.

The notices reach the two upload packages: the iOS upload bundle that
`platform/ios/release.sh` makes puts the file, with `LICENSE.md`, at the root of
`Tawara.app`, and the Google Play bundle that `platform/android/release.sh` makes
puts them under the bundle's `assets/`. The development bundle that
`platform/ios/tawara.sh` makes does not carry them, since it copies only the
executable and `Info.plist`, and the development APK that `platform/android/tawara.sh
apk` makes zips only the libraries. The desktop packages are the wallet's to
place (Tawara-wallet's `docs/RELEASE.md` section 4). The known gaps in the
notices, and the owner's decisions on them, are in Tawara-wallet's D35, item 6.

## Licence

The Mochimo Cryptocurrency Engine License Agreement, version 1.0, in
[LICENSE.md](LICENSE.md): the same file as Tawara-wallet's and the
library's.
