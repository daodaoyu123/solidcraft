//! Long press in a text field: Paste / Copy / Cut / Select all, as Android's own fields offer.
//!
//! The menu's taps are taken out of the input before egui sees them, so the field keeps its
//! focus (a tap on an ordinary button would end the edit), and its action is sent to the field
//! as the events a keyboard sends: `Paste` with Android's clipboard, `Copy`, `Cut`, Ctrl+A.

/// The menu's egui area (kept out of the keyboard pan in lib.rs: it sits where the finger is).
pub const AREA: &str = "android-text-menu";

use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect, TouchId, TouchPhase};

const LONG_PRESS: f64 = 0.5;
const MOVE_SLOP: f32 = 10.0;

#[derive(Clone, Copy, PartialEq)]
enum Item {
    Paste,
    Copy,
    Cut,
    SelectAll,
}

const ITEMS: [(Item, &str); 4] = [(Item::Paste, "Paste"), (Item::Copy, "Copy"), (Item::Cut, "Cut"), (Item::SelectAll, "Select all")];

#[derive(Default)]
pub struct TextMenu {
    /// A finger down while a field is being edited: which, where, since when.
    press: Option<(TouchId, Pos2, f64)>,
    /// The menu, open at this point.
    open: Option<Pos2>,
    /// Where its items were drawn last frame.
    items: Vec<(Rect, Item)>,
    /// A tap on the menu is under way: its pointer events are dropped until it lifts.
    eating: bool,
    /// The item that tap chose: after Select all the menu stays open for Copy or Cut (a second
    /// long press would put the cursor down and drop the selection).
    last: Option<Item>,
}

impl TextMenu {
    /// Before egui reads the input. `editing`: a text field has focus. True while a press is
    /// being timed (the caller asks for another frame).
    pub fn hook(&mut self, raw: &mut egui::RawInput, editing: bool, clipboard: impl FnOnce() -> Option<String>) -> bool {
        let now = raw.time.unwrap_or(0.0);
        if !editing {
            self.press = None;
            self.open = None;
        }
        let mut chosen = None;
        let mut keep = Vec::with_capacity(raw.events.len());
        for e in std::mem::take(&mut raw.events) {
            let pointer = matches!(e, Event::PointerMoved(_) | Event::PointerButton { .. } | Event::Touch { .. } | Event::PointerGone);
            if self.open.is_some() || self.eating {
                if let Event::PointerButton { pos, button: PointerButton::Primary, pressed: true, .. } = e {
                    match self.items.iter().find(|(r, _)| r.contains(pos)) {
                        Some((_, item)) => {
                            chosen = Some(*item);
                            self.press = None;
                            self.last = Some(*item);
                            self.eating = true;
                        }
                        None if !self.eating => self.open = None,
                        None => {}
                    }
                }
                if self.eating && pointer {
                    if matches!(e, Event::PointerButton { pressed: false, .. } | Event::PointerGone) {
                        self.eating = false;
                        if self.last != Some(Item::SelectAll) {
                            self.open = None;
                        }
                    }
                    continue;
                }
            }
            if editing && let Event::Touch { id, phase, pos, .. } = e {
                match phase {
                    // A finger on one of the open menu's items is a tap on the menu, not a new
                    // long press: timing it would reopen the menu where the item was tapped.
                    TouchPhase::Start => {
                        let on_menu = self.open.is_some() && self.items.iter().any(|(r, _)| r.contains(pos));
                        self.press = if on_menu { None } else { Some((id, pos, now)) };
                    }
                    TouchPhase::Move => {
                        if self.press.is_some_and(|(i, p, _)| i == id && p.distance(pos) > MOVE_SLOP) {
                            self.press = None;
                        }
                    }
                    TouchPhase::End | TouchPhase::Cancel => self.press = None,
                }
            }
            keep.push(e);
        }
        raw.events = keep;
        if let Some((_, pos, at)) = self.press
            && now - at >= LONG_PRESS
        {
            self.press = None;
            self.open = Some(pos);
        }
        if let Some(item) = chosen {
            match item {
                Item::Paste => {
                    if let Some(text) = clipboard() {
                        raw.events.push(Event::Paste(text));
                    }
                }
                Item::Copy => raw.events.push(Event::Copy),
                Item::Cut => raw.events.push(Event::Cut),
                Item::SelectAll => {
                    for pressed in [true, false] {
                        raw.events.push(Event::Key { key: Key::A, physical_key: None, pressed, repeat: false, modifiers: Modifiers::COMMAND });
                    }
                }
            }
        }
        self.press.is_some()
    }

    /// After the app's UI: the menu, above the finger.
    pub fn show(&mut self, ctx: &egui::Context) {
        self.items.clear();
        let Some(at) = self.open else { return };
        let area = egui::Area::new(egui::Id::new(AREA))
            .order(egui::Order::Tooltip)
            .fixed_pos(at + egui::vec2(-40.0, -64.0))
            .constrain(true);
        area.show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (item, label) in ITEMS {
                        let r = ui.add(egui::Button::new(label).min_size(egui::vec2(72.0, 40.0)));
                        self.items.push((r.rect, item));
                    }
                });
            });
        });
    }
}