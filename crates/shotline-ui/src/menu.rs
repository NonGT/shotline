//! The macOS menu bar. Items send `MenuCommand`s; the shell keeps their enabled state in step.

use std::path::PathBuf;
use std::sync::mpsc;

use eframe::egui;
use muda::accelerator::Accelerator;
use muda::{AboutMetadata, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu};

use crate::start::MenuCommand;

pub(crate) struct NativeMenu {
    // Dropping the menu takes it off the bar.
    _menu: Menu,
    events: mpsc::Receiver<MenuEvent>,
    commands: Vec<(MenuId, MenuCommand)>,
    undo: MenuItem,
    redo: MenuItem,
    close: MenuItem,
    recent: Submenu,
    /// What the menu shows now, to touch it only on change.
    shown: (bool, bool, bool, Vec<PathBuf>),
}

fn item(text: &str, keys: Option<&str>) -> MenuItem {
    let keys = keys.map(|k| k.parse::<Accelerator>().expect("menu shortcut"));
    MenuItem::new(text, true, keys)
}

impl NativeMenu {
    pub fn install(ctx: &egui::Context) -> NativeMenu {
        let (tx, events) = mpsc::channel();
        let ctx = ctx.clone();
        MenuEvent::set_event_handler(Some(move |e| {
            let _ = tx.send(e);
            ctx.request_repaint();
        }));

        let about = AboutMetadata {
            name: Some("shotline".into()),
            version: Some(env!("CARGO_PKG_VERSION").into()),
            ..Default::default()
        };
        let sep = PredefinedMenuItem::separator;
        let app = Submenu::with_items(
            "shotline",
            true,
            &[
                &PredefinedMenuItem::about(None, Some(about)),
                &sep(),
                &PredefinedMenuItem::services(None),
                &sep(),
                &PredefinedMenuItem::hide(None),
                &PredefinedMenuItem::hide_others(None),
                &PredefinedMenuItem::show_all(None),
                &sep(),
                &PredefinedMenuItem::quit(None),
            ],
        )
        .expect("app menu");

        let new = item("New Film…", Some("CmdOrCtrl+N"));
        let open = item("Open Film…", Some("CmdOrCtrl+O"));
        let recent = Submenu::new("Open Recent", true);
        let close = item("Close Film", Some("CmdOrCtrl+W"));
        let file = Submenu::with_items("File", true, &[&new, &open, &recent, &sep(), &close])
            .expect("file menu");

        let undo = item("Undo", Some("CmdOrCtrl+Z"));
        let redo = item("Redo", Some("CmdOrCtrl+Shift+Z"));
        let edit = Submenu::with_items(
            "Edit",
            true,
            &[
                &undo,
                &redo,
                &sep(),
                &PredefinedMenuItem::cut(None),
                &PredefinedMenuItem::copy(None),
                &PredefinedMenuItem::paste(None),
                &PredefinedMenuItem::select_all(None),
            ],
        )
        .expect("edit menu");

        let window = Submenu::with_items(
            "Window",
            true,
            &[
                &PredefinedMenuItem::minimize(None),
                &PredefinedMenuItem::maximize(Some("Zoom")),
                &sep(),
                &PredefinedMenuItem::bring_all_to_front(None),
            ],
        )
        .expect("window menu");

        let menu = Menu::with_items(&[&app, &file, &edit, &window]).expect("menu bar");
        menu.init_for_nsapp();
        window.set_as_windows_menu_for_nsapp();

        let commands = vec![
            (new.id().clone(), MenuCommand::NewFilm),
            (open.id().clone(), MenuCommand::OpenFilm),
            (close.id().clone(), MenuCommand::CloseFilm),
            (undo.id().clone(), MenuCommand::Undo),
            (redo.id().clone(), MenuCommand::Redo),
        ];
        let mut menu = NativeMenu {
            _menu: menu,
            events,
            commands,
            undo,
            redo,
            close,
            recent,
            shown: (true, true, true, vec![PathBuf::new()]),
        };
        menu.sync(false, false, false, &[]);
        menu
    }

    /// Commands picked since the last frame.
    pub fn poll(&self) -> Vec<MenuCommand> {
        self.events
            .try_iter()
            .filter_map(|e| {
                self.commands
                    .iter()
                    .find(|(id, _)| id == e.id())
                    .map(|(_, c)| c.clone())
            })
            .collect()
    }

    pub fn sync(&mut self, undo: bool, redo: bool, film: bool, recent: &[(PathBuf, String)]) {
        if self.shown.0 != undo {
            self.undo.set_enabled(undo);
        }
        if self.shown.1 != redo {
            self.redo.set_enabled(redo);
        }
        if self.shown.2 != film {
            self.close.set_enabled(film);
        }
        let dirs: Vec<PathBuf> = recent.iter().map(|(d, _)| d.clone()).collect();
        if self.shown.3 != dirs {
            while self.recent.remove_at(0).is_some() {}
            self.commands
                .retain(|(_, c)| !matches!(c, MenuCommand::OpenRecent(_)));
            for (dir, name) in recent {
                let i = MenuItem::new(name, true, None);
                let _ = self.recent.append(&i);
                self.commands
                    .push((i.id().clone(), MenuCommand::OpenRecent(dir.clone())));
            }
            self.recent.set_enabled(!recent.is_empty());
        }
        self.shown = (undo, redo, film, dirs);
    }
}
