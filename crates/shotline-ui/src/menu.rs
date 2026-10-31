//! The system menu bar on macOS and Windows. Items send `MenuCommand`s; the shell keeps their
//! enabled state in step.
//!
//! On Windows the shortcuts are only shown in the menu, not translated by it: the keys reach egui,
//! which handles them like on Linux.

use std::path::PathBuf;
use std::sync::mpsc;

use muda::accelerator::Accelerator;
use muda::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu};

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

const REDO: &str = if cfg!(target_os = "windows") {
    "Ctrl+Y"
} else {
    "CmdOrCtrl+Shift+Z"
};

impl NativeMenu {
    pub fn install(cc: &eframe::CreationContext) -> NativeMenu {
        let (tx, events) = mpsc::channel();
        let ctx = cc.egui_ctx.clone();
        MenuEvent::set_event_handler(Some(move |e| {
            let _ = tx.send(e);
            ctx.request_repaint();
        }));

        let sep = PredefinedMenuItem::separator;
        let new = item("&New Film…", Some("CmdOrCtrl+N"));
        let open = item("&Open Film…", Some("CmdOrCtrl+O"));
        let recent = Submenu::new("Open &Recent", true);
        let close = item("&Close Film", Some("CmdOrCtrl+W"));
        let file = Submenu::with_items("&File", true, &[&new, &open, &recent, &sep(), &close])
            .expect("file menu");
        #[cfg(target_os = "windows")]
        file.append_items(&[&sep(), &PredefinedMenuItem::quit(Some("E&xit"))])
            .expect("file menu");

        let undo = item("&Undo", Some("CmdOrCtrl+Z"));
        let redo = item("&Redo", Some(REDO));
        let edit = Submenu::with_items("&Edit", true, &[&undo, &redo, &sep()]).expect("edit menu");
        #[allow(unused_mut)]
        let mut commands = vec![
            (new.id().clone(), MenuCommand::NewFilm),
            (open.id().clone(), MenuCommand::OpenFilm),
            (close.id().clone(), MenuCommand::CloseFilm),
            (undo.id().clone(), MenuCommand::Undo),
            (redo.id().clone(), MenuCommand::Redo),
        ];
        // The system's own items work on macOS. On Windows they'd go to the window, which
        // doesn't know them, so these ask egui instead.
        #[cfg(target_os = "macos")]
        edit.append_items(&[
            &PredefinedMenuItem::cut(None),
            &PredefinedMenuItem::copy(None),
            &PredefinedMenuItem::paste(None),
            &PredefinedMenuItem::select_all(None),
        ])
        .expect("edit menu");
        #[cfg(target_os = "windows")]
        for (text, keys, cmd) in [
            ("Cu&t", "Ctrl+X", MenuCommand::Cut),
            ("&Copy", "Ctrl+C", MenuCommand::Copy),
            ("&Paste", "Ctrl+V", MenuCommand::Paste),
            ("Select &All", "Ctrl+A", MenuCommand::SelectAll),
        ] {
            let i = item(text, Some(keys));
            edit.append(&i).expect("edit menu");
            commands.push((i.id().clone(), cmd));
        }

        #[cfg(target_os = "macos")]
        let menu = {
            let about = muda::AboutMetadata {
                name: Some("shotline".into()),
                version: Some(env!("CARGO_PKG_VERSION").into()),
                ..Default::default()
            };
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
            menu
        };
        #[cfg(target_os = "windows")]
        let menu = {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            let menu = Menu::with_items(&[&file, &edit]).expect("menu bar");
            if let Ok(RawWindowHandle::Win32(h)) = cc.window_handle().map(|h| h.as_raw()) {
                // SAFETY: eframe's own window, open for as long as the app is.
                let _ =
                    unsafe { menu.init_for_hwnd_with_theme(h.hwnd.get(), muda::MenuTheme::Dark) };
            }
            menu
        };

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
