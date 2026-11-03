use std::ffi::c_void;
use std::mem::{offset_of, size_of};
use std::sync::Arc;

use michost_graph::PluginProcessor;

use crate::backend::{BackendProcessor, HostBackend, PluginHost, StreamHost};
use crate::constants::*;
use crate::ffi::*;
use crate::mock::MockBackend;
use crate::types::*;

#[test]
fn abi_struct_sizes_match_header() {
    assert_eq!(size_of::<MhCallbacks>(), 32);
    assert_eq!(size_of::<MhDeviceInfo>(), 480);
    assert_eq!(size_of::<MhStreamConfig>(), 304);
    assert_eq!(size_of::<MhStreamCallbacks>(), 40);
    assert_eq!(size_of::<MhLatency>(), 16);
    assert_eq!(size_of::<MhPluginDescriptor>(), 1168);
    assert_eq!(size_of::<MhParamInfo>(), 184);
}

#[test]
fn abi_struct_offsets_match_header() {
    assert_eq!(offset_of!(MhCallbacks, abi_version), 0);
    assert_eq!(offset_of!(MhCallbacks, size), 4);
    assert_eq!(offset_of!(MhCallbacks, ui_ctx), 8);
    assert_eq!(offset_of!(MhCallbacks, on_error), 16);
    assert_eq!(offset_of!(MhCallbacks, on_latency_changed), 24);

    assert_eq!(offset_of!(MhDeviceInfo, name), 0);
    assert_eq!(offset_of!(MhDeviceInfo, id), 256);
    assert_eq!(offset_of!(MhDeviceInfo, inputs), 384);
    assert_eq!(offset_of!(MhDeviceInfo, outputs), 388);
    assert_eq!(offset_of!(MhDeviceInfo, default_sample_rate), 392);
    assert_eq!(offset_of!(MhDeviceInfo, api), 400);
    assert_eq!(offset_of!(MhDeviceInfo, is_default), 404);
    assert_eq!(offset_of!(MhDeviceInfo, has_control_panel), 405);
    assert_eq!(offset_of!(MhDeviceInfo, pad), 406);
    assert_eq!(offset_of!(MhDeviceInfo, buffer_sizes), 408);
    assert_eq!(offset_of!(MhDeviceInfo, buffer_size_count), 472);
    assert_eq!(offset_of!(MhDeviceInfo, pad2), 476);

    assert_eq!(offset_of!(MhStreamConfig, device), 0);
    assert_eq!(offset_of!(MhStreamConfig, sample_rate), 256);
    assert_eq!(offset_of!(MhStreamConfig, block_size), 264);
    assert_eq!(offset_of!(MhStreamConfig, channels), 268);
    assert_eq!(offset_of!(MhStreamConfig, api), 272);
    assert_eq!(offset_of!(MhStreamConfig, direction), 276);
    assert_eq!(offset_of!(MhStreamConfig, channel_mask), 280);
    assert_eq!(offset_of!(MhStreamConfig, output_channels), 288);
    assert_eq!(offset_of!(MhStreamConfig, output_channel_mask), 296);

    assert_eq!(offset_of!(MhStreamCallbacks, size), 0);
    assert_eq!(offset_of!(MhStreamCallbacks, pad), 4);
    assert_eq!(offset_of!(MhStreamCallbacks, ctx), 8);
    assert_eq!(offset_of!(MhStreamCallbacks, on_input), 16);
    assert_eq!(offset_of!(MhStreamCallbacks, on_output), 24);
    assert_eq!(offset_of!(MhStreamCallbacks, on_error), 32);

    assert_eq!(offset_of!(MhPluginDescriptor, path), 0);
    assert_eq!(offset_of!(MhPluginDescriptor, class_id), 640);
    assert_eq!(offset_of!(MhPluginDescriptor, name), 768);
    assert_eq!(offset_of!(MhPluginDescriptor, vendor), 896);
    assert_eq!(offset_of!(MhPluginDescriptor, main_inputs), 1024);
    assert_eq!(offset_of!(MhPluginDescriptor, main_outputs), 1028);
    assert_eq!(offset_of!(MhPluginDescriptor, sidechain_inputs), 1032);
    assert_eq!(offset_of!(MhPluginDescriptor, flags), 1036);
    assert_eq!(offset_of!(MhPluginDescriptor, category), 1040);

    assert_eq!(offset_of!(MhParamInfo, id), 0);
    assert_eq!(offset_of!(MhParamInfo, pad), 4);
    assert_eq!(offset_of!(MhParamInfo, name), 8);
    assert_eq!(offset_of!(MhParamInfo, units), 136);
    assert_eq!(offset_of!(MhParamInfo, default_value), 168);
    assert_eq!(offset_of!(MhParamInfo, step_count), 176);
    assert_eq!(offset_of!(MhParamInfo, pad2), 180);
}

#[test]
fn callbacks_default_matches_abi() {
    let callbacks = MhCallbacks::default();
    assert_eq!(callbacks.abi_version, ABI_VERSION);
    assert_eq!(callbacks.size, size_of::<MhCallbacks>() as u32);

    let stream_callbacks = MhStreamCallbacks::default();
    assert_eq!(stream_callbacks.size, size_of::<MhStreamCallbacks>() as u32);
}

#[test]
fn write_str_terminates_and_truncates() {
    let mut buffer = [0xAAu8; 8];
    write_str(&mut buffer, "hello");
    assert_eq!(&buffer[..6], b"hello\0");
    assert_eq!(&buffer[6..], &[0, 0]);
    assert_eq!(read_str(&buffer), "hello");

    let mut small = [0xAAu8; 4];
    write_str(&mut small, "abcdef");
    assert_eq!(&small, b"abc\0");
    assert_eq!(read_str(&small), "abc");

    let empty: [u8; 0] = [];
    write_str(&mut [][..], "ignored");
    assert_eq!(read_str(&empty), "");
}

#[test]
fn write_str_truncates_on_char_boundary() {
    let mut buffer = [0u8; 6];
    write_str(&mut buffer, "ééééé");
    assert_eq!(&buffer, &[0xC3, 0xA9, 0xC3, 0xA9, 0, 0]);
    assert_eq!(read_str(&buffer), "éé");
}

#[test]
fn read_str_handles_missing_nul_and_invalid_utf8() {
    assert_eq!(read_str(b"ab"), "ab");
    assert_eq!(read_str(&[0xFF, 0xFE, 0]), "\u{FFFD}\u{FFFD}");
}

#[test]
fn audio_api_mapping() {
    assert_eq!(AudioApi::default(), AudioApi::WasapiShared);
    assert_eq!(AudioApi::WasapiShared.as_i32(), 0);
    assert_eq!(AudioApi::WasapiLowLatency.as_i32(), 1);
    assert_eq!(AudioApi::WasapiExclusive.as_i32(), 2);
    assert_eq!(AudioApi::Asio.as_i32(), 3);
    assert_eq!(AudioApi::Other.as_i32(), -1);

    assert_eq!(AudioApi::from_i32(0), AudioApi::WasapiShared);
    assert_eq!(AudioApi::from_i32(1), AudioApi::WasapiLowLatency);
    assert_eq!(AudioApi::from_i32(2), AudioApi::WasapiExclusive);
    assert_eq!(AudioApi::from_i32(3), AudioApi::Asio);
    assert_eq!(AudioApi::from_i32(4), AudioApi::Other);
    assert_eq!(AudioApi::from_i32(-1), AudioApi::Other);
}

#[test]
fn device_info_round_trip_truncates_buffer_sizes() {
    let info = DeviceInfo {
        name: "Speakers".to_owned(),
        id: "id-1".to_owned(),
        inputs: 2,
        outputs: 4,
        default_sample_rate: 48_000.0,
        api: AudioApi::WasapiExclusive,
        is_default: true,
        has_control_panel: true,
        buffer_sizes: (0..20).map(|value| value as u32).collect(),
    };

    let ffi = MhDeviceInfo::from(&info);
    assert_eq!(ffi.buffer_size_count as usize, MH_MAX_BUFFER_SIZES);
    assert_eq!(ffi.api, AudioApi::WasapiExclusive.as_i32());
    assert_eq!(ffi.is_default, 1);

    let back = DeviceInfo::from(&ffi);
    assert_eq!(back.name, info.name);
    assert_eq!(back.id, info.id);
    assert_eq!(back.inputs, info.inputs);
    assert_eq!(back.outputs, info.outputs);
    assert_eq!(back.default_sample_rate, info.default_sample_rate);
    assert_eq!(back.api, info.api);
    assert!(back.is_default);
    assert!(back.has_control_panel);
    assert_eq!(back.buffer_sizes.len(), MH_MAX_BUFFER_SIZES);
}

#[test]
fn stream_config_and_latency_round_trip() {
    let config = StreamConfig {
        device: "device".to_owned(),
        sample_rate: 44_100.0,
        block_size: 256,
        channels: 2,
        api: AudioApi::Asio,
        direction: StreamDirection::Output,
        channel_mask: 0,
        output_channels: 0,
        output_channel_mask: 0,
    };
    let ffi = MhStreamConfig::from(&config);
    assert_eq!(read_str(&ffi.device), "device");
    assert_eq!(ffi.sample_rate, 44_100.0);
    assert_eq!(ffi.block_size, 256);
    assert_eq!(ffi.channels, 2);
    assert_eq!(ffi.api, 3);
    assert_eq!(ffi.direction, MH_STREAM_OUTPUT);

    let ffi_latency = MhLatency {
        input: 10,
        output: 20,
        buffer: 5,
        sample_rate: 48_000,
    };
    let latency = DeviceLatency::from(&ffi_latency);
    assert_eq!(latency.round_trip(), 35);
}

#[test]
fn device_latency_round_trip_saturates() {
    assert_eq!(
        DeviceLatency {
            input: 10,
            output: 20,
            buffer: 5,
            sample_rate: 48_000,
        }
        .round_trip(),
        35
    );
    assert_eq!(
        DeviceLatency {
            input: u32::MAX,
            output: 1,
            buffer: 1,
            sample_rate: 48_000,
        }
        .round_trip(),
        u32::MAX
    );
}

#[test]
fn plugin_descriptor_round_trip_and_spec_helpers() {
    let descriptor = PluginDescriptor {
        path: "C:/plugins/example.vst3".to_owned(),
        class_id: "com.example.plugin".to_owned(),
        name: "Example".to_owned(),
        vendor: "Example Audio".to_owned(),
        category: "Fx|EQ".to_owned(),
        main_inputs: 2,
        main_outputs: 2,
        sidechain_inputs: 1,
        flags: 7,
    };

    let ffi = MhPluginDescriptor::from(&descriptor);
    assert_eq!(read_str(&ffi.path), descriptor.path);
    assert_eq!(read_str(&ffi.category), "Fx|EQ");
    assert_eq!(ffi.sidechain_inputs, 1);
    let back = PluginDescriptor::from(&ffi);
    assert_eq!(back, descriptor);

    let spec = descriptor.to_spec();
    assert_eq!(spec.main_inputs, 2);
    assert_eq!(spec.main_outputs, 2);
    assert_eq!(spec.sidechain_inputs, 1);
    let spec_round_trip = PluginDescriptor::from_spec(&spec);
    assert_eq!(spec_round_trip.path, descriptor.path);
    assert_eq!(spec_round_trip.class_id, descriptor.class_id);
    assert_eq!(spec_round_trip.name, descriptor.name);
    assert_eq!(spec_round_trip.vendor, descriptor.vendor);
    assert_eq!(spec_round_trip.main_inputs, descriptor.main_inputs);
    assert_eq!(spec_round_trip.main_outputs, descriptor.main_outputs);
    assert_eq!(
        spec_round_trip.sidechain_inputs,
        descriptor.sidechain_inputs
    );
    assert_eq!(spec_round_trip.flags, 0);
}

#[test]
fn param_info_round_trip() {
    let info = ParamInfo {
        id: 42,
        name: "Gain".to_owned(),
        units: "dB".to_owned(),
        default_value: -3.5,
        step_count: 100,
    };
    let ffi = MhParamInfo::from(&info);
    assert_eq!(read_str(&ffi.name), "Gain");
    assert_eq!(ffi.default_value, -3.5);
    assert_eq!(ParamInfo::from(&ffi), info);
}

#[test]
fn mock_prepare_records_sidechain_and_main_counts() {
    let mock = MockBackend::new();
    mock.set_channels(2, 1, 3);
    let handle = mock.load_plugin("plugin.dll", "class").unwrap();

    mock.prepare_plugin(handle, 48_000.0, 128, 2, 1, 3).unwrap();

    let calls = mock.prepared();
    assert_eq!(calls.len(), 1);
    let call = &calls[0];
    assert_eq!(call.handle, handle);
    assert_eq!(call.sample_rate, 48_000.0);
    assert_eq!(call.max_block, 128);
    assert_eq!(call.main_inputs, 2);
    assert_eq!(call.main_outputs, 1);
    assert_eq!(call.sidechain_channels, 3);

    let channels = mock.plugin_channels(handle).unwrap();
    assert_eq!(channels.main_inputs, 2);
    assert_eq!(channels.main_outputs, 1);
    assert_eq!(channels.sidechain_inputs, 3);

    assert_eq!(
        mock.load_calls(),
        vec![("plugin.dll".into(), "class".into())]
    );
    assert_eq!(mock.activated(), Vec::new());
    mock.activate_plugin(handle, true).unwrap();
    assert_eq!(mock.activated(), vec![(handle, true)]);
}

#[test]
fn mock_process_applies_gain_and_bias() {
    let mock = MockBackend::new();
    mock.set_gain(2.0);
    mock.set_bias(0.5);
    let handle = mock.load_plugin("plugin.dll", "class").unwrap();

    let mut left = vec![1.0f32, -1.0, 0.0];
    let mut right = vec![2.0f32, 0.0, 4.0];
    {
        let mut channels: [&mut [f32]; 2] = [&mut left, &mut right];
        mock.process_plugin(handle, &mut channels, 3).unwrap();
    }

    assert_eq!(left, vec![2.5, -1.5, 0.5]);
    assert_eq!(right, vec![4.5, 0.5, 8.5]);
    assert_eq!(mock.processed_frames(), 3);
}

#[test]
fn mock_state_and_editor_tracking() {
    let mock = MockBackend::new();
    let handle = mock.load_plugin("plugin.dll", "class").unwrap();

    mock.state_import(handle, StateKind::Component, b"component")
        .unwrap();
    assert_eq!(
        mock.state_export(handle, StateKind::Component).unwrap(),
        b"component"
    );
    assert!(
        mock.state_export(handle, StateKind::Controller)
            .unwrap()
            .is_empty()
    );

    assert!(!mock.editor_open(handle).unwrap());
    mock.open_editor(handle).unwrap();
    assert!(mock.editor_open(handle).unwrap());
    mock.close_editor(handle).unwrap();
    assert!(!mock.editor_open(handle).unwrap());
}

#[test]
fn mock_plugin_latency_and_stream_latency() {
    let mock = MockBackend::new();
    let handle = mock.load_plugin("plugin.dll", "class").unwrap();
    mock.set_latency(64);
    assert_eq!(mock.plugin_latency(handle).unwrap(), 64);

    mock.set_stream_latency(DeviceLatency {
        input: 1,
        output: 2,
        buffer: 3,
        sample_rate: 48_000,
    });
    let stream = mock
        .open_stream(&StreamConfig::default(), StreamCallbacks::default())
        .unwrap();
    assert_eq!(mock.stream_latency(&stream).unwrap().round_trip(), 6);
    mock.start_stream(&stream).unwrap();
    mock.stop_stream(&stream).unwrap();
    mock.close_stream(stream);
    assert_eq!(mock.started(), vec![stream]);
    assert_eq!(mock.closed(), vec![stream]);
}

#[test]
fn mock_events_are_polled_once() {
    let mock = MockBackend::new();
    let handle = PluginHandle::from_raw(0x1234 as *mut c_void);

    mock.push_event(UiEvent::DeviceError {
        status: MH_ERR,
        message: "device failed".to_owned(),
    });
    mock.push_event(UiEvent::LatencyChanged {
        plugin: handle,
        latency: 128,
    });

    let events = mock.poll_events();
    assert_eq!(events.len(), 2);
    assert_eq!(
        events[0],
        UiEvent::DeviceError {
            status: MH_ERR,
            message: "device failed".to_owned(),
        }
    );
    assert_eq!(
        events[1],
        UiEvent::LatencyChanged {
            plugin: handle,
            latency: 128,
        }
    );
    assert!(mock.poll_events().is_empty());
    assert_eq!(mock.name(), "mock");
    assert_eq!(mock.abi_version().unwrap(), ABI_VERSION);
}

#[test]
fn mock_enumerate_filters_by_kind() {
    let mock = MockBackend::new();
    mock.add_device(DeviceInfo {
        name: "input".to_owned(),
        inputs: 2,
        ..DeviceInfo::default()
    });
    mock.add_device(DeviceInfo {
        name: "output".to_owned(),
        outputs: 2,
        ..DeviceInfo::default()
    });

    assert_eq!(mock.enumerate_devices(DeviceKind::Input).unwrap().len(), 1);
    assert_eq!(mock.enumerate_devices(DeviceKind::Output).unwrap().len(), 1);
}

#[test]
fn mock_scan_reports_progress() {
    let mock = MockBackend::new();
    mock.add_descriptor(PluginDescriptor {
        name: "one".to_owned(),
        ..PluginDescriptor::default()
    });
    mock.add_descriptor(PluginDescriptor {
        name: "two".to_owned(),
        ..PluginDescriptor::default()
    });

    let mut seen = Vec::new();
    let descriptors = mock
        .scan(&mut |descriptor| seen.push(descriptor.name.clone()))
        .unwrap();
    assert_eq!(descriptors.len(), 2);
    assert_eq!(seen, vec!["one".to_owned(), "two".to_owned()]);
}

#[test]
fn backend_processor_delegates_to_host() {
    let mock = Arc::new(MockBackend::new());
    mock.set_gain(3.0);
    let handle = mock.load_plugin("plugin.dll", "class").unwrap();
    let host: Arc<dyn PluginHost> = mock;
    let mut processor = BackendProcessor::new(host, handle);

    let mut channel = vec![1.0f32, 2.0];
    {
        let mut channels: [&mut [f32]; 1] = [&mut channel];
        processor.process(&mut channels, 2);
    }
    assert_eq!(channel, vec![3.0, 6.0]);
}
