//! A window that holds a plugin's editor.

use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize, Size};
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::raw_window_handle::HasWindowHandle;
use winit::window::{Window, WindowId};

use crate::Result;
use crate::plugin::Kind;
use crate::session::{Session, Slot};

/// Editor sizes are in points on macOS and in pixels elsewhere, in both
/// plugin formats.
const LOGICAL: bool = cfg!(target_os = "macos");

fn window_size(width: u32, height: u32) -> Size {
    if LOGICAL {
        LogicalSize::new(f64::from(width), f64::from(height)).into()
    } else {
        PhysicalSize::new(width, height).into()
    }
}

fn editor_size(size: PhysicalSize<u32>, scale: f64) -> (u32, u32) {
    if LOGICAL {
        let logical: LogicalSize<u32> = size.to_logical(scale);
        (logical.width, logical.height)
    } else {
        (size.width, size.height)
    }
}

/// Opens the editor of the plugin in `slot` in a window, and runs until the
/// window closes or `tick` returns false. `tick` runs every few milliseconds
/// on the main thread.
///
/// Returns `Ok(false)` straight away when the plugin has no editor that can be
/// embedded.
pub fn run(
    session: &mut Session,
    slot: Slot,
    title: &str,
    tick: &mut dyn FnMut(&mut Session) -> bool,
) -> Result<bool> {
    session.plugin(slot)?;
    let event_loop = EventLoop::new()?;
    let mut app = App {
        session,
        slot,
        title,
        tick,
        window: None,
        opened: None,
        error: None,
    };
    event_loop.run_app(&mut app)?;
    // The editor goes before the window it lives in.
    app.plugin().close_editor();
    app.window.take();
    match app.error {
        Some(e) => Err(e),
        None => Ok(app.opened.unwrap_or(false)),
    }
}

struct App<'a> {
    session: &'a mut Session,
    slot: Slot,
    title: &'a str,
    tick: &'a mut dyn FnMut(&mut Session) -> bool,
    window: Option<Window>,
    opened: Option<bool>,
    error: Option<Box<dyn std::error::Error>>,
}

impl App<'_> {
    fn plugin(&mut self) -> &mut Kind {
        self.session
            .plugin(self.slot)
            .expect("run checked the slot")
    }

    fn open(&mut self, event_loop: &ActiveEventLoop) -> Result<()> {
        let attributes = Window::default_attributes()
            .with_title(self.title)
            .with_visible(false)
            .with_resizable(false)
            .with_inner_size(window_size(640, 480));
        let window = event_loop.create_window(attributes)?;
        let parent = window.window_handle()?.as_raw();
        // SAFETY: the window outlives the editor; `run` closes the editor
        // before dropping the window.
        let size = unsafe { self.plugin().open_editor(parent, window.scale_factor()) }?;
        match size {
            Some(size) => {
                let resizable = if size.resizable { ", resizable" } else { "" };
                // A plugin that accepted the window but put nothing in it
                // shows an empty window; say so rather than leave it a mystery.
                let embedded = match has_children(parent) {
                    Some(true) => ", embedded",
                    Some(false) => ", but the plugin put nothing in the window",
                    None => "",
                };
                println!(
                    "editor: {} x {}{resizable}{embedded}",
                    size.width, size.height
                );
                window.set_resizable(size.resizable);
                let _ = window.request_inner_size(window_size(size.width, size.height));
                window.set_visible(true);
                self.window = Some(window);
                self.opened = Some(true);
            }
            None => {
                self.opened = Some(false);
                event_loop.exit();
            }
        }
        Ok(())
    }

    fn close(&mut self, event_loop: &ActiveEventLoop) {
        self.plugin().close_editor();
        event_loop.exit();
    }
}

impl ApplicationHandler for App<'_> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.opened.is_none()
            && let Err(e) = self.open(event_loop)
        {
            self.error = Some(e);
            event_loop.exit();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => self.close(event_loop),
            WindowEvent::Resized(size) => {
                let Some(scale) = self.window.as_ref().map(Window::scale_factor) else {
                    return;
                };
                let (w, h) = editor_size(size, scale);
                let (aw, ah) = self.plugin().resize_editor(w, h);
                // Only push back when the editor insists on a clearly
                // different size, so rounding cannot start a resize loop.
                if (aw.abs_diff(w) > 1 || ah.abs_diff(h) > 1)
                    && let Some(window) = &self.window
                {
                    let _ = window.request_inner_size(window_size(aw, ah));
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() {
            return;
        }
        let request = self.plugin().take_editor_resize_request();
        if let (Some(window), Some((w, h))) = (&self.window, request) {
            let _ = window.request_inner_size(window_size(w, h));
        }
        if self.plugin().editor_closed() || !(self.tick)(self.session) {
            self.close(event_loop);
            return;
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(
            Instant::now() + Duration::from_millis(5),
        ));
    }
}

/// Whether `parent` has a child view or window, where that can be told.
fn has_children(parent: winit::raw_window_handle::RawWindowHandle) -> Option<bool> {
    use winit::raw_window_handle::RawWindowHandle as Raw;
    match parent {
        #[cfg(target_os = "macos")]
        Raw::AppKit(h) => Some(unsafe { platform::subview_count(h.ns_view.as_ptr()) } > 0),
        #[cfg(windows)]
        Raw::Win32(h) => Some(unsafe { platform::has_child(h.hwnd.get() as *mut _) }),
        _ => None,
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::{c_char, c_void};

    unsafe extern "C" {
        fn sel_registerName(name: *const c_char) -> *mut c_void;
        fn objc_msgSend();
    }

    /// `[[view subviews] count]`.
    pub unsafe fn subview_count(view: *mut c_void) -> usize {
        unsafe {
            let object: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
                std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let count: unsafe extern "C" fn(*mut c_void, *mut c_void) -> usize =
                std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let subviews = object(view, sel_registerName(c"subviews".as_ptr()));
            if subviews.is_null() {
                return 0;
            }
            count(subviews, sel_registerName(c"count".as_ptr()))
        }
    }
}

#[cfg(windows)]
mod platform {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GW_CHILD, GetWindow};

    pub unsafe fn has_child(hwnd: *mut std::ffi::c_void) -> bool {
        !unsafe { GetWindow(hwnd, GW_CHILD) }.is_null()
    }
}
