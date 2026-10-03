//! Safe wrapper over the parts of WinDivert 2.2 the engine needs. The DLL
//! is loaded at runtime from an explicit path, so it can live anywhere
//! (the driver file `WinDivert64.sys` must sit next to it).

use std::ffi::{c_char, c_void, CString};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::sync::Arc;

const LAYER_NETWORK: i32 = 0;
const SHUTDOWN_BOTH: i32 = 3;

pub const ERROR_NO_DATA: i32 = 232;
pub const ERROR_INSUFFICIENT_BUFFER: i32 = 122;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Address {
    timestamp: i64,
    bits: u32,
    reserved: u32,
    data: [u8; 64],
}

impl Address {
    pub fn set_inbound(&mut self) { self.bits &= !(1 << 17); }
    pub fn outbound(&self) -> bool {
        self.bits >> 17 & 1 == 1
    }
}

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryW(path: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
}

type OpenFn = unsafe extern "C" fn(*const c_char, i32, i16, u64) -> *mut c_void;
type RecvFn = unsafe extern "C" fn(*mut c_void, *mut c_void, u32, *mut u32, *mut Address) -> i32;
type SendFn =
    unsafe extern "C" fn(*mut c_void, *const c_void, u32, *mut u32, *const Address) -> i32;
type CalcFn = unsafe extern "C" fn(*mut c_void, u32, *mut Address, u64) -> i32;
type HandleFn = unsafe extern "C" fn(*mut c_void) -> i32;
type ShutdownFn = unsafe extern "C" fn(*mut c_void, i32) -> i32;
type CompileFn =
    unsafe extern "C" fn(*const c_char, i32, *mut c_char, u32, *mut *const c_char, *mut u32) -> i32;

/// The loaded WinDivert library. Never unloaded.
pub struct Api {
    open: OpenFn,
    recv: RecvFn,
    send: SendFn,
    calc: CalcFn,
    shutdown: ShutdownFn,
    close: HandleFn,
    compile: CompileFn,
}

impl Api {
    pub fn load(dll: &Path) -> io::Result<Arc<Api>> {
        let wide: Vec<u16> = dll.as_os_str().encode_wide().chain([0]).collect();
        let module = unsafe { LoadLibraryW(wide.as_ptr()) };
        if module.is_null() {
            let e = io::Error::last_os_error();
            return Err(io::Error::new(
                e.kind(),
                format!("cannot load {}: {e}", dll.display()),
            ));
        }
        macro_rules! sym {
            ($name:literal) => {{
                let p = unsafe { GetProcAddress(module, concat!($name, "\0").as_ptr()) };
                if p.is_null() {
                    return Err(io::Error::other(format!(
                        "{} lacks {}",
                        dll.display(),
                        $name
                    )));
                }
                unsafe { std::mem::transmute_copy(&p) }
            }};
        }
        Ok(Arc::new(Api {
            open: sym!("WinDivertOpen"),
            recv: sym!("WinDivertRecv"),
            send: sym!("WinDivertSend"),
            calc: sym!("WinDivertHelperCalcChecksums"),
            shutdown: sym!("WinDivertShutdown"),
            close: sym!("WinDivertClose"),
            compile: sym!("WinDivertHelperCompileFilter"),
        }))
    }

    pub fn validate_filter(&self, filter: &str) -> io::Result<()> {
        let filter = CString::new(filter).map_err(io::Error::other)?;
        let mut error = std::ptr::null();
        let mut position = 0;
        let ok = unsafe {
            (self.compile)(
                filter.as_ptr(),
                LAYER_NETWORK,
                std::ptr::null_mut(),
                0,
                &mut error,
                &mut position,
            )
        };
        if ok != 0 {
            return Ok(());
        }
        let message = if error.is_null() {
            "invalid filter".into()
        } else {
            unsafe { std::ffi::CStr::from_ptr(error) }.to_string_lossy()
        };
        Err(io::Error::other(format!(
            "packet filter at byte {position}: {message}"
        )))
    }
}

pub struct Handle {
    api: Arc<Api>,
    raw: *mut c_void,
}

// WinDivert handles may be used from any thread.
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

impl Handle {
    pub fn open(api: &Arc<Api>, filter: &str) -> io::Result<Self> {
        api.validate_filter(filter)?;
        let filter = CString::new(filter).map_err(io::Error::other)?;
        let raw = unsafe { (api.open)(filter.as_ptr(), LAYER_NETWORK, 0, 0) };
        if raw.is_null() || raw as isize == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            api: api.clone(),
            raw,
        })
    }

    pub fn recv(&self, buf: &mut [u8]) -> io::Result<(usize, Address)> {
        let mut len = 0u32;
        let mut addr = unsafe { std::mem::zeroed::<Address>() };
        let ok = unsafe {
            (self.api.recv)(
                self.raw,
                buf.as_mut_ptr().cast(),
                buf.len() as u32,
                &mut len,
                &mut addr,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((len as usize, addr))
    }

    /// Recomputes every checksum, then injects the packet.
    pub fn send(&self, pkt: &mut [u8], addr: &mut Address) -> io::Result<()> {
        let len = pkt.len() as u32;
        let ok = unsafe {
            (self.api.calc)(pkt.as_mut_ptr().cast(), len, addr, 0);
            (self.api.send)(
                self.raw,
                pkt.as_ptr().cast(),
                len,
                std::ptr::null_mut(),
                addr,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Makes a blocked `recv` return `ERROR_NO_DATA`.
    pub fn shutdown(&self) {
        unsafe { (self.api.shutdown)(self.raw, SHUTDOWN_BOTH) };
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { (self.api.close)(self.raw) };
    }
}

/// Explains the usual reasons `WinDivertOpen` fails.
pub fn explain_open_error(e: &io::Error) -> String {
    let hint = match e.raw_os_error() {
        Some(5) => "run as Administrator",
        Some(2 | 3) => "WinDivert64.sys must sit next to WinDivert.dll",
        Some(87) => "the packet filter was rejected by WinDivert",
        Some(577) => "Windows refused the WinDivert driver signature",
        Some(1275) => "the driver was blocked (security software or Secure Boot policy)",
        Some(1060) => "the WinDivert driver service is missing",
        Some(1058) => "the WinDivert driver service is disabled",
        _ => return format!("cannot open WinDivert: {e}"),
    };
    format!("cannot open WinDivert: {e} ({hint})")
}
