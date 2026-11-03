//! Loading `.vst3` files and reading the plugins they hold.

use std::ffi::{c_char, c_void};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use vst3::Steinberg::*;
use vst3::{ComPtr, Interface};

use crate::Vst3Error;

/// What a plugin says about itself.
#[derive(Clone, Debug)]
pub struct Vst3Info {
    pub name: String,
    /// The class ID, in hex. Unique, unlike the name.
    pub id: String,
    pub vendor: String,
    pub version: String,
    /// Such as `Instrument|Synth` or `Fx|Delay`.
    pub categories: String,
    pub(crate) cid: TUID,
}

impl std::fmt::Display for Vst3Info {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)?;
        if !self.vendor.is_empty() {
            write!(f, " by {}", self.vendor)?;
        }
        if !self.version.is_empty() {
            write!(f, ", version {}", self.version)?;
        }
        if !self.categories.is_empty() {
            write!(f, " [{}]", self.categories)?;
        }
        Ok(())
    }
}

/// A loaded VST3 module. Clones share it; it is unloaded when the last one
/// goes.
#[derive(Clone)]
pub struct Vst3Module {
    inner: Arc<Inner>,
}

struct Inner {
    // Released before the library is unloaded.
    factory: Option<ComPtr<IPluginFactory>>,
    library: Option<Library>,
}

struct Library {
    library: libloading::Library,
    // Kept until the module has been told to exit.
    #[cfg(target_os = "macos")]
    _bundle: mac::Bundle,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.factory.take();
        if let Some(lib) = self.library.take() {
            let exit = if cfg!(target_os = "windows") {
                &b"ExitDll"[..]
            } else {
                b"bundleExit"
            };
            unsafe {
                if let Ok(exit) = lib.library.get::<unsafe extern "system" fn() -> bool>(exit) {
                    exit();
                }
            }
        }
    }
}

// The factory is only used from the main thread; the processor holds a clone
// only to keep the library loaded while it runs.
unsafe impl Send for Inner {}
unsafe impl Sync for Inner {}

fn text(chars: &[c_char]) -> String {
    let bytes: Vec<u8> = chars
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

impl Vst3Module {
    /// Loads a `.vst3` bundle, or on Windows a single `.vst3` file.
    ///
    /// # Safety
    ///
    /// This runs the module's code in this process. A broken plugin can
    /// corrupt memory or crash the program.
    pub unsafe fn load(path: &Path) -> Result<Self, Vst3Error> {
        let binary = binary_path(path)?;
        let library = unsafe { libloading::Library::new(&binary) }
            .map_err(|e| Vst3Error::Load(format!("{}: {e}", binary.display())))?;

        #[cfg(target_os = "macos")]
        let bundle = mac::Bundle::open(path);
        unsafe {
            #[cfg(target_os = "macos")]
            if let Ok(entry) =
                library.get::<unsafe extern "system" fn(*mut c_void) -> bool>(b"bundleEntry")
                && !entry(bundle.as_ptr())
            {
                return Err(Vst3Error::Load("bundleEntry failed".into()));
            }
            #[cfg(target_os = "windows")]
            if let Ok(entry) = library.get::<unsafe extern "system" fn() -> bool>(b"InitDll")
                && !entry()
            {
                return Err(Vst3Error::Load("InitDll failed".into()));
            }
        }
        let get_factory = unsafe {
            library
                .get::<unsafe extern "system" fn() -> *mut IPluginFactory>(b"GetPluginFactory")
                .map_err(|e| Vst3Error::Load(format!("no GetPluginFactory: {e}")))?
        };
        let factory = unsafe { ComPtr::from_raw(get_factory()) }
            .ok_or_else(|| Vst3Error::Load("GetPluginFactory returned nothing".into()))?;
        Ok(Self {
            inner: Arc::new(Inner {
                factory: Some(factory),
                library: Some(Library {
                    library,
                    #[cfg(target_os = "macos")]
                    _bundle: bundle,
                }),
            }),
        })
    }

    /// Wraps a factory that is already in this process, such as one linked
    /// into a test.
    ///
    /// # Safety
    ///
    /// `factory` must be a valid factory pointer, owned by the caller, who
    /// hands its reference over.
    pub unsafe fn from_factory(factory: *mut IPluginFactory) -> Result<Self, Vst3Error> {
        let factory = unsafe { ComPtr::from_raw(factory) }
            .ok_or_else(|| Vst3Error::Load("no factory".into()))?;
        Ok(Self {
            inner: Arc::new(Inner {
                factory: Some(factory),
                library: None,
            }),
        })
    }

    fn factory(&self) -> &ComPtr<IPluginFactory> {
        self.inner
            .factory
            .as_ref()
            .expect("the factory lives as long as the module")
    }

    /// Lists the audio plugins in the module: the classes of category "Audio
    /// Module Class". Controllers and other classes are left out.
    pub fn plugins(&self) -> Vec<Vst3Info> {
        let factory = self.factory();
        let factory2 = factory.cast::<IPluginFactory2>();
        let mut vendor = String::new();
        let mut finfo: PFactoryInfo = unsafe { std::mem::zeroed() };
        if unsafe { factory.getFactoryInfo(&mut finfo) } == kResultOk {
            vendor = text(&finfo.vendor);
        }
        let mut plugins = Vec::new();
        for i in 0..unsafe { factory.countClasses() } {
            let mut info: PClassInfo = unsafe { std::mem::zeroed() };
            if unsafe { factory.getClassInfo(i, &mut info) } != kResultOk
                || text(&info.category) != "Audio Module Class"
            {
                continue;
            }
            let mut plugin = Vst3Info {
                name: text(&info.name),
                id: info
                    .cid
                    .iter()
                    .map(|b| format!("{:02X}", *b as u8))
                    .collect(),
                vendor: vendor.clone(),
                version: String::new(),
                categories: String::new(),
                cid: info.cid,
            };
            if let Some(f2) = &factory2 {
                let mut info2: PClassInfo2 = unsafe { std::mem::zeroed() };
                if unsafe { f2.getClassInfo2(i, &mut info2) } == kResultOk {
                    plugin.categories = text(&info2.subCategories);
                    plugin.version = text(&info2.version);
                    let v = text(&info2.vendor);
                    if !v.is_empty() {
                        plugin.vendor = v;
                    }
                }
            }
            plugins.push(plugin);
        }
        plugins
    }

    /// Creates an instance of class `cid`, asking for interface `I`.
    pub(crate) fn create<I: Interface>(&self, cid: &TUID) -> Option<ComPtr<I>> {
        let mut obj: *mut c_void = std::ptr::null_mut();
        let result = unsafe {
            self.factory()
                .createInstance(cid.as_ptr(), I::IID.as_ptr() as FIDString, &mut obj)
        };
        if result != kResultOk {
            return None;
        }
        unsafe { ComPtr::from_raw(obj as *mut I) }
    }
}

/// The file to load for a `.vst3` path.
fn binary_path(path: &Path) -> Result<PathBuf, Vst3Error> {
    if path.is_file() {
        return Ok(path.to_path_buf());
    }
    if !path.is_dir() {
        return Err(Vst3Error::Load(format!(
            "{} does not exist",
            path.display()
        )));
    }
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let contents = path.join("Contents");
    let candidates: Vec<PathBuf> = if cfg!(target_os = "macos") {
        let name = std::fs::read_to_string(contents.join("Info.plist"))
            .ok()
            .and_then(|plist| bundle_executable(&plist))
            .unwrap_or_else(|| stem.clone());
        vec![contents.join("MacOS").join(name)]
    } else if cfg!(target_os = "windows") {
        let arch = if cfg!(target_arch = "aarch64") {
            "arm64-win"
        } else {
            "x86_64-win"
        };
        vec![contents.join(arch).join(format!("{stem}.vst3"))]
    } else {
        let arch = format!("{}-linux", std::env::consts::ARCH);
        vec![contents.join(arch).join(format!("{stem}.so"))]
    };
    candidates
        .into_iter()
        .find(|c| c.is_file())
        .ok_or_else(|| Vst3Error::Load(format!("no binary inside {}", path.display())))
}

/// Reads `CFBundleExecutable` from an `Info.plist`.
fn bundle_executable(plist: &str) -> Option<String> {
    let after = &plist[plist.find("<key>CFBundleExecutable</key>")?..];
    let start = after.find("<string>")? + "<string>".len();
    let end = after[start..].find("</string>")?;
    Some(after[start..start + end].trim().to_owned())
}

/// Folders where VST3 plugins are installed, as the VST3 documentation lists
/// them.
pub fn standard_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            paths.push(PathBuf::from(home).join("Library/Audio/Plug-Ins/VST3"));
        }
        paths.push(PathBuf::from("/Library/Audio/Plug-Ins/VST3"));
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            paths.push(PathBuf::from(local).join("Programs/Common/VST3"));
        }
        if let Some(common) = std::env::var_os("COMMONPROGRAMFILES") {
            paths.push(PathBuf::from(common).join("VST3"));
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            paths.push(PathBuf::from(home).join(".vst3"));
        }
        paths.push(PathBuf::from("/usr/lib/vst3"));
    }
    paths
}

/// Finds `.vst3` files and bundles under `dirs`, searching subfolders.
pub fn find_vst3_files(dirs: &[PathBuf]) -> Vec<PathBuf> {
    fn walk(dir: &Path, found: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("vst3"))
            {
                // A .vst3 bundle is a folder; do not look inside it.
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

#[cfg(target_os = "macos")]
mod mac {
    //! The `CFBundleRef` that `bundleEntry` takes.

    use std::ffi::c_void;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFURLCreateFromFileSystemRepresentation(
            allocator: *const c_void,
            buffer: *const u8,
            length: isize,
            is_directory: u8,
        ) -> *const c_void;
        fn CFBundleCreate(allocator: *const c_void, url: *const c_void) -> *mut c_void;
        fn CFRelease(cf: *const c_void);
    }

    pub struct Bundle(*mut c_void);

    impl Bundle {
        /// Opens the bundle at `path`, or gives a null bundle when `path` is
        /// not one.
        pub fn open(path: &Path) -> Self {
            if !path.is_dir() {
                return Self(std::ptr::null_mut());
            }
            let bytes = path.as_os_str().as_bytes();
            unsafe {
                let url = CFURLCreateFromFileSystemRepresentation(
                    std::ptr::null(),
                    bytes.as_ptr(),
                    bytes.len() as isize,
                    1,
                );
                if url.is_null() {
                    return Self(std::ptr::null_mut());
                }
                let bundle = CFBundleCreate(std::ptr::null(), url);
                CFRelease(url);
                Self(bundle)
            }
        }

        pub fn as_ptr(&self) -> *mut c_void {
            self.0
        }
    }

    impl Drop for Bundle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { CFRelease(self.0) };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_executable_from_info_plist() {
        let plist = "<dict>\n<key>CFBundleName</key><string>X</string>\n\
                     <key>CFBundleExecutable</key>\n\t<string>Some Synth</string>\n</dict>";
        assert_eq!(bundle_executable(plist).as_deref(), Some("Some Synth"));
        assert_eq!(bundle_executable("<dict></dict>"), None);
    }
}
