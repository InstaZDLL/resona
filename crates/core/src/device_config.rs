//! Device-wide settings that the Sound control panel changes: an output
//! device's shared-mode format and the Windows default device.
//!
//! Through `IPolicyConfig`, the undocumented interface behind that panel
//! (stable since Windows 7, used by audio switchers such as SoundSwitch).
//! No administrator rights needed, as in the panel. Used to set the
//! virtual cable up so that nobody has to open the panel.

// The COM methods keep their Windows names.
#![allow(non_snake_case)]

use std::ffi::c_void;

use windows::Win32::Media::Audio::{ERole, WAVEFORMATEX, eConsole, eMultimedia};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree};
use windows::core::{GUID, HRESULT, HSTRING, PCWSTR};
use windows_core::{IUnknown, IUnknown_Vtbl};

use crate::Result;

const CLSID_POLICY_CONFIG: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);

#[windows_core::interface("f8679f50-850a-41cf-9c72-430f290290c8")]
unsafe trait IPolicyConfig: IUnknown {
    fn GetMixFormat(&self, device: PCWSTR, format: *mut *mut WAVEFORMATEX) -> HRESULT;
    /// `default` 0: the format in use, 1: the driver's default.
    fn GetDeviceFormat(
        &self,
        device: PCWSTR,
        default: i32,
        format: *mut *mut WAVEFORMATEX,
    ) -> HRESULT;
    fn ResetDeviceFormat(&self, device: PCWSTR) -> HRESULT;
    fn SetDeviceFormat(
        &self,
        device: PCWSTR,
        endpoint: *const WAVEFORMATEX,
        mix: *const WAVEFORMATEX,
    ) -> HRESULT;
    fn GetProcessingPeriod(
        &self,
        device: PCWSTR,
        default: i32,
        default_period: *mut i64,
        min_period: *mut i64,
    ) -> HRESULT;
    fn SetProcessingPeriod(&self, device: PCWSTR, period: *const i64) -> HRESULT;
    fn GetShareMode(&self, device: PCWSTR, mode: *mut c_void) -> HRESULT;
    fn SetShareMode(&self, device: PCWSTR, mode: *const c_void) -> HRESULT;
    fn GetPropertyValue(&self, device: PCWSTR, key: *const c_void, value: *mut c_void) -> HRESULT;
    fn SetPropertyValue(&self, device: PCWSTR, key: *const c_void, value: *const c_void)
    -> HRESULT;
    fn SetDefaultEndpoint(&self, device: PCWSTR, role: ERole) -> HRESULT;
    fn SetEndpointVisibility(&self, device: PCWSTR, visible: i32) -> HRESULT;
}

fn policy() -> Result<IPolicyConfig> {
    Ok(unsafe { CoCreateInstance(&CLSID_POLICY_CONFIG, None, CLSCTX_ALL)? })
}

/// Sets the shared-mode sample rate of the device `endpoint_id`, keeping
/// its channels and bit depth: what Sound → device → Advanced → Default
/// format does. Needs COM (MTA).
pub fn set_sample_rate(endpoint_id: &str, rate: u32) -> Result<()> {
    let policy = policy()?;
    let id = HSTRING::from(endpoint_id);
    let device = PCWSTR(id.as_ptr());
    unsafe {
        let mut endpoint = std::ptr::null_mut();
        policy.GetDeviceFormat(device, 0, &raw mut endpoint).ok()?;
        let endpoint = Format::take(endpoint);
        let mut mix = std::ptr::null_mut();
        policy.GetMixFormat(device, &raw mut mix).ok()?;
        let mix = Format::take(mix);
        let (endpoint, mix) = (endpoint.with_rate(rate), mix.with_rate(rate));
        policy
            .SetDeviceFormat(device, endpoint.as_ptr(), mix.as_ptr())
            .ok()?;
    }
    Ok(())
}

/// Makes `endpoint_id` the Windows default output device, for music and
/// system sounds (calls keep theirs). Needs COM (MTA).
pub fn set_default_device(endpoint_id: &str) -> Result<()> {
    let policy = policy()?;
    let id = HSTRING::from(endpoint_id);
    for role in [eConsole, eMultimedia] {
        unsafe { policy.SetDefaultEndpoint(PCWSTR(id.as_ptr()), role).ok()? };
    }
    Ok(())
}

/// A copy of a `WAVEFORMATEX` (or `WAVEFORMATEXTENSIBLE`, whose extra
/// bytes `cbSize` counts), aligned as the struct.
struct Format(Vec<u32>);

impl Format {
    /// Copies and frees a format COM allocated.
    unsafe fn take(format: *mut WAVEFORMATEX) -> Self {
        let size = size_of::<WAVEFORMATEX>() + usize::from(unsafe { (*format).cbSize });
        let mut words = vec![0_u32; size.div_ceil(4)];
        unsafe {
            std::ptr::copy_nonoverlapping(format.cast::<u8>(), words.as_mut_ptr().cast(), size);
            CoTaskMemFree(Some(format.cast()));
        }
        Self(words)
    }

    fn with_rate(mut self, rate: u32) -> Self {
        let header = self.0.as_mut_ptr().cast::<WAVEFORMATEX>();
        unsafe {
            (*header).nSamplesPerSec = rate;
            (*header).nAvgBytesPerSec = rate * u32::from((*header).nBlockAlign);
        }
        self
    }

    fn as_ptr(&self) -> *const WAVEFORMATEX {
        self.0.as_ptr().cast()
    }
}
