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
platform/
  android/       manifest, backup rules, build and check script
  ios/           Info.plist, build and check script
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

iOS needs a Mac with Xcode:

```
platform/ios/tawara.sh device      # build and link for a phone
platform/ios/tawara.sh simulator   # bundle, boot a simulator, run the checks
```

CI runs all of these (`.github/workflows/mobile.yml`), with no third-party
actions (D13).

## Licence

The Mochimo Cryptocurrency Engine License Agreement, version 1.0, in
[LICENSE.md](LICENSE.md): the same file as Tawara-wallet's and the
library's.
