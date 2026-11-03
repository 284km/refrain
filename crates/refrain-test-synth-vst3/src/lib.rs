//! VST3 instruments for testing refrain's plugin hosting.
//!
//! They play [`refrain_test_tone::Tone`], the same sound as the CLAP test
//! plugins, so the same performance must give the same samples through either
//! plugin format.
//!
//! The module holds two instruments that sound the same:
//!
//! - [`NAME`] maps MIDI controllers to parameters through `IMidiMapping`, the
//!   VST3 way of taking them: the sustain pedal, pitch bend and All Notes Off,
//!   on MIDI channel 1. It plays them itself.
//! - [`NOTES_ONLY_NAME`] maps nothing, so the host has to play the sustain
//!   pedal for it.
//!
//! VST3 has no MIDI controller events. Hosts read a plugin's `IMidiMapping`
//! and send controllers as parameter changes instead.

#![allow(non_snake_case)]

use std::cell::{Cell, RefCell};
use std::ffi::{CString, c_char, c_void};
use std::ptr;
use std::slice;

use refrain_test_tone::Tone;
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComRef, ComWrapper, uid};

pub use refrain_test_tone::{BEND_SEMITONES, FADE_SECONDS, PEAK, RIGHT_GAIN, key_frequency};

/// The instrument that maps MIDI controllers.
pub const NAME: &str = "refrain test synth";

/// The instrument that takes notes only.
pub const NOTES_ONLY_NAME: &str = "refrain test synth (notes only)";

/// Parameters of the [`NAME`] instrument, and the MIDI controllers mapped to
/// them on channel 1.
pub mod params {
    use vst3::Steinberg::Vst::ParamID;

    /// The sustain pedal, from controller 64: down from 64/127.
    pub const SUSTAIN: ParamID = 0;
    /// Pitch bend: the 14-bit value over 16383.
    pub const PITCH_BEND: ParamID = 1;
    /// All Notes Off, from controller 123: any change releases every note.
    pub const ALL_NOTES_OFF: ParamID = 2;
}

const MAX_CHANGES: usize = 4096;

fn copy_cstring(src: &str, dst: &mut [c_char]) {
    let c_string = CString::new(src).unwrap_or_default();
    let bytes = c_string.as_bytes_with_nul();
    for (s, d) in bytes.iter().zip(dst.iter_mut()) {
        *d = *s as c_char;
    }
    if bytes.len() > dst.len()
        && let Some(last) = dst.last_mut()
    {
        *last = 0;
    }
}

fn copy_wstring(src: &str, dst: &mut [TChar]) {
    let mut len = 0;
    for (s, d) in src.encode_utf16().zip(dst.iter_mut()) {
        *d = s as TChar;
        len += 1;
    }
    if len < dst.len() {
        dst[len] = 0;
    } else if let Some(last) = dst.last_mut() {
        *last = 0;
    }
}

/// Which of the two instruments a class belongs to.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Full,
    NotesOnly,
}

impl Kind {
    fn processor_cid(self) -> TUID {
        match self {
            Kind::Full => uid(0x7E5C0A31, 0x2B9D4F1A, 0x9C3E6D52, 0x1F0A8B44),
            Kind::NotesOnly => uid(0x7E5C0A32, 0x2B9D4F1A, 0x9C3E6D52, 0x1F0A8B44),
        }
    }

    fn controller_cid(self) -> TUID {
        match self {
            Kind::Full => uid(0x3D81F6C1, 0x6A2E4B7F, 0x8E05C4A9, 0x52D71E30),
            Kind::NotesOnly => uid(0x3D81F6C2, 0x6A2E4B7F, 0x8E05C4A9, 0x52D71E30),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Kind::Full => NAME,
            Kind::NotesOnly => NOTES_ONLY_NAME,
        }
    }
}

/// Something that happens at a sample inside a block.
#[derive(Clone, Copy)]
enum Change {
    NoteOn {
        channel: u16,
        key: u16,
        velocity: f32,
    },
    NoteOff {
        channel: u16,
        key: u16,
    },
    Param {
        id: ParamID,
        value: f64,
    },
}

struct SynthProcessor {
    kind: Kind,
    tone: RefCell<Tone>,
    // Filled and sorted in process(); allocated in setupProcessing().
    changes: RefCell<Vec<(i32, Change)>>,
}

impl Class for SynthProcessor {
    type Interfaces = (IComponent, IAudioProcessor, IProcessContextRequirements);
}

impl SynthProcessor {
    fn new(kind: Kind) -> Self {
        Self {
            kind,
            tone: RefCell::new(Tone::new(48_000.0)),
            changes: RefCell::new(Vec::with_capacity(MAX_CHANGES)),
        }
    }

    fn apply(&self, change: Change) {
        let mut tone = self.tone.borrow_mut();
        match change {
            Change::NoteOn {
                channel,
                key,
                velocity,
            } => tone.note_on(channel, key, velocity),
            Change::NoteOff { channel, key } => tone.note_off(Some(channel), Some(key)),
            Change::Param {
                id: params::SUSTAIN,
                value,
            } => {
                tone.pedal(0, (value * 127.0).round() >= 64.0);
            }
            Change::Param {
                id: params::PITCH_BEND,
                value,
            } => {
                tone.bend(0, (value * 16383.0).round() as u16);
            }
            Change::Param {
                id: params::ALL_NOTES_OFF,
                ..
            } => tone.all_notes_off(0),
            Change::Param { .. } => {}
        }
    }

    /// Collects the block's notes and parameter changes, in order of sample.
    /// At the same sample, parameter changes come first.
    unsafe fn collect(&self, data: &ProcessData) {
        let mut changes = self.changes.borrow_mut();
        changes.clear();
        if let Some(queues) = unsafe { ComRef::from_raw(data.inputParameterChanges) } {
            for q in 0..unsafe { queues.getParameterCount() } {
                let Some(queue) = (unsafe { ComRef::from_raw(queues.getParameterData(q)) }) else {
                    continue;
                };
                let id = unsafe { queue.getParameterId() };
                for p in 0..unsafe { queue.getPointCount() } {
                    let (mut offset, mut value) = (0, 0.0);
                    if unsafe { queue.getPoint(p, &mut offset, &mut value) } == kResultOk
                        && changes.len() < MAX_CHANGES
                    {
                        changes.push((offset, Change::Param { id, value }));
                    }
                }
            }
        }
        if let Some(events) = unsafe { ComRef::from_raw(data.inputEvents) } {
            for i in 0..unsafe { events.getEventCount() } {
                let mut e: Event = unsafe { std::mem::zeroed() };
                if unsafe { events.getEvent(i, &mut e) } != kResultOk
                    || changes.len() >= MAX_CHANGES
                {
                    continue;
                }
                let change = match u32::from(e.r#type) {
                    Event_::EventTypes_::kNoteOnEvent => {
                        let n = unsafe { e.__field0.noteOn };
                        Change::NoteOn {
                            channel: n.channel as u16,
                            key: n.pitch as u16,
                            velocity: n.velocity,
                        }
                    }
                    Event_::EventTypes_::kNoteOffEvent => {
                        let n = unsafe { e.__field0.noteOff };
                        Change::NoteOff {
                            channel: n.channel as u16,
                            key: n.pitch as u16,
                        }
                    }
                    _ => continue,
                };
                changes.push((e.sampleOffset, change));
            }
        }
        // Stable, and parameter changes go ahead of notes at the same sample.
        changes.sort_by_key(|(offset, change)| (*offset, !matches!(change, Change::Param { .. })));
    }
}

impl IPluginBaseTrait for SynthProcessor {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }

    unsafe fn terminate(&self) -> tresult {
        kResultOk
    }
}

impl IComponentTrait for SynthProcessor {
    unsafe fn getControllerClassId(&self, class_id: *mut TUID) -> tresult {
        unsafe { *class_id = self.kind.controller_cid() };
        kResultOk
    }

    unsafe fn setIoMode(&self, _mode: IoMode) -> tresult {
        kResultOk
    }

    unsafe fn getBusCount(&self, media_type: MediaType, dir: BusDirection) -> i32 {
        match (media_type as MediaTypes, dir as BusDirections) {
            (MediaTypes_::kAudio, BusDirections_::kOutput) => 1,
            (MediaTypes_::kEvent, BusDirections_::kInput) => 1,
            _ => 0,
        }
    }

    unsafe fn getBusInfo(
        &self,
        media_type: MediaType,
        dir: BusDirection,
        index: i32,
        bus: *mut BusInfo,
    ) -> tresult {
        if index != 0 {
            return kInvalidArgument;
        }
        let bus = unsafe { &mut *bus };
        match (media_type as MediaTypes, dir as BusDirections) {
            (MediaTypes_::kAudio, BusDirections_::kOutput) => {
                bus.channelCount = 2;
                copy_wstring("Output", &mut bus.name);
            }
            (MediaTypes_::kEvent, BusDirections_::kInput) => {
                bus.channelCount = 16;
                copy_wstring("Notes", &mut bus.name);
            }
            _ => return kInvalidArgument,
        }
        bus.mediaType = media_type;
        bus.direction = dir;
        bus.busType = BusTypes_::kMain as BusType;
        bus.flags = BusInfo_::BusFlags_::kDefaultActive;
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

    unsafe fn setState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }

    unsafe fn getState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }
}

impl IAudioProcessorTrait for SynthProcessor {
    unsafe fn setBusArrangements(
        &self,
        _inputs: *mut SpeakerArrangement,
        num_ins: i32,
        outputs: *mut SpeakerArrangement,
        num_outs: i32,
    ) -> tresult {
        let output = if num_outs == 1 {
            unsafe { *outputs }
        } else {
            0
        };
        if num_ins == 0 && (output == SpeakerArr::kStereo || output == SpeakerArr::kMono) {
            kResultTrue
        } else {
            kResultFalse
        }
    }

    unsafe fn getBusArrangement(
        &self,
        dir: BusDirection,
        index: i32,
        arr: *mut SpeakerArrangement,
    ) -> tresult {
        if dir as BusDirections == BusDirections_::kOutput && index == 0 {
            unsafe { *arr = SpeakerArr::kStereo };
            kResultOk
        } else {
            kInvalidArgument
        }
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
        let setup = unsafe { &*setup };
        *self.tone.borrow_mut() = Tone::new(setup.sampleRate);
        kResultOk
    }

    unsafe fn setProcessing(&self, state: TBool) -> tresult {
        if state == 0 {
            self.tone.borrow_mut().stop();
        }
        kResultOk
    }

    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        let data = unsafe { &*data };
        unsafe { self.collect(data) };
        let frames = data.numSamples.max(0) as usize;
        if data.numOutputs < 1 || data.outputs.is_null() {
            // A flush: parameters only, no audio.
            for &(_, change) in self.changes.borrow().iter() {
                self.apply(change);
            }
            return kResultOk;
        }
        let bus = unsafe { &mut *data.outputs };
        let channels = unsafe {
            slice::from_raw_parts(
                bus.__field0.channelBuffers32,
                bus.numChannels.max(0) as usize,
            )
        };
        let Some(&left_ptr) = channels.first() else {
            return kResultOk;
        };
        let left = unsafe { slice::from_raw_parts_mut(left_ptr, frames) };
        left.fill(0.0);

        let mut start = 0;
        let changes = self.changes.borrow();
        for &(offset, change) in changes.iter() {
            let at = (offset.max(0) as usize).min(frames);
            if at > start {
                self.tone.borrow_mut().render(&mut left[start..at]);
                start = at;
            }
            self.apply(change);
        }
        self.tone.borrow_mut().render(&mut left[start..]);

        for &right_ptr in &channels[1..] {
            let right = unsafe { slice::from_raw_parts_mut(right_ptr, frames) };
            Tone::write_right(left, right);
        }
        bus.silenceFlags = 0;
        kResultOk
    }

    unsafe fn getTailSamples(&self) -> u32 {
        0
    }
}

impl IProcessContextRequirementsTrait for SynthProcessor {
    unsafe fn getProcessContextRequirements(&self) -> u32 {
        0
    }
}

struct SynthController {
    kind: Kind,
    values: [Cell<f64>; 3],
}

impl Class for SynthController {
    type Interfaces = (IEditController, IMidiMapping);
}

impl SynthController {
    fn new(kind: Kind) -> Self {
        Self {
            kind,
            values: [Cell::new(0.0), Cell::new(8192.0 / 16383.0), Cell::new(0.0)],
        }
    }

    fn param_count(&self) -> i32 {
        if self.kind == Kind::Full { 3 } else { 0 }
    }
}

impl IPluginBaseTrait for SynthController {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }

    unsafe fn terminate(&self) -> tresult {
        kResultOk
    }
}

impl IEditControllerTrait for SynthController {
    unsafe fn setComponentState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }

    unsafe fn setState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }

    unsafe fn getState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }

    unsafe fn getParameterCount(&self) -> i32 {
        self.param_count()
    }

    unsafe fn getParameterInfo(&self, index: i32, info: *mut ParameterInfo) -> tresult {
        if !(0..self.param_count()).contains(&index) {
            return kInvalidArgument;
        }
        let info = unsafe { &mut *info };
        let title = ["Sustain", "Pitch bend", "All notes off"][index as usize];
        info.id = index as ParamID;
        copy_wstring(title, &mut info.title);
        copy_wstring(title, &mut info.shortTitle);
        copy_wstring("", &mut info.units);
        info.stepCount = 0;
        info.defaultNormalizedValue = self.values[index as usize].get();
        info.unitId = 0;
        info.flags = ParameterInfo_::ParameterFlags_::kCanAutomate;
        kResultOk
    }

    unsafe fn getParamStringByValue(
        &self,
        _id: ParamID,
        value: ParamValue,
        string: *mut String128,
    ) -> tresult {
        copy_wstring(&format!("{value:.3}"), unsafe { &mut *string });
        kResultOk
    }

    unsafe fn getParamValueByString(
        &self,
        _id: ParamID,
        _string: *mut TChar,
        _value: *mut ParamValue,
    ) -> tresult {
        kNotImplemented
    }

    unsafe fn normalizedParamToPlain(&self, _id: ParamID, value: ParamValue) -> ParamValue {
        value
    }

    unsafe fn plainParamToNormalized(&self, _id: ParamID, value: ParamValue) -> ParamValue {
        value
    }

    unsafe fn getParamNormalized(&self, id: ParamID) -> ParamValue {
        self.values.get(id as usize).map_or(0.0, Cell::get)
    }

    unsafe fn setParamNormalized(&self, id: ParamID, value: ParamValue) -> tresult {
        match self.values.get(id as usize) {
            Some(cell) if (id as i32) < self.param_count() => {
                cell.set(value);
                kResultOk
            }
            _ => kInvalidArgument,
        }
    }

    unsafe fn setComponentHandler(&self, _handler: *mut IComponentHandler) -> tresult {
        kResultOk
    }

    unsafe fn createView(&self, _name: FIDString) -> *mut IPlugView {
        ptr::null_mut()
    }
}

impl IMidiMappingTrait for SynthController {
    unsafe fn getMidiControllerAssignment(
        &self,
        bus: i32,
        channel: i16,
        controller: CtrlNumber,
        id: *mut ParamID,
    ) -> tresult {
        if self.kind != Kind::Full || bus != 0 || channel != 0 {
            return kResultFalse;
        }
        let param = match controller as ControllerNumbers {
            ControllerNumbers_::kCtrlSustainOnOff => params::SUSTAIN,
            ControllerNumbers_::kPitchBend => params::PITCH_BEND,
            ControllerNumbers_::kCtrlAllNotesOff => params::ALL_NOTES_OFF,
            _ => return kResultFalse,
        };
        unsafe { *id = param };
        kResultTrue
    }
}

struct Factory;

impl Class for Factory {
    type Interfaces = (IPluginFactory2,);
}

const KINDS: [Kind; 2] = [Kind::Full, Kind::NotesOnly];

impl Factory {
    /// Classes come in pairs: a processor, then its controller.
    fn class(index: i32) -> Option<(Kind, bool)> {
        let kind = *KINDS.get(usize::try_from(index).ok()? / 2)?;
        Some((kind, index % 2 == 0))
    }
}

impl IPluginFactoryTrait for Factory {
    unsafe fn getFactoryInfo(&self, info: *mut PFactoryInfo) -> tresult {
        let info = unsafe { &mut *info };
        copy_cstring("refrain", &mut info.vendor);
        copy_cstring("https://github.com/284km/refrain", &mut info.url);
        copy_cstring("", &mut info.email);
        info.flags = PFactoryInfo_::FactoryFlags_::kUnicode as int32;
        kResultOk
    }

    unsafe fn countClasses(&self) -> i32 {
        KINDS.len() as i32 * 2
    }

    unsafe fn getClassInfo(&self, index: i32, info: *mut PClassInfo) -> tresult {
        let Some((kind, processor)) = Self::class(index) else {
            return kInvalidArgument;
        };
        let info = unsafe { &mut *info };
        info.cid = if processor {
            kind.processor_cid()
        } else {
            kind.controller_cid()
        };
        info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as int32;
        let category = if processor {
            "Audio Module Class"
        } else {
            "Component Controller Class"
        };
        copy_cstring(category, &mut info.category);
        copy_cstring(kind.name(), &mut info.name);
        kResultOk
    }

    unsafe fn createInstance(
        &self,
        cid: FIDString,
        iid: FIDString,
        obj: *mut *mut c_void,
    ) -> tresult {
        let cid = unsafe { *(cid as *const TUID) };
        let instance = KINDS.iter().find_map(|&kind| {
            if cid == kind.processor_cid() {
                ComWrapper::new(SynthProcessor::new(kind)).to_com_ptr::<FUnknown>()
            } else if cid == kind.controller_cid() {
                ComWrapper::new(SynthController::new(kind)).to_com_ptr::<FUnknown>()
            } else {
                None
            }
        });
        match instance {
            Some(instance) => {
                let ptr = instance.as_ptr();
                unsafe { ((*(*ptr).vtbl).queryInterface)(ptr, iid as *mut TUID, obj) }
            }
            None => {
                unsafe { *obj = ptr::null_mut() };
                kInvalidArgument
            }
        }
    }
}

impl IPluginFactory2Trait for Factory {
    unsafe fn getClassInfo2(&self, index: i32, info: *mut PClassInfo2) -> tresult {
        let Some((kind, processor)) = Self::class(index) else {
            return kInvalidArgument;
        };
        let mut base: PClassInfo = unsafe { std::mem::zeroed() };
        unsafe { self.getClassInfo(index, &mut base) };
        let info = unsafe { &mut *info };
        info.cid = base.cid;
        info.cardinality = base.cardinality;
        info.category = base.category;
        copy_cstring(kind.name(), &mut info.name);
        info.classFlags = 0;
        copy_cstring(
            if processor { "Instrument|Synth" } else { "" },
            &mut info.subCategories,
        );
        copy_cstring("refrain", &mut info.vendor);
        copy_cstring(env!("CARGO_PKG_VERSION"), &mut info.version);
        copy_cstring("VST 3.8.0", &mut info.sdkVersion);
        kResultOk
    }
}

/// The module's factory. Hosts find it through this exported symbol; tests
/// call it directly.
#[unsafe(no_mangle)]
pub extern "system" fn GetPluginFactory() -> *mut IPluginFactory {
    ComWrapper::new(Factory)
        .to_com_ptr::<IPluginFactory>()
        .map_or(ptr::null_mut(), |p| p.into_raw())
}

#[cfg(target_os = "windows")]
#[unsafe(no_mangle)]
extern "system" fn InitDll() -> bool {
    true
}

#[cfg(target_os = "windows")]
#[unsafe(no_mangle)]
extern "system" fn ExitDll() -> bool {
    true
}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
extern "system" fn bundleEntry(_bundle: *mut c_void) -> bool {
    true
}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
extern "system" fn bundleExit() -> bool {
    true
}

#[cfg(target_os = "linux")]
#[unsafe(no_mangle)]
extern "system" fn ModuleEntry(_library: *mut c_void) -> bool {
    true
}

#[cfg(target_os = "linux")]
#[unsafe(no_mangle)]
extern "system" fn ModuleExit() -> bool {
    true
}
