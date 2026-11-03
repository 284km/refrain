//! Hosts VST3 plugins inside the refrain engine.
//!
//! A [`Vst3Module`] is a loaded `.vst3` file. A [`Vst3Plugin`] lives on the
//! main thread: it creates the plugin's component and controller, connects
//! them, and activates the component. Activating gives a [`Vst3Processor`],
//! which is a [`refrain_engine::Processor`] and runs on the audio thread.
//!
//! VST3 has no MIDI controller events. A plugin that wants controllers maps
//! them to parameters through `IMidiMapping`, and refrain sends them as
//! parameter changes. For a plugin that maps no sustain pedal, refrain plays
//! the pedal itself, as it does for CLAP plugins that take notes only.
//!
//! Stop the audio stream before dropping the [`Vst3Plugin`]. If the processor
//! is still alive when the plugin is dropped, the plugin is leaked rather than
//! torn down under the audio thread.

mod com;
mod module;

use std::error::Error;
use std::ffi::c_void;
use std::fmt;
use std::ptr;
use std::sync::Arc;

use refrain_engine::{
    ALL_NOTES_OFF, Event as PlayEvent, MAX_BLOCK_EVENTS, MAX_PEDAL_RELEASES, Processor, SUSTAIN,
    SustainPedal, TimedEvent, interleave,
};
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{ComPtr, ComWrapper};

use com::{ComponentHandler, EventList, HostApplication, MemoryStream, ParamChanges, PlugFrame};
pub use module::{Vst3Info, Vst3Module, find_vst3_files, standard_paths};

#[derive(Debug)]
pub enum Vst3Error {
    Load(String),
    NoPlugins,
    /// The module holds several instruments or effects and none was chosen.
    Ambiguous(Vec<String>),
    NotFound(String),
    /// A call into the plugin failed. Names the call.
    Call(&'static str),
    /// Opening the editor failed at the named step.
    Editor(&'static str),
    /// Saving or loading the plugin's state failed.
    State(&'static str),
    NoAudioOutput,
}

impl fmt::Display for Vst3Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Load(e) => write!(f, "could not load the VST3 module: {e}"),
            Self::NoPlugins => f.write_str("the VST3 module holds no audio plugins"),
            Self::Ambiguous(names) => write!(
                f,
                "the VST3 module holds several plugins; choose one by name: {}",
                names.join(", ")
            ),
            Self::NotFound(name) => write!(f, "the VST3 module holds no plugin named {name}"),
            Self::Call(call) => write!(f, "the plugin failed {call}"),
            Self::Editor(step) => write!(f, "the plugin's editor failed to {step}"),
            Self::State(step) => write!(f, "the plugin's state failed to {step}"),
            Self::NoAudioOutput => f.write_str("the plugin has no audio output"),
        }
    }
}

impl Error for Vst3Error {}

fn check(result: tresult, call: &'static str) -> Result<(), Vst3Error> {
    if result == kResultOk {
        Ok(())
    } else {
        Err(Vst3Error::Call(call))
    }
}

/// A VST3 plugin's state: its component's and its controller's, each as
/// bytes only the plugin understands.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Vst3State {
    pub component: Vec<u8>,
    pub controller: Vec<u8>,
}

/// The size of a plugin's editor: in points on macOS, pixels elsewhere.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EditorSize {
    pub width: u32,
    pub height: u32,
    pub resizable: bool,
}

/// MIDI controller numbers in VST3: the 128 of MIDI 1.0, then channel
/// pressure and pitch bend.
const CONTROLLERS: usize = 130;

/// For each MIDI channel and controller, the parameter the plugin maps it to.
type MidiMap = [[Option<ParamID>; CONTROLLERS]; 16];

/// A plugin instance, owned by the main thread.
pub struct Vst3Plugin {
    // Field order is drop order: the plugin's objects go before the module.
    view: Option<ComPtr<IPlugView>>,
    frame: ComWrapper<PlugFrame>,
    component: ComPtr<IComponent>,
    processor: ComPtr<IAudioProcessor>,
    controller: Option<ComPtr<IEditController>>,
    // Whether the controller is an object of its own, rather than the
    // component itself.
    separate_controller: bool,
    connection: Option<(ComPtr<IConnectionPoint>, ComPtr<IConnectionPoint>)>,
    _host: ComWrapper<HostApplication>,
    _handler: ComWrapper<ComponentHandler>,
    info: Vst3Info,
    active: bool,
    // Held by the processor; the plugin is only torn down once it is gone.
    alive: Arc<()>,
    module: Vst3Module,
}

impl Vst3Plugin {
    /// Creates the plugin called `name` in `module`. With no name, the module
    /// must hold exactly one audio plugin.
    pub fn new(module: &Vst3Module, name: Option<&str>) -> Result<Self, Vst3Error> {
        let plugins = module.plugins();
        let info = match name {
            Some(name) => plugins
                .into_iter()
                .find(|p| p.name == name || p.id == name)
                .ok_or_else(|| Vst3Error::NotFound(name.to_owned()))?,
            None => match <[Vst3Info; 1]>::try_from(plugins) {
                Ok([only]) => only,
                Err(p) if p.is_empty() => return Err(Vst3Error::NoPlugins),
                Err(p) => {
                    return Err(Vst3Error::Ambiguous(
                        p.into_iter().map(|p| p.name).collect(),
                    ));
                }
            },
        };

        let host = ComWrapper::new(HostApplication);
        let host_ptr = host
            .as_com_ref::<IHostApplication>()
            .expect("the host implements IHostApplication")
            .as_ptr() as *mut FUnknown;
        let handler = ComWrapper::new(ComponentHandler::default());

        let component: ComPtr<IComponent> = module
            .create(&info.cid)
            .ok_or(Vst3Error::Call("to create its component"))?;
        check(unsafe { component.initialize(host_ptr) }, "to initialize")?;
        let processor = component
            .cast::<IAudioProcessor>()
            .ok_or(Vst3Error::Call("to provide an audio processor"))?;

        // The controller is either a class of its own, or the component itself.
        let mut controller_cid: TUID = [0; 16];
        let separate = unsafe { component.getControllerClassId(&mut controller_cid) } == kResultOk
            && controller_cid != [0; 16];
        let controller = if separate {
            let controller: Option<ComPtr<IEditController>> = module.create(&controller_cid);
            if let Some(c) = &controller {
                check(
                    unsafe { c.initialize(host_ptr) },
                    "to initialize its controller",
                )?;
            }
            controller
        } else {
            component.cast::<IEditController>()
        };

        let mut plugin = Self {
            view: None,
            frame: ComWrapper::new(PlugFrame::default()),
            component,
            processor,
            controller,
            separate_controller: separate,
            connection: None,
            _host: host,
            info,
            active: false,
            alive: Arc::new(()),
            module: module.clone(),
            _handler: handler,
        };
        if let Some(controller) = &plugin.controller {
            if let Some(h) = plugin._handler.as_com_ref::<IComponentHandler>() {
                unsafe { controller.setComponentHandler(h.as_ptr()) };
            }
            if separate {
                plugin.connection = connect(&plugin.component, controller);
                // Let the controller see the component's state.
                let stream = ComWrapper::new(MemoryStream::default());
                let s = stream.as_com_ref::<IBStream>().unwrap().as_ptr();
                if unsafe { plugin.component.getState(s) } == kResultOk {
                    stream.rewind();
                    unsafe { controller.setComponentState(s) };
                }
            }
        }
        Ok(plugin)
    }

    pub fn info(&self) -> &Vst3Info {
        &self.info
    }

    /// Activates the plugin for a sample rate and a largest block size.
    ///
    /// The returned processor must be used at that sample rate, with blocks
    /// of at most `max_block_frames`.
    pub fn activate(
        &mut self,
        sample_rate: f32,
        max_block_frames: usize,
    ) -> Result<Vst3Processor, Vst3Error> {
        let audio = MediaTypes_::kAudio as MediaType;
        let event = MediaTypes_::kEvent as MediaType;
        let input = BusDirections_::kInput as BusDirection;
        let output = BusDirections_::kOutput as BusDirection;

        let bus_channels = |dir| -> Vec<i32> {
            (0..unsafe { self.component.getBusCount(audio, dir) })
                .map(|i| {
                    let mut info: BusInfo = unsafe { std::mem::zeroed() };
                    if unsafe { self.component.getBusInfo(audio, dir, i, &mut info) } == kResultOk {
                        info.channelCount.max(0)
                    } else {
                        0
                    }
                })
                .collect()
        };
        let inputs = bus_channels(input);
        let outputs = bus_channels(output);
        if outputs.first().is_none_or(|&c| c == 0) {
            return Err(Vst3Error::NoAudioOutput);
        }

        // Keep the arrangements the plugin offers.
        let arrangement = |dir, i| {
            let mut arr: SpeakerArrangement = 0;
            unsafe { self.processor.getBusArrangement(dir, i, &mut arr) };
            arr
        };
        let mut in_arr: Vec<_> = (0..inputs.len() as i32)
            .map(|i| arrangement(input, i))
            .collect();
        let mut out_arr: Vec<_> = (0..outputs.len() as i32)
            .map(|i| arrangement(output, i))
            .collect();
        unsafe {
            self.processor.setBusArrangements(
                in_arr.as_mut_ptr(),
                in_arr.len() as i32,
                out_arr.as_mut_ptr(),
                out_arr.len() as i32,
            )
        };

        // Main buses on; the rest as the plugin left them.
        unsafe {
            self.component.activateBus(audio, output, 0, 1);
            if !inputs.is_empty() {
                self.component.activateBus(audio, input, 0, 1);
            }
            if self.component.getBusCount(event, input) > 0 {
                self.component.activateBus(event, input, 0, 1);
            }
        }

        let mut setup = ProcessSetup {
            processMode: ProcessModes_::kRealtime as int32,
            symbolicSampleSize: SymbolicSampleSizes_::kSample32 as int32,
            maxSamplesPerBlock: max_block_frames as int32,
            sampleRate: f64::from(sample_rate),
        };
        check(
            unsafe { self.processor.setupProcessing(&mut setup) },
            "setupProcessing",
        )?;
        check(unsafe { self.component.setActive(1) }, "to activate")?;
        self.active = true;

        // Edits made in the plugin's editor reach the processor through this.
        let (to_processor, edits) = rtrb::RingBuffer::new(1024);
        self._handler.send_edits_to(to_processor);

        Ok(Vst3Processor::new(
            self.processor.clone(),
            self.midi_map(),
            &inputs,
            &outputs,
            sample_rate,
            max_block_frames,
            Arc::clone(&self.alive),
            self.module.clone(),
            edits,
        ))
    }

    /// The plugin's state, as bytes only the plugin understands.
    pub fn save_state(&mut self) -> Result<Vst3State, Vst3Error> {
        let stream = ComWrapper::new(MemoryStream::default());
        let s = stream.as_com_ref::<IBStream>().unwrap().as_ptr();
        if unsafe { self.component.getState(s) } != kResultOk {
            return Err(Vst3Error::State("save"));
        }
        let component = stream.bytes();
        let mut controller = Vec::new();
        if self.separate_controller
            && let Some(c) = &self.controller
        {
            let stream = ComWrapper::new(MemoryStream::default());
            let s = stream.as_com_ref::<IBStream>().unwrap().as_ptr();
            // A controller with nothing to save may say so; that is not an error.
            if unsafe { c.getState(s) } == kResultOk {
                controller = stream.bytes();
            }
        }
        Ok(Vst3State {
            component,
            controller,
        })
    }

    /// Restores a state from [`save_state`](Self::save_state).
    pub fn load_state(&mut self, state: &Vst3State) -> Result<(), Vst3Error> {
        let stream = ComWrapper::new(MemoryStream::from_bytes(&state.component));
        let s = stream.as_com_ref::<IBStream>().unwrap().as_ptr();
        if unsafe { self.component.setState(s) } != kResultOk {
            return Err(Vst3Error::State("load"));
        }
        if self.separate_controller
            && let Some(c) = &self.controller
        {
            // The controller learns the component's state, then its own.
            stream.rewind();
            unsafe { c.setComponentState(s) };
            if !state.controller.is_empty() {
                let stream = ComWrapper::new(MemoryStream::from_bytes(&state.controller));
                let s = stream.as_com_ref::<IBStream>().unwrap().as_ptr();
                if unsafe { c.setState(s) } != kResultOk {
                    return Err(Vst3Error::State("load into the controller"));
                }
            }
        }
        Ok(())
    }

    /// Opens the plugin's editor inside `parent`, at the window's `scale`.
    ///
    /// Returns the editor's size, in points on macOS and pixels elsewhere,
    /// or `None` when the plugin has no editor for this kind of window.
    ///
    /// # Safety
    ///
    /// `parent` must stay valid until [`close_editor`](Self::close_editor).
    pub unsafe fn open_editor(
        &mut self,
        parent: raw_window_handle::RawWindowHandle,
        scale: f64,
    ) -> Result<Option<EditorSize>, Vst3Error> {
        use raw_window_handle::RawWindowHandle as Raw;
        self.close_editor();
        let (window, platform): (*mut c_void, FIDString) = match parent {
            Raw::AppKit(h) => (h.ns_view.as_ptr(), kPlatformTypeNSView),
            Raw::Win32(h) => (h.hwnd.get() as *mut c_void, kPlatformTypeHWND),
            Raw::Xlib(h) => (h.window as *mut c_void, kPlatformTypeX11EmbedWindowID),
            _ => return Ok(None),
        };
        let Some(controller) = &self.controller else {
            return Ok(None);
        };
        let Some(view) = (unsafe { ComPtr::from_raw(controller.createView(c"editor".as_ptr())) })
        else {
            return Ok(None);
        };
        if unsafe { view.isPlatformTypeSupported(platform) } != kResultTrue {
            return Ok(None);
        }
        unsafe {
            if let Some(frame) = self.frame.as_com_ref::<IPlugFrame>() {
                view.setFrame(frame.as_ptr());
            }
            if !cfg!(target_os = "macos")
                && let Some(scaling) = view.cast::<IPlugViewContentScaleSupport>()
            {
                scaling.setContentScaleFactor(scale as f32);
            }
            if view.attached(window, platform) != kResultOk {
                view.setFrame(ptr::null_mut());
                return Err(Vst3Error::Editor("attach"));
            }
        }
        let mut rect = ViewRect {
            left: 0,
            top: 0,
            right: 640,
            bottom: 480,
        };
        unsafe { view.getSize(&mut rect) };
        let resizable = unsafe { view.canResize() } == kResultTrue;
        self.view = Some(view);
        Ok(Some(EditorSize {
            width: (rect.right - rect.left).max(1) as u32,
            height: (rect.bottom - rect.top).max(1) as u32,
            resizable,
        }))
    }

    /// Tells the editor its window changed size, and returns the size it took.
    pub fn resize_editor(&mut self, width: u32, height: u32) -> (u32, u32) {
        let Some(view) = &self.view else {
            return (width, height);
        };
        let mut rect = ViewRect {
            left: 0,
            top: 0,
            right: width as i32,
            bottom: height as i32,
        };
        unsafe {
            if view.canResize() == kResultTrue {
                view.checkSizeConstraint(&mut rect);
                view.onSize(&mut rect);
            } else {
                view.getSize(&mut rect);
            }
        }
        (
            (rect.right - rect.left).max(1) as u32,
            (rect.bottom - rect.top).max(1) as u32,
        )
    }

    /// A size the editor asked its window to take, if it asked since the last
    /// call.
    pub fn take_editor_resize_request(&self) -> Option<(u32, u32)> {
        self.frame.take_request()
    }

    pub fn close_editor(&mut self) {
        if let Some(view) = self.view.take() {
            unsafe {
                view.removed();
                view.setFrame(ptr::null_mut());
            }
        }
    }

    /// Reads which controllers the plugin maps to which parameters.
    fn midi_map(&self) -> Box<MidiMap> {
        let mut map = Box::new([[None; CONTROLLERS]; 16]);
        let Some(mapping) = self
            .controller
            .as_ref()
            .and_then(|c| c.cast::<IMidiMapping>())
        else {
            return map;
        };
        for (channel, row) in map.iter_mut().enumerate() {
            for (number, slot) in row.iter_mut().enumerate() {
                let mut id: ParamID = 0;
                let found = unsafe {
                    mapping.getMidiControllerAssignment(
                        0,
                        channel as i16,
                        number as CtrlNumber,
                        &mut id,
                    )
                };
                if found == kResultOk {
                    *slot = Some(id);
                }
            }
        }
        map
    }
}

/// Connects a component and its controller, if both can be connected.
fn connect(
    component: &ComPtr<IComponent>,
    controller: &ComPtr<IEditController>,
) -> Option<(ComPtr<IConnectionPoint>, ComPtr<IConnectionPoint>)> {
    let a = component.cast::<IConnectionPoint>()?;
    let b = controller.cast::<IConnectionPoint>()?;
    unsafe {
        a.connect(b.as_ptr());
        b.connect(a.as_ptr());
    }
    Some((a, b))
}

impl Drop for Vst3Plugin {
    fn drop(&mut self) {
        self.close_editor();
        if Arc::strong_count(&self.alive) > 1 {
            // The processor is still running somewhere. Tearing the plugin
            // down under it would crash; leak it instead.
            let leaked = std::mem::replace(&mut self.alive, Arc::new(()));
            std::mem::forget(leaked);
            std::mem::forget(self.component.clone());
            std::mem::forget(self.processor.clone());
            std::mem::forget(self.module.clone());
            return;
        }
        unsafe {
            if self.active {
                self.processor.setProcessing(0);
                self.component.setActive(0);
            }
            if let Some((a, b)) = self.connection.take() {
                a.disconnect(b.as_ptr());
                b.disconnect(a.as_ptr());
            }
            if let Some(controller) = self.controller.take() {
                controller.setComponentHandler(ptr::null_mut());
                if controller.cast::<IComponent>().is_none() {
                    controller.terminate();
                }
            }
            self.component.terminate();
        }
    }
}

/// The most changes to one parameter in a block, and the most parameters
/// changed in a block.
const MAX_POINTS: usize = MAX_BLOCK_EVENTS;
const MAX_PARAMS: usize = 64;

/// A running plugin, owned by the audio thread.
pub struct Vst3Processor {
    processor: ComPtr<IAudioProcessor>,
    midi_map: Box<MidiMap>,
    sample_rate: f32,
    max_block_frames: usize,
    // One buffer per bus, holding its channels one after another, each
    // `max_block_frames` long, and the channel pointers into it.
    inputs: Vec<Vec<f32>>,
    outputs: Vec<Vec<f32>>,
    input_ptrs: Vec<Vec<*mut f32>>,
    output_ptrs: Vec<Vec<*mut f32>>,
    input_buses: Vec<AudioBusBuffers>,
    output_buses: Vec<AudioBusBuffers>,
    events: ComWrapper<EventList>,
    params: ComWrapper<ParamChanges>,
    context: ProcessContext,
    pedal: SustainPedal,
    to_pedal: Vec<TimedEvent>,
    notes: Vec<TimedEvent>,
    // Edits from the plugin's editor.
    edits: rtrb::Consumer<(ParamID, ParamValue)>,
    started: bool,
    failed: bool,
    _alive: Arc<()>,
    _module: Vst3Module,
}

// The raw pointers point into the processor's own buffers, and the COM
// objects are only touched by whichever thread owns the processor.
unsafe impl Send for Vst3Processor {}

impl Vst3Processor {
    #[allow(clippy::too_many_arguments)]
    fn new(
        processor: ComPtr<IAudioProcessor>,
        midi_map: Box<MidiMap>,
        inputs: &[i32],
        outputs: &[i32],
        sample_rate: f32,
        max_block_frames: usize,
        alive: Arc<()>,
        module: Vst3Module,
        edits: rtrb::Consumer<(ParamID, ParamValue)>,
    ) -> Self {
        let alloc = |channels: &[i32]| -> Vec<Vec<f32>> {
            channels
                .iter()
                .map(|&c| vec![0.0; c as usize * max_block_frames])
                .collect()
        };
        let mut context: ProcessContext = unsafe { std::mem::zeroed() };
        context.sampleRate = f64::from(sample_rate);
        context.tempo = 120.0;
        context.timeSigNumerator = 4;
        context.timeSigDenominator = 4;
        // VST3 enums are `c_int` on Windows and `c_uint` elsewhere.
        #[allow(clippy::unnecessary_cast)]
        let state = (ProcessContext_::StatesAndFlags_::kTempoValid
            | ProcessContext_::StatesAndFlags_::kTimeSigValid
            | ProcessContext_::StatesAndFlags_::kContTimeValid) as u32;
        context.state = state;
        let mut p = Self {
            processor,
            midi_map,
            sample_rate,
            max_block_frames,
            inputs: alloc(inputs),
            outputs: alloc(outputs),
            input_ptrs: Vec::new(),
            output_ptrs: Vec::new(),
            input_buses: Vec::new(),
            output_buses: Vec::new(),
            events: ComWrapper::new(EventList::with_capacity(MAX_EVENTS)),
            params: ComWrapper::new(ParamChanges::new(MAX_PARAMS, MAX_POINTS)),
            context,
            pedal: SustainPedal::default(),
            to_pedal: Vec::with_capacity(MAX_BLOCK_EVENTS),
            notes: Vec::with_capacity(MAX_EVENTS),
            edits,
            started: false,
            failed: false,
            _alive: alive,
            _module: module,
        };
        let ptrs = |buffers: &mut Vec<Vec<f32>>| -> Vec<Vec<*mut f32>> {
            buffers
                .iter_mut()
                .map(|b| {
                    b.chunks_exact_mut(max_block_frames)
                        .map(|c| c.as_mut_ptr())
                        .collect()
                })
                .collect()
        };
        p.input_ptrs = ptrs(&mut p.inputs);
        p.output_ptrs = ptrs(&mut p.outputs);
        let buses = |ptrs: &mut Vec<Vec<*mut f32>>| -> Vec<AudioBusBuffers> {
            ptrs.iter_mut()
                .map(|channels| AudioBusBuffers {
                    numChannels: channels.len() as int32,
                    silenceFlags: 0,
                    __field0: AudioBusBuffers__type0 {
                        channelBuffers32: channels.as_mut_ptr(),
                    },
                })
                .collect()
        };
        p.input_buses = buses(&mut p.input_ptrs);
        p.output_buses = buses(&mut p.output_ptrs);
        p
    }

    /// Whether the plugin has failed a `process` call. Its output is silence
    /// from then on.
    pub fn has_failed(&self) -> bool {
        self.failed
    }

    /// Turns the block's events into VST3 notes and parameter changes.
    ///
    /// Edits from the plugin's editor come first, at the start of the block.
    /// Controllers the plugin maps become changes to their parameters.
    /// Everything else goes through the host's sustain pedal, which holds
    /// notes for an unmapped pedal and drops other unmapped controllers.
    fn push_events(&mut self, events: &[TimedEvent]) {
        let list = unsafe { self.events.events() };
        list.clear();
        self.params.clear();
        self.to_pedal.clear();
        // Edits made in the editor since the last block take effect at its
        // start. The plugin's controller already has them.
        while let Ok((id, value)) = self.edits.pop() {
            unsafe { self.params.push(id, 0, value) };
        }
        for e in events.iter().take(MAX_BLOCK_EVENTS) {
            let mapped =
                |channel: u8, number: usize| self.midi_map[usize::from(channel & 0x0f)][number];
            let change = match e.event {
                PlayEvent::Controller {
                    channel,
                    number,
                    value,
                } => mapped(channel, usize::from(number & 0x7f))
                    .map(|id| (id, f64::from(value) / 127.0)),
                PlayEvent::ChannelPressure { channel, value } => {
                    mapped(channel, ControllerNumbers_::kAfterTouch as usize)
                        .map(|id| (id, f64::from(value) / 127.0))
                }
                PlayEvent::PitchBend { channel, value } => {
                    mapped(channel, ControllerNumbers_::kPitchBend as usize)
                        .map(|id| (id, f64::from(value) / 16383.0))
                }
                _ => None,
            };
            match change {
                Some((id, value)) => unsafe {
                    self.params.push(id, e.frame as int32, value);
                },
                None => self.to_pedal.push(*e),
            }
        }
        self.notes.clear();
        self.pedal.apply(&self.to_pedal, &mut self.notes);
        for n in &self.notes {
            let mut event: Event = unsafe { std::mem::zeroed() };
            event.busIndex = 0;
            event.sampleOffset = n.frame as int32;
            event.flags = Event_::EventFlags_::kIsLive as u16;
            match n.event {
                PlayEvent::NoteOn {
                    channel,
                    key,
                    velocity,
                } => {
                    event.r#type = Event_::EventTypes_::kNoteOnEvent as u16;
                    event.__field0.noteOn = NoteOnEvent {
                        channel: i16::from(channel),
                        pitch: i16::from(key),
                        tuning: 0.0,
                        velocity,
                        length: 0,
                        noteId: -1,
                    };
                }
                PlayEvent::NoteOff {
                    channel,
                    key,
                    velocity,
                } => {
                    event.r#type = Event_::EventTypes_::kNoteOffEvent as u16;
                    event.__field0.noteOff = NoteOffEvent {
                        channel: i16::from(channel),
                        pitch: i16::from(key),
                        velocity,
                        noteId: -1,
                        tuning: 0.0,
                    };
                }
                _ => continue,
            }
            if list.len() < list.capacity() {
                list.push(event);
            }
        }
    }
}

/// The most events one block can hand a plugin: the block's own, plus the
/// note offs the sustain pedal can add.
const MAX_EVENTS: usize = MAX_BLOCK_EVENTS + MAX_PEDAL_RELEASES;

// Mapped controllers never reach the pedal, so a plugin that maps the pedal
// plays it itself, and so does one that maps All Notes Off.
const _: () = assert!(SUSTAIN < 128 && ALL_NOTES_OFF < 128);

impl Processor for Vst3Processor {
    fn prepare(&mut self, sample_rate: f32, max_block_frames: usize) {
        // The plugin was set up for these on the main thread.
        assert!(
            sample_rate == self.sample_rate && max_block_frames <= self.max_block_frames,
            "activate the plugin again to change the sample rate or block size"
        );
        self.context.continousTimeSamples = 0;
        self.pedal.reset();
    }

    fn process(&mut self, out: &mut [f32], channels: usize, events: &[TimedEvent]) {
        let frames = out.len() / channels;
        debug_assert!(frames <= self.max_block_frames);
        if self.failed {
            out.fill(0.0);
            return;
        }
        if !self.started {
            // VST3 asks for this on the thread that processes.
            unsafe { self.processor.setProcessing(1) };
            self.started = true;
        }
        self.push_events(events);
        for buffer in &mut self.inputs {
            buffer.fill(0.0);
        }

        let mut data = ProcessData {
            processMode: ProcessModes_::kRealtime as int32,
            symbolicSampleSize: SymbolicSampleSizes_::kSample32 as int32,
            numSamples: frames as int32,
            numInputs: self.input_buses.len() as int32,
            numOutputs: self.output_buses.len() as int32,
            inputs: self.input_buses.as_mut_ptr(),
            outputs: self.output_buses.as_mut_ptr(),
            inputParameterChanges: self
                .params
                .as_com_ref::<IParameterChanges>()
                .map_or(ptr::null_mut(), |p| p.as_ptr()),
            outputParameterChanges: ptr::null_mut(),
            inputEvents: self
                .events
                .as_com_ref::<IEventList>()
                .map_or(ptr::null_mut(), |e| e.as_ptr()),
            outputEvents: ptr::null_mut(),
            processContext: &mut self.context,
        };
        let result = unsafe { self.processor.process(&mut data) };
        self.context.continousTimeSamples += frames as i64;
        if result != kResultOk {
            self.failed = true;
            out.fill(0.0);
            return;
        }
        let max = self.max_block_frames;
        let main = &self.outputs[0];
        interleave(main, main.len() / max, max, out, channels);
    }

    fn has_failed(&self) -> bool {
        self.failed
    }
}
