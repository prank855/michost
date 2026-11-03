use std::collections::HashSet;

use crate::dsp_util::{self, Biquad, DcBlocker, DelayLine, EnvelopeFollower, OnePole};
use crate::{
    BuiltinKind, BuiltinProcessor, MAX_BLOCK, ParamCurve, ParamSet, ParamSpec, ParamStyle,
};
use michost_graph::PluginProcessor;

const KINDS: [BuiltinKind; 11] = [
    BuiltinKind::Gain,
    BuiltinKind::NoiseGate,
    BuiltinKind::Compressor,
    BuiltinKind::Eq,
    BuiltinKind::Limiter,
    BuiltinKind::NoiseRemoval,
    BuiltinKind::Reverb,
    BuiltinKind::Delay,
    BuiltinKind::Echo,
    BuiltinKind::PitchShift,
    BuiltinKind::DynamicGate,
];

fn spec(
    unit: &'static str,
    min: f32,
    max: f32,
    default: f32,
    curve: ParamCurve,
    decimals: u8,
    style: ParamStyle,
) -> ParamSpec {
    ParamSpec {
        key: "test",
        name: "Test",
        unit,
        min,
        max,
        default,
        curve,
        decimals,
        style,
        section: "Test",
        visible_when: None,
    }
}

#[test]
fn kinds_have_unique_ids_and_round_trip_paths() {
    assert_eq!(BuiltinKind::ALL, KINDS);
    let mut ids = HashSet::new();
    let mut paths = HashSet::new();
    for kind in BuiltinKind::ALL {
        assert!(ids.insert(kind.id()), "duplicate id {}", kind.id());
        assert!(paths.insert(kind.path()), "duplicate path {}", kind.path());
        assert_eq!(kind.path(), format!("builtin:{}", kind.id()).as_str());
        assert_eq!(BuiltinKind::from_id(kind.id()), Some(kind));
        assert_eq!(BuiltinKind::from_path(kind.path()), Some(kind));
        assert_eq!(BuiltinKind::from_path(kind.id()), Some(kind));
        assert_eq!(kind.vendor(), "MicHost");
        assert!(!kind.name().is_empty());
        assert!(!kind.category().is_empty());
        assert!(!kind.params().is_empty());
    }
    assert_eq!(BuiltinKind::from_id("nope"), None);
    assert_eq!(BuiltinKind::from_path("builtin:nope"), None);
    assert_eq!(BuiltinKind::from_path(""), None);
    assert_eq!(BuiltinKind::from_path("builtin:"), None);
}

#[test]
fn kinds_declare_expected_latency() {
    assert_eq!(BuiltinKind::Limiter.latency_samples(48_000.0), 480);
    assert_eq!(BuiltinKind::PitchShift.latency_samples(48_000.0), 2_400);
    assert_eq!(BuiltinKind::PitchShift.latency_samples(44_100.0), 2_205);
    for kind in [
        BuiltinKind::Gain,
        BuiltinKind::NoiseGate,
        BuiltinKind::Compressor,
        BuiltinKind::Eq,
        BuiltinKind::Reverb,
        BuiltinKind::Delay,
        BuiltinKind::Echo,
    ] {
        assert_eq!(kind.latency_samples(48_000.0), 0);
    }
    assert_eq!(BuiltinKind::NoiseRemoval.latency_samples(48_000.0), 256);
    assert_eq!(BuiltinKind::NoiseRemoval.latency_samples(44_100.0), 256);
    assert_eq!(BuiltinKind::Limiter.latency_samples(0.0), 0);
    assert_eq!(BuiltinKind::Limiter.latency_samples(f64::NAN), 0);
}

#[test]
fn normalization_round_trips() {
    for kind in BuiltinKind::ALL {
        for spec in kind.params() {
            assert!((spec.from_normalized(0.0) - spec.min).abs() < 1e-5);
            assert!((spec.from_normalized(1.0) - spec.max).abs() < 1e-4);
            for n in [0.0, 0.25, 0.5, 0.75, 1.0] {
                let real = spec.from_normalized(n);
                let back = spec.normalized(real);
                assert!(
                    (back - n).abs() < 2e-3,
                    "{} / {}: {back} != {n}",
                    kind.id(),
                    spec.key
                );
            }
        }
    }
}

#[test]
fn log_curve_uses_ratio_mapping() {
    let log = spec(
        "Hz",
        20.0,
        20_000.0,
        1_000.0,
        ParamCurve::Log,
        0,
        ParamStyle::Knob,
    );
    let midpoint = log.from_normalized(0.5);
    assert!((midpoint - 632.455_5).abs() < 0.5, "{midpoint}");
    assert!((log.normalized(midpoint) - 0.5).abs() < 1e-4);

    let degenerate = spec("Hz", 0.0, 100.0, 10.0, ParamCurve::Log, 0, ParamStyle::Knob);
    assert!((degenerate.from_normalized(0.5) - 50.0).abs() < 1e-4);
    assert!((degenerate.normalized(50.0) - 0.5).abs() < 1e-4);
}

#[test]
fn parameter_values_are_clamped_and_non_finite_values_fall_back() {
    let knob = spec(
        "dB",
        -24.0,
        24.0,
        0.0,
        ParamCurve::Linear,
        1,
        ParamStyle::Knob,
    );
    assert_eq!(knob.clamp(-100.0), -24.0);
    assert_eq!(knob.clamp(100.0), 24.0);
    assert_eq!(knob.clamp(f32::NAN), 0.0);
    assert_eq!(knob.clamp(f32::INFINITY), 0.0);
    assert_eq!(knob.normalized(0.0), 0.5);
    assert_eq!(knob.from_normalized(0.5), 0.0);
    assert_eq!(knob.from_normalized(f32::NAN), -24.0);
    assert_eq!(knob.from_normalized(-1.0), -24.0);
    assert_eq!(knob.from_normalized(2.0), 24.0);
}

#[test]
fn format_value_respects_unit_decimals_and_toggles() {
    let db = spec(
        "dB",
        -24.0,
        24.0,
        0.0,
        ParamCurve::Linear,
        1,
        ParamStyle::Knob,
    );
    assert_eq!(db.format_value(-12.0), "-12.0 dB");
    assert_eq!(db.format_value(0.04), "0.0 dB");

    let percent = spec(
        "%",
        0.0,
        100.0,
        50.0,
        ParamCurve::Linear,
        0,
        ParamStyle::Knob,
    );
    assert_eq!(percent.format_value(45.0), "45 %");

    let seconds = spec("s", 0.0, 5.0, 1.0, ParamCurve::Linear, 2, ParamStyle::Knob);
    assert_eq!(seconds.format_value(2.5), "2.50 s");

    let bare = spec("", 0.0, 10.0, 1.0, ParamCurve::Linear, 2, ParamStyle::Knob);
    assert_eq!(bare.format_value(3.5), "3.50");

    let toggle = spec("", 0.0, 1.0, 0.0, ParamCurve::Linear, 0, ParamStyle::Toggle);
    assert_eq!(toggle.format_value(0.0), "Off");
    assert_eq!(toggle.format_value(1.0), "On");
    assert_eq!(toggle.format_value(0.75), "On");

    let choice = spec(
        "",
        0.0,
        2.0,
        0.0,
        ParamCurve::Linear,
        0,
        ParamStyle::Choice(&["Multiband", "Spectral", "Hum"]),
    );
    assert_eq!(choice.format_value(0.0), "Multiband");
    assert_eq!(choice.format_value(1.2), "Spectral");
    assert_eq!(choice.format_value(99.0), "Hum");
}

#[test]
fn param_set_defaults_and_lookup() {
    for kind in BuiltinKind::ALL {
        let params = ParamSet::new(kind);
        assert_eq!(params.kind(), kind);
        assert_eq!(params.specs().len(), kind.params().len());
        for (index, spec) in params.specs().iter().enumerate() {
            assert_eq!(params.get(index), spec.default);
            assert_eq!(
                params.default_normalized(index),
                spec.normalized(spec.default)
            );
            assert!((params.normalized(index) - params.default_normalized(index)).abs() < 1e-6);
        }
        assert!(!params.take_edited());
    }

    let params = ParamSet::new(BuiltinKind::Gain);
    assert_eq!(params.index_of("gain_db"), Some(0));
    assert_eq!(params.index_of("missing"), None);
    assert_eq!(params.get_key("gain_db"), params.get(0));
    assert_eq!(params.get_key("missing"), 0.0);
    assert_eq!(params.get(99), 0.0);
    assert_eq!(params.normalized(99), 0.0);
    assert_eq!(params.default_normalized(99), 0.0);
}

#[test]
fn edited_flag_tracks_set_and_is_cleared_by_take() {
    let params = ParamSet::new(BuiltinKind::Gain);
    assert!(!params.take_edited());

    params.set(0, params.get(0) + 3.0);
    assert!(params.take_edited());
    assert!(!params.take_edited());

    params.set_all_defaults();
    assert!(!params.take_edited());
    assert_eq!(params.get(0), params.specs()[0].default);

    params.set_normalized(0, 0.75);
    assert!(params.take_edited());
    let expected = params.specs()[0].from_normalized(0.75);
    assert_eq!(params.get(0), expected);
}

#[test]
fn encode_decode_round_trip_and_clamp() {
    let source = ParamSet::new(BuiltinKind::Gain);
    source.set(0, 12.0);
    let encoded = source.encode();
    assert_eq!(encoded.len(), 12 + 4 * source.specs().len());
    assert_eq!(&encoded[0..4], b"MHB1");

    let restored = ParamSet::new(BuiltinKind::Gain);
    assert!(restored.decode(&encoded));
    assert_eq!(restored.get(0), 12.0);
    assert!(!restored.take_edited());

    let spec = &restored.specs()[0];
    let mut bytes = restored.encode();
    bytes[12..16].copy_from_slice(&(spec.max + 5.0).to_le_bytes());
    assert!(restored.decode(&bytes));
    assert_eq!(restored.get(0), spec.max);

    bytes[12..16].copy_from_slice(&f32::NAN.to_le_bytes());
    assert!(restored.decode(&bytes));
    assert_eq!(restored.get(0), spec.default);
}

#[test]
fn decode_accepts_short_states_from_older_parameter_lists() {
    let params = ParamSet::new(BuiltinKind::Gain);
    params.set(0, -6.0);
    params.set(1, 50.0);
    let encoded = params.encode();
    let short_len = 12 + 2 * 4;
    let mut short = encoded[..short_len].to_vec();
    short[4..8].copy_from_slice(&2u32.to_le_bytes());

    let restored = ParamSet::new(BuiltinKind::Gain);
    assert!(restored.decode(&short));
    assert_eq!(restored.get(0), -6.0);
    assert_eq!(restored.get(1), 50.0);
    assert_eq!(restored.get(2), restored.specs()[2].default);
}

#[test]
fn decode_ignores_extra_trailing_values() {
    let params = ParamSet::new(BuiltinKind::Gain);
    params.set(0, -6.0);
    let mut encoded = params.encode();
    let count = u32::from_le_bytes([encoded[4], encoded[5], encoded[6], encoded[7]]) as usize;
    encoded.extend_from_slice(&7.0f32.to_le_bytes());
    encoded[4..8].copy_from_slice(&((count + 1) as u32).to_le_bytes());

    let restored = ParamSet::new(BuiltinKind::Gain);
    assert!(restored.decode(&encoded));
    assert_eq!(restored.get(0), -6.0);
}

#[test]
fn decode_rejects_invalid_state() {
    let encoded = ParamSet::new(BuiltinKind::Gain).encode();

    let mut wrong_version = encoded.clone();
    wrong_version[0..4].copy_from_slice(b"MHB2");
    assert!(!ParamSet::new(BuiltinKind::Gain).decode(&wrong_version));

    let mut wrong_magic = encoded.clone();
    wrong_magic[0..4].copy_from_slice(b"XXXX");
    assert!(!ParamSet::new(BuiltinKind::Gain).decode(&wrong_magic));

    assert!(!ParamSet::new(BuiltinKind::Gain).decode(&[]));
    assert!(!ParamSet::new(BuiltinKind::Gain).decode(&encoded[..11]));
    assert!(!ParamSet::new(BuiltinKind::Gain).decode(&encoded[..encoded.len() - 1]));

    let mut too_many = encoded.clone();
    too_many[4..8].copy_from_slice(&99u32.to_le_bytes());
    assert!(!ParamSet::new(BuiltinKind::Gain).decode(&too_many));

    assert!(!ParamSet::new(BuiltinKind::NoiseGate).decode(&encoded));

    let mut wrong_kind = encoded.clone();
    wrong_kind[8] = 255;
    assert!(!ParamSet::new(BuiltinKind::Gain).decode(&wrong_kind));
}

#[test]
fn processor_accepts_mono_stereo_and_oversized_blocks() {
    let params = ParamSet::new(BuiltinKind::Gain);
    let mut processor = BuiltinProcessor::new(BuiltinKind::Gain, params, 48_000.0);

    let mut mono = vec![0.5; 512];
    processor.process(&mut [&mut mono], 512);

    let mut left = vec![0.25; MAX_BLOCK + 64];
    let mut right = vec![-0.25; MAX_BLOCK + 64];
    processor.process(&mut [&mut left, &mut right], MAX_BLOCK + 64);

    let mut quiet = vec![0.0; 8];
    processor.process(&mut [&mut quiet], 0);
}

#[test]
fn db_helpers_are_inverses() {
    assert_eq!(dsp_util::db_to_gain(0.0), 1.0);
    for db in [-60.0, -12.0, 0.0, 6.0] {
        let round_trip = dsp_util::gain_to_db(dsp_util::db_to_gain(db));
        assert!((round_trip - db).abs() < 1e-4, "{db} -> {round_trip}");
    }
    assert!(dsp_util::gain_to_db(0.0).is_infinite());
    assert_eq!(dsp_util::clamp(5.0, 0.0, 1.0), 1.0);
    assert_eq!(dsp_util::clamp(f32::NAN, 0.0, 1.0), 0.0);
}

#[test]
fn smoothing_and_one_pole_converge() {
    let coefficient = dsp_util::smoothing_coefficient(10.0, 48_000.0);
    assert!(coefficient > 0.0 && coefficient < 1.0);
    assert_eq!(dsp_util::smoothing_coefficient(0.0, 48_000.0), 1.0);

    let mut pole = OnePole::from_time(1.0, 48_000.0);
    for _ in 0..4_800 {
        pole.process(1.0);
    }
    assert!((pole.value() - 1.0).abs() < 1e-3);

    let mut follower = EnvelopeFollower::new(1.0, 100.0, 48_000.0);
    for _ in 0..4_800 {
        follower.process(1.0);
    }
    assert!((follower.value() - 1.0).abs() < 1e-2);
    for _ in 0..480 {
        follower.process(0.0);
    }
    assert!(follower.value() > 0.5);
}

#[test]
fn dc_blocker_removes_constant_offset() {
    let mut blocker = DcBlocker::new(48_000.0);
    let mut output = 0.0;
    for _ in 0..48_000 {
        output = blocker.process(1.0);
    }
    assert!(output.abs() < 1e-2, "{output}");
    blocker.reset();
    assert_eq!(blocker.process(0.0), 0.0);
}

#[test]
fn biquad_filters_have_expected_response() {
    let mut lowpass = Biquad::new();
    lowpass.lowpass(48_000.0, 1_000.0, 0.707);
    let mut output = 0.0;
    for _ in 0..4_800 {
        output = lowpass.process(1.0);
    }
    assert!((output - 1.0).abs() < 1e-2, "{output}");

    let mut highpass = Biquad::new();
    highpass.highpass(48_000.0, 1_000.0, 0.707);
    for _ in 0..4_800 {
        output = highpass.process(1.0);
    }
    assert!(output.abs() < 1e-2, "{output}");

    let mut shaped = Biquad::new();
    shaped.peaking(48_000.0, 1_000.0, 1.0, 6.0);
    shaped.low_shelf(48_000.0, 200.0, -3.0, 1.0);
    shaped.high_shelf(48_000.0, 8_000.0, 3.0, 1.0);
    for _ in 0..256 {
        assert!(shaped.process(0.5).is_finite());
    }
    shaped.reset();
    assert_eq!(shaped.process(0.0), 0.0);
}

#[test]
fn delay_line_delays_and_interpolates() {
    let mut delay = DelayLine::new(0.01, 48_000.0);
    assert_eq!(delay.max_delay_samples(), 480.0);

    delay.push(1.0);
    assert_eq!(delay.read(1.0), 1.0);
    delay.push(2.0);
    assert_eq!(delay.read(1.0), 2.0);
    delay.push(0.0);
    assert_eq!(delay.read(2.0), 2.0);
    assert!((delay.read(1.5) - 1.0).abs() < 1e-6);

    delay.clear();
    assert_eq!(delay.read(1.0), 0.0);
}

#[test]
fn soft_clip_is_bounded_and_zero_preserving() {
    assert_eq!(dsp_util::soft_clip(0.0), 0.0);
    for input in [-10.0, -1.5, -0.5, 0.5, 1.5, 10.0] {
        let output = dsp_util::soft_clip(input);
        assert!((-1.0..=1.0).contains(&output), "{input} -> {output}");
    }
    assert!((dsp_util::soft_clip(1.5) - 1.0).abs() < 1e-6);
    assert!((dsp_util::soft_clip(-1.5) + 1.0).abs() < 1e-6);
}
