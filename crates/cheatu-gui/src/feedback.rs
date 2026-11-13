//! Short confirmations and errors shown next to the control that caused
//! them, instead of only in the status bar.

use std::time::{Duration, Instant};

use eframe::egui;

/// How long a confirmation stays.
pub const OK_TTL: Duration = Duration::from_secs(3);

struct Note {
    at: egui::Rect,
    text: String,
    error: bool,
    since: Instant,
}

#[derive(Default)]
pub struct Feedback {
    notes: Vec<Note>,
}

impl Note {
    fn live(&self, now: Instant) -> bool {
        self.error || now.saturating_duration_since(self.since) < OK_TTL
    }
}

impl Feedback {
    /// A note below `at`, replacing one already shown there.
    fn push(&mut self, at: egui::Rect, text: String, error: bool, now: Instant) {
        self.notes.retain(|n| n.at.min.distance(at.min) > 1.0);
        self.notes.push(Note {
            at,
            text,
            error,
            since: now,
        });
    }

    /// A confirmation below `at`, gone after [`OK_TTL`].
    #[allow(dead_code)] // first callers: the scanner and My cheats
    pub fn ok(&mut self, at: egui::Rect, text: impl Into<String>, now: Instant) {
        self.push(at, text.into(), false, now);
    }

    /// An error below `at`, shown until dismissed.
    #[allow(dead_code)] // first callers: the scanner and My cheats
    pub fn error(&mut self, at: egui::Rect, text: impl Into<String>, now: Instant) {
        self.push(at, text.into(), true, now);
    }

    /// Notes still on screen at `now`.
    pub fn live(&self, now: Instant) -> usize {
        self.notes.iter().filter(|n| n.live(now)).count()
    }

    pub fn dismiss(&mut self, i: usize) {
        if i < self.notes.len() {
            self.notes.remove(i);
        }
    }

    /// Draw the notes; × dismisses an error.
    pub fn show(&mut self, ctx: &egui::Context, now: Instant) {
        self.notes.retain(|n| n.live(now));
        let mut dismiss = None;
        for (i, n) in self.notes.iter().enumerate() {
            egui::Area::new(egui::Id::new(("feedback", i)))
                .order(egui::Order::Foreground)
                .fixed_pos(n.at.left_bottom() + egui::vec2(0.0, 4.0))
                .interactable(n.error)
                .show(ctx, |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let text = egui::RichText::new(&n.text);
                            if n.error {
                                ui.label(text.color(ui.visuals().error_fg_color));
                                if ui.small_button("×").on_hover_text("Dismiss").clicked() {
                                    dismiss = Some(i);
                                }
                            } else {
                                ui.label(text);
                            }
                        });
                    });
                });
        }
        if let Some(i) = dismiss {
            self.dismiss(i);
        }
        // Repaint when the next confirmation should disappear.
        if let Some(left) = self
            .notes
            .iter()
            .filter(|n| !n.error)
            .map(|n| OK_TTL.saturating_sub(now.saturating_duration_since(n.since)))
            .min()
        {
            ctx.request_repaint_after(left);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(y: f32) -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(10.0, y), egui::vec2(50.0, 20.0))
    }

    #[test]
    fn ok_expires_after_three_seconds() {
        let t0 = Instant::now();
        let mut f = Feedback::default();
        f.ok(rect(0.0), "✔ Wrote 999", t0);
        assert_eq!(f.live(t0 + Duration::from_millis(2900)), 1);
        assert_eq!(f.live(t0 + Duration::from_millis(3100)), 0);
    }

    #[test]
    fn errors_stay_until_dismissed() {
        let t0 = Instant::now();
        let mut f = Feedback::default();
        f.error(rect(0.0), "Write failed", t0);
        assert_eq!(f.live(t0 + Duration::from_secs(600)), 1);
        f.dismiss(0);
        assert_eq!(f.live(t0), 0);
    }

    #[test]
    fn same_text_same_place_replaces_instead_of_stacking() {
        let t0 = Instant::now();
        let mut f = Feedback::default();
        f.ok(rect(0.0), "Already in My cheats", t0);
        f.ok(
            rect(0.0),
            "Already in My cheats",
            t0 + Duration::from_secs(2),
        );
        assert_eq!(
            f.live(t0 + Duration::from_millis(4000)),
            1,
            "the second one restarts the clock"
        );
        // A note at the same place with other text replaces it too.
        f.ok(rect(0.0), "✔ Added", t0 + Duration::from_secs(4));
        assert_eq!(f.live(t0 + Duration::from_secs(4)), 1);
        f.ok(rect(100.0), "✔ Added", t0 + Duration::from_secs(4));
        assert_eq!(f.live(t0 + Duration::from_secs(4)), 2);
    }
}
