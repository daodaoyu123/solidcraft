# SolidCraft for Android (APK)

**This repository branch (`android`) ships an Android APK — not the Windows/macOS/Linux desktop app and not the browser build.**

| | Desktop / web (upstream `main`) | This fork (`android`) |
|---|--------------------------------|------------------------|
| Install | MSI, DMG, Flatpak, browser | **`.apk`** from [Releases](https://github.com/johnrcassidy/solidcraft/releases) |
| Version label | `v0.22.1` etc. | **`0.22.1-android1`** (`versionName` in Gradle) |
| Package id | — | `io.archipelagos.solidcraft` |

**Build and modification details:** [android/README.md](android/README.md)  
**Latest APK:** [github.com/johnrcassidy/solidcraft/releases](https://github.com/johnrcassidy/solidcraft/releases) (tag `v*-android*`)

Upstream coordination: [tangency-labs/solidcraft#2412](https://github.com/tangency-labs/solidcraft/pull/2412).

## Commits and tags (so work is not lost in history)

- **Commit subject:** start with `android:` (example: `android: SMB save upload fix`).
- **Commit body (optional):** `APK: 0.22.1-androidN | package io.archipelagos.solidcraft | not desktop`
- **Tags / releases:** `v0.22.1-android1` — never `v0.22.1` alone on this fork.
- **Pull requests to upstream:** title starts with `Android:` and links this file.
