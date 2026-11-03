//! A window that holds a plugin's editor.

use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize, Size};
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::raw_window_handle::HasWindowHandle;
use winit::window::{Window, WindowId};

use crate::Result;
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

/// A window holding the editor of one plugin of a session.
pub struct EditorWindow {
    pub slot: Slot,
    window: Window,
}

impl EditorWindow {
    /// Opens the editor of the plugin in `slot` in a new window. `None` when
    /// the plugin has no editor that can be embedded.
    pub fn open(
        event_loop: &ActiveEventLoop,
        session: &mut Session,
        slot: Slot,
        title: &str,
    ) -> Result<Option<Self>> {
        let attributes = Window::default_attributes()
            .with_title(title)
            .with_visible(false)
            .with_resizable(false)
            .with_inner_size(window_size(640, 480));
        let window = event_loop.create_window(attributes)?;
        let parent = window.window_handle()?.as_raw();
        let plugin = session.plugin(slot)?;
        // SAFETY: the window outlives the editor; `close` closes the editor
        // before the window is dropped.
        let Some(size) = unsafe { plugin.open_editor(parent, window.scale_factor()) }? else {
            return Ok(None);
        };
        let resizable = if size.resizable { ", resizable" } else { "" };
        // A plugin that accepted the window but put nothing in it shows an
        // empty window; say so rather than leave it a mystery.
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
        Ok(Some(Self { slot, window }))
    }

    pub fn id(&self) -> WindowId {
        self.window.id()
    }

    /// Handles an event for this window. Returns false once the window is to
    /// close.
    pub fn on_event(&mut self, session: &mut Session, event: &WindowEvent) -> bool {
        let Ok(plugin) = session.plugin(self.slot) else {
            return false;
        };
        match event {
            WindowEvent::CloseRequested => false,
            WindowEvent::Resized(size) => {
                let (w, h) = editor_size(*size, self.window.scale_factor());
                let (aw, ah) = plugin.resize_editor(w, h);
                // Only push back when the editor insists on a clearly
                // different size, so rounding cannot start a resize loop.
                if aw.abs_diff(w) > 1 || ah.abs_diff(h) > 1 {
                    let _ = self.window.request_inner_size(window_size(aw, ah));
                }
                true
            }
            _ => true,
        }
    }

    /// Follows the editor's requests. Returns false once the plugin has
    /// closed its editor.
    pub fn poll(&mut self, session: &mut Session) -> bool {
        let Ok(plugin) = session.plugin(self.slot) else {
            return false;
        };
        if let Some((w, h)) = plugin.take_editor_resize_request() {
            let _ = self.window.request_inner_size(window_size(w, h));
        }
        !plugin.editor_closed()
    }

    /// Closes the editor, then the window it lives in.
    pub fn close(self, session: &mut Session) {
        if let Ok(plugin) = session.plugin(self.slot) {
            plugin.close_editor();
        }
        drop(self.window);
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
        editor: None,
        opened: None,
        error: None,
    };
    event_loop.run_app(&mut app)?;
    if let Some(editor) = app.editor.take() {
        editor.close(app.session);
    }
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
    editor: Option<EditorWindow>,
    opened: Option<bool>,
    error: Option<Box<dyn std::error::Error>>,
}

impl App<'_> {
    fn close(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(editor) = self.editor.take() {
            editor.close(self.session);
        }
        event_loop.exit();
    }
}

impl ApplicationHandler for App<'_> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.opened.is_some() {
            return;
        }
        match EditorWindow::open(event_loop, self.session, self.slot, self.title) {
            Ok(editor) => {
                self.opened = Some(editor.is_some());
                self.editor = editor;
                if self.editor.is_none() {
                    event_loop.exit();
                }
            }
            Err(e) => {
                self.error = Some(e);
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let keep = match &mut self.editor {
            Some(editor) => editor.on_event(self.session, &event),
            None => true,
        };
        if !keep {
            self.close(event_loop);
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        if !editor.poll(self.session) || !(self.tick)(self.session) {
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
