//! Touch on the 3D view: the camera, through the same bindings as a mouse.
//!
//! egui-winit turns the first finger into the primary button, which on the model view selects
//! and box-selects. Here a gesture that starts on the model view (not on a panel, menu or
//! dialog) is read instead:
//! - a tap is a click (select);
//! - one finger dragged orbits;
//! - two fingers pan with their midpoint and zoom with their spread (pinch);
//! - a long press (0.5 s, still) is a right click: the context menu.
//!
//! Orbit and pan are sent as the button and keys the navigation preset binds to them (Fusion:
//! Shift+middle orbits, middle pans; the keys are sent as `ModifiersChanged`), so preferences and the navigation code stay the same.
//! Touches anywhere else go to egui unchanged (buttons, sliders, scrolling lists).

use egui::{Event, Modifiers, PointerButton, Pos2, TouchId, TouchPhase};
use solidcraft_ui_egui::SolveApp;
use solidcraft_ui_egui::prefs::NavButton;
use solidcraft_ui_egui::viewport::NavMode;

const MOVE_SLOP: f32 = 10.0;
const LONG_PRESS: f64 = 0.5;

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// No gesture, or one on the UI: egui's own pointer events pass.
    Pass,
    /// A finger down on the model view, not moved yet.
    Pending { start: Pos2, at: f64 },
    /// A camera drag: this button and these keys are held.
    Drag { button: PointerButton, mods: Modifiers, two: bool, spread: f32 },
    /// Finished; ignore the pointer until every finger is up.
    Done,
}

pub struct Touch {
    fingers: Vec<(TouchId, Pos2)>,
    mode: Mode,
    /// Drop egui-winit's pointer events until it says the pointer is gone.
    swallow: bool,
}

impl Default for Touch {
    fn default() -> Self {
        Self { fingers: Vec::new(), mode: Mode::Pass, swallow: false }
    }
}

fn binding(app: &SolveApp, want: NavMode, fallback: (PointerButton, Modifiers)) -> (PointerButton, Modifiers) {
    for mods in [Modifiers::NONE, Modifiers::SHIFT, Modifiers::CTRL, Modifiers::ALT] {
        for (nav, button) in [(NavButton::Middle, PointerButton::Middle), (NavButton::Right, PointerButton::Secondary)] {
            if solidcraft_ui_egui::prefs::nav_mode(app, nav, mods) == Some(want) {
                return (button, mods);
            }
        }
    }
    fallback
}

fn centroid(f: &[(TouchId, Pos2)]) -> Pos2 {
    let n = f.len().max(1) as f32;
    let s = f.iter().fold(egui::Vec2::ZERO, |a, (_, p)| a + p.to_vec2());
    (s / n).to_pos2()
}

fn spread(f: &[(TouchId, Pos2)]) -> f32 {
    if f.len() < 2 { 0.0 } else { f[0].1.distance(f[1].1) }
}

fn on_view(app: &SolveApp, ctx: &egui::Context, p: Pos2) -> bool {
    solidcraft_ui_egui::shell::on_model_view(app, p) && ctx.layer_id_at(p).is_none_or(|l| l.order == egui::Order::Background)
}

impl Touch {
    pub fn hook(&mut self, app: &SolveApp, ctx: &egui::Context, raw: &mut egui::RawInput) {
        let now = raw.time.unwrap_or(0.0);
        let mut out = Vec::with_capacity(raw.events.len() + 4);
        if let Mode::Pending { start, at } = self.mode {
            if now - at >= LONG_PRESS {
                // Long press: the context menu where the finger is.
                out.push(Event::PointerMoved(start));
                out.push(Event::PointerButton { pos: start, button: PointerButton::Secondary, pressed: true, modifiers: Modifiers::NONE });
                out.push(Event::PointerButton { pos: start, button: PointerButton::Secondary, pressed: false, modifiers: Modifiers::NONE });
                self.mode = Mode::Done;
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(60));
            }
        }
        for event in std::mem::take(&mut raw.events) {
            match event {
                Event::Touch { id, phase, pos, .. } => {
                    out.push(event.clone());
                    self.touch(app, ctx, id, phase, pos, now, &mut out);
                }
                Event::PointerMoved(_) | Event::PointerButton { .. } => {
                    if !self.swallow && self.mode == Mode::Pass {
                        out.push(event);
                    }
                }
                Event::PointerGone => {
                    if self.fingers.is_empty() {
                        self.swallow = false;
                    }
                    out.push(event);
                }
                e => out.push(e),
            }
        }
        raw.events = out;
    }

    #[allow(clippy::too_many_arguments)]
    fn touch(&mut self, app: &SolveApp, ctx: &egui::Context, id: TouchId, phase: TouchPhase, pos: Pos2, now: f64, out: &mut Vec<Event>) {
        match phase {
            TouchPhase::Start => {
                self.fingers.retain(|(i, _)| *i != id);
                self.fingers.push((id, pos));
                match (self.fingers.len(), self.mode) {
                    (1, _) => {
                        self.swallow = false;
                        self.mode = if on_view(app, ctx, pos) { Mode::Pending { start: pos, at: now } } else { Mode::Pass };
                        if self.mode != Mode::Pass {
                            self.swallow = true;
                        }
                    }
                    (2, Mode::Pending { .. } | Mode::Drag { two: false, .. }) => {
                        if let Mode::Drag { button, mods, .. } = self.mode {
                            out.push(Event::PointerButton { pos, button, pressed: false, modifiers: mods });
                            out.push(Event::ModifiersChanged(Modifiers::NONE));
                        }
                        let (button, mods) = binding(app, NavMode::Pan, (PointerButton::Middle, Modifiers::NONE));
                        let c = centroid(&self.fingers);
                        out.push(Event::ModifiersChanged(mods));
                        out.push(Event::PointerMoved(c));
                        out.push(Event::PointerButton { pos: c, button, pressed: true, modifiers: mods });
                        self.mode = Mode::Drag { button, mods, two: true, spread: spread(&self.fingers) };
                    }
                    _ => {}
                }
            }
            TouchPhase::Move => {
                if let Some(f) = self.fingers.iter_mut().find(|(i, _)| *i == id) {
                    f.1 = pos;
                }
                match self.mode {
                    Mode::Pending { start, .. } if start.distance(pos) > MOVE_SLOP => {
                        let (button, mods) = binding(app, NavMode::Orbit, (PointerButton::Middle, Modifiers::SHIFT));
                        out.push(Event::ModifiersChanged(mods));
                        out.push(Event::PointerMoved(start));
                        out.push(Event::PointerButton { pos: start, button, pressed: true, modifiers: mods });
                        out.push(Event::PointerMoved(pos));
                        self.mode = Mode::Drag { button, mods, two: false, spread: 0.0 };
                    }
                    Mode::Drag { two: false, .. } => out.push(Event::PointerMoved(pos)),
                    Mode::Drag { button, mods, two: true, spread: before } => {
                        out.push(Event::PointerMoved(centroid(&self.fingers)));
                        let now_spread = spread(&self.fingers);
                        if before > 1.0 && now_spread > 1.0 {
                            out.push(Event::Zoom(now_spread / before));
                        }
                        self.mode = Mode::Drag { button, mods, two: true, spread: now_spread };
                    }
                    _ => {}
                }
            }
            TouchPhase::End | TouchPhase::Cancel => {
                let at = self.fingers.iter().find(|(i, _)| *i == id).map_or(pos, |f| f.1);
                self.fingers.retain(|(i, _)| *i != id);
                match self.mode {
                    Mode::Pending { start, .. } => {
                        if phase == TouchPhase::End {
                            // A tap: a click where the finger went down.
                            out.push(Event::PointerMoved(start));
                            out.push(Event::PointerButton { pos: start, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
                            out.push(Event::PointerButton { pos: start, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
                        }
                        self.mode = Mode::Done;
                    }
                    Mode::Drag { button, mods, two, .. } => {
                        let p = if two { centroid(&self.fingers) } else { at };
                        out.push(Event::PointerButton { pos: p, button, pressed: false, modifiers: mods });
                        out.push(Event::ModifiersChanged(Modifiers::NONE));
                        self.mode = Mode::Done;
                    }
                    _ => {}
                }
                if self.fingers.is_empty() {
                    if self.mode == Mode::Done {
                        out.push(Event::PointerGone);
                    }
                    self.mode = Mode::Pass;
                }
            }
        }
    }
}
