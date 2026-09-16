//! Cloaking through the shell's application views.
//!
//! `DWMWA_CLOAK` only works on windows of the calling process; any other
//! window answers `E_ACCESSDENIED`. The shell keeps an application view for
//! every top-level window it tracks and lets any process on the desktop cloak
//! a window through that view, which is how virtual desktops hide windows.
//! Tool windows, splash screens and a few others have no view, and the caller
//! falls back to another way of taking those off screen.
//!
//! The interfaces are not in the SDK. Their identifiers and the vtable order
//! of the leading methods are stable across Windows 10 and 11 and are widely
//! documented by virtual desktop tooling. Only the methods up to `SetCloak`
//! are declared here, and nothing past that slot is ever called.

// The method names have to match the vtable exactly, so they keep the Windows
// spelling rather than Rust's.
#![allow(non_snake_case)]

use std::cell::RefCell;
use std::ffi::c_void;

use anyhow::{Context, Result, anyhow};
use windows::Win32::Foundation::{HWND, RPC_E_CHANGED_MODE};
use windows::Win32::System::Com::{
    CLSCTX_LOCAL_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, IServiceProvider,
};
use windows::core::{GUID, HRESULT, IUnknown, IUnknown_Vtbl, Interface, PCWSTR, interface};

/// The shell object that hands out its internal services.
const CLSID_IMMERSIVE_SHELL: GUID = GUID::from_u128(0xC2F0_3A33_21F5_47FA_B4BB_1563_62A2_F239);

/// `TYPE_E_ELEMENTNOTFOUND`: the shell has no view for this window.
const ELEMENT_NOT_FOUND: HRESULT = HRESULT(0x8002_802B_u32 as i32);

/// Cloak as a plain hidden view rather than a virtual desktop move.
const CLOAK_TYPE_DEFAULT: i32 = 1;

#[interface("1841C6D7-4F9D-42C0-AF41-8747538F10E5")]
unsafe trait IApplicationViewCollection: IUnknown {
    fn GetViews(&self, views: *mut *mut c_void) -> HRESULT;
    fn GetViewsByZOrder(&self, views: *mut *mut c_void) -> HRESULT;
    fn GetViewsByAppUserModelId(&self, id: PCWSTR, views: *mut *mut c_void) -> HRESULT;
    fn GetViewForHwnd(&self, hwnd: HWND, view: *mut Option<IApplicationView>) -> HRESULT;
}

/// Declared on top of `IUnknown` with the three `IInspectable` slots spelled
/// out, because the interface macro cannot derive from `IInspectable`.
#[interface("372E1D3B-38D3-42E4-A15B-8AB2B178F513")]
unsafe trait IApplicationView: IUnknown {
    fn GetIids(&self, count: *mut u32, iids: *mut *mut GUID) -> HRESULT;
    fn GetRuntimeClassName(&self, name: *mut *mut c_void) -> HRESULT;
    fn GetTrustLevel(&self, level: *mut i32) -> HRESULT;
    fn SetFocus(&self) -> HRESULT;
    fn SwitchTo(&self) -> HRESULT;
    fn TryInvokeBack(&self, callback: *mut c_void) -> HRESULT;
    fn GetThumbnailWindow(&self, hwnd: *mut HWND) -> HRESULT;
    fn GetMonitor(&self, monitor: *mut *mut c_void) -> HRESULT;
    fn GetVisibility(&self, visibility: *mut i32) -> HRESULT;
    fn SetCloak(&self, cloak_type: i32, flags: i32) -> HRESULT;
}

thread_local! {
    /// One proxy per thread. COM proxies are apartment bound and the window
    /// manager thread is the only caller, so a thread local is the simplest
    /// correct cache.
    static COLLECTION: RefCell<Option<IApplicationViewCollection>> = const { RefCell::new(None) };
}

/// Why the shell could not cloak a window.
#[derive(Debug)]
pub enum ViewCloakError {
    /// The shell does not track this window, so this route cannot cloak it.
    NoView,
    /// The shell is unreachable or refused. The proxy is dropped so the next
    /// call reconnects, which covers a restart of the shell.
    Shell(anyhow::Error),
}

impl std::fmt::Display for ViewCloakError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoView => f.write_str("the shell has no application view for this window"),
            Self::Shell(e) => write!(f, "the shell could not cloak the window: {e:#}"),
        }
    }
}

impl std::error::Error for ViewCloakError {}

fn connect() -> Result<IApplicationViewCollection> {
    // COM has to be initialised once per thread. `RPC_E_CHANGED_MODE` means the
    // thread already runs in another apartment model, which is fine for
    // outgoing calls to an out of process server.
    let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    if hr.is_err() && hr != RPC_E_CHANGED_MODE {
        return Err(anyhow!("CoInitializeEx failed: {hr}"));
    }
    let shell: IServiceProvider =
        unsafe { CoCreateInstance(&CLSID_IMMERSIVE_SHELL, None, CLSCTX_LOCAL_SERVER) }
            .context("the shell's service provider is not available")?;
    unsafe { shell.QueryService::<IApplicationViewCollection>(&IApplicationViewCollection::IID) }
        .context("the shell does not offer the application view collection")
}

fn with_collection<T>(
    f: impl FnOnce(&IApplicationViewCollection) -> Result<T, ViewCloakError>,
) -> Result<T, ViewCloakError> {
    COLLECTION.with(|slot| {
        let collection = match slot.borrow().as_ref() {
            Some(c) => c.clone(),
            None => connect().map_err(ViewCloakError::Shell)?,
        };
        let result = f(&collection);
        match &result {
            Err(ViewCloakError::Shell(_)) => *slot.borrow_mut() = None,
            _ => *slot.borrow_mut() = Some(collection),
        }
        result
    })
}

/// Cloaks or uncloaks a window through its application view.
///
/// Works for every window the shell tracks, whichever process owns it.
pub fn set_cloak(hwnd: HWND, cloaked: bool) -> Result<(), ViewCloakError> {
    with_collection(|collection| {
        let mut view: Option<IApplicationView> = None;
        let hr = unsafe { collection.GetViewForHwnd(hwnd, &raw mut view) };
        if hr == ELEMENT_NOT_FOUND {
            return Err(ViewCloakError::NoView);
        }
        let view = match (hr.is_ok(), view) {
            (true, Some(view)) => view,
            (true, None) => return Err(ViewCloakError::NoView),
            (false, _) => {
                return Err(ViewCloakError::Shell(anyhow!(
                    "GetViewForHwnd failed: {hr}"
                )));
            }
        };
        let hr = unsafe { view.SetCloak(CLOAK_TYPE_DEFAULT, i32::from(cloaked)) };
        if hr.is_ok() {
            Ok(())
        } else {
            Err(ViewCloakError::Shell(anyhow!(
                "SetCloak({cloaked}) failed: {hr}"
            )))
        }
    })
}
