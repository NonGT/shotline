//! The window around the film: a start page when none is open, and the commands of the File and Edit
//! menus.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use eframe::egui::{self, RichText};
use shotline_core::protocol::Description;
use shotline_core::slug::slugify;
use shotline_core::{Git, Project};

use crate::prefs::Prefs;
use crate::{App, Host, caption, hint, icon, kit, read, theme};

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
    /// The open film's format, models and budget.
    FilmSettings,
    /// A page, by its place in the sidebar: Story is 0.
    View(usize),
    Reload,
    Commit,
    /// The film's folder in Finder or Explorer.
    ShowFilm,
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
    recent: Vec<Recent>,
    /// What the start page says is set up, worked out when it's first shown.
    setup: Option<Setup>,
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
    /// Closing the film, or the app, asked for while paid work is going on.
    leaving: Option<Leave>,
    /// Agreed to: the window's own close goes through from now on.
    quitting: bool,
}

enum Leave {
    Close,
    Quit,
    Open(Opened),
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

#[derive(Clone)]
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
            setup: None,
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
            leaving: None,
            quitting: false,
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
                // Next to the film that's open.
                self.dialog = Some(Form::new(&self.folder()));
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
            MenuCommand::CloseFilm if self.holds() => self.leaving = Some(Leave::Close),
            MenuCommand::Quit if self.holds() => self.leaving = Some(Leave::Quit),
            MenuCommand::CloseFilm => self.close_film(),
            MenuCommand::Preferences => self.preferences(None),
            MenuCommand::FilmSettings => self.film_settings(None),
            MenuCommand::View(n) => {
                if let Screen::Film(app) = &mut self.screen {
                    app.view(n);
                }
            }
            MenuCommand::Reload => {
                if let Screen::Film(app) = &mut self.screen {
                    app.read_again();
                }
            }
            MenuCommand::Commit => {
                if let Screen::Film(app) = &mut self.screen {
                    app.ask_commit();
                }
            }
            MenuCommand::ShowFilm => {
                if let Screen::Film(app) = &mut self.screen
                    && let Err(e) = crate::reveal(&app.project.root)
                {
                    app.error(format!("can't show the film's folder: {e}"));
                }
            }
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

    fn start_do(&mut self, ctx: &egui::Context, d: StartDo) {
        match d {
            StartDo::Open(dir) => self.go(Opened::Existing(dir)),
            StartDo::New => self.command(MenuCommand::NewFilm),
            StartDo::Choose => self.command(MenuCommand::OpenFilm),
            StartDo::Forget(dir) => {
                forget(&self.user_dir, &dir);
                self.recent = recent(&self.user_dir);
            }
            StartDo::Reveal(dir) => {
                let _ = crate::reveal(&dir);
            }
            StartDo::Keys(plugin) => self.preferences(Some(&plugin)),
            StartDo::Recheck => self.setup = None,
            StartDo::Url(u) => ctx.open_url(egui::OpenUrl::new_tab(u)),
        }
    }

    fn check_setup(&self) -> Setup {
        let (list, _) = self.host.as_ref().map(|h| h.plugins()).unwrap_or_default();
        Setup {
            ffmpeg: shotline_media::Ffmpeg::find().is_ok(),
            git: self.git.is_some(),
            keys: list
                .iter()
                .filter(|p| !p.missing.is_empty())
                .map(|p| (p.name.clone(), p.missing.clone()))
                .collect(),
            plugins: list.len(),
        }
    }

    /// The recent films that are still there, for the menus.
    fn found(&self) -> Vec<(PathBuf, String)> {
        self.recent
            .iter()
            .filter_map(|r| Some((r.dir.clone(), r.name.clone()?)))
            .collect()
    }

    /// Whether the open film has paid work going on that closing it would lose.
    fn holds(&self) -> bool {
        self.film().is_some_and(|a| !a.held().is_empty())
    }

    fn close_film(&mut self) {
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

    /// Asks before going while paid work is going on; goes by itself once it's done.
    fn leave_dialog(&mut self, ui: &mut egui::Ui) {
        if ui.input(|i| i.viewport().close_requested()) && !self.quitting && self.holds() {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.leaving = Some(Leave::Quit);
        }
        let Some(leave) = &self.leaving else { return };
        let held = self.film().map(|a| a.held()).unwrap_or_default();
        let mut go = held.is_empty();
        let mut wait = false;
        if !go {
            let quit = matches!(leave, Leave::Quit);
            let modal = crate::modal(ui.ctx(), "leave", |ui| {
                ui.set_width(crate::kit::modal_width(ui.ctx(), 1.0, 420.0, 420.0));
                let n = held.len();
                ui.heading(if n == 1 {
                    "Something is being paid for"
                } else {
                    "Some things are being paid for"
                });
                ui.add_space(10.0);
                ui.label(RichText::new(crate::busy::said(&held)).color(theme::PAPER));
                ui.add_space(8.0);
                crate::hint(
                    ui,
                    "Closing stops waiting for it: what's made is lost, the provider may still \
                     charge, and the hold stays in the ledger. Wait, and it closes by itself \
                     once it's done.",
                );
                ui.add_space(14.0);
                let anyway = if quit { "Quit anyway" } else { "Close anyway" };
                (go, wait) = crate::kit::footer(ui, anyway, crate::kit::Main::Danger, true, "Wait");
            });
            wait |= modal.should_close();
        }
        if wait {
            self.leaving = None;
        } else if go {
            match self.leaving.take() {
                Some(Leave::Close) => self.close_film(),
                Some(Leave::Quit) => {
                    self.quitting = true;
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
                Some(Leave::Open(o)) => self.open_now(o),
                None => {}
            }
        } else {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(250));
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
        let mut asked = None;
        match &mut self.screen {
            Screen::Film(app) => app.show(ui),
            Screen::Start(_) => {
                if self.setup.is_none() {
                    self.setup = Some(self.check_setup());
                }
                if let Some(setup) = &self.setup {
                    asked = start_page(ui, &self.recent, setup);
                }
            }
        }
        let (opened, closed) = match &mut self.dialog {
            Some(f) => new_film_dialog(ui, f, has_git),
            None => (None, false),
        };
        if closed {
            self.dialog = None;
        }
        if let Some(o) = opened {
            self.go(o);
        }
        if let Some(d) = asked {
            self.start_do(ui.ctx(), d);
        }
        if let Screen::Film(app) = &mut self.screen
            && let Some(plugin) = app.wants_keys.take()
        {
            self.preferences(Some(&plugin));
        }
        if let Screen::Film(app) = &mut self.screen
            && std::mem::take(&mut app.wants_prefs)
        {
            self.preferences(None);
        }
        if let Screen::Film(app) = &mut self.screen
            && std::mem::take(&mut app.wants_format)
        {
            self.film_settings(Some(crate::prefs::Page::Format));
        }
        self.prefs_window(ui);
        self.leave_dialog(ui);
        let title = title(self.film().map(|a| &a.project));
        if title != self.titled {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.titled = title;
        }
    }

    /// Opens Preferences, at `plugin`'s page if given.
    fn preferences(&mut self, plugin: Option<&str>) {
        self.prefs = Some(Prefs::new(&self.user_dir, self.host.as_ref(), None, plugin));
    }

    /// The open film's own settings, at `page` if given.
    fn film_settings(&mut self, page: Option<crate::prefs::Page>) {
        let Screen::Film(app) = &mut self.screen else {
            return;
        };
        app.flush();
        let film = Some((&app.project.config, app.project.format(), app.film.made));
        let p = Prefs::new(&self.user_dir, self.host.as_ref(), film, None);
        self.prefs = Some(match page {
            Some(page) => p.at(page),
            None => p,
        });
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
                app.check_keys();
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
        } else if let Some(n) = [
            egui::Key::Num1,
            egui::Key::Num2,
            egui::Key::Num3,
            egui::Key::Num4,
            egui::Key::Num5,
        ]
        .into_iter()
        .position(key)
        {
            MenuCommand::View(n)
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
                        let found = self.found();
                        ui.add_enabled_ui(!found.is_empty(), |ui| {
                            ui.menu_button("Open Recent", |ui| {
                                for (dir, name) in &found {
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
                    ui.menu_button("View", |ui| {
                        for (i, name) in ["Story", "Elements", "Shots", "Cut", "Cost"]
                            .iter()
                            .enumerate()
                        {
                            let keys = format!("Ctrl+{}", i + 1);
                            item(ui, name, &keys, film, MenuCommand::View(i));
                        }
                        ui.separator();
                        item(ui, "Reload", "F5", film, MenuCommand::Reload);
                    });
                    ui.menu_button("Film", |ui| {
                        item(ui, "Commit...", "", film, MenuCommand::Commit);
                        item(ui, "Film Settings...", "", film, MenuCommand::FilmSettings);
                        item(ui, "Show in Folder", "", film, MenuCommand::ShowFilm);
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
        let found = self.found();
        let Some(menu) = &mut self.menu else { return };
        menu.sync(undo, redo, film, &found);
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
        if self.holds() {
            self.leaving = Some(Leave::Open(o));
            return;
        }
        self.open_now(o);
    }

    fn open_now(&mut self, o: Opened) {
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
    let modal = crate::modal(ui.ctx(), "new film", |ui| {
        ui.set_width(kit::modal_width(ui.ctx(), 1.0, 460.0, 460.0));
        ui.heading("New film");
        ui.add_space(10.0);
        (out, cancel) = form.show(ui, has_git);
    });
    (out, cancel || modal.should_close())
}

/// What the start page asked for.
enum StartDo {
    Open(PathBuf),
    New,
    Choose,
    Forget(PathBuf),
    Reveal(PathBuf),
    Keys(String),
    Recheck,
    Url(&'static str),
}

/// What a film needs from the machine, checked when the start page is first shown.
pub(crate) struct Setup {
    ffmpeg: bool,
    git: bool,
    /// Plugins with keys still to set, and which.
    keys: Vec<(String, Vec<String>)>,
    plugins: usize,
}

fn start_page(ui: &mut egui::Ui, recent: &[Recent], setup: &Setup) -> Option<StartDo> {
    let mut out = None;
    egui::CentralPanel::default()
        .frame(egui::Frame::central_panel(ui.style()).inner_margin(0.0))
        .show(ui, |ui| {
            crate::page(ui, egui::vec2(24.0, 24.0), |ui| {
                let wide = ui.available_width() >= 900.0;
                let w = if wide {
                    880.0
                } else {
                    520.0_f32.min(ui.available_width())
                };
                ui.add_space(if wide { 64.0 } else { 24.0 });
                ui.vertical_centered(|ui| {
                    ui.set_max_width(w);
                    ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                        ui.horizontal(|ui| {
                            let (mark, _) = ui
                                .allocate_exact_size(egui::vec2(40.0, 40.0), egui::Sense::hover());
                            ui.painter().rect_filled(mark, 11.0, theme::TUNGSTEN);
                            kit::glyph(ui.painter(), mark.center(), icon::PLAY, 20.0, theme::RACK);
                            ui.add_space(6.0);
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 0.0;
                                ui.label(
                                    RichText::new("shotline")
                                        .font(theme::title())
                                        .color(theme::PAPER),
                                );
                                ui.label(
                                    RichText::new("Plan and shoot a film shot by shot.")
                                        .color(theme::DUST),
                                );
                            });
                        });
                        ui.add_space(28.0);
                        let side = |ui: &mut egui::Ui, out: &mut Option<StartDo>| {
                            kit::card(ui, |ui| {
                                ui.horizontal(|ui| {
                                    if kit::primary_button(ui, Some(icon::ADD), "New film…")
                                        .clicked()
                                    {
                                        *out = Some(StartDo::New);
                                    }
                                    if kit::icon_text_button(ui, icon::FOLDER, "Open…")
                                        .on_hover_text("a film's folder")
                                        .clicked()
                                    {
                                        *out = Some(StartDo::Choose);
                                    }
                                });
                            });
                            ui.add_space(theme::GAP);
                            kit::card(ui, |ui| setup_card(ui, setup, out));
                        };
                        if wide {
                            ui.horizontal_top(|ui| {
                                let right = 320.0;
                                ui.vertical(|ui| {
                                    ui.set_width(ui.available_width() - right - theme::GAP);
                                    kit::card(ui, |ui| recent_list(ui, recent, &mut out));
                                });
                                ui.add_space(theme::GAP);
                                ui.vertical(|ui| {
                                    ui.set_width(right);
                                    side(ui, &mut out);
                                });
                            });
                        } else {
                            side(ui, &mut out);
                            ui.add_space(theme::GAP);
                            kit::card(ui, |ui| recent_list(ui, recent, &mut out));
                        }
                    });
                });
            });
        });
    out
}

fn recent_list(ui: &mut egui::Ui, recent: &[Recent], out: &mut Option<StartDo>) {
    kit::card_title(ui, "Recent films", None);
    ui.add_space(6.0);
    if recent.is_empty() {
        hint(
            ui,
            "Films you open or make show up here. Start one with New film…, or open a folder \
             made with shotline init.",
        );
    }
    for r in recent {
        if let Some(d) = recent_row(ui, r) {
            *out = Some(d);
        }
    }
}

/// ffmpeg, git and the plugins' keys, each with what to do when it's missing.
fn setup_card(ui: &mut egui::Ui, s: &Setup, out: &mut Option<StartDo>) {
    ui.horizontal(|ui| {
        kit::card_title(ui, "Setup", None);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .small_button("Check again")
                .on_hover_text("looks for ffmpeg, git and the keys again")
                .clicked()
            {
                *out = Some(StartDo::Recheck);
            }
        });
    });
    ui.add_space(6.0);
    let row = |ui: &mut egui::Ui, ok: bool, what: &str, about: &str| {
        ui.horizontal(|ui| {
            let (mark, color) = if ok {
                (icon::CHECK, theme::GO)
            } else {
                (icon::WARNING, theme::FLAG)
            };
            let (r, _) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
            kit::glyph(ui.painter(), r.center(), mark, 16.0, color);
            ui.label(RichText::new(what).color(theme::PAPER));
            ui.label(RichText::new(about).small().color(theme::DUST));
        });
    };
    let fix = |ui: &mut egui::Ui, text: &str, about: &str| -> bool {
        ui.horizontal(|ui| {
            ui.add_space(26.0);
            ui.small_button(text).on_hover_text(about).clicked()
        })
        .inner
    };
    row(
        ui,
        s.ffmpeg,
        "ffmpeg",
        if s.ffmpeg { "found" } else { "not found" },
    );
    if !s.ffmpeg
        && fix(
            ui,
            "How to install…",
            "plays, stitches and renders takes; shotline runs it from PATH",
        )
    {
        *out = Some(StartDo::Url("https://ffmpeg.org/download.html"));
    }
    row(
        ui,
        s.git,
        "git",
        if s.git {
            "found"
        } else {
            "not found, so no history"
        },
    );
    if !s.git && fix(ui, "How to install…", "keeps each film's history") {
        *out = Some(StartDo::Url("https://git-scm.com/downloads"));
    }
    let keys = if s.plugins == 0 {
        "no plugins found".to_string()
    } else if s.keys.is_empty() {
        format!("set for all {} plugins", s.plugins)
    } else {
        let n = s.keys.len();
        format!(
            "{n} plugin{} need{} one",
            if n == 1 { "" } else { "s" },
            if n == 1 { "s" } else { "" }
        )
    };
    row(ui, s.plugins > 0 && s.keys.is_empty(), "Keys", &keys);
    for (plugin, vars) in &s.keys {
        if fix(ui, &format!("Set {plugin}'s key…"), &vars.join(", ")) {
            *out = Some(StartDo::Keys(plugin.clone()));
        }
    }
}

/// A film opened before: its name, where it is cut short from the left, when, and ⋯. One that
/// can't be found says so and can only be taken off the list.
fn recent_row(ui: &mut egui::Ui, film: &Recent) -> Option<StartDo> {
    let mut out = None;
    let gone = film.name.is_none();
    let name = film.name.clone().unwrap_or_else(|| {
        film.dir
            .file_name()
            .map_or_else(|| "film".into(), |n| n.to_string_lossy().into_owned())
    });
    let dir = &film.dir;
    let sense = if gone {
        egui::Sense::hover()
    } else {
        egui::Sense::click()
    };
    let (rect, r) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 52.0), sense);
    let path = dir.display().to_string();
    let when = match (gone, film.at) {
        (true, _) => "can't be found".to_string(),
        (false, Some(t)) => ago(t),
        (false, None) => String::new(),
    };
    r.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::Role::Button,
            !gone,
            format!("{name}  {path}  {when}").trim_end(),
        )
    });
    let more = egui::Rect::from_center_size(
        egui::pos2(rect.right() - 22.0, rect.center().y),
        egui::vec2(32.0, 32.0),
    );
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        if r.hovered() && !gone {
            p.rect_filled(rect, theme::R_ROW, theme::RAISED);
        }
        kit::glyph(
            p,
            egui::pos2(rect.left() + 20.0, rect.center().y),
            if gone { icon::WARNING } else { icon::FOLDER },
            18.0,
            if gone { theme::FLAG } else { theme::DUST },
        );
        let x = rect.left() + 42.0;
        let small = egui::FontId::proportional(12.0);
        p.text(
            egui::pos2(more.left() - 8.0, rect.top() + 9.0),
            egui::Align2::RIGHT_TOP,
            &when,
            small.clone(),
            if gone { theme::FLAG } else { theme::DUST },
        );
        p.text(
            egui::pos2(x, rect.top() + 8.0),
            egui::Align2::LEFT_TOP,
            &name,
            egui::FontId::new(13.5, egui::FontFamily::Name(theme::SEMIBOLD.into())),
            if gone { theme::DUST } else { theme::PAPER },
        );
        // Keep the end of the path, which tells films apart.
        let w = more.left() - x - 8.0;
        let mut shown = path.clone();
        let fits = |s: &str| {
            p.layout_no_wrap(s.to_string(), small.clone(), theme::DUST)
                .size()
                .x
                <= w
        };
        while !fits(&shown) && shown.chars().count() > 4 {
            let rest: String = shown
                .chars()
                .skip(if shown.starts_with('…') { 2 } else { 1 })
                .collect();
            shown = format!("…{rest}");
        }
        p.text(
            egui::pos2(x, rect.top() + 28.0),
            egui::Align2::LEFT_TOP,
            shown,
            small,
            theme::DUST,
        );
    }
    let m = ui
        .put(more, |ui: &mut egui::Ui| {
            kit::icon_button(ui, icon::MORE, &format!("More for {name}"))
        })
        .on_hover_text("more");
    egui::Popup::menu(&m).show(|ui| {
        if !gone && ui.button(crate::REVEAL).clicked() {
            out = Some(StartDo::Reveal(dir.clone()));
        }
        if ui
            .button("Remove from list")
            .on_hover_text("the film stays where it is")
            .clicked()
        {
            out = Some(StartDo::Forget(dir.clone()));
        }
    });
    if r.clicked() {
        out = Some(StartDo::Open(dir.clone()));
    }
    if !gone {
        r.on_hover_cursor(egui::CursorIcon::PointingHand);
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

    /// The fields, then Cancel and Create film. Returns what to make, and whether it was given up.
    fn show(&mut self, ui: &mut egui::Ui, has_git: bool) -> (Option<Opened>, bool) {
        let label = caption(ui, "Name");
        let name = ui
            .add(
                egui::TextEdit::singleline(&mut self.name)
                    .hint_text(RichText::new("What the film is called").color(theme::DUST))
                    .desired_width(f32::INFINITY)
                    .margin(egui::vec2(8.0, 6.0)),
            )
            .labelled_by(label.id);
        let entered = kit::entered(ui, &name);
        if ui.memory(|m| m.focused().is_none()) && self.name.is_empty() {
            name.request_focus();
        }
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
            "Every shot is made this shape. It can change later, in Film settings.",
        );
        ui.add_space(6.0);
        if has_git {
            ui.checkbox(&mut self.git, "Keep history with git");
        }
        if let Some(e) = &self.error {
            ui.add_space(6.0);
            ui.label(RichText::new(e).color(theme::FLAG));
        }
        ui.add_space(14.0);
        let ready = dir.is_some() && !taken;
        let (create, cancel) = kit::footer(ui, "Create film", kit::Main::Primary, ready, "Cancel");
        let create = create || ready && entered;
        let made = match dir {
            Some(dir) if ready && create => Some(Opened::New {
                dir,
                name: self.name.trim().into(),
                aspect: self.aspect.clone(),
                git: has_git && self.git,
            }),
            _ => None,
        };
        (made, cancel)
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

/// A film opened before. Its name is None once it can't be found.
#[derive(Clone)]
struct Recent {
    dir: PathBuf,
    name: Option<String>,
    /// When it was last opened, in seconds since 1970; lists from before didn't say.
    at: Option<i64>,
}

/// Films opened lately, newest first: a line each, the folder and when, a tab between.
fn recent(user_dir: &Path) -> Vec<Recent> {
    read(&recent_file(user_dir))
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let (dir, at) = l.split_once('\t').unwrap_or((l, ""));
            let dir = PathBuf::from(dir);
            let name = Project::open(&dir).ok().map(|p| p.config.name);
            Recent {
                dir,
                name,
                at: at.parse().ok(),
            }
        })
        .collect()
}

fn write_recent(user_dir: &Path, list: &[Recent]) {
    let text: String = list
        .iter()
        .map(|r| match r.at {
            Some(t) => format!("{}\t{t}\n", r.dir.display()),
            None => format!("{}\n", r.dir.display()),
        })
        .collect();
    let _ = std::fs::create_dir_all(user_dir);
    let _ = std::fs::write(recent_file(user_dir), text);
}

fn remember(user_dir: &Path, dir: &Path) {
    let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let mut list = vec![Recent {
        dir: dir.clone(),
        name: None,
        at: Some(jiff::Timestamp::now().as_second()),
    }];
    list.extend(recent(user_dir).into_iter().filter(|r| r.dir != dir));
    list.truncate(RECENT);
    write_recent(user_dir, &list);
}

fn forget(user_dir: &Path, dir: &Path) {
    let list: Vec<Recent> = recent(user_dir)
        .into_iter()
        .filter(|r| r.dir != dir)
        .collect();
    write_recent(user_dir, &list);
}

/// "2 days ago", for when a film was last opened.
fn ago(at: i64) -> String {
    let s = (jiff::Timestamp::now().as_second() - at).max(0);
    let (n, unit) = match s {
        0..60 => return "just now".into(),
        60..3600 => (s / 60, "minute"),
        3600..86400 => (s / 3600, "hour"),
        86400..2_592_000 => (s / 86400, "day"),
        _ => (s / 2_592_000, "month"),
    };
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
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
