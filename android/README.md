# SolidCraft for Android (APK)

This tree is **not** the desktop app. It is the **Android APK** build: a native `.so` (`apps/solidcraft-android`) plus a small Gradle shell (`android/`). The CAD UI and engine are the same Rust crates as desktop (`solidcraft-ui-egui`, `solidcraft-engine`), run through **eframe / winit NativeActivity** and **wgpu** (Vulkan, GLES fallback).

Until this lands on [tangency-labs/solidcraft](https://github.com/tangency-labs/solidcraft) `main`, treat it as a **branch/fork deliverable**: installable APKs are published separately (for example [johnrcassidy/solidcraft releases](https://github.com/johnrcassidy/solidcraft/releases)) with a **`versionName` like `0.22.1-android1`**, which tracks the **workspace engine version** (`0.22.1`) plus an Android-only build number (`android1`, `android2`, …).

| Label | Meaning |
|--------|---------|
| `Cargo.toml` `version` | Engine/UI library line (should match the upstream release you synced from). |
| `versionName` in `app/build.gradle.kts` | What users see on the APK; suffix `-androidN` marks Android packaging, not a different product. |
| `applicationId` | `io.archipelagos.solidcraft` (Archipelagos packaging; upstream may choose another id if merged). |

## What we added (paths)

| Path | Role |
|------|------|
| `android/` | Gradle app, `MainActivity.java`, launcher assets, JNI entry for pickers/IME/clipboard/secrets. |
| `apps/solidcraft-android/` | Rust shell: `SolveApp`, SAF file links, touch navigation, agent HTTP, SMB browser. |
| `Cargo.toml` `[profile.android-small]` | Smaller release `.so` for phones (LTO, `opt-level = "z"`). |
| `xtask/src/layers.rs` | `solidcraft-android` registered as **exempt** (app crate). |

## Modifications vs upstream desktop

Shared crates (minimal hooks only where Android has no desktop API):

- **`crates/ui-egui`**: `shell::on_model_view` (touch camera vs panels); agent chat paste skips **arboard** on Android (`arboard` has no Android backend).
- **Everything else in `crates/*`**: unchanged for Android unless pulled in by normal desktop development.

Android-only behaviour (see `apps/solidcraft-android` and `MainActivity.java`):

- **Files**: Storage Access Framework open/create/save; documents linked by URI; write-back via `bridge::Links`; open from other apps (`ACTION_VIEW`).
- **Network (SMB)**: saved connections (`connections.json`), passwords in Keystore, in-app SMB browse for open/save/show-in-folder (`smb_core`, `smb_ui`).
- **Input**: soft keyboard inset + IME composition; long-press text menu; `touch.rs` orbit/pan/pinch on the model view.
- **Agent chat**: HTTPS via `HttpURLConnection`; API keys in Keystore (`net.rs`). Clipboard images/files for chat are **not** supported on Android.
- **Not in the APK**: desktop CLI, MCP control server, update installer, slicer pickers, SpaceMouse, arboard-based paste.

Feature-level parity notes (emulator-tested; version in that file may lag the branch): `dist/android/PARITY.md`.

## Build

Prerequisites: Android SDK, NDK, `rustup target add aarch64-linux-android x86_64-linux-android`, [cargo-ndk](https://github.com/bbqsrc/cargo-ndk).

```bash
# From repo root
cargo ndk -t arm64-v8a,x86_64 -o android/app/src/main/jniLibs build -p solidcraft-android --profile android-small
cd android && ./gradlew assembleRelease
```

APK: `android/app/build/output/apk/release/app-release.apk` (debug-signed release type in this tree).

## Contributing upstream

Do **not** open a second large Android PR without checking existing work ([#2412](https://github.com/tangency-labs/solidcraft/pull/2412), [#2658](https://github.com/tangency-labs/solidcraft/pull/2658)). Prefer folding this tree into the lowest-numbered active Android PR or coordinating in comments there.

When proposing a merge to `tangency-labs/solidcraft`, this README plus an updated `PARITY.md` (or a short section in official docs) should state explicitly that **releases can be APKs** with the `X.Y.Z-androidN` naming scheme.
