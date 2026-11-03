use std::sync::Arc;

use michost_graph::PluginProcessor;

use crate::{AudioAnalysis, BuiltinDsp, BuiltinKind, MAX_BLOCK, ParamSet, make_dsp};

/// Graph adapter around a built-in DSP module.
///
/// Supports one- or two-channel slices. Mono input is duplicated into a
/// preallocated right-channel scratch buffer; extra channels are ignored.
/// Scratch is allocated in [`BuiltinProcessor::new`] only.
pub struct BuiltinProcessor {
    kind: BuiltinKind,
    params: Arc<ParamSet>,
    dsp: Box<dyn BuiltinDsp>,
    mono_scratch: Vec<f32>,
    analysis: Option<Arc<AudioAnalysis>>,
}

impl BuiltinProcessor {
    pub fn new(kind: BuiltinKind, params: Arc<ParamSet>, sample_rate: f64) -> Self {
        Self::build(kind, params, sample_rate, None)
    }

    /// Attaches a lock-free analysis tap used by the editor visualisation.
    pub fn with_analysis(
        kind: BuiltinKind,
        params: Arc<ParamSet>,
        sample_rate: f64,
        analysis: Arc<AudioAnalysis>,
    ) -> Self {
        Self::build(kind, params, sample_rate, Some(analysis))
    }

    fn build(
        kind: BuiltinKind,
        params: Arc<ParamSet>,
        sample_rate: f64,
        analysis: Option<Arc<AudioAnalysis>>,
    ) -> Self {
        let dsp = make_dsp(kind, sample_rate, analysis.clone());
        Self {
            kind,
            params,
            dsp,
            mono_scratch: vec![0.0; MAX_BLOCK],
            analysis,
        }
    }

    /// Built-in kind this processor renders.
    pub fn kind(&self) -> BuiltinKind {
        self.kind
    }
}

impl PluginProcessor for BuiltinProcessor {
    fn process(&mut self, channels: &mut [&mut [f32]], frames: usize) {
        if channels.is_empty() || frames == 0 {
            return;
        }
        let available = channels
            .iter()
            .map(|channel| channel.len())
            .min()
            .unwrap_or(0)
            .min(frames);
        if available == 0 {
            return;
        }

        let Self {
            dsp,
            params,
            mono_scratch,
            analysis,
            ..
        } = self;

        if channels.len() == 1 {
            let channel: &mut [f32] = &mut *channels[0];
            let mut offset = 0;
            while offset < available {
                let len = (available - offset).min(MAX_BLOCK);
                let mono = &mut channel[offset..offset + len];
                mono_scratch[..len].copy_from_slice(mono);
                let (left, right) = (mono, &mut mono_scratch[..len]);
                if let Some(analysis) = analysis.as_ref() {
                    analysis.push(left, right);
                }
                dsp.process(left, right, params);
                if let Some(analysis) = analysis.as_ref() {
                    analysis.push_output(left, right);
                }
                offset += len;
            }
        } else {
            let (first, rest) = channels.split_at_mut(1);
            let left_channel: &mut [f32] = &mut *first[0];
            let right_channel: &mut [f32] = &mut *rest[0];
            let mut offset = 0;
            while offset < available {
                let len = (available - offset).min(MAX_BLOCK);
                let left = &mut left_channel[offset..offset + len];
                let right = &mut right_channel[offset..offset + len];
                if let Some(analysis) = analysis.as_ref() {
                    analysis.push(left, right);
                }
                dsp.process(left, right, params);
                if let Some(analysis) = analysis.as_ref() {
                    analysis.push_output(left, right);
                }
                offset += len;
            }
        }
    }
}
