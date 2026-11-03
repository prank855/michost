use super::{
    BlockResampler, LoudnessMeter, sanitize_step, virtual_mic_output_target,
    virtual_mic_ring_target,
};
use michost_graph::drift_correction;

#[test]
fn downsample_step_two_halves_the_rate() {
    let input: Vec<f32> = (0..8).map(|value| value as f32).collect();
    let mut output = vec![0.0f32; 16];
    let mut resampler = BlockResampler::new(2.0);
    let produced = resampler.process_block(&input, &mut output);
    assert_eq!(produced, 4);
    assert_eq!(&output[..produced], &[0.0, 2.0, 4.0, 6.0]);
}

#[test]
fn upsample_keeps_continuity_across_blocks() {
    let mut resampler = BlockResampler::new(0.5);
    let first: Vec<f32> = (0..8).map(|value| value as f32).collect();
    let second: Vec<f32> = (8..16).map(|value| value as f32).collect();
    let mut first_out = vec![0.0f32; 32];
    let mut second_out = vec![0.0f32; 32];
    let first_count = resampler.process_block(&first, &mut first_out);
    let second_count = resampler.process_block(&second, &mut second_out);
    assert!(first_count > 8);
    assert!(second_count > 8);
    assert!((first_out[first_count - 1] - 7.0).abs() < 1e-6);
    assert!((second_out[0] - 7.5).abs() < 1e-6);
    assert!(
        second_out[..second_count]
            .iter()
            .all(|sample| sample.is_finite())
    );
}

#[test]
fn step_is_sanitized() {
    assert_eq!(sanitize_step(f64::NAN), 1.0);
    assert_eq!(sanitize_step(0.0), 0.1);
    assert_eq!(sanitize_step(100.0), 10.0);
}

#[test]
fn stream_resampler_consumes_and_continues_across_blocks() {
    let mut samples = (0..64).map(|value| value as f32).collect::<Vec<_>>();
    samples.reverse();
    let mut resampler = BlockResampler::new(0.5);
    let mut first = vec![0.0f32; 8];
    let (produced, consumed) = resampler.process_stream(&mut first, || samples.pop());
    assert_eq!(produced, 8);
    assert!(consumed <= 5);

    let mut second = vec![0.0f32; 8];
    let (produced, _) = resampler.process_stream(&mut second, || samples.pop());
    assert_eq!(produced, 8);
    assert!(first.windows(2).all(|pair| pair[1] >= pair[0]));
    assert!(second.windows(2).all(|pair| pair[1] >= pair[0]));
    assert!((second[0] - (first[7] + 0.5)).abs() < 1e-6);
}

#[test]
fn stream_resampler_pads_when_input_runs_out() {
    let mut index = 0;
    let source = [1.0f32, 2.0, 3.0, 4.0];
    let mut resampler = BlockResampler::new(1.0);
    let mut output = vec![0.0f32; 8];
    let (produced, consumed) = resampler.process_stream(&mut output, || {
        let sample = source.get(index).copied();
        index += 1;
        sample
    });
    assert_eq!(produced, 4);
    assert!(consumed >= 4);
    assert_eq!(&output[..produced], &source);

    let mut again = vec![0.0f32; 4];
    let (produced, _) = resampler.process_stream(&mut again, || None);
    assert_eq!(produced, 0);
}

#[test]
fn drift_direction_tracks_ring_fill() {
    let over = drift_correction(4000, 2048, 0.005);
    let under = drift_correction(512, 2048, 0.005);
    assert!(over > 1.0);
    assert!(under < 1.0);
    assert!(over <= 1.005 && under >= 0.995);
}

#[test]
fn virtual_mic_targets_hold_one_and_a_half_packets_plus_one_block() {
    assert_eq!(virtual_mic_ring_target(480, 4096), 720);
    assert_eq!(virtual_mic_ring_target(480, 1024), 720);
    assert_eq!(virtual_mic_ring_target(480, 600), 600);
    assert_eq!(virtual_mic_ring_target(0, 4096), 0);
    assert_eq!(virtual_mic_ring_target(480, 0), 0);

    assert_eq!(virtual_mic_output_target(256, 480), 480);
    assert_eq!(virtual_mic_output_target(1024, 480), 1024);
    assert_eq!(virtual_mic_output_target(16, 16), 128);
}

#[test]
fn loudness_meter_reports_silence_floor() {
    let mut meter = LoudnessMeter::new(48_000.0, 2);
    let silence = vec![0.0f32; 512];
    for _ in 0..20 {
        meter.process(&silence, &silence);
    }
    assert!(meter.lufs() <= -119.0);
}

#[test]
fn loudness_meter_tracks_a_reference_sine() {
    let mut meter = LoudnessMeter::new(48_000.0, 2);
    let mut left = vec![0.0f32; 512];
    let mut right = vec![0.0f32; 512];
    let mut phase = 0.0f32;
    for _ in 0..120 {
        for index in 0..512 {
            let sample = 0.1 * phase.sin();
            left[index] = sample;
            right[index] = sample;
            phase += 2.0 * std::f32::consts::PI * 1_000.0 / 48_000.0;
            if phase > std::f32::consts::TAU {
                phase -= std::f32::consts::TAU;
            }
        }
        meter.process(&left, &right);
    }
    let lufs = meter.lufs();
    assert!(
        (-25.0..=-22.0).contains(&lufs),
        "1 kHz -20 dBFS sine measured {lufs} LUFS"
    );
}

#[test]
fn loudness_meter_treats_mono_and_dual_mono_alike() {
    let mut mono_meter = LoudnessMeter::new(48_000.0, 1);
    let mut stereo_meter = LoudnessMeter::new(48_000.0, 2);
    let samples: Vec<f32> = (0..48_000)
        .map(|index| 0.1 * (2.0 * std::f32::consts::PI * 1_000.0 * index as f32 / 48_000.0).sin())
        .collect();
    for chunk in samples.chunks(512) {
        mono_meter.process(chunk, &[]);
        stereo_meter.process(chunk, chunk);
    }
    assert!((mono_meter.lufs() - stereo_meter.lufs()).abs() < 0.1);
}
