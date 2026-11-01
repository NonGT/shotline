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
        ui.horizontal_wrapped(|ui| {
            for a in &anchors {
                ui.vertical(|ui| {
                    let path = self.project.root.join(&a.path);
                    if self.thumbs.show(ui, &path, 110.0, &a.path) {
                        zoom = Some(path);
                    }
                    ui.label(
                        RichText::new(format!("{} · @{}", a.label, a.handle))
                            .small()
                            .color(theme::GRAPHITE),
                    )
                    .on_hover_text(format!("from @{}; change it in Elements", a.handle));
                    let rm = remove_button(ui)
                        .on_hover_text(format!("don't send it with {r}; @{} keeps it", a.handle));
                    rm.widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::Role::Button,
                            true,
                            format!("Leave out {}", a.label),
                        )
                    });
                    if rm.clicked() {
                        leave = Some(a.path.clone());
                    }
                });
            }
            for (i, img) in s.images.iter().enumerate() {
                ui.vertical(|ui| {
                    ui.set_width(180.0);
                    let path = self.project.root.join(&img.file);
                    if self.thumbs.show(ui, &path, 110.0, &img.file) {
                        zoom = Some(path);
                    }
                    let name = label(&img.file).unwrap_or_else(|| format!("{}", i + 1));
                    ui.label(RichText::new(&name).small().color(theme::GRAPHITE));
                    if let Some(note) = self.form.image_notes.get_mut(i) {
                        ui.add(
                            egui::TextEdit::singleline(note)
                                .hint_text(RichText::new("what it shows").color(theme::DUST))
                                .desired_width(170.0),
                        )
                        .on_hover_text(format!("the prompt gets \"{name}: <this>.\""))
                        .widget_info(|| {
                            egui::WidgetInfo::labeled(
                                egui::Role::TextInput,
                                true,
                                format!("Note {}", i + 1),
                            )
                        });
                    }
                    let rm = remove_button(ui).on_hover_text(format!("take {} off {r}", img.file));
                    rm.widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::Role::Button,
                            true,
                            format!("Remove {name}"),
                        )
                    });
                    if rm.clicked() {
                        remove = Some(i);
                    }
                });
            }
        });
        if let Some(p) = zoom {
            self.zoom = Some(p);
        }
        if let Some(i) = remove {
            acts.push(Action::RemoveShotImage(r, i));
        }
        if let Some(a) = leave {
            acts.push(Action::LeaveOut(r, a, true));
        }
        if !s.leave_out.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.weak("Left out:");
                for a in &s.leave_out {
                    ui.label(RichText::new(a).small().color(theme::GRAPHITE));
                    if ui
                        .small_button("Put back")
                        .on_hover_text(format!("send it with {r} again"))
                        .clicked()
                    {
                        acts.push(Action::LeaveOut(r, a.clone(), false));
                    }
                }
            });
        }
        let models = self.image_models();
        let me = anchor::For::Shot(r);
        let making = self.maker.as_ref().is_some_and(|m| m.target == me);
        let sent: Vec<&str> = anchors.iter().map(|a| a.path.as_str()).collect();
        let left: Vec<(String, String, String)> = self
            .film
            .elements
            .iter()
            .flat_map(|(el, all)| {
                all.iter().enumerate().map(|(i, a)| {
                    (
                        format!("@{} {}", el.handle, i + 1),
                        el.handle.clone(),
                        self.project.rel(a),
                    )
                })
            })
            .filter(|(_, _, a)| !sent.contains(&a.as_str()))
            .collect();
        ui.horizontal(|ui| {
            ui.add_enabled_ui(!left.is_empty(), |ui| {
                egui::ComboBox::from_id_salt(("send anchor", r.to_string()))
                    .selected_text("Add an anchor")
                    .width(170.0)
                    .show_ui(ui, |ui| {
                        for (name, handle, a) in &left {
                            if ui.selectable_label(false, name).clicked() {
                                acts.push(Action::SendAnchor(r, handle.clone(), a.clone()));
                            }
                        }
                    })
                    .response
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
                        egui::Button::new("Shoot a take…"),
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
