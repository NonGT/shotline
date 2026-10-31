//! What the window shows before a film is open: make a new one or pick one.

use std::path::{Path, PathBuf};

use eframe::egui::{self, RichText};
use shotline_core::slug::slugify;
use shotline_core::{Git, Project};

use crate::{App, caption, hint, read, theme};

const RECENT: usize = 8;

pub struct Shell {
    user_dir: PathBuf,
    git: Option<Git>,
    testing: bool,
    themed: bool,
    screen: Screen,
}

enum Screen {
    Start(Start),
    Film(Box<App>),
}

struct Start {
    name: String,
    folder: String,
    git: bool,
    recent: Vec<(PathBuf, String)>,
    error: Option<String>,
}

impl Shell {
    /// Opens `project`, or without one asks for one; new films go in `here` unless changed.
    pub fn new(project: Option<Project>, user_dir: PathBuf, here: &Path) -> Shell {
        let start = Start {
            name: String::new(),
            folder: here.display().to_string(),
            git: true,
            recent: recent(&user_dir),
            error: None,
        };
        let mut shell = Shell {
            user_dir,
            git: Git::find(),
            testing: false,
            themed: false,
            screen: Screen::Start(start),
        };
        if let Some(p) = project {
            shell.open(p, None);
        }
        shell
    }

    pub fn with_git(mut self, git: Option<Git>) -> Shell {
        self.git = git;
        self
    }

    /// No player, sound or remembered place, as in the app's own tests.
    pub fn testing(mut self) -> Shell {
        self.testing = true;
        self
    }

    pub fn film(&self) -> Option<&App> {
        match &self.screen {
            Screen::Film(app) => Some(app),
            Screen::Start(_) => None,
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        let opened = match &mut self.screen {
            Screen::Film(app) => return app.show(ui),
            // The fonts only take from the next frame on.
            Screen::Start(_) if !self.themed => {
                self.themed = true;
                theme::apply(ui.ctx());
                ui.ctx().request_repaint();
                return;
            }
            Screen::Start(s) => s.show(ui, self.git.is_some()),
        };
        let Some(dir) = opened else { return };
        let made = match dir {
            Opened::Existing(dir) => Project::discover(&dir).map(|p| (p, None)),
            Opened::New { dir, name, git } => self.create(&dir, &name, git),
        };
        match made {
            Ok((p, note)) => {
                ui.ctx()
                    .send_viewport_cmd(egui::ViewportCommand::Title(title(Some(&p))));
                self.open(p, note);
            }
            Err(e) => {
                if let Screen::Start(s) = &mut self.screen {
                    s.error = Some(e.to_string());
                }
            }
        }
    }

    fn create(
        &self,
        dir: &Path,
        name: &str,
        git: bool,
    ) -> shotline_core::Result<(Project, Option<Note>)> {
        let p = Project::init(dir, Some(name))?;
        let note = match &self.git {
            Some(g) if git && !g.is_repo(dir) => {
                match g.init(dir, &format!("start {}", p.config.name)) {
                    Ok(h) => Ok(format!("made {name}, git history started at {h}")),
                    Err(e) => Err(format!("made {name} but not its git history: {e}")),
                }
            }
            _ => Ok(format!("made {name}")),
        };
        Ok((p, Some(note)))
    }

    fn open(&mut self, p: Project, note: Option<Note>) {
        remember(&self.user_dir, &p.root);
        let mut app = App::new(p, self.user_dir.clone()).with_git(self.git.clone());
        app = if self.testing {
            app.with_viewer(None).silent()
        } else {
            app.remember_place()
        };
        match note {
            Some(Ok(n)) => app.info(n),
            Some(Err(e)) => app.error(e),
            None => {}
        }
        self.screen = Screen::Film(Box::new(app));
    }
}

/// What to say once the film is open, good or bad.
type Note = Result<String, String>;

enum Opened {
    Existing(PathBuf),
    New {
        dir: PathBuf,
        name: String,
        git: bool,
    },
}

impl Start {
    fn show(&mut self, ui: &mut egui::Ui, has_git: bool) -> Option<Opened> {
        let mut out = None;
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                let w = 460.0_f32.min(ui.available_width());
                ui.add_space(72.0);
                ui.vertical_centered(|ui| {
                    ui.set_max_width(w);
                    ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                        out = self.new_film(ui, has_git).or_else(|| self.open_film(ui));
                    });
                });
            });
        });
        out
    }

    fn new_film(&mut self, ui: &mut egui::Ui, has_git: bool) -> Option<Opened> {
        ui.label(
            RichText::new("shotline")
                .font(theme::slate())
                .color(theme::PAPER),
        );
        ui.add_space(32.0);
        ui.heading("New film");
        ui.add_space(10.0);
        let label = caption(ui, "Name");
        let name = ui
            .add(
                egui::TextEdit::singleline(&mut self.name)
                    .hint_text(RichText::new("What the film is called").color(theme::DUST))
                    .desired_width(f32::INFINITY)
                    .margin(egui::vec2(8.0, 6.0)),
            )
            .labelled_by(label.id);
        ui.add_space(10.0);
        let label = caption(ui, "Goes in");
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.folder)
                    .desired_width(ui.available_width() - 84.0)
                    .margin(egui::vec2(8.0, 6.0)),
            )
            .labelled_by(label.id);
            let choose = ui.button("Choose");
            if choose.clicked()
                && let Some(d) = rfd::FileDialog::new()
                    .set_directory(expand(&self.folder))
                    .pick_folder()
            {
                self.folder = d.display().to_string();
            }
        });
        let dir = self.new_dir();
        if let Some(d) = &dir {
            hint(ui, &format!("Makes {}", d.display()));
        }
        ui.add_space(6.0);
        if has_git {
            ui.checkbox(&mut self.git, "Keep history with git");
        }
        ui.add_space(10.0);
        let create = ui.add_enabled(dir.is_some(), egui::Button::new("Create film"));
        let enter = name.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if let Some(e) = &self.error {
            ui.add_space(6.0);
            ui.label(RichText::new(e).color(theme::FLAG));
        }
        match dir {
            Some(dir) if create.clicked() || enter => Some(Opened::New {
                dir,
                name: self.name.trim().into(),
                git: has_git && self.git,
            }),
            _ => None,
        }
    }

    fn open_film(&mut self, ui: &mut egui::Ui) -> Option<Opened> {
        let mut out = None;
        ui.add_space(36.0);
        ui.heading("Open a film");
        ui.add_space(10.0);
        if ui.button("Choose a folder").clicked()
            && let Some(d) = rfd::FileDialog::new()
                .set_directory(expand(&self.folder))
                .pick_folder()
        {
            out = Some(Opened::Existing(d));
        }
        if !self.recent.is_empty() {
            ui.add_space(14.0);
            caption(ui, "Recent");
        }
        for (dir, name) in &self.recent {
            let row = ui.add(
                egui::Button::new(
                    RichText::new(format!("{name}  {}", dir.display())).color(theme::GRAPHITE),
                )
                .frame(false),
            );
            if row.clicked() {
                out = Some(Opened::Existing(dir.clone()));
            }
        }
        out
    }

    /// Where a film with the typed name would go.
    fn new_dir(&self) -> Option<PathBuf> {
        let name = self.name.trim();
        if name.is_empty() || self.folder.trim().is_empty() {
            return None;
        }
        // Thai names have no ASCII for a folder name.
        let slug = Some(slugify(name)).filter(|s| !s.is_empty());
        Some(expand(&self.folder).join(slug.as_deref().unwrap_or("film")))
    }
}

fn expand(folder: &str) -> PathBuf {
    let folder = folder.trim();
    match folder.strip_prefix("~/").or((folder == "~").then_some("")) {
        Some(rest) => std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map_or_else(|| PathBuf::from(folder), |h| PathBuf::from(h).join(rest)),
        None => PathBuf::from(folder),
    }
}

pub(crate) fn title(p: Option<&Project>) -> String {
    match p {
        Some(p) => format!("{} - shotline", p.config.name),
        None => "shotline".into(),
    }
}

fn recent_file(user_dir: &Path) -> PathBuf {
    user_dir.join("recent-films")
}

/// Films opened lately, newest first, leaving out any that have gone.
fn recent(user_dir: &Path) -> Vec<(PathBuf, String)> {
    read(&recent_file(user_dir))
        .lines()
        .filter_map(|l| {
            let p = Project::open(Path::new(l)).ok()?;
            Some((p.root.clone(), p.config.name.clone()))
        })
        .collect()
}

fn remember(user_dir: &Path, dir: &Path) {
    let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let mut list = vec![dir.display().to_string()];
    for l in read(&recent_file(user_dir)).lines() {
        if l != list[0] && list.len() < RECENT {
            list.push(l.to_string());
        }
    }
    let _ = std::fs::create_dir_all(user_dir);
    let _ = std::fs::write(recent_file(user_dir), list.join("\n") + "\n");
}
