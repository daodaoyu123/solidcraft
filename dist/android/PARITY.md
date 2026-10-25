# SolidCraft 0.19.0 on Android: feature parity with the desktop app

Native Rust build (`apps/solidcraft-android`, branch `android`): the same `SolveApp`
(crates/ui-egui) and engine as the desktop app, run by eframe 0.36 through winit 0.30's
NativeActivity backend, wgpu 30 (Vulkan, GLES 3 fallback). `MainActivity.java` adds what needs
Java. Status as of android4 (22 Oct 2026, upstream v0.19.0). "Verified" = exercised in the emulator (x86_64 Android 15 tablet AVD) with screenshots in `emulator_test/android2/`, `emulator_test/android3/` and `emulator_test/android4/` (android4 run: files `a4x_*.png`, x86_64 android-small release build of the same source as the arm64 APK).

Legend: **works** (same code as desktop, checked in the emulator) / **android2** (added for
Android in this build) / **partial** / **gap** (not on Android yet) / **n/a** (desktop-only by
nature, hidden or inert).

## Core CAD (crates/engine, crates/kernel, crates/ui-egui)

| Feature | Status | Notes |
|---|---|---|
| Sketch, constraints, dimensions | works | Same code; precise picking with a finger is harder than a mouse (see touch). |
| Extrude, revolve, sweep, loft, fillet, chamfer, shell, hole, thread, patterns, mirror | works | Same kernel (truck). Opened the samples; modelling commands run unchanged. |
| Sheet metal, surface, assemblies, joints, motion | works | Same code; not exercised one by one in the emulator. |
| Parameters, configurations, BOM, inspect/measure | works | Same code. |
| Undo / redo | works | Toolbar buttons; Ctrl+Z/Y with a hardware keyboard. |
| Timeline / history bar | works | |
| Samples on the start page | works | Opened Sample Plate (android1 and android2). |
| Rendering (viewport, thumbnails, render workspace) | works | Vulkan on the emulator's host GPU; GLES 3 fallback offered by wgpu. |
| Crash recovery autosave | android2, verified (after a killed session the next start offered "Recover unsaved design?") | `recovery_ui::start` on the app's files dir (60 s), plus a save when Android pauses the app (`App::save`). |

## Files

| Feature | Status | Notes |
|---|---|---|
| File > Open (.solidcraft, JSON, STEP/IGES, FCStd/3dm, 3MF/STL/OBJ/glTF, DXF/SVG) | android2, verified (.solidcraft); android4 re-verified (Download/TestPart.solidcraft, 105 bodies) | Storage Access Framework `ACTION_OPEN_DOCUMENT`; the document is copied into app storage and opened from there. The type is taken from the file name, as on desktop. |
| File > Save / Save As (.solidcraft, JSON) | android4 re-verified (Save rewrote Download/TestPart.solidcraft; Save As created Download/TestPart_android4.solidcraft, 149,942 B); android2, verified (.solidcraft: Save wrote the edited design back to Download/TestPart.solidcraft; Save As created Download/TestPart_android2.solidcraft) | `ACTION_CREATE_DOCUMENT` for Save As; the app writes a local file linked to the document's URI and `bridge::Links` copies it back once written (and again after each Save). Links persist, so Recent designs still save to their documents. |
| File > Insert STEP / Mesh / Design | android2, verified (Insert STEP: 105 -> 210 bodies) | Same picker as Open. |
| File > Export STL / 3MF / STEP / OBJ / URDF / DXF / JSON, Save as mesh, parameters/BOM/library/keymap/prefs export, render/issue PNG, motion GIF | android4 re-verified on TestPart (105 bodies): STEP 633,332 B, STL (All visible bodies) 8,963,484 B, 3MF 2,065,135 B, URDF in a picked folder (see URDF meshes); android2/3, verified STEP (633 KB), OBJ (67 MB), STL (1 MB); android3 on TestBracket: 3MF of all 442 bodies (9.96 MB, valid zip), DXF view (3.1 MB), Design JSON (193 KB), URDF (5.9 KB, but see URDF below) | All go through `Services::pick_save`, so all use the same create-document picker and write-back. |
| Open Recent | partial | Lists the app-storage copies; reopening works; the original document is written on save through the persisted link. |
| URDF meshes | android4, verified (Export URDF > folder picker > new folder Documents/URDF_android4 > Allow: TestPart_android4.urdf 5,973 B plus 8 mesh STLs, 8.9 MB) | `ACTION_OPEN_DOCUMENT_TREE`: the .urdf and its meshes are written into the picked folder. The root of a volume and Download itself are refused by Android ("Can't use this folder"); pick or create a subfolder. |
| Save As Mesh body choice | android3, verified (File > Export 3MF > All visible bodies: 442 selected, saved) | New **All visible bodies** button in the mesh dialog (also on desktop) fills the Bodies input; picking single bodies still works. |
| Files opened from other apps (Files, browser downloads) | android2, verified (Files > TestPart.solidcraft > Open with SolidCraft) | `ACTION_VIEW` intent filter (octet-stream, zip, STL, STEP, 3MF, OBJ, glTF, IGES MIME types) on `MainActivity` (singleTask); the document is copied in and opened. Android has no per-extension filter for content:// so `.solidcraft` arrives as octet-stream/zip. |
| Drag and drop | n/a | No desktop drag and drop on Android; SAF and "Open with" cover it. |
| Show in folder | android4, verified (Start > Recent card > long press > Show in folder opened the Files picker at Downloads, where the design's document lives) | `bridge::show_in_folder` opens the linked document's folder via the system Files UI. Android shows its document picker there, not a standalone Files window. |
| Linked components / relink to a file | partial | Uses Open; linked files must be reopened through the picker. |

## Input

| Feature | Status | Notes |
|---|---|---|
| Tap UI (menus, ribbon, buttons, sliders, lists) | works | egui-winit maps the first finger to the primary button. |
| Touch on the 3D view | android4 re-verified orbit (one-finger drag redrew 59 % of sampled viewport pixels) and long-press context menu (Extrude, Fillet, Undo, Redo, Hide Origin); android2, verified orbit + long-press context menu; two-finger pan/pinch not testable with adb (single pointer) | `touch.rs`: tap = click/select, one-finger drag = orbit, two fingers = pan + pinch zoom, long press = right click (context menu). Uses the navigation preset's bindings. |
| Soft keyboard for text and number fields | android2/3, verified (android3: the Parameters expression field at y 1305 px pans up to 770 px, above the 736 px keyboard; typed value applied) | `adjustNothing` + a WindowInsets listener report the IME height; every egui layer is translated up just enough for the focused field (adjustPan), so scrolled lists keep the field and its focus. The 3D view's GPU paint is not panned. Shown via `InputMethodManager` while egui has a text field focused (`PlatformOutput::ime`), hidden otherwise. android4: `MainActivity` now serves an `InputConnection` (`ImeView`, verified: Gboard bound to `MainActivity$Connection`, inputType Normal) and passes committed and composing text to egui as IME events. Verified in the command palette with adb key input: "front" then two Backspaces gave "fro", "Front hub" typed exactly (no lost characters). Composing (suggestions, swipe, CJK) is **untested**: on this AVD Gboard opens only in its hardware-keyboard accessory mode, so its on-screen keys never appear. Also seen: the command palette focuses its field but the keyboard only appears after tapping the field. |
| Hardware keyboard and shortcuts | works | winit key events; Ctrl+S etc. |
| Clipboard (text), long-press text menu | android4: round trip verified, no lost characters (Select all > Copy > long press > Paste pasted all 9 characters of "Front hub" at the cursor). Bug found and fixed in 4b91b9d7: a tap on a menu item was also timed as a new long press, so 0.5 s later the menu reopened where the item was tapped (shifted 350 px after Select all) and the next tap on Copy at the old place missed and closed the palette. Re-tested on the fixed x86_64 build: the menu stays put after Select all, Copy works, the palette stays open, and Paste inserts the full text. android3, menu verified (Paste / Copy / Cut / Select all shown at the finger); copy-paste round trip pasted only part of the text in the x86_64 test (Select all was lost by the second long press), fixed in 603fd57 (menu stays open after Select all), re-test pending | egui copy goes to Android's ClipboardManager; menu Paste and hardware Ctrl+V paste from it. |
| Clipboard pictures/files into the agent chat | n/a | arboard has no Android backend; disabled on Android. |
| 3D mouse (SpaceMouse) | n/a | HID over USB not wired; inert. |
| Mouse gestures, right-drag marking menu | works | With a mouse/trackpad attached. |
| Stylus hover | gap | Not distinguished from touch. |

## App, platform, desktop-only pieces

| Feature | Status | Notes |
|---|---|---|
| Preferences, languages, themes | works | Stored in the app's files dir (`app.ron`). |
| UI scale | android4, verified (Preferences > UI scale slider enlarged the whole UI about 1.25x; Cancel restored it) | First start (no saved preferences) picks 125 % on a tablet (shortest side >= 600 dp), 110 % on a phone; Preferences > UI scale as on desktop. First-start choice not re-tested (needs cleared app data). |
| Help / documentation / What's new links | android3, verified (Start > Documentation opened https://docs.solidcraft.io/... in Chrome) | egui `OpenUrl` commands go to `MainActivity.openUrl` (ACTION_VIEW). |
| Update check / one-click updates | n/a | `http_get` unset: no update check (app updates come as APKs). |
| Agent chat (API agents, MCP bridge) | android4, code only, not tested | HTTP through `HttpURLConnection`, API keys in the Android Keystore. Deliberately not exercised in the android4 run (no agents, models or keys entered). MCP/control endpoint still off. |
| MCP / control server, CLI | n/a | Desktop processes (`apps/solidcraft-cli`, `control_server.rs`); not in the APK. |
| Send to slicer (pick program) | n/a | Android can't launch a desktop slicer; export 3MF and open it in a slicer app instead. |
| Native crash capture, Windows frame, macOS open-documents | n/a | Desktop-only modules of `apps/solidcraft`. |
| Multi-window / dialogs | works | egui windows inside the one activity window. |
| Lifecycle: pause/resume | partial | android-activity/eframe recreate the surface on resume; autosave on pause (above). Process death restores from the recovery folder (offered on next start). |
| Fonts (incl. CJK fallback) | android3, not verified | `/system/fonts` NotoSansCJK (zh/ja/ko) and NotoSansThai are fallback candidates, and `/system/fonts` is scanned for other CJK names. A Chinese-UI screenshot is still to do. |
| App icon | android3 | Launcher icon from `assets/icons/app` (mipmap mdpi-xxxhdpi); aapt shows it in the APK. |
| Phone layout | android4, verified on a phone-size display (`wm size 1080x2400`, density 420, then reset) | Narrower than 600 pt the browser is a drawer: a Browser / Hide browser button bottom-left, swipe in from the left edge, swipe left to hide (both verified). First start on a phone also picks the compact layout and 110 % UI scale (not re-tested: needs cleared app data). The ribbon is still the desktop ribbon, cramped at phone width. |

## Known issues found in testing

- Fixed in android3: the TestBracket ANR (442 bodies). Cause: a parameter edit ran
  `settle_value_edit` and the full evaluate on the UI thread, and the Android shell did not
  turn on `apply_off_thread`. Now parameter edits use `run_heavy` and the shell sets
  `apply_off_thread`, as desktop does. Verified: Enter on a new value shows "Computing...";
  the File menu opens during the rebuild; the rebuild finished after about 30 s on x86_64;
  no ANR in logcat.
- Fixed in android3: Save As Mesh needed bodies picked one by one (fiddly by touch). Use All visible bodies.
- Fixed in android3: the soft keyboard covered the focused field.
- The arm64 build runs slowly under the emulator's ARM translation. This is expected; it runs natively on a device.

## Remaining gaps (priority order)

1. Composing IMEs (suggestions, swipe, CJK): the InputConnection is in (android4) but untested; needs a device or an AVD where Gboard shows its on-screen keys.
2. Two-finger pan and pinch zoom: not testable with adb (single pointer); test on a device.
3. First-start UI scale and phone compact layout (need cleared app data), Chinese UI (CJK fallback).
4. Command palette: the keyboard only opens after a tap on the field.
5. Export-copy cleanup in the app's private folders (android4 code) not checked: the release build is not debuggable, so `run-as` can't list them.
6. Agent chat: in the code, not tested (by choice).

## android4 emulator run (22 Oct 2026)

x86_64 android-small release build of ba3161bd on SolidCraft_Tablet_API35, installed over the earlier build (app data kept). Pass: launch, Open via picker, Save, Save As, export STEP/STL/3MF, URDF to a picked folder, Show in folder, typing with adb key input (incl. Backspace, capitals, space), clipboard round trip, orbit, long-press context menu, UI scale, phone drawer layout. Untested: composing text, pinch, two-finger pan, Assistant panel (skipped). Bug fixed: text menu reopening (4b91b9d7). No crash or ANR in logcat.
## android3 build

`dist/android/apk/SolidCraft-0.18.2-android3-arm64-small.apk`: 23,837,910 bytes, SHA-256 6459D40DEFB64D04972175235437BDB3EAC3098D0DEDCE7EC697615D90ED4D06, versionName 0.18.2-android3 (1804), arm64-v8a, profile android-small. apksigner verify passed. Built from 603fd57. The emulator tests above used the x86_64 ci build of the same source (before 603fd57).

## android4 build

- `dist/android/apk/SolidCraft-0.19.0-android4-arm64-small.apk`: 24,073,306 bytes, SHA-256 DC1D39505781A84396D7BC0527A694B7A674B2ABCB9F0DE5875ADBAFCB2F98A1, versionName 0.19.0-android4 (1901), arm64-v8a, profile android-small, built from 0b83cc14 (includes the text menu fix 4b91b9d7). apksigner verify passed.
- `dist/android/apk/SolidCraft-0.19.0-android4-x86_64-small.apk` (emulator): 25,455,487 bytes, SHA-256 4AF1E6F4F0014C6586FBEEBBFC968E76035B9848DE342B3D8D0D4BA12E20967D, built from 4b91b9d7. apksigner verify passed.
- `dist/android/apk/SolidCraft-0.19.0-android4-arm64-small_ba3161bd.apk`: the earlier arm64 build (24,073,734 bytes, from ba3161bd), without the text menu fix. Kept for reference.
