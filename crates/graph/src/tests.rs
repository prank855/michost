use super::*;
use std::collections::HashMap;

struct GainAdd {
    gain: f32,
    add: f32,
}

impl PluginProcessor for GainAdd {
    fn process(&mut self, channels: &mut [&mut [f32]], frames: usize) {
        if let Some(main) = channels.first_mut() {
            for sample in main.iter_mut().take(frames) {
                *sample = *sample * self.gain + self.add;
            }
        }
    }
}

struct Identity;

impl PluginProcessor for Identity {
    fn process(&mut self, _channels: &mut [&mut [f32]], _frames: usize) {}
}

struct SumAux {
    main_inputs: usize,
    aux_inputs: usize,
}

impl PluginProcessor for SumAux {
    fn process(&mut self, channels: &mut [&mut [f32]], frames: usize) {
        let (main, aux) = channels.split_at_mut(self.main_inputs);
        let main_channel = &mut main[0];
        let aux_channels = &aux[..self.aux_inputs];
        for (frame, sample) in main_channel.iter_mut().take(frames).enumerate() {
            for aux_channel in aux_channels.iter() {
                *sample += aux_channel[frame];
            }
        }
    }
}

struct SumStereo;

impl PluginProcessor for SumStereo {
    fn process(&mut self, channels: &mut [&mut [f32]], frames: usize) {
        let (main, others) = channels.split_at_mut(1);
        let main_channel = &mut main[0];
        if let Some(other) = others.first() {
            for (sample, other_sample) in main_channel.iter_mut().zip(other.iter()).take(frames) {
                *sample += *other_sample;
            }
        }
    }
}

fn entry(
    id: u64,
    main_inputs: u16,
    main_outputs: u16,
    sidechain_channels: u16,
    source: SidechainSource,
    bypassed: bool,
) -> PlanNode {
    PlanNode {
        id,
        main_inputs,
        main_outputs,
        sidechain_channels,
        sidechain_source: source,
        bypassed,
        latency: 0,
    }
}

fn plan(entries: &[PlanNode], latencies: &[(u64, u32)], block_size: usize) -> ChainPlan {
    let latencies: HashMap<u64, u32> = latencies.iter().copied().collect();
    let nodes = entries
        .iter()
        .map(|node| PlanNode {
            latency: latencies.get(&node.id).copied().unwrap_or(node.latency),
            ..*node
        })
        .collect();
    ChainPlan::from_nodes(nodes, block_size)
}

fn node(
    id: u64,
    main_inputs: u16,
    main_outputs: u16,
    sidechain_channels: u16,
    bypassed: bool,
) -> PlanNode {
    entry(
        id,
        main_inputs,
        main_outputs,
        sidechain_channels,
        SidechainSource::Off,
        bypassed,
    )
}

#[test]
fn chain_passes_audio_through_identity_processor() {
    let entries = [entry(1, 2, 2, 0, SidechainSource::Off, false)];
    let plan = plan(&entries, &[(1, 7)], 16);
    let mut engine = ChainEngine::new(
        plan,
        vec![(1, Box::new(Identity) as Box<dyn PluginProcessor>)],
        16,
    )
    .unwrap();

    let left = [0.25, -0.5, 1.0, 0.0];
    let right = [-0.25, 0.75, 2.0, -1.0];
    let mut out_left = [0.0; 4];
    let mut out_right = [0.0; 4];
    let inputs: [&[f32]; 2] = [&left, &right];
    let mut outputs: [&mut [f32]; 2] = [&mut out_left, &mut out_right];

    engine.process(&inputs, &[], &mut outputs, 4);

    assert_eq!(out_left, left);
    assert_eq!(out_right, right);
    assert_eq!(engine.total_latency(), 7);
    assert_eq!(engine.block_size(), 16);
    assert!(engine.estimated_memory_bytes() > 0);
}

#[test]
fn processing_order_is_observable() {
    let input = [1.0, 2.0, 3.0];
    let inputs: [&[f32]; 1] = [&input];

    let entries_ab = [
        entry(1, 1, 1, 0, SidechainSource::Off, false),
        entry(2, 1, 1, 0, SidechainSource::Off, false),
    ];
    let mut engine_ab = ChainEngine::new(
        plan(&entries_ab, &[], 8),
        vec![
            (
                1,
                Box::new(GainAdd {
                    gain: 1.0,
                    add: 1.0,
                }) as Box<dyn PluginProcessor>,
            ),
            (
                2,
                Box::new(GainAdd {
                    gain: 2.0,
                    add: 0.0,
                }) as Box<dyn PluginProcessor>,
            ),
        ],
        8,
    )
    .unwrap();
    let mut ab_left = [0.0; 3];
    let mut ab_right = [0.0; 3];
    let mut ab_outputs: [&mut [f32]; 2] = [&mut ab_left, &mut ab_right];
    engine_ab.process(&inputs, &[], &mut ab_outputs, 3);
    assert_eq!(ab_left, [4.0, 6.0, 8.0]);

    let entries_ba = [
        entry(1, 1, 1, 0, SidechainSource::Off, false),
        entry(2, 1, 1, 0, SidechainSource::Off, false),
    ];
    let mut engine_ba = ChainEngine::new(
        plan(&entries_ba, &[], 8),
        vec![
            (
                1,
                Box::new(GainAdd {
                    gain: 2.0,
                    add: 0.0,
                }) as Box<dyn PluginProcessor>,
            ),
            (
                2,
                Box::new(GainAdd {
                    gain: 1.0,
                    add: 1.0,
                }) as Box<dyn PluginProcessor>,
            ),
        ],
        8,
    )
    .unwrap();
    let mut ba_left = [0.0; 3];
    let mut ba_right = [0.0; 3];
    let mut ba_outputs: [&mut [f32]; 2] = [&mut ba_left, &mut ba_right];
    engine_ba.process(&inputs, &[], &mut ba_outputs, 3);

    assert_eq!(ba_left, [3.0, 5.0, 7.0]);
    assert_ne!(ab_left, ba_left);
}

#[test]
fn bypass_skips_processor_and_latency() {
    let entries = [entry(1, 2, 2, 0, SidechainSource::Off, true)];
    let plan = plan(&entries, &[(1, 500)], 8);
    let mut engine = ChainEngine::new(
        plan,
        vec![(
            1,
            Box::new(GainAdd {
                gain: 1000.0,
                add: 1.0,
            }) as Box<dyn PluginProcessor>,
        )],
        8,
    )
    .unwrap();

    let left = [0.5, -1.0];
    let right = [2.0, 0.0];
    let mut out_left = [0.0; 2];
    let mut out_right = [0.0; 2];
    let inputs: [&[f32]; 2] = [&left, &right];
    let mut outputs: [&mut [f32]; 2] = [&mut out_left, &mut out_right];

    engine.process(&inputs, &[], &mut outputs, 2);

    assert_eq!(out_left, left);
    assert_eq!(out_right, right);
    assert_eq!(engine.total_latency(), 0);
    assert_eq!(engine.plan.active_nodes(), 0);
}

#[test]
fn sidechain_channels_follow_main_and_use_raw_input() {
    let raw = [
        [1.0, 1.0, 1.0, 1.0],
        [2.0, 2.0, 2.0, 2.0],
        [3.0, 3.0, 3.0, 3.0],
        [4.0, 4.0, 4.0, 4.0],
    ];
    let inputs: [&[f32]; 4] = [&raw[0], &raw[1], &raw[2], &raw[3]];

    let cases = [
        (SidechainSource::Off, 1.0),
        (SidechainSource::MainMic, 4.0),
        (SidechainSource::InputChannel(1), 6.0),
        (SidechainSource::InputChannel(2), 8.0),
        (SidechainSource::Silence, 1.0),
    ];

    for (source, expected) in cases {
        let entries = [entry(1, 1, 1, 2, source, false)];
        let mut engine = ChainEngine::new(
            plan(&entries, &[], 4),
            vec![(
                1,
                Box::new(SumAux {
                    main_inputs: 1,
                    aux_inputs: 2,
                }) as Box<dyn PluginProcessor>,
            )],
            4,
        )
        .unwrap();
        let mut out_left = [0.0; 4];
        let mut out_right = [0.0; 4];
        let mut outputs: [&mut [f32]; 2] = [&mut out_left, &mut out_right];
        engine.process(&inputs, &[], &mut outputs, 4);
        assert_eq!(out_left, [expected; 4], "source {source:?}");
        assert_eq!(out_right, [expected; 4], "source {source:?}");
    }
}

#[test]
fn second_input_sidechain_reads_second_device_channels() {
    let main = [1.0f32; 4];
    let channel0 = [10.0f32; 4];
    let channel1 = [20.0f32; 4];
    let channel2 = [30.0f32; 4];
    let inputs: [&[f32]; 1] = [&main];
    let second: [&[f32]; 3] = [&channel0, &channel1, &channel2];

    let cases = [
        (SidechainSource::SecondInput(0), 31.0),
        (SidechainSource::SecondInput(1), 51.0),
        (SidechainSource::SecondInput(2), 61.0),
        (SidechainSource::SecondInput(3), 1.0),
    ];
    for (source, expected) in cases {
        let entries = [entry(1, 1, 1, 2, source, false)];
        let mut engine = ChainEngine::new(
            plan(&entries, &[], 4),
            vec![(
                1,
                Box::new(SumAux {
                    main_inputs: 1,
                    aux_inputs: 2,
                }) as Box<dyn PluginProcessor>,
            )],
            4,
        )
        .unwrap();
        let mut out_left = [0.0; 4];
        let mut out_right = [0.0; 4];
        let mut outputs: [&mut [f32]; 2] = [&mut out_left, &mut out_right];
        engine.process(&inputs, &second, &mut outputs, 4);
        assert_eq!(out_left, [expected; 4], "source {source:?}");
        assert_eq!(out_right, [expected; 4], "source {source:?}");
    }

    let mono_second: [&[f32]; 1] = [&channel0];
    let entries = [entry(1, 1, 1, 2, SidechainSource::SecondInput(0), false)];
    let mut engine = ChainEngine::new(
        plan(&entries, &[], 4),
        vec![(
            1,
            Box::new(SumAux {
                main_inputs: 1,
                aux_inputs: 2,
            }) as Box<dyn PluginProcessor>,
        )],
        4,
    )
    .unwrap();
    let mut out_left = [0.0; 4];
    let mut out_right = [0.0; 4];
    let mut outputs: [&mut [f32]; 2] = [&mut out_left, &mut out_right];
    engine.process(&inputs, &mono_second, &mut outputs, 4);
    assert_eq!(out_left, [21.0; 4]);
}

#[test]
fn second_input_sidechain_follows_chain_order() {
    let main = [1.0f32; 3];
    let channel0 = [10.0f32; 3];
    let inputs: [&[f32]; 1] = [&main];
    let second: [&[f32]; 1] = [&channel0];

    let gain_then_sidechain = [
        entry(1, 1, 1, 0, SidechainSource::Off, false),
        entry(2, 1, 1, 1, SidechainSource::SecondInput(0), false),
    ];
    let mut first = ChainEngine::new(
        plan(&gain_then_sidechain, &[], 4),
        vec![
            (
                1,
                Box::new(GainAdd {
                    gain: 2.0,
                    add: 0.0,
                }) as Box<dyn PluginProcessor>,
            ),
            (
                2,
                Box::new(SumAux {
                    main_inputs: 1,
                    aux_inputs: 1,
                }) as Box<dyn PluginProcessor>,
            ),
        ],
        4,
    )
    .unwrap();
    let mut first_left = [0.0; 3];
    let mut first_right = [0.0; 3];
    let mut first_outputs: [&mut [f32]; 2] = [&mut first_left, &mut first_right];
    first.process(&inputs, &second, &mut first_outputs, 3);

    let sidechain_then_gain = [
        entry(1, 1, 1, 1, SidechainSource::SecondInput(0), false),
        entry(2, 1, 1, 0, SidechainSource::Off, false),
    ];
    let mut second_engine = ChainEngine::new(
        plan(&sidechain_then_gain, &[], 4),
        vec![
            (
                1,
                Box::new(SumAux {
                    main_inputs: 1,
                    aux_inputs: 1,
                }) as Box<dyn PluginProcessor>,
            ),
            (
                2,
                Box::new(GainAdd {
                    gain: 2.0,
                    add: 0.0,
                }) as Box<dyn PluginProcessor>,
            ),
        ],
        4,
    )
    .unwrap();
    let mut second_left = [0.0; 3];
    let mut second_right = [0.0; 3];
    let mut second_outputs: [&mut [f32]; 2] = [&mut second_left, &mut second_right];
    second_engine.process(&inputs, &second, &mut second_outputs, 3);

    assert_eq!(first_left, [12.0; 3]);
    assert_eq!(second_left, [22.0; 3]);
    assert_ne!(first_left, second_left);
}

#[test]
fn mono_input_duplicates_and_mono_output_works() {
    let mono = [1.0, -2.0, 0.5];
    let mono_inputs: [&[f32]; 1] = [&mono];
    let entries = [entry(1, 2, 2, 0, SidechainSource::Off, false)];
    let mut engine = ChainEngine::new(
        plan(&entries, &[], 8),
        vec![(1, Box::new(Identity) as Box<dyn PluginProcessor>)],
        8,
    )
    .unwrap();
    let mut out_left = [0.0; 3];
    let mut out_right = [0.0; 3];
    let mut outputs: [&mut [f32]; 2] = [&mut out_left, &mut out_right];
    engine.process(&mono_inputs, &[], &mut outputs, 3);
    assert_eq!(out_left, mono);
    assert_eq!(out_right, mono);

    let left = [1.0, 2.0];
    let right = [10.0, 20.0];
    let stereo_inputs: [&[f32]; 2] = [&left, &right];
    let entries = [entry(1, 2, 1, 0, SidechainSource::Off, false)];
    let mut engine = ChainEngine::new(
        plan(&entries, &[], 8),
        vec![(1, Box::new(SumStereo) as Box<dyn PluginProcessor>)],
        8,
    )
    .unwrap();
    let mut out_left = [0.0; 2];
    let mut out_right = [0.0; 2];
    let mut outputs: [&mut [f32]; 2] = [&mut out_left, &mut out_right];
    engine.process(&stereo_inputs, &[], &mut outputs, 2);
    assert_eq!(out_left, [11.0, 22.0]);
    assert_eq!(out_right, [11.0, 22.0]);
}

#[test]
fn latency_sums_only_active_nodes() {
    let entries = [
        entry(1, 2, 2, 0, SidechainSource::Off, false),
        entry(2, 2, 2, 0, SidechainSource::Off, true),
        entry(3, 2, 2, 0, SidechainSource::Off, false),
    ];
    let plan = plan(&entries, &[(1, 11), (2, 13), (3, 17)], 8);
    assert_eq!(plan.total_latency(), 28);
    assert_eq!(plan.active_nodes(), 2);
}

#[test]
fn drift_correction_direction_and_clamping() {
    assert!(drift_correction(120, 100, 0.1) > 1.0);
    assert!(drift_correction(80, 100, 0.1) < 1.0);
    assert!((drift_correction(200, 100, 0.1) - 1.1).abs() < 1.0e-9);
    assert!((drift_correction(0, 100, 0.1) - 0.9).abs() < 1.0e-9);
    assert!((drift_correction(1000, 100, 5.0) - 1.1).abs() < 1.0e-9);
    assert_eq!(drift_correction(150, 100, 0.0), 1.0);
    assert_eq!(drift_correction(150, 100, -2.0), 1.0);
    assert_eq!(drift_correction(1, 0, 0.1), 1.0);
}

#[test]
fn invalid_plans_are_rejected() {
    let duplicate =
        ChainPlan::from_nodes(vec![node(1, 2, 2, 0, false), node(1, 2, 2, 0, false)], 16);
    assert!(ChainEngine::new(duplicate, Vec::new(), 16).is_err());

    let too_many_channels = ChainPlan::from_nodes(vec![node(1, 32, 2, 1, false)], 16);
    assert!(ChainEngine::new(too_many_channels, Vec::new(), 16).is_err());

    let too_many_outputs = ChainPlan::from_nodes(vec![node(1, 2, 33, 0, false)], 16);
    assert!(ChainEngine::new(too_many_outputs, Vec::new(), 16).is_err());

    let missing_processor = ChainPlan::from_nodes(vec![node(1, 2, 2, 0, false)], 16);
    assert!(ChainEngine::new(missing_processor, Vec::new(), 16).is_err());

    let valid = ChainPlan::from_nodes(vec![node(1, 2, 2, 0, false)], 16);
    assert!(ChainEngine::new(valid.clone(), Vec::new(), 0).is_err());
    assert!(ChainEngine::new(valid, Vec::new(), MAX_FRAMES + 1).is_err());

    let valid = ChainPlan::from_nodes(vec![node(1, 2, 2, 0, false)], 16);
    let engine = ChainEngine::new(
        valid,
        vec![
            (999, Box::new(Identity) as Box<dyn PluginProcessor>),
            (1, Box::new(Identity) as Box<dyn PluginProcessor>),
        ],
        16,
    );
    assert!(engine.is_ok());
}

#[test]
fn empty_block_and_bypassed_node_without_processor_are_safe() {
    let entries = [entry(1, 2, 2, 0, SidechainSource::Off, true)];
    let plan = plan(&entries, &[(1, 40)], 8);
    let mut engine = ChainEngine::new(plan, Vec::new(), 8).unwrap();
    assert_eq!(engine.total_latency(), 0);

    let left = [1.0, 2.0, 3.0, 4.0];
    let right = [5.0, 6.0, 7.0, 8.0];
    let inputs: [&[f32]; 2] = [&left, &right];
    let mut out_left = [9.0; 4];
    let mut out_right = [9.0; 4];

    {
        let mut outputs: [&mut [f32]; 2] = [&mut out_left, &mut out_right];
        engine.process(&inputs, &[], &mut outputs, 0);
    }
    assert_eq!(out_left, [9.0; 4]);
    assert_eq!(out_right, [9.0; 4]);

    {
        let mut outputs: [&mut [f32]; 2] = [&mut out_left, &mut out_right];
        engine.process(&inputs, &[], &mut outputs, 4);
    }
    assert_eq!(out_left, left);
    assert_eq!(out_right, right);
}

fn plan_mono(entries: &[PlanNode], block_size: usize, mono_source: Option<u8>) -> ChainPlan {
    let mut plan = plan(entries, &[], block_size);
    plan.set_processing_options(48000.0, mono_source);
    plan
}

#[test]
fn mono_source_routes_the_selected_raw_channel_to_both_outputs() {
    let left = [1.0f32, 2.0, 3.0];
    let right = [10.0f32, 20.0, 30.0];
    let inputs: [&[f32]; 2] = [&left, &right];
    let entries = [entry(1, 2, 2, 0, SidechainSource::Off, false)];

    for (mono, expected) in [
        (Some(0u8), [1.0f32, 2.0, 3.0]),
        (Some(1u8), [10.0f32, 20.0, 30.0]),
    ] {
        let mut engine = ChainEngine::new(
            plan_mono(&entries, 8, mono),
            vec![(1, Box::new(Identity) as Box<dyn PluginProcessor>)],
            8,
        )
        .unwrap();
        let mut out_left = [0.0; 3];
        let mut out_right = [0.0; 3];
        let mut outputs: [&mut [f32]; 2] = [&mut out_left, &mut out_right];
        engine.process(&inputs, &[], &mut outputs, 3);
        assert_eq!(out_left, expected, "mono {mono:?}");
        assert_eq!(out_right, expected, "mono {mono:?}");
    }

    let mut engine = ChainEngine::new(
        plan_mono(&entries, 8, None),
        vec![(1, Box::new(Identity) as Box<dyn PluginProcessor>)],
        8,
    )
    .unwrap();
    let mut out_left = [0.0; 3];
    let mut out_right = [0.0; 3];
    let mut outputs: [&mut [f32]; 2] = [&mut out_left, &mut out_right];
    engine.process(&inputs, &[], &mut outputs, 3);
    assert_eq!(out_left, left);
    assert_eq!(out_right, right);
}

#[test]
fn mono_right_falls_back_to_the_only_captured_channel() {
    let only = [4.0f32, -5.0, 6.0];
    let inputs: [&[f32]; 1] = [&only];
    let entries = [entry(1, 2, 2, 0, SidechainSource::Off, false)];
    let mut engine = ChainEngine::new(
        plan_mono(&entries, 8, Some(1)),
        vec![(1, Box::new(Identity) as Box<dyn PluginProcessor>)],
        8,
    )
    .unwrap();
    let mut out_left = [0.0; 3];
    let mut out_right = [0.0; 3];
    let mut outputs: [&mut [f32]; 2] = [&mut out_left, &mut out_right];
    engine.process(&inputs, &[], &mut outputs, 3);
    assert_eq!(out_left, only);
    assert_eq!(out_right, only);
}

#[test]
fn mono_source_keeps_sidechain_input_indices_raw() {
    let raw0 = [1.0f32; 4];
    let raw1 = [2.0f32; 4];
    let inputs: [&[f32]; 2] = [&raw0, &raw1];
    let entries = [entry(1, 1, 1, 1, SidechainSource::InputChannel(1), false)];
    let mut engine = ChainEngine::new(
        plan_mono(&entries, 4, Some(0)),
        vec![(
            1,
            Box::new(SumAux {
                main_inputs: 1,
                aux_inputs: 1,
            }) as Box<dyn PluginProcessor>,
        )],
        4,
    )
    .unwrap();
    let mut out_left = [0.0; 4];
    let mut out_right = [0.0; 4];
    let mut outputs: [&mut [f32]; 2] = [&mut out_left, &mut out_right];
    engine.process(&inputs, &[], &mut outputs, 4);
    assert_eq!(out_left, [3.0; 4]);
    assert_eq!(out_right, [3.0; 4]);
}

#[test]
fn meter_averages_cpu_fraction_over_one_second() {
    let meter = PluginMeter::new();
    meter.record(1000, std::time::Duration::from_millis(50), 1000.0);
    assert_eq!(meter.calls(), 1);
    assert_eq!(meter.frames(), 1000);
    assert!((meter.cpu_fraction() - 0.05).abs() < 1.0e-4);
    assert!(meter.busy_nanos() >= 50_000_000);

    meter.record(1000, std::time::Duration::from_millis(100), 1000.0);
    assert_eq!(meter.calls(), 2);
    assert!((meter.cpu_fraction() - 0.1).abs() < 1.0e-4);

    meter.reset();
    assert_eq!(meter.calls(), 0);
    assert_eq!(meter.frames(), 0);
    assert_eq!(meter.busy_nanos(), 0);
    assert_eq!(meter.cpu_fraction(), 0.0);
}

#[test]
fn meter_ignores_invalid_records() {
    let meter = PluginMeter::new();
    meter.record(0, std::time::Duration::from_millis(1), 48000.0);
    meter.record(64, std::time::Duration::from_millis(1), 0.0);
    meter.record(64, std::time::Duration::from_millis(1), f64::NAN);
    assert_eq!(meter.calls(), 0);
    assert_eq!(meter.cpu_fraction(), 0.0);
}

#[test]
fn metered_processor_records_through_the_engine() {
    let entries = [entry(1, 2, 2, 0, SidechainSource::Off, false)];
    let meter = std::sync::Arc::new(PluginMeter::new());
    let mut engine = ChainEngine::new(
        plan_mono(&entries, 8, None),
        vec![(
            1,
            Box::new(MeteredProcessor::new(
                Box::new(Identity) as Box<dyn PluginProcessor>,
                meter.clone(),
                48000.0,
            )) as Box<dyn PluginProcessor>,
        )],
        8,
    )
    .unwrap();
    let left = [1.0f32, 2.0];
    let right = [3.0f32, 4.0];
    let inputs: [&[f32]; 2] = [&left, &right];
    let mut out_left = [0.0; 2];
    let mut out_right = [0.0; 2];
    let mut outputs: [&mut [f32]; 2] = [&mut out_left, &mut out_right];
    engine.process(&inputs, &[], &mut outputs, 2);
    assert_eq!(out_left, left);
    assert_eq!(meter.calls(), 1);
    assert_eq!(meter.frames(), 2);
    assert!(meter.cpu_fraction().is_finite());
    assert!(meter.cpu_fraction() >= 0.0);
}

#[test]
fn samples_to_milliseconds_converts_and_rejects_invalid_rates() {
    assert_eq!(samples_to_milliseconds(48_000, 48_000.0), Some(1000.0));
    assert_eq!(
        samples_to_milliseconds(256, 48_000.0),
        Some(256.0 * 1000.0 / 48_000.0)
    );
    assert_eq!(samples_to_milliseconds(256, 0.0), None);
    assert_eq!(samples_to_milliseconds(256, -1.0), None);
    assert_eq!(samples_to_milliseconds(256, f64::NAN), None);
}

#[test]
fn plan_order_tracks_insert_reorder_and_remove() {
    let a = entry(1, 2, 2, 0, SidechainSource::Off, false);
    let b = entry(2, 2, 2, 0, SidechainSource::Off, false);
    let c = entry(3, 2, 2, 0, SidechainSource::Off, false);
    let latency = [(1, 10), (2, 20), (3, 40)];

    let original = plan(&[a, b], &latency, 8);
    assert_eq!(
        original
            .nodes
            .iter()
            .map(|node| node.id)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(original.total_latency(), 30);

    let inserted = plan(&[a, c, b], &latency, 8);
    assert_eq!(
        inserted
            .nodes
            .iter()
            .map(|node| node.id)
            .collect::<Vec<_>>(),
        vec![1, 3, 2]
    );
    assert_eq!(inserted.total_latency(), 70);

    let reordered = plan(&[c, b, a], &latency, 8);
    assert_eq!(
        reordered
            .nodes
            .iter()
            .map(|node| node.id)
            .collect::<Vec<_>>(),
        vec![3, 2, 1]
    );

    let removed = plan(&[a, b], &latency, 8);
    assert_eq!(
        removed.nodes.iter().map(|node| node.id).collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(removed.total_latency(), 30);
}
