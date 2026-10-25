//! SolidCraft on Android: the same [`solidcraft_ui_egui::SolveApp`] as the desktop and web apps,
//! run by eframe through winit's NativeActivity backend, with MainActivity.java for what needs
//! Java (see [`bridge`]): document pickers, clipboard, soft keyboard, files from other apps.
//!
//! Build: `cargo ndk -t arm64-v8a -o android/app/src/main/jniLibs build -p solidcraft-android
//! --profile android-small`, then `android/gradlew assembleRelease` (signed with the debug key).
#![cfg(target_os = "android")]

mod bridge;
mod net;
mod smb_core;
mod smb_ui;
mod textmenu;
mod touch;

use solidcraft_engine::Session;
use solidcraft_ui_egui::{Services, SolveApp};
use winit::platform::android::activity::AndroidApp;

const DEPTH_BITS: u8 = 24;
/// The UI scale a first start picks (shortest side under / over 600 dp).
const PHONE_SCALE: f32 = 1.1;
/// Narrower than this (points), the browser is a drawer (see [Shell::drawer]).
const DRAWER_BELOW: f32 = 600.0;
/// How near the left edge a swipe must start to pull the drawer out, how far it must go, and how
/// far from the edge one that pushes it back may start (points).
const EDGE: f32 = 24.0;
const SWIPE: f32 = 80.0;
const DRAWER_WIDTH: f32 = 320.0;
/// Room (points) left between a focused field and the soft keyboard.
const PAN_MARGIN: f32 = 24.0;
const TABLET_SCALE: f32 = 1.25;
/// Autosave for crash recovery, as the desktop app's default.
const AUTOSAVE: std::time::Duration = std::time::Duration::from_secs(60);

struct Shell {
    app: SolveApp,
    links: bridge::Links,
    touch: touch::Touch,
    keyboard: bool,
    text_menu: textmenu::TextMenu,
    /// First start (no saved preferences): pick a UI scale for touch once the window's size is known.
    first_scale: bool,
    /// How far the UI is panned up (points) so the focused field clears the soft keyboard.
    pan: f32,
    /// A save was running last frame.
    was_busy: bool,
    /// The text field the keyboard types into (a new one restarts the keyboard's text).
    focus: Option<egui::Id>,
    /// The narrow-screen browser drawer: a swipe in progress (where it started, where it is).
    swipe: Option<(egui::TouchId, egui::Pos2, egui::Pos2)>,
    /// The file dialogs' location chooser and network (SMB) browser.
    smb: smb_ui::Chooser,
}

impl Shell {
    fn files_tick(&mut self, ctx: &egui::Context) {
        for path in bridge::take_incoming() {
            log::info!("opening {path} from another app");
            self.app.open_path_async(&path);
        }
        let now = ctx.input(|i| i.time);
        let busy = self.app.file_picker_busy();
        for r in self.links.sync(now, busy) {
            match r {
                Ok(msg) => self.app.set_status(msg, false),
                Err(msg) => self.app.set_status(msg, true),
            }
        }
        // Keep frames coming while a save runs and just after it ends, so the finished file is
        // written back to its document without waiting for the next touch.
        if busy || self.was_busy {
            ctx.request_repaint_after(std::time::Duration::from_millis(if busy { 200 } else { 450 }));
        }
        self.was_busy = busy;
    }

    /// On a narrow screen the browser is a drawer: a swipe right from the left edge shows it, a
    /// swipe left across it hides it again.
    fn drawer(&mut self, ctx: &egui::Context, raw: &egui::RawInput) {
        let Some(screen) = raw.screen_rect else { return };
        if screen.width() >= DRAWER_BELOW {
            self.swipe = None;
            return;
        }
        for e in &raw.events {
            let egui::Event::Touch { id, phase, pos, .. } = e else { continue };
            match phase {
                egui::TouchPhase::Start => self.swipe = Some((*id, *pos, *pos)),
                egui::TouchPhase::Move => {
                    if let Some((sid, _, last)) = &mut self.swipe
                        && sid == id
                    {
                        *last = *pos;
                    }
                }
                egui::TouchPhase::End | egui::TouchPhase::Cancel => {
                    let Some((sid, from, to)) = self.swipe.take() else { continue };
                    if sid != *id || *phase == egui::TouchPhase::Cancel {
                        continue;
                    }
                    let d = to - from;
                    if d.x.abs() < SWIPE || d.y.abs() > d.x.abs() * 0.5 {
                        continue;
                    }
                    let open = self.app.ui.show_browser;
                    if !open && d.x > 0.0 && from.x - screen.left() <= EDGE {
                        self.app.ui.show_browser = true;
                        ctx.request_repaint();
                    } else if open && d.x < 0.0 && from.x - screen.left() <= DRAWER_WIDTH {
                        self.app.ui.show_browser = false;
                        ctx.request_repaint();
                    }
                }
            }
        }
    }

    /// Show the soft keyboard while a text field has focus; copy what egui copies to the clipboard.
    fn platform(&mut self, ctx: &egui::Context) {
        let smb_form = self.smb.form_wants_keyboard();
        let wants_kb = ctx.egui_wants_keyboard_input();
        let (wants_ime, copied) = ctx.output(|o| {
            let copied: Vec<String> = o
                .commands
                .iter()
                .filter_map(|c| match c {
                    egui::OutputCommand::CopyText(t) => Some(t.clone()),
                    _ => None,
                })
                .collect();
            (o.ime.is_some(), copied)
        });
        let wants = wants_kb || wants_ime || smb_form;
        // Links (help, documentation, issue reports) open through an Android intent.
        let urls: Vec<String> = ctx.output_mut(|o| {
            let mut urls = Vec::new();
            o.commands.retain(|c| match c {
                egui::OutputCommand::OpenUrl(u) => {
                    urls.push(u.url.clone());
                    false
                }
                _ => true,
            });
            urls
        });
        for url in urls {
            bridge::open_url(&url);
        }
        let focus = ctx.memory(|m| m.focused());
        if wants != self.keyboard || (wants && focus != self.focus) {
            self.keyboard = wants;
            bridge::show_keyboard(wants, wants && smb_ui::is_password_field(focus));
        }
        self.focus = focus;
        if let Some(t) = copied.last() {
            bridge::set_clipboard(t);
        }
    }
}

impl eframe::App for Shell {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.app.logic(ctx);
        self.files_tick(ctx);
        if self.first_scale {
            let r = ctx.content_rect();
            if r.width() > 1.0 {
                self.first_scale = false;
                // Fingers need bigger targets than a mouse: a little larger on a phone (where room
                // is short), more on a tablet. Preferences > UI scale changes it as on desktop.
                let short_dp = r.width().min(r.height()) * ctx.zoom_factor();
                self.app.preferences.ui_scale = if short_dp >= 600.0 { TABLET_SCALE } else { PHONE_SCALE };
                if short_dp < 600.0 {
                    // A phone: the compact layout (no floating cards, more room for the model)
                    // and the browser as a drawer, swiped in from the left edge when needed.
                    let _ = self.app.run("view.layout", serde_json::json!({"mode": "compact"}));
                    self.app.ui.show_browser = false;
                }
            }
        }
    }
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        // The keyboard covers the window's bottom: pan every layer up until the focused field
        // clears it (Android's adjustPan). The layout keeps its size, so a scrolled list does not
        // move or drop the field (egui takes the focus away from a field it no longer shows), and
        // the layer transform moves input with the graphics.
        let inset = bridge::ime_inset_px();
        let covered = if self.keyboard && inset > 0 { inset as f32 / ctx.pixels_per_point().max(0.1) } else { 0.0 };
        let field = ctx.memory(|m| m.focused()).and_then(|id| ctx.read_response(id)).map(|r| r.rect);
        let pan = match (covered > 0.0, field, raw.screen_rect) {
            (true, Some(f), Some(s)) => (f.max.y + PAN_MARGIN - (s.max.y - covered)).clamp(0.0, covered),
            (true, None, _) => self.pan,
            _ => 0.0,
        };
        if pan > 0.0 || self.pan > 0.0 {
            let t = egui::emath::TSTransform::from_translation(egui::vec2(0.0, -pan));
            let menu = egui::LayerId::new(egui::Order::Tooltip, egui::Id::new(textmenu::AREA));
            let layers: Vec<_> = ctx.memory(|m| m.layer_ids().collect());
            for layer in layers.into_iter().filter(|l| *l != menu) {
                ctx.set_transform_layer(layer, t);
            }
            if pan != self.pan {
                ctx.request_repaint();
            }
        }
        self.pan = pan;
        let editing = self.keyboard || ctx.egui_wants_keyboard_input() || self.smb.form_wants_keyboard();
        if self.text_menu.hook(raw, editing, bridge::clipboard) {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        self.drawer(ctx, raw);
        self.touch.hook(&self.app, ctx, raw);
        // Ctrl+V from a hardware keyboard pastes Android's clipboard.
        let paste = raw.events.iter().any(|e| matches!(e, egui::Event::Key { key: egui::Key::V, pressed: true, modifiers, .. } if modifiers.command));
        if paste && let Some(text) = bridge::clipboard() {
            raw.events.push(egui::Event::Paste(text));
        }
        // The soft keyboard's text (swipe, suggestions, composing) arrives as IME events.
        let _ = bridge::EGUI.set(ctx.clone());
        raw.events.extend(bridge::take_ime_text().into_iter().map(egui::Event::Ime));
        self.app.raw_input_hook(raw);
    }
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.app.ui(ui);
        let ctx = ui.ctx().clone();
        // The drawer's button, on a narrow screen with a design open: the edge swipe alone would
        // fight the system's back gesture.
        if ctx.content_rect().width() < DRAWER_BELOW && !self.app.home.open {
            let label = if self.app.ui.show_browser { "Hide browser" } else { "Browser" };
            egui::Area::new(egui::Id::new("android-drawer"))
                .order(egui::Order::Foreground)
                .anchor(egui::Align2::LEFT_BOTTOM, egui::vec2(8.0, -8.0))
                .show(&ctx, |ui| {
                    if ui.add(egui::Button::new(label).min_size(egui::vec2(48.0, 40.0))).clicked() {
                        self.app.ui.show_browser = !self.app.ui.show_browser;
                    }
                });
        }
        self.smb.show(&ctx);
        self.text_menu.show(&ctx);
        self.platform(&ctx);
    }
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        storage.set_string(solidcraft_ui_egui::prefs::STORAGE_KEY, self.app.prefs());
        // Also when Android pauses the app: unsaved work goes to the recovery folder.
        if let Some(a) = self.app.autosave.as_mut()
            && let Err(e) = a.save(&self.app.session)
        {
            log::warn!("autosave: {e}");
        }
    }
    fn on_exit(&mut self) {
        solidcraft_ui_egui::save_job::tick(&mut self.app, true);
        solidcraft_ui_egui::recovery_ui::close(&mut self.app);
    }
}

#[unsafe(no_mangle)]
fn android_main(android_app: AndroidApp) {
    android_logger::init_once(android_logger::Config::default().with_max_level(log::LevelFilter::Info).with_tag("SolidCraft"));
    let data = android_app.internal_data_path();
    let persistence_path = data.as_ref().map(|d| d.join("app.ron"));
    let options = eframe::NativeOptions {
        android_app: Some(android_app),
        depth_buffer: DEPTH_BITS,
        persistence_path,
        ..Default::default()
    };
    let result = eframe::run_native(
        "SolidCraft",
        options,
        Box::new(move |cc| {
            solidcraft_ui_egui::fonts::setup_fonts(&cc.egui_ctx);
            let services = Services {
                pick_open: Some(Box::new(bridge::pick_open)),
                pick_save: Some(Box::new(bridge::pick_save)),
                show_in_folder: Some(Box::new(bridge::show_in_folder)),
                agent_http: Some(std::sync::Arc::new(net::agent_http)),
                secrets: Some(std::sync::Arc::new(net::Keystore)),
                ..Default::default()
            };
            let mut app = SolveApp::new(Session::default(), services);
            // As on desktop: a slow OK, undo or history change computes off the window thread, so
            // Android's input watchdog (5 s) never sees the app stop answering.
            app.apply_off_thread = Some(solidcraft_ui_egui::dialogs::APPLY_AT_ONCE);
            let stored = cc.storage.and_then(|s| solidcraft_ui_egui::prefs::stored(|k| s.get_string(k)));
            let first_scale = stored.is_none();
            if let Some(p) = stored {
                app.load_prefs(&p);
            }
            if let Some(rs) = &cc.wgpu_render_state {
                log::info!("SolidCraft graphics: {:?} {}", rs.adapter.get_info().backend, rs.adapter.get_info().name);
                app.set_wgpu(rs, DEPTH_BITS, 0, rs.msaa_samples);
                solidcraft_ui_egui::gpu::install_picture_renderer(rs);
            }
            solidcraft_ui_egui::prefs::apply_new_design(&mut app);
            let rev = app.session.revision;
            app.home.show(rev);
            if let Some(dir) = &data {
                solidcraft_ui_egui::recovery_ui::start(&mut app, &dir.join("recovery"), AUTOSAVE);
            }
            let links = bridge::Links::load(data.as_ref().map(|d| d.join("documents").join("links.json")));
            if let Some(dir) = &data {
                smb_ui::init(dir);
                bridge::sweep_documents(&dir.join("documents"), &links);
            }
            Ok(Box::new(Shell { app, links, touch: touch::Touch::default(), keyboard: false, text_menu: textmenu::TextMenu::default(), first_scale, pan: 0.0, was_busy: false, focus: None, swipe: None, smb: smb_ui::Chooser::default() }))
        }),
    );
    if let Err(e) = result {
        log::error!("SolidCraft failed to start: {e}");
    }
}
