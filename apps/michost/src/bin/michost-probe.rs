use michost_host_abi::PluginHost;
use michost_host_juce::JuceBackend;

fn main() {
    #[cfg(windows)]
    let private_desktop = PrivateDesktop::create();

    let code = run();

    #[cfg(windows)]
    drop(private_desktop);
    std::process::exit(code);
}

fn run() -> i32 {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() != Some("--describe") {
        eprintln!("usage: michost-probe --describe <path.vst3>");
        return 2;
    }
    let Some(path) = args.next() else {
        eprintln!("usage: michost-probe --describe <path.vst3>");
        return 2;
    };

    let backend = match JuceBackend::load() {
        Ok(backend) => backend,
        Err(error) => {
            eprintln!("MICHPROBE_ERR:{error}");
            return 1;
        }
    };
    let descriptors = match backend.plugin_descriptions(&path) {
        Ok(descriptors) => descriptors,
        Err(error) => {
            eprintln!("MICHPROBE_ERR:{error}");
            return 1;
        }
    };
    match serde_json::to_string(&descriptors) {
        Ok(json) => {
            println!("MICHPROBE:{json}");
            0
        }
        Err(error) => {
            eprintln!("MICHPROBE_ERR:{error}");
            1
        }
    }
}

#[cfg(windows)]
struct PrivateDesktop {
    handle: windows_sys::Win32::System::StationsAndDesktops::HDESK,
    previous: windows_sys::Win32::System::StationsAndDesktops::HDESK,
}

#[cfg(windows)]
impl PrivateDesktop {
    fn create() -> Option<Self> {
        use std::ptr::null;
        use windows_sys::Win32::System::StationsAndDesktops::{
            CreateDesktopW, GetThreadDesktop, SetThreadDesktop,
        };
        use windows_sys::Win32::System::Threading::GetCurrentThreadId;

        let mut name: Vec<u16> = format!("MicHostScan-{}", std::process::id())
            .encode_utf16()
            .collect();
        name.push(0);

        let previous = unsafe { GetThreadDesktop(GetCurrentThreadId()) };
        let handle =
            unsafe { CreateDesktopW(name.as_ptr(), null(), null(), 0, 0x000F_01FF, null()) };
        if handle.is_null() {
            eprintln!(
                "MICHPROBE_WARN:creating the private desktop failed: {}",
                std::io::Error::last_os_error()
            );
            return None;
        }
        if unsafe { SetThreadDesktop(handle) } == 0 {
            eprintln!(
                "MICHPROBE_WARN:switching to the private desktop failed: {}",
                std::io::Error::last_os_error()
            );
            unsafe {
                windows_sys::Win32::System::StationsAndDesktops::CloseDesktop(handle);
            }
            return None;
        }
        Some(Self { handle, previous })
    }
}

#[cfg(windows)]
impl Drop for PrivateDesktop {
    fn drop(&mut self) {
        unsafe {
            if !self.previous.is_null() {
                windows_sys::Win32::System::StationsAndDesktops::SetThreadDesktop(self.previous);
            }
            if windows_sys::Win32::System::StationsAndDesktops::CloseDesktop(self.handle) == 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(windows_sys::Win32::Foundation::ERROR_BUSY as i32) {
                    eprintln!("MICHPROBE_WARN:closing the private desktop failed: {error}");
                }
            }
        }
    }
}
