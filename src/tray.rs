//! The notification area icon, which keeps Irminsul reachable while its
//! window is hidden.

#[derive(Clone, Copy, Debug)]
#[cfg_attr(not(windows), allow(dead_code))]
pub enum TrayCommand {
    Quit,
}

#[cfg(windows)]
pub use windows_tray::Tray;

#[cfg(not(windows))]
pub struct Tray;

#[cfg(not(windows))]
impl Tray {
    pub fn new(_ctx: &egui::Context) -> Option<Self> {
        None
    }

    pub fn commands(&self) -> Vec<TrayCommand> {
        Vec::new()
    }
}

#[cfg(windows)]
mod windows_tray {
    use std::sync::mpsc::{self, Receiver};

    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{
        Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    };

    use super::TrayCommand;
    use crate::window_visibility;

    pub struct Tray {
        _icon: TrayIcon,
        rx: Receiver<TrayCommand>,
    }

    impl Tray {
        /// Creates the icon.  Its events are handled on another thread, so `ctx`
        /// is used to wake the UI up for them.
        pub fn new(ctx: &egui::Context) -> Option<Self> {
            match Self::try_new(ctx) {
                Ok(tray) => Some(tray),
                Err(e) => {
                    tracing::error!("unable to create the tray icon: {e:#}");
                    None
                }
            }
        }

        fn try_new(ctx: &egui::Context) -> anyhow::Result<Self> {
            let icon =
                eframe::icon_data::from_png_bytes(&include_bytes!("../assets/icon-256.png")[..])?;
            let icon = Icon::from_rgba(icon.rgba, icon.width, icon.height)?;

            let open = MenuItem::new("Open Irminsul", true, None);
            let quit = MenuItem::new("Quit", true, None);
            let menu = Menu::new();
            menu.append(&open)?;
            menu.append(&PredefinedMenuItem::separator())?;
            menu.append(&quit)?;
            let (open_id, quit_id) = (open.id().clone(), quit.id().clone());

            let (tx, rx) = mpsc::channel();

            let click_ctx = ctx.clone();
            TrayIconEvent::set_event_handler(Some(move |event| {
                if let TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } = event
                {
                    // Done right here: no frame is needed to show or hide the window.
                    window_visibility::set_visible(!window_visibility::is_visible());
                    click_ctx.request_repaint();
                }
            }));

            let menu_ctx = ctx.clone();
            MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
                if event.id == open_id {
                    window_visibility::set_visible(true);
                } else if event.id == quit_id {
                    let _ = tx.send(TrayCommand::Quit);
                }
                menu_ctx.request_repaint();
            }));

            let icon = TrayIconBuilder::new()
                .with_icon(icon)
                .with_tooltip("Irminsul")
                .with_menu(Box::new(menu))
                .with_menu_on_left_click(false)
                .build()?;

            Ok(Self { _icon: icon, rx })
        }

        pub fn commands(&self) -> Vec<TrayCommand> {
            self.rx.try_iter().collect()
        }
    }
}
