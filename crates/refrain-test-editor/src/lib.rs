//! The editor of refrain's test plugins.
//!
//! It is a fixed-size panel showing the plugin's name, added as a child of
//! the window the host gives: an `NSView` on macOS, an `HWND` on Windows.
//! [`Panel::attach`] checks that the panel really became a child of that
//! window, so a host that hands over the wrong handle fails to open the
//! editor instead of showing nothing.

use std::ffi::c_void;

/// The editor's size, in points on macOS and pixels on Windows.
pub const WIDTH: u32 = 400;
pub const HEIGHT: u32 = 300;

/// The panel inside the host's window. Dropping it removes it.
pub struct Panel {
    #[allow(dead_code)]
    handle: *mut c_void,
}

impl Panel {
    /// Adds a panel showing `title` to `parent`. Returns `None` when this
    /// platform has no panel, or when it did not end up inside `parent`.
    ///
    /// # Safety
    ///
    /// `parent` must be a live `NSView` on macOS or `HWND` on Windows, and
    /// this must run on the main thread.
    pub unsafe fn attach(parent: *mut c_void, title: &str) -> Option<Panel> {
        if parent.is_null() {
            return None;
        }
        let handle = unsafe { platform::attach(parent, title) }?;
        Some(Panel { handle })
    }
}

impl Drop for Panel {
    fn drop(&mut self) {
        unsafe { platform::detach(self.handle) };
    }
}

#[cfg(target_os = "macos")]
mod platform {
    //! An `NSView` with a coloured layer and a label, through the
    //! Objective-C runtime.

    use std::ffi::{CString, c_char, c_void};

    use super::{HEIGHT, WIDTH};

    type Id = *mut c_void;
    type Sel = *mut c_void;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Rect {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    }

    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {
        fn objc_getClass(name: *const c_char) -> Id;
        fn sel_registerName(name: *const c_char) -> Sel;
        fn objc_msgSend();
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGColorCreateSRGB(r: f64, g: f64, b: f64, a: f64) -> *mut c_void;
        fn CGColorRelease(color: *mut c_void);
    }

    unsafe fn sel(name: &std::ffi::CStr) -> Sel {
        unsafe { sel_registerName(name.as_ptr()) }
    }

    // objc_msgSend has to be called through a pointer of the right type for
    // each signature.
    macro_rules! send {
        ($obj:expr, $sel:expr $(, $arg:expr => $ty:ty)* ; -> $ret:ty) => {{
            let f: unsafe extern "C" fn(Id, Sel $(, $ty)*) -> $ret =
                std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            f($obj, sel($sel) $(, $arg)*)
        }};
    }

    pub unsafe fn attach(parent: Id, title: &str) -> Option<Id> {
        unsafe {
            let frame = Rect {
                x: 0.0,
                y: 0.0,
                width: f64::from(WIDTH),
                height: f64::from(HEIGHT),
            };
            let view: Id = send!(objc_getClass(c"NSView".as_ptr()), c"alloc"; -> Id);
            let view: Id = send!(view, c"initWithFrame:", frame => Rect; -> Id);
            if view.is_null() {
                return None;
            }
            let () = send!(view, c"setWantsLayer:", true => bool; -> ());
            let layer: Id = send!(view, c"layer"; -> Id);
            if !layer.is_null() {
                let color = CGColorCreateSRGB(0.16, 0.36, 0.52, 1.0);
                let () = send!(layer, c"setBackgroundColor:", color => *mut c_void; -> ());
                CGColorRelease(color);
            }

            let text = CString::new(title).unwrap_or_default();
            let string: Id = send!(objc_getClass(c"NSString".as_ptr()), c"stringWithUTF8String:", text.as_ptr() => *const c_char; -> Id);
            let label: Id = send!(objc_getClass(c"NSTextField".as_ptr()), c"labelWithString:", string => Id; -> Id);
            if !label.is_null() {
                let label_frame = Rect {
                    x: 20.0,
                    y: f64::from(HEIGHT) / 2.0 - 12.0,
                    width: f64::from(WIDTH) - 40.0,
                    height: 24.0,
                };
                let () = send!(label, c"setFrame:", label_frame => Rect; -> ());
                let () = send!(view, c"addSubview:", label => Id; -> ());
            }

            let () = send!(parent, c"addSubview:", view => Id; -> ());
            let superview: Id = send!(view, c"superview"; -> Id);
            if superview != parent {
                detach(view);
                return None;
            }
            Some(view)
        }
    }

    pub unsafe fn detach(view: Id) {
        unsafe {
            let () = send!(view, c"removeFromSuperview"; -> ());
            let () = send!(view, c"release"; -> ());
        }
    }
}

#[cfg(windows)]
mod platform {
    //! A child `STATIC` window showing the title.

    use std::ffi::c_void;

    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::SystemServices::{SS_CENTER, SS_CENTERIMAGE};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, IsChild, WS_CHILD, WS_VISIBLE,
    };

    use super::{HEIGHT, WIDTH};

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    pub unsafe fn attach(parent: *mut c_void, title: &str) -> Option<*mut c_void> {
        let class = wide("STATIC");
        let text = wide(title);
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                text.as_ptr(),
                WS_CHILD | WS_VISIBLE | SS_CENTER | SS_CENTERIMAGE,
                0,
                0,
                WIDTH as i32,
                HEIGHT as i32,
                parent,
                std::ptr::null_mut(),
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null(),
            )
        };
        if hwnd.is_null() {
            return None;
        }
        if unsafe { IsChild(parent, hwnd) } == 0 {
            unsafe { DestroyWindow(hwnd) };
            return None;
        }
        Some(hwnd)
    }

    pub unsafe fn detach(hwnd: *mut c_void) {
        unsafe { DestroyWindow(hwnd) };
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod platform {
    use std::ffi::c_void;

    pub unsafe fn attach(_parent: *mut c_void, _title: &str) -> Option<*mut c_void> {
        None
    }

    pub unsafe fn detach(_handle: *mut c_void) {}
}
