//! Committing the film from the app, or starting its git history.

use eframe::egui::{self, RichText};
use shotline_core::change_summary;

use crate::edit::History;
use crate::{App, caption, theme};

#[derive(Default)]
pub(crate) enum Repo {
    /// No git, or not looked yet.
    #[default]
    None,
    NotYet,
    Changes(Vec<String>),
}

pub(crate) enum Dialog {
    Commit(String),
    Start,
}

impl App {
    pub(crate) fn refresh_repo(&mut self) {
        let dir = &self.project.root;
        self.repo = match &self.git {
            None => Repo::None,
            Some(g) if !g.is_repo(dir) => Repo::NotYet,
            Some(g) => Repo::Changes(g.changes(dir).unwrap_or_default()),
        };
    }

    /// The bar's git button, right of Undo and Redo.
    pub(crate) fn git_button(&mut self, ui: &mut egui::Ui) {
        match &self.repo {
            Repo::None => {}
            Repo::NotYet => {
                if ui
                    .button("Keep history with git")
                    .on_hover_text("start a git repository for this film")
                    .clicked()
                {
                    self.status = None;
                    self.dialog = Some(Dialog::Start);
                }
            }
            Repo::Changes(c) if c.is_empty() => {}
            Repo::Changes(c) => {
                let n = c.len();
                let text = format!("Commit {n} change{}", if n == 1 { "" } else { "s" });
                if ui.button(text).clicked() {
                    self.flush();
                    self.refresh_repo();
                    self.status = None;
                    if let Repo::Changes(c) = &self.repo {
                        self.dialog = Some(Dialog::Commit(change_summary(c)));
                    }
                }
            }
        }
    }

    pub(crate) fn git_dialog(&mut self, ui: &mut egui::Ui) {
        let Some(dialog) = &mut self.dialog else {
            return;
        };
        let mut go = false;
        let mut close = false;
        let changes = match &self.repo {
            Repo::Changes(c) => c.as_slice(),
            _ => &[],
        };
        let film = self.project.root.display().to_string();
        let modal = egui::Modal::new(egui::Id::unique("git")).show(ui.ctx(), |ui| {
            ui.set_width(440.0);
            match dialog {
                Dialog::Commit(message) => {
                    ui.heading("Commit to git");
                    ui.add_space(10.0);
                    let label = caption(ui, "Message");
                    let field = ui
                        .add(egui::TextEdit::singleline(message).desired_width(f32::INFINITY))
                        .labelled_by(label.id);
                    if ui.memory(|m| m.focused().is_none()) {
                        field.request_focus();
                    }
                    go = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    ui.add_space(8.0);
                    for c in changes.iter().take(8) {
                        ui.label(RichText::new(c).color(theme::DUST));
                    }
                    if changes.len() > 8 {
                        ui.label(
                            RichText::new(format!("and {} more", changes.len() - 8))
                                .color(theme::DUST),
                        );
                    }
                }
                Dialog::Start => {
                    ui.heading("Keep history with git");
                    ui.add_space(10.0);
                    ui.label(format!(
                        "Starts a git repository in {film} and commits the film as it is now. \
                         Video files of takes stay out of it, as .gitignore says."
                    ));
                }
            }
            if let Some((e, true)) = &self.status {
                ui.add_space(8.0);
                ui.label(RichText::new(e).color(theme::FLAG));
            }
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                let ok = match dialog {
                    Dialog::Commit(m) => {
                        ui.add_enabled(!m.trim().is_empty(), egui::Button::new("Commit"))
                    }
                    Dialog::Start => ui.button("Start history"),
                };
                go |= ok.clicked();
                close = ui.button("Cancel").clicked();
            });
        });
        if go {
            self.run_git();
        } else if close || modal.should_close() {
            self.dialog = None;
        }
    }

    fn run_git(&mut self) {
        let (Some(git), Some(dialog)) = (&self.git, self.dialog.take()) else {
            return;
        };
        let dir = &self.project.root;
        let done = match &dialog {
            Dialog::Commit(m) => git
                .commit(dir, m.trim())
                .map(|h| format!("committed {h}: {}", m.trim())),
            Dialog::Start => {
                let m = format!("start {}", self.project.config.name);
                git.init(dir, &m)
                    .map(|h| format!("git history started at {h}"))
            }
        };
        match done {
            Ok(msg) => {
                self.info(msg);
                // The commit is the checkpoint now; going further back is git's job.
                self.history = History::default();
            }
            Err(e) => {
                self.error(e.to_string());
                self.dialog = Some(dialog);
            }
        }
        self.refresh_repo();
    }
}
