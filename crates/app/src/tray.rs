//! The notification area icon: keeps Resona reachable while its window is
//! closed during a recording.
//!
//! Its menu is native (not Slint), so its few strings are translated here.

use resona_core::settings::Language;
use slint::{ComponentHandle, Weak};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::AppWindow;

pub struct Tray {
    icon: TrayIcon,
    show: MenuItem,
    toggle: MenuItem,
    quit: MenuItem,
    state: Option<(bool, Language)>,
}

impl Tray {
    /// Needs the UI thread: the icon lives on its message loop.
    pub fn new(window: &Weak<AppWindow>) -> anyhow::Result<Self> {
        let show = MenuItem::new("", true, None);
        let toggle = MenuItem::new("", true, None);
        let quit = MenuItem::new("", true, None);
        let menu = Menu::new();
        menu.append_items(&[&show, &toggle, &PredefinedMenuItem::separator(), &quit])?;
        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .with_icon(icon(false)?)
            .with_tooltip("Resona")
            .build()?;

        // Both handlers run on the UI thread's message loop, but outside
        // Slint's own callbacks: hand the work back to Slint.
        let (show_id, toggle_id, quit_id) =
            (show.id().clone(), toggle.id().clone(), quit.id().clone());
        let menu_window = window.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let id = event.id;
            if id == quit_id {
                let _ = slint::quit_event_loop();
            } else if id == toggle_id {
                let _ = menu_window.upgrade_in_event_loop(|w| w.invoke_toggle_recording());
            } else if id == show_id {
                let _ = menu_window.upgrade_in_event_loop(|w| bring_back(&w));
            }
        }));
        let click_window = window.clone();
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            let left_up = matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            );
            if left_up {
                let _ = click_window.upgrade_in_event_loop(|w| bring_back(&w));
            }
        }));

        Ok(Self {
            icon,
            show,
            toggle,
            quit,
            state: None,
        })
    }

    /// Follows the window: red while recording, texts in the UI language.
    pub fn update(&mut self, recording: bool, language: Language) {
        if self.state == Some((recording, language)) {
            return;
        }
        self.state = Some((recording, language));
        let text = Texts::of(language);
        self.show.set_text(text.show);
        self.toggle
            .set_text(if recording { text.stop } else { text.start });
        self.quit.set_text(text.quit);
        let tooltip = if recording { text.recording } else { "Resona" };
        let _ = self.icon.set_tooltip(Some(tooltip));
        if let Ok(icon) = icon(recording) {
            let _ = self.icon.set_icon(Some(icon));
        }
    }
}

pub fn bring_back(window: &AppWindow) {
    let _ = window.show();
    window.window().set_minimized(false);
}

struct Texts {
    show: &'static str,
    start: &'static str,
    stop: &'static str,
    quit: &'static str,
    recording: &'static str,
}

impl Texts {
    fn of(language: Language) -> Self {
        match language {
            Language::Fr => Self {
                show: "Afficher Resona",
                start: "Démarrer l'enregistrement",
                stop: "Arrêter l'enregistrement",
                quit: "Quitter",
                recording: "Resona — enregistrement en cours",
            },
            Language::En => Self {
                show: "Show Resona",
                start: "Start recording",
                stop: "Stop recording",
                quit: "Quit",
                recording: "Resona — recording",
            },
        }
    }
}

/// The app icon, red while recording.
fn icon(recording: bool) -> anyhow::Result<Icon> {
    let png: &[u8] = if recording {
        include_bytes!("../assets/tray-recording.png")
    } else {
        include_bytes!("../assets/tray.png")
    };
    let image = image::load_from_memory(png)?.into_rgba8();
    let (width, height) = image.dimensions();
    Ok(Icon::from_rgba(image.into_raw(), width, height)?)
}
