//! On a shot's page: the pictures sent with it, the prompt as the model gets it, and shooting a
//! take, priced first and spent only on a second click.

use eframe::egui::{self, RichText};
use shotline_core::{Built, Catalog, Shot, ShotRef};

use crate::edit::Action;
use crate::{App, anchor, hint, remove_button, short, spend_button, theme};

#[derive(Clone, Default)]
pub(crate) enum Shoot {
    #[default]
    Idle,
    Pricing(ShotRef),
    /// What a take of the shot costs.
    Priced(ShotRef, String),
    /// What it's doing now, e.g. "running (40%)".
    Running(ShotRef, String),
    /// A take ran, or tried to: a line about it, or why it failed.
    Done(ShotRef, Result<String, String>),
    Failed(ShotRef, String),
}

impl Shoot {
    fn shot(&self) -> Option<ShotRef> {
        match self {
            Shoot::Idle => None,
            Shoot::Pricing(r)
            | Shoot::Priced(r, _)
            | Shoot::Running(r, _)
            | Shoot::Done(r, _)
            | Shoot::Failed(r, _) => Some(*r),
        }
    }
}

impl App {
    /// The shot as it would be sent, worked out once per reload.
    pub(crate) fn built(&mut self, r: ShotRef) -> Result<Built, String> {
        if let Some((at, b)) = &self.built
            && *at == r
        {
            return b.clone();
        }
        let mut c = Catalog::builtin();
        for d in &self.plugins {
            c.add_plugin(&d.name, d.models.clone());
        }
        let b = self
            .project
            .catalog(c)
            .and_then(|c| shotline_core::build(&self.project, &c, r))
            .map_err(|e| e.to_string());
        self.built = Some((r, b.clone()));
        b
    }

    /// The pictures that go with the shot: the elements' anchors, then its own.
    pub(crate) fn references(
        &mut self,
        ui: &mut egui::Ui,
        r: ShotRef,
        s: &Shot,
        acts: &mut Vec<Action>,
    ) {
        let built = self.built(r).ok();
        let label = |path: &str| {
            built.as_ref().and_then(|b| {
                b.references
                    .iter()
                    .find(|x| x.path == path)
                    .map(|x| x.label.clone())
            })
        };
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("References")
                    .text_style(egui::TextStyle::Heading)
                    .color(theme::PAPER),
            );
            if let Some(b) = &built
                && let Some(max) = self
                    .plugins
                    .iter()
                    .filter(|d| d.name == b.plugin)
                    .flat_map(|d| &d.models)
                    .find(|m| m.id == b.model)
                    .and_then(|m| m.max_images)
            {
                // The storyboard frame takes one.
                let max = max.saturating_sub(b.frame_ref as usize);
                ui.label(
                    RichText::new(format!("up to {max}"))
                        .small()
                        .color(theme::DUST),
                );
            }
        });
        ui.add_space(4.0);
        let anchors: Vec<_> = built
            .as_ref()
            .map(|b| {
                b.references
                    .iter()
                    .filter(|x| !x.handle.is_empty())
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        if anchors.is_empty() && s.images.is_empty() {
            hint(
                ui,
                "Pictures the video model is shown. Elements named in the prompt bring their \
                 anchors; add others from Elements, or the shot's own, e.g. a place or a prop \
                 that isn't an element.",
            );
        }
        let mut zoom = None;
        let mut remove = None;
        let mut leave = None;
        // One picture a row, its note beside it with room to say what it shows.
        const TILE: f32 = 76.0;
        // "<Picture 2>" reads as "Picture 2".
        let plain = |l: &str| l.trim_matches(|c| c == '<' || c == '>').to_string();
        let total = anchors.len() + s.images.len();
        for k in 0..total {
            if k > 0 {
                ui.add_space(8.0);
            }
            ui.horizontal_top(|ui| {
                let anchor = anchors.get(k);
                let i = k.saturating_sub(anchors.len());
                let (file, name, from) = match anchor {
                    Some(a) => (a.path.clone(), a.label.clone(), Some(&a.handle)),
                    None => {
                        let f = s.images[i].file.clone();
                        let name = label(&f).unwrap_or_else(|| format!("{}", i + 1));
                        (f, name, None)
                    }
                };
                let path = self.project.root.join(&file);
                if self.thumbs.tile(ui, &path, TILE, &file) {
                    zoom = Some(path);
                }
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 6.0;
                    ui.horizontal(|ui| {
                        let text = match from {
                            Some(h) => format!("{} · @{h}", plain(&name)),
                            None => plain(&name),
                        };
                        let hover = match from {
                            Some(h) => format!("{file}, from @{h}; change it in Elements"),
                            None => file.clone(),
                        };
                        ui.label(RichText::new(text).small().color(theme::GRAPHITE))
                            .on_hover_text(hover);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let (hover, label) = match from {
                                Some(h) => (
                                    format!("don't send it with {r}; @{h} keeps it"),
                                    format!("Leave out {name}"),
                                ),
                                None => (format!("take {file} off {r}"), format!("Remove {name}")),
                            };
                            let rm = remove_button(ui).on_hover_text(hover);
                            rm.widget_info(|| {
                                egui::WidgetInfo::labeled(egui::Role::Button, true, &label)
                            });
                            if rm.clicked() {
                                match from {
                                    Some(_) => leave = Some(file.clone()),
                                    None => remove = Some(i),
                                }
                            }
                        });
                    });
                    let note = match from {
                        Some(_) => Some(self.form.anchor_notes.entry(file.clone()).or_default()),
                        None => self.form.image_notes.get_mut(i),
                    };
                    if let Some(note) = note {
                        let said = note.trim();
                        let hover = if said.is_empty() {
                            format!(
                                "what it shows in this shot; goes in the prompt as \"{name}: …\""
                            )
                        } else {
                            format!("the prompt gets \"{name}: {said}.\"")
                        };
                        let hint = if from.is_some() {
                            "what it shows here, e.g. from behind"
                        } else {
                            "what it shows, e.g. the café at night"
                        };
                        ui.add(
                            egui::TextEdit::multiline(note)
                                .hint_text(RichText::new(hint).color(theme::DUST))
                                .desired_rows(2)
                                .desired_width(ui.available_width()),
                        )
                        .on_hover_text(hover)
                        .widget_info(|| {
                            egui::WidgetInfo::labeled(
                                egui::Role::MultilineTextInput,
                                true,
                                format!("Note on {name}"),
                            )
                        });
                    }
                });
            });
        }
        ui.add_space(8.0);
        if let Some(p) = zoom {
            self.zoom = Some(p);
        }
        if let Some(i) = remove {
            acts.push(Action::RemoveShotImage(r, i));
        }
        if let Some(a) = leave {
            acts.push(Action::LeaveOut(r, a, true));
        }
        let models = self.image_models();
        let me = anchor::For::Shot(r);
        let making = self.maker.as_ref().is_some_and(|m| m.target == me);
        let sent: Vec<&str> = anchors.iter().map(|a| a.path.as_str()).collect();
        let left: Vec<(String, String, (String, String, std::path::PathBuf))> = self
            .film
            .elements
            .iter()
            .flat_map(|(el, all)| {
                let group = format!("@{}  {}", el.handle, el.name);
                all.iter().enumerate().map(move |(i, a)| {
                    (
                        "Elements".to_string(),
                        group.clone(),
                        (
                            format!("@{} {}", el.handle, i + 1),
                            el.handle.clone(),
                            a.clone(),
                        ),
                    )
                })
            })
            .filter(|(_, _, (_, _, a))| !sent.contains(&self.project.rel(a).as_str()))
            .collect();
        // Wrapped: the column is narrow.
        ui.horizontal_wrapped(|ui| {
            ui.add_enabled_ui(!left.is_empty(), |ui| {
                let mut picked = None;
                let thumbs = &mut self.thumbs;
                let open = egui::ComboBox::from_id_salt(("send anchor", r.to_string()))
                    .selected_text("Add an anchor")
                    .width(170.0)
                    .height(crate::grouped::MAX)
                    .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                    .show_ui(ui, |ui| {
                        ui.set_width(320.0);
                        let grown = crate::grouped::list(
                            ui,
                            &left,
                            |ui, (name, handle, a)| {
                                let hover = self.project.rel(a);
                                if crate::grouped::picture(ui, thumbs, a, name, &hover) {
                                    picked = Some((handle.clone(), hover));
                                }
                            },
                            |_| {},
                        );
                        crate::grouped::remeasure_later(ui, grown);
                        if picked.is_some() {
                            ui.close();
                        }
                    });
                crate::grouped::remeasure(ui.ctx());
                if let Some((handle, a)) = picked {
                    acts.push(Action::SendAnchor(r, handle, a));
                }
                open.response
                    .on_hover_text("an element's anchor image, sent with this shot only")
                    .on_disabled_hover_text("every anchor is sent already")
                    .widget_info(|| {
                        egui::WidgetInfo::labeled(egui::Role::ComboBox, true, "Add an anchor")
                    });
            });
            if ui.button("Add image…").clicked()
                && let Some(file) = rfd::FileDialog::new()
                    .add_filter("Images", &["png", "jpg", "jpeg", "webp"])
                    .pick_file()
            {
                acts.push(Action::AddShotImage(r, file));
            }
            let busy = self.making_for();
            if !models.is_empty()
                && !making
                && ui
                    .add_enabled(busy.is_none(), egui::Button::new("Make image…"))
                    .on_disabled_hover_text(format!(
                        "still making one for {}",
                        busy.unwrap_or_default()
                    ))
                    .clicked()
            {
                let film = &self.project.config;
                let cfg = &film.image;
                let aspect = s
                    .model
                    .as_ref()
                    .and_then(|m| m.aspect.clone())
                    .unwrap_or_else(|| film.aspect.clone());
                self.maker = Some(anchor::Maker::new(
                    me.clone(),
                    vec![],
                    &models,
                    (&cfg.plugin, &cfg.model),
                    (aspect, cfg.resolution.clone()),
                ));
            }
        });
        if making {
            ui.add_space(6.0);
            self.show_maker(ui, &models);
        }
    }

    /// What the video model gets, and what might go wrong with it.
    pub(crate) fn sent(&mut self, ui: &mut egui::Ui, r: ShotRef) {
        match self.built(r) {
            Ok(b) => {
                // Said once, e.g. one cast member with two lines and no voice.
                let mut said = std::collections::HashSet::new();
                for w in b.warnings.iter().filter(|w| said.insert(*w)) {
                    ui.label(RichText::new(w).color(theme::FLAG));
                }
                for c in &b.cautions {
                    ui.label(RichText::new(c).color(theme::TUNGSTEN));
                }
                egui::CollapsingHeader::new(
                    RichText::new("What the model gets").color(theme::GRAPHITE),
                )
                .id_salt(("sent", r.to_string()))
                .show(ui, |ui| {
                    let mut about = format!(
                        "{} · {} · {}s · {} · {}",
                        b.plugin,
                        b.model,
                        shotline_core::ledger::money(b.duration),
                        b.resolution,
                        b.aspect
                    );
                    if let Some(f) = &b.start_frame {
                        about.push_str(&format!(" · starts on {}", f.path));
                    }
                    ui.weak(about);
                    ui.add_space(4.0);
                    ui.add(egui::Label::new(b.text.as_str()).selectable(true).wrap());
                });
            }
            Err(e) => {
                ui.label(RichText::new(e).color(theme::FLAG));
            }
        }
    }

    /// A take that ran, once: the shot's takes are read again.
    pub(crate) fn shoot_finished(&mut self) {
        let done = {
            let mut s = self.shoot.lock().unwrap();
            let Shoot::Done(r, out) = s.clone() else {
                return;
            };
            *s = match &out {
                Ok(_) => Shoot::Idle,
                Err(e) => Shoot::Failed(r, e.clone()),
            };
            (r, out)
        };
        self.reload();
        if let Err(e) = self.load_takes() {
            self.error(e.to_string());
        }
        match done.1 {
            Ok(msg) => self.info(msg),
            Err(e) => self.error(format!("{}: {e}", done.0)),
        }
    }

    /// Get price, then the spend button, then progress.
    pub(crate) fn shoot_row(&mut self, ui: &mut egui::Ui, r: ShotRef) {
        let state = self.shoot.lock().unwrap().clone();
        let elsewhere = state
            .shot()
            .filter(|x| *x != r && matches!(state, Shoot::Pricing(_) | Shoot::Running(..)));
        ui.horizontal(|ui| match &state {
            Shoot::Pricing(x) if *x == r => {
                ui.spinner();
                ui.weak("asking the price…");
            }
            Shoot::Running(x, step) if *x == r => {
                ui.spinner();
                ui.weak(format!("shooting; this takes a few minutes · {step}"));
            }
            Shoot::Priced(x, cost) if *x == r => {
                if spend_button(ui, &format!("Shoot it for {cost}"))
                    .on_hover_text("spends this and writes it to the ledger")
                    .clicked()
                {
                    self.start_shoot(ui.ctx(), r, Some(cost.clone()));
                }
                if ui.button("Cancel").clicked() {
                    *self.shoot.lock().unwrap() = Shoot::Idle;
                }
            }
            _ => {
                let host = self.host.is_some();
                if ui
                    .add_enabled(
                        host && elsewhere.is_none(),
                        egui::Button::new(
                            RichText::new("Shoot a take…")
                                .family(egui::FontFamily::Name(theme::SEMIBOLD.into()))
                                .color(theme::TUNGSTEN_LIT),
                        )
                        .stroke(egui::Stroke::new(1.5, theme::TUNGSTEN))
                        .min_size(egui::vec2(140.0, 32.0)),
                    )
                    .on_hover_text("asks the price first; nothing is spent yet")
                    .on_disabled_hover_text(match elsewhere {
                        Some(x) => format!("still busy with {x}"),
                        None => "no plugins".into(),
                    })
                    .clicked()
                {
                    self.start_shoot(ui.ctx(), r, None);
                }
                if let Shoot::Failed(x, e) = &state
                    && *x == r
                {
                    ui.label(RichText::new(short(e, 80)).color(theme::FLAG))
                        .on_hover_text(e);
                }
            }
        });
    }

    /// Prices a take, or with `at` shoots one at that price.
    fn start_shoot(&mut self, ctx: &egui::Context, r: ShotRef, at: Option<String>) {
        // What's on screen is what's sent.
        self.flush();
        let Some(host) = self.host.clone() else {
            return;
        };
        *self.shoot.lock().unwrap() = match at {
            Some(_) => Shoot::Running(r, "starting".into()),
            None => Shoot::Pricing(r),
        };
        let (state, ctx, film) = (self.shoot.clone(), ctx.clone(), self.project.root.clone());
        let translate = self.translate_prompt;
        std::thread::spawn(move || {
            let done = match at {
                None => match host.take_quote(&film, r, translate) {
                    Ok(cost) => Shoot::Priced(r, cost),
                    Err(e) => Shoot::Failed(r, format!("{e:#}")),
                },
                Some(at) => {
                    let (step, c) = (state.clone(), ctx.clone());
                    let mut on = move |s: String| {
                        *step.lock().unwrap() = Shoot::Running(r, s);
                        c.request_repaint();
                    };
                    let out = host.take(&film, r, &at, translate, &mut on);
                    Shoot::Done(r, out.map_err(|e| format!("{e:#}")))
                }
            };
            *state.lock().unwrap() = done;
            ctx.request_repaint();
        });
    }
}
