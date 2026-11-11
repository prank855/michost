use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::Duration;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows_sys::Win32::System::Threading::{CreateEventW, SetEvent, WaitForSingleObject};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    FindWindowW, PostMessageW, SW_HIDE, SW_RESTORE, SetForegroundWindow, ShowWindow, WM_PAINT,
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
    let title: Vec<u16> = crate::WINDOW_TITLE
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
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

/// Keeps the egui event loop alive while the window is hidden.
///
/// winit only runs `App::update` on `WM_PAINT`, and Windows does not generate
/// `WM_PAINT` for a hidden window, so eframe's 1 s `request_repaint_after`
/// would never fire after `SW_HIDE`. A posted `WM_PAINT` is delivered even to a
/// hidden window, so this thread wakes the loop once per second. Stop it with
/// a channel send; the thread only reads the shared hidden flag.
pub struct HiddenWake {
    stop: mpsc::Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl HiddenWake {
    pub fn new(hidden: Arc<AtomicBool>) -> Result<Self, String> {
        let (stop, receiver) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("michost-hidden-wake".to_string())
            .spawn(move || {
                while let Err(RecvTimeoutError::Timeout) =
                    receiver.recv_timeout(Duration::from_secs(1))
                {
                    if hidden.load(Ordering::Acquire) {
                        let window = main_window();
                        if !window.is_null() {
                            unsafe {
                                PostMessageW(window, WM_PAINT, 0, 0);
                            }
                        }
                    }
                }
            })
            .map_err(|error| format!("starting the hidden repaint thread: {error}"))?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for HiddenWake {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
