//! The window around the film: a start page when none is open, and the commands of the File and Edit
//! menus.

use std::path::{Path, PathBuf};

use eframe::egui::{self, RichText};
use shotline_core::slug::slugify;
use shotline_core::{Git, Project};

use crate::{App, caption, hint, read, theme};

const RECENT: usize = 8;

/// What the menus can ask for, whichever menu it comes from.
#[derive(Debug, Clone, PartialEq)]
pub enum MenuCommand {
    NewFilm,
    OpenFilm,
    OpenRecent(PathBuf),
    CloseFilm,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
}

pub struct Shell {
    user_dir: PathBuf,
    git: Option<Git>,
    testing: bool,
    themed: bool,
    /// The window title last sent.
    titled: String,
    here: PathBuf,
    recent: Vec<(PathBuf, String)>,
    screen: Screen,
    /// New Film asked for over an open film.
    dialog: Option<Form>,
    /// Edit commands for egui, sent with the next frame.
    edits: Vec<MenuCommand>,
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    menu: Option<crate::menu::NativeMenu>,
}

enum Screen {
    Start(Form),
    Film(Box<App>),
}

/// Name and place of a new film.
struct Form {
    name: String,
    folder: String,
    git: bool,
    error: Option<String>,
}

enum Opened {
    Existing(PathBuf),
    New {
        dir: PathBuf,
        name: String,
        git: bool,
    },
}

/// What to say once the film is open, good or bad.
type Note = Result<String, String>;

impl Shell {
    /// Opens `project`, or without one asks for one; new films go in `here` unless changed.
    pub fn new(project: Option<Project>, user_dir: PathBuf, here: &Path) -> Shell {
        let mut shell = Shell {
            recent: recent(&user_dir),
            user_dir,
            git: Git::find(),
            testing: false,
            themed: false,
            titled: String::new(),
            here: here.to_path_buf(),
            screen: Screen::Start(Form::new(here)),
            dialog: None,
            edits: vec![],
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            menu: None,
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

    /// Puts File and Edit in the system's menu bar.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    pub fn with_native_menu(mut self, cc: &eframe::CreationContext) -> Shell {
        self.menu = Some(crate::menu::NativeMenu::install(cc));
        self
    }

    pub fn film(&self) -> Option<&App> {
        match &self.screen {
            Screen::Film(app) => Some(app),
            Screen::Start(_) => None,
        }
    }

    pub fn command(&mut self, cmd: MenuCommand) {
        match cmd {
            MenuCommand::NewFilm => {
                let folder = match &self.screen {
                    // Next to the film that's open.
                    Screen::Film(app) => app.project.root.parent().unwrap_or(&self.here),
                    Screen::Start(_) => return,
                };
                self.dialog = Some(Form::new(folder));
            }
            MenuCommand::OpenFilm => {
                if let Some(d) = rfd::FileDialog::new()
                    .set_directory(self.folder())
                    .pick_folder()
                {
                    self.go(Opened::Existing(d));
                }
            }
            MenuCommand::OpenRecent(d) => self.go(Opened::Existing(d)),
            MenuCommand::CloseFilm => {
                if let Screen::Film(app) = &mut self.screen {
                    app.flush();
                    let here = app
                        .project
                        .root
                        .parent()
                        .unwrap_or(&self.here)
                        .to_path_buf();
                    self.screen = Screen::Start(Form::new(&here));
                    self.dialog = None;
                }
            }
            MenuCommand::Cut | MenuCommand::Copy | MenuCommand::Paste | MenuCommand::SelectAll => {
                self.edits.push(cmd)
            }
            MenuCommand::Undo | MenuCommand::Redo => {
                if let Screen::Film(app) = &mut self.screen {
                    if cmd == MenuCommand::Undo {
                        app.undo();
                    } else {
                        app.redo();
                    }
                }
            }
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        // The fonts only take from the next frame on.
        if !self.themed {
            self.themed = true;
            theme::apply(ui.ctx());
            ui.ctx().request_repaint();
            return;
        }
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        self.native_menu(ui.ctx());
        self.keys(ui);
        self.send_edits(ui.ctx());
        let has_git = self.git.is_some();
        let opened = match &mut self.screen {
            Screen::Film(app) => {
                app.show(ui);
                let (opened, closed) = match &mut self.dialog {
                    Some(f) => new_film_dialog(ui, f, has_git),
                    None => (None, false),
                };
                if closed {
                    self.dialog = None;
                }
                opened
            }
            Screen::Start(form) => start_page(ui, form, has_git, &self.recent),
        };
        if let Some(o) = opened {
            self.go(o);
        }
        let title = title(self.film().map(|a| &a.project));
        if title != self.titled {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.titled = title;
        }
    }

    /// The File shortcuts, where the menu doesn't take them first.
    fn keys(&mut self, ui: &egui::Ui) {
        let key = |k| {
            ui.input_mut(|i| {
                i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, k))
            })
        };
        let cmd = if key(egui::Key::N) {
            MenuCommand::NewFilm
        } else if key(egui::Key::O) {
            MenuCommand::OpenFilm
        } else if key(egui::Key::W) {
            MenuCommand::CloseFilm
        } else {
            return;
        };
        self.command(cmd);
    }

    fn send_edits(&mut self, ctx: &egui::Context) {
        for e in self.edits.drain(..) {
            let vc = match e {
                MenuCommand::Cut => egui::ViewportCommand::RequestCut,
                MenuCommand::Copy => egui::ViewportCommand::RequestCopy,
                MenuCommand::Paste => egui::ViewportCommand::RequestPaste,
                _ => {
                    ctx.input_mut(|i| {
                        i.events.push(egui::Event::Key {
                            key: egui::Key::A,
                            physical_key: None,
                            pressed: true,
                            repeat: false,
                            modifiers: egui::Modifiers::COMMAND,
                        })
                    });
                    continue;
                }
            };
            ctx.send_viewport_cmd(vc);
        }
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn native_menu(&mut self, ctx: &egui::Context) {
        let film = self.film().is_some();
        let Some(menu) = &mut self.menu else { return };
        let cmds = menu.poll();
        let (undo, redo) = match &self.screen {
            // Typing has its own undo, and the menu would take the keys from it.
            Screen::Film(_) if ctx.egui_wants_keyboard_input() => (false, false),
            Screen::Film(app) => (
                app.history.undo_what().is_some(),
                app.history.redo_what().is_some(),
            ),
            Screen::Start(_) => (false, false),
        };
        menu.sync(undo, redo, film, &self.recent);
        for c in cmds {
            self.command(c);
        }
    }

    fn folder(&self) -> PathBuf {
        match &self.screen {
            Screen::Film(app) => app.project.root.parent().unwrap_or(&self.here).into(),
            Screen::Start(f) => expand(&f.folder),
        }
    }

    /// Opens or makes a film; a failure is shown on the form it came from.
    fn go(&mut self, o: Opened) {
        let made = match o {
            Opened::Existing(dir) => Project::discover(&dir).map(|p| (p, None)),
            Opened::New { dir, name, git } => self.create(&dir, &name, git),
        };
        match made {
            Ok((p, note)) => self.open(p, note),
            Err(e) => match (&mut self.dialog, &mut self.screen) {
                (Some(f), _) | (None, Screen::Start(f)) => f.error = Some(e.to_string()),
                (None, Screen::Film(app)) => app.error(e.to_string()),
            },
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
        if let Screen::Film(app) = &mut self.screen {
            app.flush();
        }
        remember(&self.user_dir, &p.root);
        self.recent = recent(&self.user_dir);
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
        self.dialog = None;
    }
}

/// What the form gave, and whether the dialog was closed without it.
fn new_film_dialog(ui: &mut egui::Ui, form: &mut Form, has_git: bool) -> (Option<Opened>, bool) {
    let mut out = None;
    let mut cancel = false;
    let modal = egui::Modal::new(egui::Id::unique("new film")).show(ui.ctx(), |ui| {
        ui.set_width(460.0);
        ui.heading("New film");
        ui.add_space(10.0);
        out = form.show(ui, has_git, |ui| cancel = ui.button("Cancel").clicked());
    });
    (out, cancel || modal.should_close())
}

fn start_page(
    ui: &mut egui::Ui,
    form: &mut Form,
    has_git: bool,
    recent: &[(PathBuf, String)],
) -> Option<Opened> {
    let mut out = None;
    egui::CentralPanel::default().show(ui, |ui| {
        egui::ScrollArea::vertical().show(ui, |ui| {
            let w = 460.0_f32.min(ui.available_width());
            ui.add_space(72.0);
            ui.vertical_centered(|ui| {
                ui.set_max_width(w);
                ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.label(
                        RichText::new("shotline")
                            .font(theme::slate())
                            .color(theme::PAPER),
                    );
                    ui.add_space(32.0);
                    ui.heading("New film");
                    ui.add_space(10.0);
                    out = form
                        .show(ui, has_git, |_| {})
                        .or_else(|| open_film(ui, &form.folder, recent));
                });
            });
        });
    });
    out
}

fn open_film(ui: &mut egui::Ui, folder: &str, recent: &[(PathBuf, String)]) -> Option<Opened> {
    let mut out = None;
    ui.add_space(36.0);
    ui.heading("Open a film");
    ui.add_space(10.0);
    if ui.button("Choose a folder").clicked()
        && let Some(d) = rfd::FileDialog::new()
            .set_directory(expand(folder))
            .pick_folder()
    {
        out = Some(Opened::Existing(d));
    }
    if !recent.is_empty() {
        ui.add_space(14.0);
        caption(ui, "Recent");
    }
    for (dir, name) in recent {
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

impl Form {
    fn new(folder: &Path) -> Form {
        Form {
            name: String::new(),
            folder: folder.display().to_string(),
            git: true,
            error: None,
        }
    }

    /// The fields and the Create button, with anything `buttons` adds after it.
    fn show(
        &mut self,
        ui: &mut egui::Ui,
        has_git: bool,
        buttons: impl FnOnce(&mut egui::Ui),
    ) -> Option<Opened> {
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
            if ui.button("Choose").clicked()
                && let Some(d) = rfd::FileDialog::new()
                    .set_directory(expand(&self.folder))
                    .pick_folder()
            {
                self.folder = d.display().to_string();
            }
        });
        let dir = self.dir();
        if let Some(d) = &dir {
            hint(ui, &format!("Makes {}", d.display()));
        }
        ui.add_space(6.0);
        if has_git {
            ui.checkbox(&mut self.git, "Keep history with git");
        }
        ui.add_space(10.0);
        let create = ui
            .horizontal(|ui| {
                let create = ui.add_enabled(dir.is_some(), egui::Button::new("Create film"));
                buttons(ui);
                create
            })
            .inner;
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

    /// Where a film with the typed name would go.
    fn dir(&self) -> Option<PathBuf> {
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
