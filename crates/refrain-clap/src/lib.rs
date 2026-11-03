//! Hosts CLAP plugins inside the refrain engine.
//!
//! A [`ClapPlugin`] lives on the main thread: it loads, instantiates and
//! activates the plugin, and answers the plugin's main-thread callbacks.
//! Activating it gives a [`ClapProcessor`], which is a
//! [`refrain_engine::Processor`] and runs on the audio thread.
//!
//! Stop the audio stream before dropping the [`ClapPlugin`]. If the processor
//! is still alive when the plugin is dropped, clack leaks the instance rather
//! than destroying it under the audio thread, so the mistake is a leak, not a
//! crash.

use std::error::Error;
use std::ffi::CString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use clack_extensions::audio_ports::{AudioPortFlags, AudioPortInfoBuffer, PluginAudioPorts};
use clack_extensions::gui::{
    GuiApiType, GuiConfiguration, GuiSize, HostGui, HostGuiImpl, PluginGui, Window as ClapWindow,
};
use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_extensions::note_ports::{NoteDialects, NotePortInfoBuffer, PluginNotePorts};
use clack_extensions::params::{
    HostParams, HostParamsImplMainThread, HostParamsImplShared, ParamClearFlags, ParamRescanFlags,
};
use clack_host::events::event_types::{MidiEvent, NoteOffEvent, NoteOnEvent};
use clack_host::events::{Event as _, EventFlags, Match};
use clack_host::prelude::*;

/// A loaded CLAP file. Get one from [`load_entry`].
pub use clack_host::entry::PluginEntry;
use refrain_engine::{
    Event, MAX_BLOCK_EVENTS, MAX_PEDAL_RELEASES, Processor, SustainPedal, TimedEvent, interleave,
};

/// What a plugin says about itself.
#[derive(Clone, Debug)]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub vendor: String,
    pub version: String,
}

impl fmt::Display for PluginInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.name, self.id)?;
        if !self.vendor.is_empty() {
            write!(f, " by {}", self.vendor)?;
        }
        if !self.version.is_empty() {
            write!(f, ", version {}", self.version)?;
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum ClapError {
    Load(String),
    NoFactory,
    NoPlugins,
    /// The file holds several plugins and none was chosen.
    Ambiguous(Vec<String>),
    NotFound(String),
    NoAudioOutput,
    /// Opening the editor failed at the named step.
    Editor(&'static str),
    Instance(PluginInstanceError),
}

impl fmt::Display for ClapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Load(e) => write!(f, "could not load the CLAP file: {e}"),
            Self::NoFactory => f.write_str("the CLAP file has no plugin factory"),
            Self::NoPlugins => f.write_str("the CLAP file holds no plugins"),
            Self::Ambiguous(ids) => write!(
                f,
                "the CLAP file holds several plugins; choose one with its id: {}",
                ids.join(", ")
            ),
            Self::NotFound(id) => write!(f, "the CLAP file holds no plugin with id {id}"),
            Self::NoAudioOutput => f.write_str("the plugin has no audio output"),
            Self::Editor(step) => write!(f, "the plugin's editor failed to {step}"),
            Self::Instance(e) => write!(f, "{e}"),
        }
    }
}

impl Error for ClapError {}

impl From<PluginInstanceError> for ClapError {
    fn from(e: PluginInstanceError) -> Self {
        Self::Instance(e)
    }
}

/// Folders where CLAP plugins are installed, as the CLAP specification lists
/// them, followed by any in the `CLAP_PATH` environment variable.
pub fn standard_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            paths.push(PathBuf::from(home).join("Library/Audio/Plug-Ins/CLAP"));
        }
        paths.push(PathBuf::from("/Library/Audio/Plug-Ins/CLAP"));
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(common) = std::env::var_os("COMMONPROGRAMFILES") {
            paths.push(PathBuf::from(common).join("CLAP"));
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            paths.push(PathBuf::from(local).join("Programs/Common/CLAP"));
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            paths.push(PathBuf::from(home).join(".clap"));
        }
        paths.push(PathBuf::from("/usr/lib/clap"));
    }
    if let Some(extra) = std::env::var_os("CLAP_PATH") {
        paths.extend(std::env::split_paths(&extra));
    }
    paths
}

/// Finds `.clap` files and bundles under `dirs`, searching subfolders.
pub fn find_clap_files(dirs: &[PathBuf]) -> Vec<PathBuf> {
    fn walk(dir: &Path, found: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("clap"))
            {
                // On macOS a .clap is a bundle folder; do not look inside it.
                found.push(path);
            } else if path.is_dir() {
                walk(&path, found);
            }
        }
    }
    let mut found = Vec::new();
    for dir in dirs {
        walk(dir, &mut found);
    }
    found.sort();
    found
}

/// Loads a CLAP file.
///
/// # Safety
///
/// This runs the file's code in this process. A broken plugin can corrupt
/// memory or crash the program.
pub unsafe fn load_entry(path: &Path) -> Result<PluginEntry, ClapError> {
    // SAFETY: passed on to the caller.
    unsafe { PluginEntry::load(path) }.map_err(|e| ClapError::Load(e.to_string()))
}

/// Lists the plugins an entry holds.
pub fn plugins_in(entry: &PluginEntry) -> Result<Vec<PluginInfo>, ClapError> {
    let factory = entry.get_plugin_factory().ok_or(ClapError::NoFactory)?;
    let text = |s: Option<&std::ffi::CStr>| {
        s.map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    Ok(factory
        .plugin_descriptors()
        .filter_map(|d| {
            Some(PluginInfo {
                id: d.id()?.to_str().ok()?.to_owned(),
                name: text(d.name()),
                vendor: text(d.vendor()),
                version: text(d.version()),
            })
        })
        .collect())
}

/// Our side of the CLAP host interface.
pub struct RefrainHost;

impl HostHandlers for RefrainHost {
    type Shared<'a> = HostShared;
    type MainThread<'a> = HostMainThread;
    type AudioProcessor<'a> = ();

    fn declare_extensions(builder: &mut HostExtensions<Self>, _shared: &Self::Shared<'_>) {
        builder
            .register::<HostLog>()
            .register::<HostParams>()
            .register::<HostGui>();
    }
}

/// Host state any of the plugin's threads may touch.
pub struct HostShared {
    callback_requested: AtomicBool,
    // Requests from the plugin's editor, answered on the main thread.
    resize_requested: Mutex<Option<GuiSize>>,
    editor_closed: AtomicBool,
}

impl SharedHandler<'_> for HostShared {
    fn request_restart(&self) {
        // Not supported yet: the plugin keeps running as it is.
    }

    fn request_process(&self) {
        // The device drives processing and never pauses.
    }

    fn request_callback(&self) {
        self.callback_requested.store(true, Ordering::Release);
    }
}

impl HostLogImpl for HostShared {
    fn log(&self, severity: LogSeverity, message: &str) {
        // Plugins may log from the audio thread, where printing is not
        // real-time safe. Debug messages are dropped to keep that rare.
        if severity > LogSeverity::Debug {
            eprintln!("[plugin {severity}] {message}");
        }
    }
}

impl HostGuiImpl for HostShared {
    fn resize_hints_changed(&self) {}

    fn request_resize(&self, new_size: GuiSize) -> Result<(), HostError> {
        *self.resize_requested.lock().unwrap() = Some(new_size);
        Ok(())
    }

    fn request_show(&self) -> Result<(), HostError> {
        Ok(())
    }

    fn request_hide(&self) -> Result<(), HostError> {
        Ok(())
    }

    fn closed(&self, _was_destroyed: bool) {
        self.editor_closed.store(true, Ordering::Release);
    }
}

impl HostParamsImplShared for HostShared {
    fn request_flush(&self) {
        // Processing never stops, so parameters are flushed in process().
    }
}

/// Host state that only the main thread touches.
pub struct HostMainThread;

impl MainThreadHandler<'_> for HostMainThread {}

impl HostParamsImplMainThread for HostMainThread {
    fn rescan(&self, _flags: ParamRescanFlags) {}

    fn clear(&self, _param_id: ClapId, _flags: ParamClearFlags) {}
}

/// A plugin instance, owned by the main thread.
pub struct ClapPlugin {
    editor: Option<Editor>,
    instance: PluginInstance<RefrainHost>,
    info: PluginInfo,
}

impl ClapPlugin {
    /// Instantiates the plugin with `id` from `entry`. With no id, the entry
    /// must hold exactly one plugin.
    pub fn new(entry: &PluginEntry, id: Option<&str>) -> Result<Self, ClapError> {
        let plugins = plugins_in(entry)?;
        let info = match id {
            Some(id) => plugins
                .into_iter()
                .find(|p| p.id == id)
                .ok_or_else(|| ClapError::NotFound(id.to_owned()))?,
            None => match <[PluginInfo; 1]>::try_from(plugins) {
                Ok([only]) => only,
                Err(plugins) if plugins.is_empty() => return Err(ClapError::NoPlugins),
                Err(plugins) => {
                    return Err(ClapError::Ambiguous(
                        plugins.into_iter().map(|p| p.id).collect(),
                    ));
                }
            },
        };
        let host_info = HostInfo::new(
            "refrain",
            "refrain",
            "https://github.com/284km/refrain",
            env!("CARGO_PKG_VERSION"),
        )
        .expect("host info has no NUL bytes");
        let plugin_id = CString::new(info.id.as_str()).expect("plugin ids are C strings");
        let instance = PluginInstance::<RefrainHost>::new(
            |_| HostShared {
                callback_requested: AtomicBool::new(false),
                resize_requested: Mutex::new(None),
                editor_closed: AtomicBool::new(false),
            },
            |_| HostMainThread,
            entry,
            &plugin_id,
            &host_info,
        )?;
        Ok(Self {
            editor: None,
            instance,
            info,
        })
    }

    /// Opens the plugin's editor inside `parent`, at the window's `scale`.
    ///
    /// Returns the editor's size, in points on macOS and pixels elsewhere,
    /// or `None` when the plugin has no editor that can be embedded.
    ///
    /// # Safety
    ///
    /// `parent` must stay valid until [`close_editor`](Self::close_editor).
    pub unsafe fn open_editor(
        &mut self,
        parent: raw_window_handle::RawWindowHandle,
        scale: f64,
    ) -> Result<Option<EditorSize>, ClapError> {
        self.close_editor();
        let handle = self.instance.plugin_handle();
        let Some(gui) = handle.get_extension::<PluginGui>() else {
            return Ok(None);
        };
        let Some(api_type) = GuiApiType::default_for_current_platform() else {
            return Ok(None);
        };
        let config = GuiConfiguration {
            api_type,
            is_floating: false,
        };
        if !gui.is_api_supported(&handle, config) {
            return Ok(None);
        }
        let failed = |what: &'static str| ClapError::Editor(what);
        gui.create(&handle, config).map_err(|_| failed("create"))?;
        if !api_type.uses_logical_size() {
            let _ = gui.set_scale(&handle, scale);
        }
        let size = gui.get_size(&handle).unwrap_or(GuiSize {
            width: 640,
            height: 480,
        });
        use raw_window_handle::RawWindowHandle as Raw;
        let window = match parent {
            Raw::AppKit(h) => unsafe { ClapWindow::from_cocoa_nsview(h.ns_view.as_ptr()) },
            Raw::Win32(h) => unsafe { ClapWindow::from_win32_hwnd(h.hwnd.get() as *mut _) },
            Raw::Xlib(h) => ClapWindow::from_x11_handle(h.window),
            _ => return Err(failed("embed in this kind of window")),
        };
        if unsafe { gui.set_parent(&handle, window) }.is_err() {
            gui.destroy(&handle);
            return Err(failed("set_parent"));
        }
        // Some plugins draw nothing until shown; others refuse to be.
        let _ = gui.show(&handle);
        let resizable = gui.can_resize(&handle);
        self.instance
            .access_shared_handler(|h| h.editor_closed.store(false, Ordering::Release));
        self.editor = Some(Editor { gui, resizable });
        Ok(Some(EditorSize {
            width: size.width,
            height: size.height,
            resizable,
        }))
    }

    /// Asks the editor to take a new size, and returns the size it took.
    pub fn resize_editor(&mut self, width: u32, height: u32) -> (u32, u32) {
        let Some(editor) = &self.editor else {
            return (width, height);
        };
        let handle = self.instance.plugin_handle();
        let wanted = GuiSize { width, height };
        let size = if editor.resizable {
            let size = editor.gui.adjust_size(&handle, wanted).unwrap_or(wanted);
            let _ = editor.gui.set_size(&handle, size);
            size
        } else {
            editor.gui.get_size(&handle).unwrap_or(wanted)
        };
        (size.width, size.height)
    }

    /// A size the editor asked its window to take, if it asked since the last
    /// call.
    pub fn take_editor_resize_request(&self) -> Option<(u32, u32)> {
        self.instance
            .access_shared_handler(|h| h.resize_requested.lock().unwrap().take())
            .map(|s| (s.width, s.height))
    }

    /// Whether the plugin closed its own editor.
    pub fn editor_closed(&self) -> bool {
        self.instance
            .access_shared_handler(|h| h.editor_closed.load(Ordering::Acquire))
    }

    pub fn close_editor(&mut self) {
        if let Some(editor) = self.editor.take() {
            editor.gui.destroy(&self.instance.plugin_handle());
        }
    }

    pub fn info(&self) -> &PluginInfo {
        &self.info
    }

    /// Runs the plugin's main-thread callback if it asked for one. Call this
    /// regularly from the main thread while the plugin is alive.
    pub fn poll_main_thread(&mut self) {
        let requested = self
            .instance
            .access_shared_handler(|h| h.callback_requested.swap(false, Ordering::AcqRel));
        if requested {
            self.instance.call_on_main_thread_callback();
        }
    }

    /// Activates the plugin for a sample rate and a largest block size.
    ///
    /// The returned processor must be used at that sample rate, with blocks
    /// of at most `max_block_frames`. Activate again to change either.
    pub fn activate(
        &mut self,
        sample_rate: f32,
        max_block_frames: usize,
    ) -> Result<ClapProcessor, ClapError> {
        let handle = self.instance.plugin_handle();
        let audio_ports = handle.get_extension::<PluginAudioPorts>();
        let mut buffer = AudioPortInfoBuffer::new();
        let mut port_channels = |is_input: bool| -> (Vec<u32>, Option<usize>) {
            let Some(ext) = audio_ports else {
                return (Vec::new(), None);
            };
            let mut channels = Vec::new();
            let mut main = None;
            for i in 0..ext.count(&handle, is_input) {
                let Some(info) = ext.get(&handle, i, is_input, &mut buffer) else {
                    continue;
                };
                if info.flags.contains(AudioPortFlags::IS_MAIN) && main.is_none() {
                    main = Some(channels.len());
                }
                channels.push(info.channel_count);
            }
            (channels, main)
        };
        let (input_channels, _) = port_channels(true);
        let (output_channels, main_output) = port_channels(false);
        let main_output = match main_output {
            Some(i) => i,
            None if !output_channels.is_empty() => 0,
            None => return Err(ClapError::NoAudioOutput),
        };
        if output_channels[main_output] == 0 {
            return Err(ClapError::NoAudioOutput);
        }
        let note_port = find_note_port(&handle);

        let config = PluginAudioConfiguration {
            sample_rate: f64::from(sample_rate),
            min_frames_count: 1,
            max_frames_count: max_block_frames as u32,
        };
        let stopped = self.instance.activate(|_, _| (), config)?;

        let alloc = |channels: &[u32]| -> Vec<Vec<f32>> {
            channels
                .iter()
                .map(|&c| vec![0.0; c as usize * max_block_frames])
                .collect()
        };
        Ok(ClapProcessor {
            processor: stopped.into(),
            sample_rate,
            max_block_frames,
            input_ports: AudioPorts::with_capacity(
                input_channels.iter().sum::<u32>() as usize,
                input_channels.len(),
            ),
            output_ports: AudioPorts::with_capacity(
                output_channels.iter().sum::<u32>() as usize,
                output_channels.len(),
            ),
            inputs: alloc(&input_channels),
            outputs: alloc(&output_channels),
            main_output,
            events: EventBuffer::with_capacity(MAX_EVENTS),
            note_port,
            pedal: SustainPedal::default(),
            notes: Vec::with_capacity(MAX_EVENTS),
            steady_time: 0,
            failed: false,
        })
    }
}

impl Drop for ClapPlugin {
    fn drop(&mut self) {
        // The editor goes before the plugin it belongs to.
        self.close_editor();
    }
}

/// An open editor.
struct Editor {
    gui: PluginGui,
    resizable: bool,
}

/// The size of a plugin's editor: in points on macOS, pixels elsewhere.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EditorSize {
    pub width: u32,
    pub height: u32,
    pub resizable: bool,
}

/// The most events one block can hand a plugin: the block's own, plus the
/// note offs the sustain pedal can add.
const MAX_EVENTS: usize = MAX_BLOCK_EVENTS + MAX_PEDAL_RELEASES;

/// The note port a plugin takes notes on, and the dialects it understands.
#[derive(Clone, Copy)]
struct NotePort {
    index: u16,
    clap: bool,
    midi: bool,
}

fn find_note_port(handle: &PluginMainThreadHandle) -> Option<NotePort> {
    let ext = handle.get_extension::<PluginNotePorts>()?;
    let mut buffer = NotePortInfoBuffer::new();
    let count = ext.count(handle, true).min(u32::from(u16::MAX));
    (0..count).find_map(|i| {
        let info = ext.get(handle, i, true, &mut buffer)?;
        let dialects = info.supported_dialects;
        if !dialects.intersects(NoteDialects::CLAP | NoteDialects::MIDI) {
            return None;
        }
        Some(NotePort {
            index: i as u16,
            clap: dialects.contains(NoteDialects::CLAP),
            midi: dialects.contains(NoteDialects::MIDI),
        })
    })
}

/// A running plugin, owned by the audio thread.
pub struct ClapProcessor {
    processor: PluginAudioProcessor<RefrainHost>,
    sample_rate: f32,
    max_block_frames: usize,
    input_ports: AudioPorts,
    output_ports: AudioPorts,
    // One buffer per port, holding its channels one after another, each
    // `max_block_frames` long. Plugins read and write channels separately.
    inputs: Vec<Vec<f32>>,
    outputs: Vec<Vec<f32>>,
    main_output: usize,
    events: EventBuffer,
    note_port: Option<NotePort>,
    // For plugins that take notes only, the host plays the sustain pedal.
    pedal: SustainPedal,
    notes: Vec<TimedEvent>,
    steady_time: u64,
    failed: bool,
}

impl ClapProcessor {
    /// Whether the plugin has failed a `process` call. Its output is silence
    /// from then on.
    pub fn has_failed(&self) -> bool {
        self.failed
    }

    /// Turns the block's events into CLAP events, in the dialects the
    /// plugin understands.
    ///
    /// A plugin that takes MIDI gets notes as CLAP notes, when it takes those
    /// too, and everything else as MIDI: it plays the pedal itself. A plugin
    /// that takes CLAP notes only gets notes, with the pedal played for it.
    fn push_events(&mut self, events: &[TimedEvent]) {
        self.events.clear();
        let Some(port) = self.note_port else {
            return;
        };
        let events = &events[..events.len().min(MAX_BLOCK_EVENTS)];
        if port.midi {
            for e in events {
                if port.clap && e.event.is_note() {
                    push_note(&mut self.events, port.index, e);
                } else {
                    let midi = MidiEvent::new(e.frame, port.index, e.event.to_midi());
                    self.events.push(&midi.with_flags(EventFlags::IS_LIVE));
                }
            }
        } else {
            self.notes.clear();
            self.pedal.apply(events, &mut self.notes);
            for e in &self.notes {
                push_note(&mut self.events, port.index, e);
            }
        }
    }
}

/// Pushes a note on or off as a CLAP note event. Other events are ignored.
fn push_note(buffer: &mut EventBuffer, port: u16, e: &TimedEvent) {
    let (channel, key) = match e.event {
        Event::NoteOn { channel, key, .. } | Event::NoteOff { channel, key, .. } => (channel, key),
        _ => return,
    };
    let pckn = Pckn::new(port, u16::from(channel), u16::from(key), Match::All);
    match e.event {
        Event::NoteOn { velocity, .. } => buffer.push(
            &NoteOnEvent::new(e.frame, pckn, f64::from(velocity)).with_flags(EventFlags::IS_LIVE),
        ),
        Event::NoteOff { velocity, .. } => buffer.push(
            &NoteOffEvent::new(e.frame, pckn, f64::from(velocity)).with_flags(EventFlags::IS_LIVE),
        ),
        _ => {}
    }
}

impl Processor for ClapProcessor {
    fn prepare(&mut self, sample_rate: f32, max_block_frames: usize) {
        // The plugin was activated for these on the main thread; it cannot be
        // re-activated from here.
        assert!(
            sample_rate == self.sample_rate && max_block_frames <= self.max_block_frames,
            "activate the plugin again to change the sample rate or block size"
        );
        self.steady_time = 0;
        self.pedal.reset();
    }

    fn process(&mut self, out: &mut [f32], channels: usize, events: &[TimedEvent]) {
        let frames = out.len() / channels;
        debug_assert!(frames <= self.max_block_frames);
        if self.failed {
            out.fill(0.0);
            return;
        }
        self.push_events(events);

        let max = self.max_block_frames;
        let ins = self
            .input_ports
            .with_input_buffers(self.inputs.iter_mut().map(|port| AudioPortBuffer {
                latency: 0,
                channels: AudioPortBufferType::f32_input_only(port.chunks_exact_mut(max).map(
                    |channel| {
                        let buffer = &mut channel[..frames];
                        buffer.fill(0.0);
                        InputChannel {
                            buffer,
                            is_constant: true,
                        }
                    },
                )),
            }));
        let mut outs = self
            .output_ports
            .with_output_buffers(self.outputs.iter_mut().map(|port| {
                AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_output_only(
                        port.chunks_exact_mut(max)
                            .map(|channel| &mut channel[..frames]),
                    ),
                }
            }));

        let result = self.processor.ensure_processing_started().and_then(|p| {
            p.process(
                &ins,
                &mut outs,
                &self.events.as_input(),
                &mut OutputEvents::void(),
                Some(self.steady_time),
                None,
            )
        });
        self.steady_time += frames as u64;
        if result.is_err() {
            self.failed = true;
            out.fill(0.0);
            return;
        }

        let main = &self.outputs[self.main_output];
        interleave(main, main.len() / max, max, out, channels);
    }

    fn has_failed(&self) -> bool {
        self.failed
    }
}
