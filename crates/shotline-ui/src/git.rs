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
                if crate::kit::ghost_button(ui, crate::icon::GIT, "Keep history with git")
                    .on_hover_text("start a git repository for this film")
                    .clicked()
                {
                    self.git_error = None;
                    self.dialog = Some(Dialog::Start);
                }
            }
            Repo::Changes(c) if c.is_empty() => {
                ui.add_enabled_ui(false, |ui| {
                    crate::kit::ghost_button(ui, crate::icon::GIT, "Nothing to commit")
                });
            }
            Repo::Changes(c) => {
                let n = c.len();
                let text = format!("Commit {n} change{}", if n == 1 { "" } else { "s" });
                if crate::kit::ghost_button(ui, crate::icon::GIT, &text).clicked() {
                    self.ask_commit();
                }
            }
        }
    }

    /// Film > Commit…, or the bar's button: the dialog for what changed, or for starting a
    /// history when there's none. Says so when there's nothing to commit.
    pub(crate) fn ask_commit(&mut self) {
        self.flush();
        self.refresh_repo();
        self.git_error = None;
        match &self.repo {
            Repo::None => self.error("git isn't installed".to_string()),
            Repo::NotYet => self.dialog = Some(Dialog::Start),
            Repo::Changes(c) if c.is_empty() => self.info("nothing to commit".to_string()),
            Repo::Changes(c) => self.dialog = Some(Dialog::Commit(change_summary(c))),
        }
    }

    pub(crate) fn git_dialog(&mut self, ui: &mut egui::Ui) {
        let Some(dialog) = &mut self.dialog else {
            return;
        };
        let mut go = false;
        let mut close = false;
        let mut entered = false;
        let changes = match &self.repo {
            Repo::Changes(c) => c.as_slice(),
            _ => &[],
        };
        let film = self.project.root.display().to_string();
        let has_undo = self.history.undo_what().is_some();
        let modal = crate::modal(ui.ctx(), "git", |ui| {
            ui.set_width(crate::kit::modal_width(ui.ctx(), 1.0, 440.0, 440.0));
            match dialog {
                Dialog::Commit(message) => {
                    ui.heading("Commit to git");
                    ui.add_space(10.0);
                    let label = caption(ui, "Message");
                    let field = ui
                        .add(egui::TextEdit::singleline(message).desired_width(f32::INFINITY))
                        .labelled_by(label.id);
                    entered = crate::kit::entered(ui, &field);
                    if ui.memory(|m| m.focused().is_none()) {
                        field.request_focus();
                    }
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
            if matches!(dialog, Dialog::Commit(_)) && has_undo {
                ui.add_space(8.0);
                crate::hint(
                    ui,
                    "Undo starts again from this commit; going further back is git's job.",
                );
            }
            if let Some(e) = &self.git_error {
                ui.add_space(8.0);
                ui.label(RichText::new(e).color(theme::FLAG));
            }
            ui.add_space(14.0);
            let (main, ready) = match dialog {
                Dialog::Commit(m) => ("Commit", !m.trim().is_empty()),
                Dialog::Start => ("Start history", true),
            };
            (go, close) = crate::kit::footer(ui, main, crate::kit::Main::Primary, ready, "Cancel");
            go |= ready && entered;
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
                self.git_error = Some(e.to_string());
                self.dialog = Some(dialog);
            }
        }
        self.refresh_repo();
    }
}
