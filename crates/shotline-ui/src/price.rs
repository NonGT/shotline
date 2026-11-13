//! Every paid action looks the same: "<Verb>…" asks the price (free), then "<Verb> for $X"
//! spends it and Cancel (or Esc) backs out, then a spinner while it's made.

use eframe::egui::{self, RichText};

use crate::{kit, short, theme};

/// Where a paid action is.
pub(crate) enum Price<'a> {
    Idle,
    Asking,
    Priced(&'a str),
    /// Being made, said as what's happening.
    Working(&'a str),
    /// Asking the price didn't work.
    Failed(&'a str),
}

pub(crate) enum Pressed {
    Ask,
    Go,
    Cancel,
    /// Stop what's running, with the provider.
    Stop,
    /// Set key…: the plugin needs a key that isn't set.
    Keys,
}

pub(crate) struct PriceRow<'a> {
    verb: &'a str,
    /// What it's for, for screen readers and tests: "Record line 2".
    name: Option<String>,
    big: bool,
    /// Why it can't be asked now.
    blocked: Option<String>,
    /// What key the plugin is missing, which blocks it before the rest.
    keys: Option<String>,
    go_hover: Option<String>,
    /// While working, a Cancel that stops it.
    stoppable: bool,
}

impl<'a> PriceRow<'a> {
    pub(crate) fn new(verb: &'a str) -> Self {
        PriceRow {
            verb,
            name: None,
            big: false,
            blocked: None,
            keys: None,
            go_hover: None,
            stoppable: false,
        }
    }

    pub(crate) fn stoppable(mut self) -> Self {
        self.stoppable = true;
        self
    }

    pub(crate) fn named(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// The page's main action, drawn bigger.
    pub(crate) fn big(mut self) -> Self {
        self.big = true;
        self
    }

    pub(crate) fn blocked(mut self, why: Option<String>) -> Self {
        self.blocked = why;
        self
    }

    /// From App::needs_keys.
    pub(crate) fn keys(mut self, missing: Option<String>) -> Self {
        self.keys = missing;
        self
    }

    pub(crate) fn go_hover(mut self, text: impl Into<String>) -> Self {
        self.go_hover = Some(text.into());
        self
    }

    pub(crate) fn show(self, ui: &mut egui::Ui, state: Price) -> Option<Pressed> {
        let mut out = None;
        ui.horizontal(|ui| match state {
            Price::Asking => {
                ui.spinner();
                ui.weak("asking the price…");
            }
            Price::Working(what) => {
                kit::ring(
                    ui.painter(),
                    ui.cursor().min + egui::vec2(9.0, ui.spacing().interact_size.y / 2.0),
                    7.0,
                    2.0,
                    theme::STEEL,
                    ui.input(|i| i.time),
                );
                ui.ctx().request_repaint();
                ui.add_space(24.0);
                ui.weak(what);
                if self.stoppable
                    && kit::danger_button(ui, "Cancel")
                        .on_hover_text(
                            "stops it if the provider hasn't started on it yet; once it has, it \
                             runs to the end and is charged",
                        )
                        .clicked()
                {
                    out = Some(Pressed::Stop);
                }
            }
            Price::Priced(cost) => {
                let go = crate::spend_button(ui, &format!("{} for {cost}", self.verb))
                    .on_hover_text(
                        self.go_hover
                            .unwrap_or_else(|| "spends this and writes it to the ledger".into()),
                    );
                if go.clicked() {
                    out = Some(Pressed::Go);
                } else if ui.button("Cancel").clicked()
                    || ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
                {
                    out = Some(Pressed::Cancel);
                }
            }
            Price::Idle | Price::Failed(_) => {
                let text = format!("{}…", self.verb);
                let b = if self.big {
                    egui::Button::new(
                        RichText::new(&text)
                            .family(egui::FontFamily::Name(theme::SEMIBOLD.into()))
                            .color(theme::TUNGSTEN_LIT),
                    )
                    .stroke(egui::Stroke::new(1.5, theme::TUNGSTEN))
                    .min_size(egui::vec2(200.0, 38.0))
                } else {
                    egui::Button::new(&text)
                };
                let blocked = self.keys.clone().or(self.blocked);
                let on = blocked.is_none();
                let r = ui.add_enabled(on, b);
                if let Some(name) = &self.name {
                    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, on, name));
                }
                let r = r
                    .on_hover_text("asks the price first; nothing is spent yet")
                    .on_disabled_hover_text(blocked.as_deref().unwrap_or_default());
                if r.clicked() {
                    out = Some(Pressed::Ask);
                }
                if let Some(why) = &self.keys {
                    ui.label(RichText::new(why).color(theme::DUST));
                    if ui.button("Set key…").clicked() {
                        out = Some(Pressed::Keys);
                    }
                }
                if let Price::Failed(e) = state {
                    ui.label(
                        RichText::new(format!("Couldn't get the price: {}", short(e, 80)))
                            .color(theme::FLAG),
                    )
                    .on_hover_text(e);
                }
            }
        });
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT, Queryable};

    #[derive(Default)]
    struct Row {
        keys: Option<String>,
        priced: bool,
        pressed: Vec<&'static str>,
    }

    fn harness(row: Row) -> Harness<'static, Row> {
        let mut h = Harness::new_ui_state(
            |ui, r: &mut Row| {
                let state = if r.priced {
                    Price::Priced("$0.40")
                } else {
                    Price::Idle
                };
                let got = PriceRow::new("Shoot").keys(r.keys.clone()).show(ui, state);
                r.pressed.extend(got.map(|p| match p {
                    Pressed::Ask => "ask",
                    Pressed::Go => "go",
                    Pressed::Cancel => "cancel",
                    Pressed::Keys => "keys",
                    Pressed::Stop => "stop",
                }));
            },
            row,
        );
        h.run();
        h
    }

    #[test]
    fn a_missing_key_blocks_the_price_and_offers_to_set_it() {
        let mut h = harness(Row {
            keys: Some("fal needs FAL_API_KEY".into()),
            ..Row::default()
        });
        assert!(h.get_by_label("Shoot…").accesskit_node().is_disabled());
        h.get_by_label("fal needs FAL_API_KEY");
        h.get_by_label("Set key…").click();
        h.run();
        assert_eq!(h.state().pressed, ["keys"]);
    }

    #[test]
    fn priced_it_spends_or_backs_out() {
        let mut h = harness(Row::default());
        h.get_by_label("Shoot…").click();
        h.run();
        assert_eq!(h.state().pressed, ["ask"]);

        let mut h = harness(Row {
            priced: true,
            ..Row::default()
        });
        assert!(h.query_by_label("Shoot…").is_none());
        h.get_by_label("Shoot for $0.40").click();
        h.run();
        h.key_press(egui::Key::Escape);
        h.run();
        assert_eq!(h.state().pressed, ["go", "cancel"]);
    }
}
