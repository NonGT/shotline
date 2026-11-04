//! The window around the film: a start page when none is open, and the commands of the File and Edit
//! menus.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use eframe::egui::{self, RichText};
use shotline_core::protocol::Description;
use shotline_core::slug::slugify;
use shotline_core::{Git, Project};

use crate::prefs::Prefs;
use crate::{App, Host, caption, hint, read, theme};

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
    Preferences,
    Quit,
}

pub struct Shell {
    user_dir: PathBuf,
    git: Option<Git>,
    plugins: Vec<Description>,
    host: Option<Arc<dyn Host>>,
    testing: bool,
    themed: bool,
    /// The window title last sent.
    titled: String,
    here: PathBuf,
    recent: Vec<(PathBuf, String)>,
    screen: Screen,
    /// New Film asked for over an open film.
    dialog: Option<Form>,
    /// Preferences, while open.
    prefs: Option<Prefs>,
    /// Edit commands for egui, sent with the next frame.
    edits: Vec<MenuCommand>,
    /// The text box last typed in, which a click on the window menu takes focus from.
    typing: Option<egui::Id>,
    /// File and Edit drawn in the window, where there's no system menu bar to put them in.
    window_menu: bool,
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
    aspect: String,
    git: bool,
    error: Option<String>,
}

enum Opened {
    Existing(PathBuf),
    New {
        dir: PathBuf,
        name: String,
        aspect: String,
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
            plugins: vec![],
            host: None,
            testing: false,
            themed: false,
            titled: String::new(),
            here: here.to_path_buf(),
            screen: Screen::Start(Form::new(here)),
            dialog: None,
            prefs: None,
            edits: vec![],
            typing: None,
            window_menu: cfg!(not(any(target_os = "macos", target_os = "windows"))),
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            menu: None,
        };
        if let Some(p) = project {
            shell.open(p, None);
        }
        shell
    }

    /// What the installed plugins said about themselves, for picking them in the app.
    pub fn with_plugins(mut self, plugins: Vec<Description>, host: Option<Arc<dyn Host>>) -> Shell {
        if let Screen::Film(app) = &mut self.screen {
            app.set_plugins(plugins.clone(), host.clone());
        }
        self.plugins = plugins;
        self.host = host;
        self
    }

    pub fn with_git(mut self, git: Option<Git>) -> Shell {
        self.git = git;
        self
    }

    /// The menu Linux gets, wherever it runs.
    pub fn with_window_menu(mut self) -> Shell {
        self.window_menu = true;
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
            MenuCommand::Preferences => self.preferences(None),
            MenuCommand::Cut
            | MenuCommand::Copy
            | MenuCommand::Paste
            | MenuCommand::SelectAll
            | MenuCommand::Quit => self.edits.push(cmd),
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
        // A frame after the menu was clicked, or the click takes the focus away again.
        self.send_edits(ui.ctx());
        if ui.ctx().egui_wants_keyboard_input() {
            self.typing = ui.memory(|m| m.focused());
        }
        if self.window_menu {
            self.window_menu(ui);
        }
        self.keys(ui);
        if !self.edits.is_empty() {
            ui.ctx().request_repaint();
        }
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
        if let Screen::Film(app) = &mut self.screen
            && let Some(plugin) = app.wants_keys.take()
        {
            self.preferences(Some(&plugin));
        }
        self.prefs_window(ui);
        let title = title(self.film().map(|a| &a.project));
        if title != self.titled {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.titled = title;
        }
    }

    /// Opens Preferences, at `plugin`'s page if given.
    fn preferences(&mut self, plugin: Option<&str>) {
        let film = match &self.screen {
            Screen::Film(app) => Some(&app.project.config),
            Screen::Start(_) => None,
        };
        self.prefs = Some(Prefs::new(&self.user_dir, self.host.as_ref(), film, plugin));
    }

    fn prefs_window(&mut self, ui: &mut egui::Ui) {
        let Some(page) = &mut self.prefs else {
            return;
        };
        let done = page.show(ui, self.host.clone());
        if !done.open {
            self.prefs = None;
        }
        if done.plugins_changed || done.keys_changed {
            // What a plugin says it needs is worked out again from the keys.
            if let Some(h) = &self.host {
                self.plugins = h
                    .plugins()
                    .0
                    .into_iter()
                    .filter_map(|p| p.description.ok())
                    .collect();
            }
        }
        if let Screen::Film(app) = &mut self.screen {
            if done.plugins_changed {
                app.set_plugins(self.plugins.clone(), self.host.clone());
            }
            if done.keys_changed {
                app.forget_voices();
            }
            if !done.film.is_empty() {
                app.apply(done.film);
            }
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
        } else if key(egui::Key::Comma) {
            MenuCommand::Preferences
        } else if self.window_menu && key(egui::Key::Q) {
            MenuCommand::Quit
        } else {
            return;
        };
        self.command(cmd);
    }

    fn window_menu(&mut self, ui: &mut egui::Ui) {
        let (undo, redo) = match &self.screen {
            Screen::Film(app) => (
                app.history.undo_what().is_some(),
                app.history.redo_what().is_some(),
            ),
            Screen::Start(_) => (false, false),
        };
        let film = self.film().is_some();
        let mut picked = None;
        egui::Panel::top("menu")
            .frame(
                egui::Frame::new()
                    .fill(theme::RACK)
                    .inner_margin(egui::vec2(10.0, 2.0)),
            )
            .show(ui, |ui| {
                egui::MenuBar::new().ui(ui, |ui| {
                    let mut item = |ui: &mut egui::Ui, text: &str, keys: &str, on: bool, c| {
                        let mut b = egui::Button::new(text);
                        if !keys.is_empty() {
                            b = b.shortcut_text(keys);
                        }
                        if ui.add_enabled(on, b).clicked() {
                            picked = Some(c);
                        }
                    };
                    ui.menu_button("File", |ui| {
                        item(ui, "New Film...", "Ctrl+N", true, MenuCommand::NewFilm);
                        item(ui, "Open Film...", "Ctrl+O", true, MenuCommand::OpenFilm);
                        ui.add_enabled_ui(!self.recent.is_empty(), |ui| {
                            ui.menu_button("Open Recent", |ui| {
                                for (dir, name) in &self.recent {
                                    let c = MenuCommand::OpenRecent(dir.clone());
                                    item(ui, name, "", true, c);
                                }
                            });
                        });
                        ui.separator();
                        item(ui, "Close Film", "Ctrl+W", film, MenuCommand::CloseFilm);
                        ui.separator();
                        item(ui, "Quit", "Ctrl+Q", true, MenuCommand::Quit);
                    });
                    ui.menu_button("Edit", |ui| {
                        item(ui, "Undo", "Ctrl+Z", undo, MenuCommand::Undo);
                        item(ui, "Redo", "Ctrl+Shift+Z", redo, MenuCommand::Redo);
                        ui.separator();
                        item(ui, "Cut", "Ctrl+X", true, MenuCommand::Cut);
                        item(ui, "Copy", "Ctrl+C", true, MenuCommand::Copy);
                        item(ui, "Paste", "Ctrl+V", true, MenuCommand::Paste);
                        item(ui, "Select All", "Ctrl+A", true, MenuCommand::SelectAll);
                        ui.separator();
                        item(
                            ui,
                            "Preferences...",
                            "Ctrl+,",
                            true,
                            MenuCommand::Preferences,
                        );
                    });
                });
            });
        if let Some(c) = picked {
            self.command(c);
        }
    }

    fn send_edits(&mut self, ctx: &egui::Context) {
        if let (false, Some(id)) = (self.edits.is_empty(), self.typing) {
            ctx.memory_mut(|m| m.request_focus(id));
        }
        for e in self.edits.drain(..) {
            let vc = match e {
                MenuCommand::Cut => egui::ViewportCommand::RequestCut,
                MenuCommand::Copy => egui::ViewportCommand::RequestCopy,
                MenuCommand::Paste => egui::ViewportCommand::RequestPaste,
                MenuCommand::Quit => egui::ViewportCommand::Close,
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
            Opened::New {
                dir,
                name,
                aspect,
                git,
            } => self.create(&dir, &name, &aspect, git),
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
        aspect: &str,
        git: bool,
    ) -> shotline_core::Result<(Project, Option<Note>)> {
        if has_files(dir) {
            return Err(shotline_core::Error::Invalid(format!(
                "{} already has files in it",
                dir.display()
            )));
        }
        let mut p = Project::init(dir, Some(name))?;
        p.set_format(&shotline_core::Format {
            aspect: aspect.to_string(),
            ..p.format()
        })?;
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
        app.set_plugins(self.plugins.clone(), self.host.clone());
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
    egui::CentralPanel::default()
        .frame(egui::Frame::central_panel(ui.style()).inner_margin(0.0))
        .show(ui, |ui| {
            crate::page(ui, egui::vec2(8.0, 8.0), |ui| {
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
            aspect: "9:16".into(),
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
        let taken = dir.as_deref().is_some_and(has_files);
        match &dir {
            Some(d) if taken => {
                ui.label(
                    RichText::new(format!(
                        "{} already has files in it. Pick another name or folder.",
                        d.display()
                    ))
                    .color(theme::FLAG),
                );
            }
            Some(d) => hint(ui, &format!("Makes {}", d.display())),
            None => {}
        }
        ui.add_space(10.0);
        let label = caption(ui, "Shape");
        egui::ComboBox::from_id_salt("new film shape")
            .selected_text(shape_for(&self.aspect))
            .width(ui.available_width())
            .show_ui(ui, |ui| {
                for a in crate::SHAPES {
                    ui.selectable_value(&mut self.aspect, a.to_string(), shape_for(a));
                }
            })
            .response
            .labelled_by(label.id);
        hint(
            ui,
            "Every shot is made this shape. It can change later, on the Story tab.",
        );
        ui.add_space(6.0);
        if has_git {
            ui.checkbox(&mut self.git, "Keep history with git");
        }
        ui.add_space(10.0);
        let create = ui
            .horizontal(|ui| {
                let create =
                    ui.add_enabled(dir.is_some() && !taken, egui::Button::new("Create film"));
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
            Some(dir) if !taken && (create.clicked() || enter) => Some(Opened::New {
                dir,
                name: self.name.trim().into(),
                aspect: self.aspect.clone(),
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

/// A new film only goes in a folder of its own.
fn has_files(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|mut r| r.next().is_some())
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

/// "9:16 tall, for Reels, TikTok and vertical drama"
fn shape_for(aspect: &str) -> String {
    let use_ = match aspect {
        "9:16" => ", for Reels, TikTok and vertical drama",
        "16:9" => ", for YouTube",
        "1:1" => ", for feeds",
        _ => "",
    };
    format!("{}{use_}", crate::shape(aspect))
}
