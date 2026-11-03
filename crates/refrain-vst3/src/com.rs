//! The COM objects refrain gives a VST3 plugin: the host application, the
//! component handler, a memory stream, and the event list and parameter
//! changes passed to `process`.
//!
//! The event list and parameter changes are written by the host and read by
//! the plugin, both on the audio thread and never at the same time. They keep
//! their contents in `UnsafeCell`s and allocate everything up front.

use std::cell::{Cell, RefCell, UnsafeCell};
use std::ffi::c_void;
use std::ptr;

use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComWrapper};

/// The host, as the plugin sees it in `initialize`.
pub struct HostApplication;

impl Class for HostApplication {
    type Interfaces = (IHostApplication,);
}

impl IHostApplicationTrait for HostApplication {
    unsafe fn getName(&self, name: *mut String128) -> tresult {
        let name = unsafe { &mut *name };
        let mut len = 0;
        for (d, s) in name.iter_mut().zip("refrain".encode_utf16()) {
            *d = s as TChar;
            len += 1;
        }
        name[len] = 0;
        kResultOk
    }

    unsafe fn createInstance(
        &self,
        _cid: *mut TUID,
        _iid: *mut TUID,
        obj: *mut *mut c_void,
    ) -> tresult {
        // Messages between a plugin's component and controller go through
        // their connection points directly, so refrain makes none.
        unsafe { *obj = ptr::null_mut() };
        kNotImplemented
    }
}

/// Receives edits the plugin makes to its own parameters. Ignored for now.
pub struct ComponentHandler;

impl Class for ComponentHandler {
    type Interfaces = (IComponentHandler,);
}

impl IComponentHandlerTrait for ComponentHandler {
    unsafe fn beginEdit(&self, _id: ParamID) -> tresult {
        kResultOk
    }

    unsafe fn performEdit(&self, _id: ParamID, _value: ParamValue) -> tresult {
        kResultOk
    }

    unsafe fn endEdit(&self, _id: ParamID) -> tresult {
        kResultOk
    }

    unsafe fn restartComponent(&self, _flags: int32) -> tresult {
        kResultOk
    }
}

/// A stream over bytes in memory, for passing state between a plugin's
/// component and its controller.
#[derive(Default)]
pub struct MemoryStream {
    data: RefCell<Vec<u8>>,
    position: Cell<usize>,
}

impl Class for MemoryStream {
    type Interfaces = (IBStream,);
}

impl MemoryStream {
    pub fn rewind(&self) {
        self.position.set(0);
    }
}

impl IBStreamTrait for MemoryStream {
    unsafe fn read(&self, buffer: *mut c_void, count: int32, read: *mut int32) -> tresult {
        let data = self.data.borrow();
        let start = self.position.get().min(data.len());
        let n = (count.max(0) as usize).min(data.len() - start);
        unsafe { ptr::copy_nonoverlapping(data[start..].as_ptr(), buffer as *mut u8, n) };
        self.position.set(start + n);
        if !read.is_null() {
            unsafe { *read = n as int32 };
        }
        kResultOk
    }

    unsafe fn write(&self, buffer: *mut c_void, count: int32, written: *mut int32) -> tresult {
        let n = count.max(0) as usize;
        let bytes = unsafe { std::slice::from_raw_parts(buffer as *const u8, n) };
        let mut data = self.data.borrow_mut();
        let start = self.position.get();
        if data.len() < start + n {
            data.resize(start + n, 0);
        }
        data[start..start + n].copy_from_slice(bytes);
        self.position.set(start + n);
        if !written.is_null() {
            unsafe { *written = n as int32 };
        }
        kResultOk
    }

    unsafe fn seek(&self, pos: int64, mode: int32, result: *mut int64) -> tresult {
        // VST3 enums are `c_int` on Windows and `c_uint` elsewhere.
        #[allow(clippy::unnecessary_cast)]
        let mode = mode as IBStream_::IStreamSeekMode;
        let base = match mode {
            IBStream_::IStreamSeekMode_::kIBSeekSet => 0,
            IBStream_::IStreamSeekMode_::kIBSeekCur => self.position.get() as i64,
            IBStream_::IStreamSeekMode_::kIBSeekEnd => self.data.borrow().len() as i64,
            _ => return kInvalidArgument,
        };
        let Ok(position) = usize::try_from(base + pos) else {
            return kInvalidArgument;
        };
        self.position.set(position);
        if !result.is_null() {
            unsafe { *result = position as int64 };
        }
        kResultOk
    }

    unsafe fn tell(&self, pos: *mut int64) -> tresult {
        if pos.is_null() {
            return kInvalidArgument;
        }
        unsafe { *pos = self.position.get() as int64 };
        kResultOk
    }
}

/// The notes of one block.
pub struct EventList {
    events: UnsafeCell<Vec<Event>>,
}

impl Class for EventList {
    type Interfaces = (IEventList,);
}

impl EventList {
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            events: UnsafeCell::new(Vec::with_capacity(capacity)),
        }
    }

    /// # Safety
    /// The plugin must not be reading the list, and no other reference from
    /// this method may be alive.
    #[allow(clippy::mut_from_ref)] // The list lives in an UnsafeCell.
    pub unsafe fn events(&self) -> &mut Vec<Event> {
        unsafe { &mut *self.events.get() }
    }
}

impl IEventListTrait for EventList {
    unsafe fn getEventCount(&self) -> int32 {
        unsafe { (*self.events.get()).len() as int32 }
    }

    unsafe fn getEvent(&self, index: int32, e: *mut Event) -> tresult {
        let events = unsafe { &*self.events.get() };
        match usize::try_from(index).ok().and_then(|i| events.get(i)) {
            Some(event) => {
                unsafe { *e = *event };
                kResultOk
            }
            None => kInvalidArgument,
        }
    }

    unsafe fn addEvent(&self, _e: *mut Event) -> tresult {
        // Input lists are read-only for the plugin.
        kResultFalse
    }
}

/// The changes to one parameter within a block.
pub struct ParamQueue {
    id: Cell<ParamID>,
    points: UnsafeCell<Vec<(int32, ParamValue)>>,
}

impl Class for ParamQueue {
    type Interfaces = (IParamValueQueue,);
}

impl IParamValueQueueTrait for ParamQueue {
    unsafe fn getParameterId(&self) -> ParamID {
        self.id.get()
    }

    unsafe fn getPointCount(&self) -> int32 {
        unsafe { (*self.points.get()).len() as int32 }
    }

    unsafe fn getPoint(&self, index: int32, offset: *mut int32, value: *mut ParamValue) -> tresult {
        let points = unsafe { &*self.points.get() };
        match usize::try_from(index).ok().and_then(|i| points.get(i)) {
            Some(&(o, v)) => {
                unsafe {
                    *offset = o;
                    *value = v;
                }
                kResultOk
            }
            None => kInvalidArgument,
        }
    }

    unsafe fn addPoint(&self, _offset: int32, _value: ParamValue, _index: *mut int32) -> tresult {
        kResultFalse
    }
}

/// The parameter changes of one block: a queue per changed parameter.
pub struct ParamChanges {
    queues: Vec<ComWrapper<ParamQueue>>,
    used: Cell<usize>,
}

impl Class for ParamChanges {
    type Interfaces = (IParameterChanges,);
}

impl ParamChanges {
    pub fn new(queues: usize, points: usize) -> Self {
        Self {
            queues: (0..queues)
                .map(|_| {
                    ComWrapper::new(ParamQueue {
                        id: Cell::new(0),
                        points: UnsafeCell::new(Vec::with_capacity(points)),
                    })
                })
                .collect(),
            used: Cell::new(0),
        }
    }

    pub fn clear(&self) {
        for q in &self.queues[..self.used.get()] {
            unsafe { (*q.points.get()).clear() };
        }
        self.used.set(0);
    }

    /// Adds a change. Points must come in order of offset. Returns false when
    /// there is no room left, without allocating.
    ///
    /// # Safety
    /// The plugin must not be reading the changes.
    pub unsafe fn push(&self, id: ParamID, offset: int32, value: ParamValue) -> bool {
        let used = self.used.get();
        let queue = match self.queues[..used].iter().find(|q| q.id.get() == id) {
            Some(q) => q,
            None if used < self.queues.len() => {
                let q = &self.queues[used];
                q.id.set(id);
                self.used.set(used + 1);
                q
            }
            None => return false,
        };
        let points = unsafe { &mut *queue.points.get() };
        if points.len() == points.capacity() {
            return false;
        }
        points.push((offset, value));
        true
    }
}

impl IParameterChangesTrait for ParamChanges {
    unsafe fn getParameterCount(&self) -> int32 {
        self.used.get() as int32
    }

    unsafe fn getParameterData(&self, index: int32) -> *mut IParamValueQueue {
        usize::try_from(index)
            .ok()
            .filter(|&i| i < self.used.get())
            .and_then(|i| self.queues[i].as_com_ref::<IParamValueQueue>())
            .map_or(ptr::null_mut(), |q| q.as_ptr())
    }

    unsafe fn addParameterData(
        &self,
        _id: *const ParamID,
        _index: *mut int32,
    ) -> *mut IParamValueQueue {
        ptr::null_mut()
    }
}
