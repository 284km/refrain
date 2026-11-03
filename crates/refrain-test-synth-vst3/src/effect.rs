//! The VST3 test effect's processor: [`refrain_test_tone::effect`], with a
//! stereo input and output and its gain as its state. It takes no events.

use std::cell::Cell;
use std::ffi::c_void;
use std::slice;
use std::sync::atomic::{AtomicU32, Ordering};

use refrain_test_tone::effect;
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComRef};

use crate::{Kind, copy_wstring};

pub(crate) struct EffectProcessor {
    // The gain, as f32 bits: set from a saved state on the main thread.
    gain: AtomicU32,
    process_mode: Cell<i32>,
}

impl Class for EffectProcessor {
    type Interfaces = (IComponent, IAudioProcessor, IProcessContextRequirements);
}

impl EffectProcessor {
    pub(crate) fn new() -> Self {
        Self {
            gain: AtomicU32::new(effect::DEFAULT_GAIN.to_bits()),
            process_mode: Cell::new(ProcessModes_::kRealtime as int32),
        }
    }
}

impl IPluginBaseTrait for EffectProcessor {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }

    unsafe fn terminate(&self) -> tresult {
        kResultOk
    }
}

impl IComponentTrait for EffectProcessor {
    unsafe fn getControllerClassId(&self, class_id: *mut TUID) -> tresult {
        unsafe { *class_id = Kind::Effect.controller_cid() };
        kResultOk
    }

    unsafe fn setIoMode(&self, _mode: IoMode) -> tresult {
        kResultOk
    }

    unsafe fn getBusCount(&self, media_type: MediaType, _dir: BusDirection) -> i32 {
        if media_type as MediaTypes == MediaTypes_::kAudio {
            1
        } else {
            0
        }
    }

    unsafe fn getBusInfo(
        &self,
        media_type: MediaType,
        dir: BusDirection,
        index: i32,
        bus: *mut BusInfo,
    ) -> tresult {
        if index != 0 || media_type as MediaTypes != MediaTypes_::kAudio {
            return kInvalidArgument;
        }
        let bus = unsafe { &mut *bus };
        bus.channelCount = 2;
        let input = dir as BusDirections == BusDirections_::kInput;
        copy_wstring(if input { "Input" } else { "Output" }, &mut bus.name);
        bus.mediaType = media_type;
        bus.direction = dir;
        bus.busType = BusTypes_::kMain as BusType;
        // VST3 enums are `c_int` on Windows and `c_uint` elsewhere.
        #[allow(clippy::unnecessary_cast)]
        let flags = BusInfo_::BusFlags_::kDefaultActive as u32;
        bus.flags = flags;
        kResultOk
    }

    unsafe fn getRoutingInfo(&self, _in: *mut RoutingInfo, _out: *mut RoutingInfo) -> tresult {
        kNotImplemented
    }

    unsafe fn activateBus(
        &self,
        _media_type: MediaType,
        _dir: BusDirection,
        _index: i32,
        _state: TBool,
    ) -> tresult {
        kResultOk
    }

    unsafe fn setActive(&self, _state: TBool) -> tresult {
        kResultOk
    }

    unsafe fn setState(&self, state: *mut IBStream) -> tresult {
        let Some(stream) = (unsafe { ComRef::from_raw(state) }) else {
            return kInvalidArgument;
        };
        let mut bytes = [0u8; 8];
        let mut read = 0;
        let ok = unsafe { stream.read(bytes.as_mut_ptr() as *mut c_void, 8, &mut read) };
        match effect::load_state(&bytes) {
            Some(gain) if ok == kResultOk && read == 8 => {
                self.gain.store(gain.to_bits(), Ordering::Relaxed);
                kResultOk
            }
            _ => kResultFalse,
        }
    }

    unsafe fn getState(&self, state: *mut IBStream) -> tresult {
        let Some(stream) = (unsafe { ComRef::from_raw(state) }) else {
            return kInvalidArgument;
        };
        let gain = f32::from_bits(self.gain.load(Ordering::Relaxed));
        let mut bytes = effect::save_state(gain);
        let mut written = 0;
        let ok = unsafe { stream.write(bytes.as_mut_ptr() as *mut c_void, 8, &mut written) };
        if ok == kResultOk && written == 8 {
            kResultOk
        } else {
            kResultFalse
        }
    }
}

impl IAudioProcessorTrait for EffectProcessor {
    unsafe fn setBusArrangements(
        &self,
        inputs: *mut SpeakerArrangement,
        num_ins: i32,
        outputs: *mut SpeakerArrangement,
        num_outs: i32,
    ) -> tresult {
        if num_ins != 1 || num_outs != 1 {
            return kResultFalse;
        }
        let (input, output) = unsafe { (*inputs, *outputs) };
        if input == output && (output == SpeakerArr::kStereo || output == SpeakerArr::kMono) {
            kResultTrue
        } else {
            kResultFalse
        }
    }

    unsafe fn getBusArrangement(
        &self,
        _dir: BusDirection,
        index: i32,
        arr: *mut SpeakerArrangement,
    ) -> tresult {
        if index != 0 {
            return kInvalidArgument;
        }
        unsafe { *arr = SpeakerArr::kStereo };
        kResultOk
    }

    unsafe fn canProcessSampleSize(&self, size: i32) -> tresult {
        if size as SymbolicSampleSizes == SymbolicSampleSizes_::kSample32 {
            kResultOk
        } else {
            kNotImplemented
        }
    }

    unsafe fn getLatencySamples(&self) -> u32 {
        0
    }

    unsafe fn setupProcessing(&self, setup: *mut ProcessSetup) -> tresult {
        self.process_mode
            .set(crate::note_process_mode(unsafe { &*setup }));
        kResultOk
    }

    unsafe fn setProcessing(&self, _state: TBool) -> tresult {
        kResultOk
    }

    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        let data = unsafe { &*data };
        if data.processMode != self.process_mode.get() {
            return kResultFalse;
        }
        if data.numOutputs < 1 || data.outputs.is_null() {
            // A flush: no audio.
            return kResultOk;
        }
        let frames = data.numSamples.max(0) as usize;
        let gain = f32::from_bits(self.gain.load(Ordering::Relaxed));
        let channels = |bus: &AudioBusBuffers| unsafe {
            slice::from_raw_parts(
                bus.__field0.channelBuffers32,
                bus.numChannels.max(0) as usize,
            )
        };
        let output = unsafe { &mut *data.outputs };
        let inputs = if data.numInputs > 0 && !data.inputs.is_null() {
            channels(unsafe { &*data.inputs })
        } else {
            &[]
        };
        for (c, &out_ptr) in channels(output).iter().enumerate() {
            let out = unsafe { slice::from_raw_parts_mut(out_ptr, frames) };
            match inputs.get(c) {
                // The host may process in place.
                Some(&in_ptr) if in_ptr == out_ptr => {}
                Some(&in_ptr) => {
                    out.copy_from_slice(unsafe { slice::from_raw_parts(in_ptr, frames) })
                }
                None => out.fill(0.0),
            }
            effect::process(gain, out);
        }
        output.silenceFlags = 0;
        kResultOk
    }

    unsafe fn getTailSamples(&self) -> u32 {
        0
    }
}

impl IProcessContextRequirementsTrait for EffectProcessor {
    unsafe fn getProcessContextRequirements(&self) -> u32 {
        0
    }
}
