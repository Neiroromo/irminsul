//! The chests tab: the chests that have come into view, and which of them the
//! account has opened.

use egui::{Color32, RichText, ScrollArea, Sense, Ui, vec2};

use crate::achievement_data::Language;
use crate::achievement_tab::DIM;
use crate::chests::{Chest, Snapshot, kind_name, kinds, tier_name};
use crate::roster_tab::language_choice;

const OPENED: Color32 = Color32::from_rgb(0x4a, 0xde, 0x80);
const CLOSED: Color32 = Color32::from_rgb(0x6a, 0xb7, 0xff);
const ROW: f32 = 30.;

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    All,
    Closed,
    Opened,
}

pub struct ChestsTab {
    view: View,
    /// Only the chests of this tier.
    tier: Option<u8>,
}

impl ChestsTab {
    pub fn new() -> Self {
        Self {
            view: View::All,
            tier: None,
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, snapshot: &Snapshot, language: &mut Language) {
        let russian = *language == Language::Ru;
        let tr = |en: &'static str, ru: &'static str| if russian { ru } else { en };

        ui.horizontal(|ui| {
            ui.label(RichText::new(tr("Chests", "Сундуки")).size(34.).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                language_choice(ui, language);
            });
        });
        let russian = *language == Language::Ru;
        let tr = |en: &'static str, ru: &'static str| if russian { ru } else { en };

        let (opened, known) = snapshot.totals();
        ui.label(
            RichText::new(format!(
                "{}: {opened} / {known}",
                tr("Opened of the chests seen", "Открыто из увиденных сундуков")
            ))
            .size(18.),
        );
        ui.label(
            RichText::new(tr(
                "The game does not list the chests: each is added when it comes into view while \
                 capture runs, and marked opened when you open it.",
                "Игра не присылает список сундуков: каждый добавляется, когда он попадает в поле \
                 зрения при включённом захвате, и отмечается открытым, когда вы его открываете.",
            ))
            .size(12.)
            .color(DIM),
        );
        ui.add_space(8.);

        // Per kind: opened of seen, and a click shows only these.
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 10.;
            ui.selectable_value(&mut self.tier, None, RichText::new(tr("All kinds", "Все виды")).size(15.));
            for tier in 1..=4u8 {
                let of_tier: Vec<&Chest> = snapshot
                    .chests
                    .iter()
                    .filter(|chest| chest.tier() == tier)
                    .collect();
                let done = of_tier.iter().filter(|chest| chest.opened_at.is_some()).count();
                ui.selectable_value(
                    &mut self.tier,
                    Some(tier),
                    RichText::new(format!(
                        "{} {done}/{}",
                        tier_name(tier, russian),
                        of_tier.len()
                    ))
                    .size(15.),
                );
            }
        });
        ui.horizontal(|ui| {
            for (view, label) in [
                (View::All, tr("All", "Все")),
                (View::Closed, tr("Not opened", "Не открыты")),
                (View::Opened, tr("Opened", "Открыты")),
            ] {
                ui.selectable_value(&mut self.view, view, RichText::new(label).size(15.));
            }
        });
        ui.add_space(6.);
        ui.separator();

        let mut shown: Vec<&Chest> = snapshot
            .chests
            .iter()
            .filter(|chest| self.tier.is_none_or(|tier| chest.tier() == tier))
            .filter(|chest| match self.view {
                View::All => true,
                View::Closed => chest.opened_at.is_none(),
                View::Opened => chest.opened_at.is_some(),
            })
            .collect();
        // The newest first: opened by when it was opened, else by when it was seen.
        shown.sort_by(|a, b| {
            let when = |chest: &Chest| chest.opened_at.clone().unwrap_or_else(|| chest.seen_at.clone());
            when(b).cmp(&when(a))
        });

        if snapshot.chests.is_empty() {
            ui.add_space(20.);
            ui.label(
                RichText::new(tr(
                    "No chests yet. Start capture, enter the game and walk around.",
                    "Сундуков пока нет. Включите захват, зайдите в игру и пройдитесь по миру.",
                ))
                .size(16.)
                .color(DIM),
            );
            return;
        }

        ScrollArea::vertical()
            .auto_shrink([false, false])
            .show_rows(ui, ROW, shown.len(), |ui, range| {
                for chest in &shown[range] {
                    row(ui, chest, russian);
                }
            });
    }
}

/// `2026-11-07T23:39:03+03:00` as `2026-11-07 23:39`.
fn short_time(time: &str) -> String {
    time.chars()
        .take(16)
        .map(|c| if c == 'T' { ' ' } else { c })
        .collect()
}

fn row(ui: &mut Ui, chest: &Chest, russian: bool) {
    let opened = chest.opened_at.is_some();
    ui.horizontal(|ui| {
        ui.set_height(ROW);
        let (rect, _) = ui.allocate_exact_size(vec2(16., ROW), Sense::hover());
        ui.painter().circle_filled(
            rect.center(),
            6.,
            if opened { OPENED } else { CLOSED },
        );

        let mut name = kind_name(chest.gadget, russian);
        if kinds().get(&chest.gadget).is_some_and(|kind| kind.locked) {
            name.push_str(if russian { " (запертый)" } else { " (locked)" });
        }
        if kinds().get(&chest.gadget).is_some_and(|kind| kind.water) {
            name.push_str(if russian { " (под водой)" } else { " (underwater)" });
        }
        ui.label(RichText::new(name).size(16.));

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let status = match &chest.opened_at {
                Some(at) => format!(
                    "{} {}",
                    if russian { "открыт" } else { "opened" },
                    short_time(at)
                ),
                None => format!(
                    "{} {}",
                    if russian { "замечен" } else { "seen" },
                    short_time(&chest.seen_at)
                ),
            };
            ui.label(
                RichText::new(status)
                    .size(14.)
                    .color(if opened { OPENED } else { DIM }),
            );
            if let Some([x, _, z]) = chest.position {
                ui.add_space(14.);
                ui.label(
                    RichText::new(format!("x {x:.0}  z {z:.0}"))
                        .size(14.)
                        .color(DIM),
                )
                .on_hover_text(format!("{} {}", chest.group, chest.config));
            }
        });
    });
}
