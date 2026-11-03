use std::sync::mpsc::{self, Receiver};

use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayCommand {
    Show,
    Start,
    Stop,
    Quit,
}

pub struct Tray {
    icon: TrayIcon,
    start: MenuItem,
    stop: MenuItem,
    commands: Receiver<TrayCommand>,
}

impl Tray {
    pub fn new(ctx: egui::Context) -> Result<Self, String> {
        let menu = Menu::new();
        let show = MenuItem::with_id("show", "Open MicHost", true, None);
        let start = MenuItem::with_id("start", "Start Engine", true, None);
        let stop = MenuItem::with_id("stop", "Stop Engine", false, None);
        let separator_top = PredefinedMenuItem::separator();
        let separator_bottom = PredefinedMenuItem::separator();
        let quit = MenuItem::with_id("quit", "Quit MicHost", true, None);
        menu.append_items(&[
            &show,
            &separator_top,
            &start,
            &stop,
            &separator_bottom,
            &quit,
        ])
        .map_err(|error| format!("building the tray menu: {error}"))?;

        let (sender, commands) = mpsc::channel();
        let menu_sender = sender.clone();
        let menu_ctx = ctx.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let command = match event.id().as_ref() {
                "show" => Some(TrayCommand::Show),
                "start" => Some(TrayCommand::Start),
                "stop" => Some(TrayCommand::Stop),
                "quit" => Some(TrayCommand::Quit),
                _ => None,
            };
            if let Some(command) = command {
                let _ = menu_sender.send(command);
                if matches!(
                    command,
                    TrayCommand::Show | TrayCommand::Start | TrayCommand::Stop
                ) {
                    crate::activation::show_window();
                    menu_ctx.send_viewport_cmd(egui::viewport::ViewportCommand::Visible(true));
                    menu_ctx.send_viewport_cmd(egui::viewport::ViewportCommand::Focus);
                }
                menu_ctx.request_repaint();
            }
        }));

        let click_ctx = ctx;
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let _ = sender.send(TrayCommand::Show);
                crate::activation::show_window();
                click_ctx.send_viewport_cmd(egui::viewport::ViewportCommand::Visible(true));
                click_ctx.send_viewport_cmd(egui::viewport::ViewportCommand::Focus);
                click_ctx.request_repaint();
            }
        }));

        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("MicHost - stopped")
            .with_icon(tray_icon()?)
            .with_menu_on_left_click(false)
            .build()
            .map_err(|error| format!("creating the tray icon: {error}"))?;

        Ok(Self {
            icon,
            start,
            stop,
            commands,
        })
    }

    pub fn poll(&self) -> Vec<TrayCommand> {
        self.commands.try_iter().collect()
    }

    pub fn set_running(&self, running: bool) {
        self.start.set_enabled(!running);
        self.stop.set_enabled(running);
    }

    pub fn set_tooltip(&self, text: &str) {
        let _ = self.icon.set_tooltip(Some(text));
    }
}

fn tray_icon() -> Result<Icon, String> {
    let (rgba, width, height) = microphone_rgba(32);
    Icon::from_rgba(rgba, width, height)
        .map_err(|error| format!("building the tray icon image: {error}"))
}

pub fn window_icon() -> egui::IconData {
    let (rgba, width, height) = microphone_rgba(64);
    egui::IconData {
        rgba,
        width,
        height,
    }
}

fn microphone_rgba(size: u32) -> (Vec<u8>, u32, u32) {
    let mut data = vec![0u8; (size * size * 4) as usize];
    let scale = size as f32 / 32.0;
    let capsule_left = 12.0 * scale;
    let capsule_right = 20.0 * scale;
    let capsule_top = 4.0 * scale;
    let capsule_bottom = 19.0 * scale;
    let radius = (capsule_right - capsule_left) * 0.5;
    let center_x = (capsule_left + capsule_right) * 0.5;
    let stem_top = capsule_bottom;
    let stem_bottom = 26.0 * scale;

    for y in 0..size {
        for x in 0..size {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let mut color = (0u8, 0u8, 0u8, 0u8);
            let in_capsule = px >= capsule_left
                && px <= capsule_right
                && py >= capsule_top
                && py <= capsule_bottom;
            let cap_radius_ok = ((px - center_x).powi(2) + (py - capsule_top - radius).powi(2))
                <= radius.powi(2)
                || ((px - center_x).powi(2) + (py - capsule_bottom + radius).powi(2))
                    <= radius.powi(2);
            if in_capsule
                && (cap_radius_ok || (py >= capsule_top + radius && py <= capsule_bottom - radius))
            {
                color = (238, 242, 250, 255);
            }

            let stem = (px - center_x).abs() <= 1.1 * scale && py >= stem_top && py <= stem_bottom;
            let base =
                (px - center_x).abs() <= 5.5 * scale && (py - stem_bottom).abs() <= 1.4 * scale;
            let arc_radius = 8.0 * scale;
            let arc = ((px - center_x).powi(2) + (py - 17.0 * scale).powi(2) - arc_radius.powi(2))
                .abs()
                <= 1.4 * scale
                && py >= 17.0 * scale
                && py <= 24.0 * scale;
            if stem || base || arc {
                color = (122, 162, 247, 255);
            }

            if color.3 > 0 {
                let index = ((y * size + x) * 4) as usize;
                data[index] = color.0;
                data[index + 1] = color.1;
                data[index + 2] = color.2;
                data[index + 3] = color.3;
            }
        }
    }
    (data, size, size)
}
