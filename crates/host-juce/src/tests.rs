use std::sync::{Mutex, PoisonError};

use michost_host_abi::{ABI_VERSION, DeviceKind, HostBackend, StreamHost};

use crate::{JuceBackend, find_dll};

static DLL_TEST_LOCK: Mutex<()> = Mutex::new(());

fn dll_test_guard() -> std::sync::MutexGuard<'static, ()> {
    DLL_TEST_LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

#[test]
fn load_skips_cleanly_without_dll() {
    let _guard = dll_test_guard();
    if find_dll().is_none() {
        eprintln!("skipping: mic_host.dll not found");
        return;
    }

    let backend = JuceBackend::load().expect("mic_host.dll was found but failed to load");
    assert_eq!(backend.name(), "juce");
    assert_eq!(backend.abi_version().unwrap(), ABI_VERSION);
}

#[test]
fn device_change_export_loads_and_starts_clear() {
    let _guard = dll_test_guard();
    let Some(path) = find_dll() else {
        eprintln!("skipping: mic_host.dll not found");
        return;
    };

    let backend = JuceBackend::from_path(&path).expect("mic_host.dll was found but failed to load");
    if !backend.device_change_notifications_supported() {
        eprintln!(
            "skipping: {} was built before mh_set_device_change_callback",
            path.display()
        );
        return;
    }

    assert!(!backend.has_device_changes());
    assert!(!backend.take_device_changes());
    assert!(!backend.has_device_changes());
}

#[test]
fn real_dll_layout_when_present() {
    let _guard = dll_test_guard();
    let Some(path) = find_dll() else {
        eprintln!("skipping: mic_host.dll not found");
        return;
    };

    let backend =
        JuceBackend::from_path(&path).expect("real mic_host.dll should match ABI and layouts");
    assert_eq!(backend.abi_version().unwrap(), ABI_VERSION);
    assert_eq!(backend.name(), "juce");
    backend
        .enumerate_devices(DeviceKind::Input)
        .expect("enumerate input devices");
    backend
        .enumerate_devices(DeviceKind::Output)
        .expect("enumerate output devices");
    let _ = backend.poll_events();
}
