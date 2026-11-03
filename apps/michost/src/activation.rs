use std::sync::mpsc::{self, Receiver};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows_sys::Win32::System::Threading::{CreateEventW, SetEvent, WaitForSingleObject};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    FindWindowW, SW_HIDE, SW_RESTORE, SetForegroundWindow, ShowWindow,
};

const EVENT_NAME: &str = "Local\\MicHost.Show\0";

pub struct Activation {
    receiver: Receiver<()>,
}

impl Activation {
    pub fn create(ctx: egui::Context) -> Result<Self, String> {
        let name: Vec<u16> = EVENT_NAME.encode_utf16().collect();
        let handle = unsafe { CreateEventW(std::ptr::null(), 0, 0, name.as_ptr()) };
        if handle.is_null() {
            return Err("creating the activation event failed".to_string());
        }

        let (sender, receiver) = mpsc::channel();
        let handle = handle as usize;
        std::thread::Builder::new()
            .name("michost-activation".to_string())
            .spawn(move || {
                let handle = handle as HANDLE;
                loop {
                    let result = unsafe { WaitForSingleObject(handle, u32::MAX) };
                    if result != WAIT_OBJECT_0 {
                        break;
                    }
                    let _ = sender.send(());
                    show_window();
                    ctx.send_viewport_cmd(egui::viewport::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::viewport::ViewportCommand::Focus);
                    ctx.request_repaint();
                }
                unsafe {
                    CloseHandle(handle);
                }
            })
            .map_err(|error| format!("starting the activation thread: {error}"))?;

        Ok(Self { receiver })
    }

    pub fn poll(&self) -> bool {
        self.receiver.try_recv().is_ok()
    }
}

fn main_window() -> windows_sys::Win32::Foundation::HWND {
    let title: Vec<u16> = "MicHost\0".encode_utf16().collect();
    unsafe { FindWindowW(std::ptr::null(), title.as_ptr()) }
}

pub fn show_window() {
    let window = main_window();
    if !window.is_null() {
        unsafe {
            ShowWindow(window, SW_RESTORE);
            SetForegroundWindow(window);
        }
    }
}

/// Hides the window and removes its taskbar button. The tray icon is separate.
pub fn hide_window() {
    let window = main_window();
    if !window.is_null() {
        unsafe {
            ShowWindow(window, SW_HIDE);
        }
    }
}

pub fn signal_existing() -> Result<(), String> {
    let name: Vec<u16> = EVENT_NAME.encode_utf16().collect();
    let handle: HANDLE = unsafe { CreateEventW(std::ptr::null(), 0, 0, name.as_ptr()) };
    if handle.is_null() {
        return Err("opening the activation event failed".to_string());
    }
    unsafe {
        SetEvent(handle);
        CloseHandle(handle);
    }
    Ok(())
}
