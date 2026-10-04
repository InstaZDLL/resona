//! Which output device Spotify plays on, per application, as set in
//! Windows Settings → Volume mixer → Spotify → Output device.
//!
//! Through `Windows.Media.Internal.AudioPolicyConfig`, the undocumented
//! interface that Settings page uses (the C# version's `AudioRouter`).
//! Windows stores the choice per executable, so it outlives Spotify's
//! process and ours.

// The COM methods keep their Windows names.
#![allow(non_snake_case)]

use std::ffi::c_void;

use windows::Win32::Media::Audio::{EDataFlow, ERole, eConsole, eMultimedia, eRender};
use windows::Win32::System::WinRT::RoGetActivationFactory;
use windows::core::{HRESULT, HSTRING, h};
use windows_core::{IUnknown, IUnknown_Vtbl};

use crate::Result;

/// Wraps an endpoint id (`{0.0.0.00000000}.{guid}`) the way the policy
/// interface wants it.
const MMDEVAPI_PREFIX: &str = r"\\?\SWD#MMDEVAPI#";
const RENDER_INTERFACE: &str = "#{e6327cad-dcec-4949-ae8a-991e976a79d2}";

macro_rules! policy_config {
    ($name:ident, $iid:literal) => {
        #[windows_core::interface($iid)]
        unsafe trait $name: IUnknown {
            // IInspectable, declared by hand: the macro only derives from
            // IUnknown.
            fn GetIids(&self) -> HRESULT;
            fn GetRuntimeClassName(&self) -> HRESULT;
            fn GetTrustLevel(&self) -> HRESULT;
            // 19 methods we do not use (volume groups, ringer, chat apps).
            fn _0(&self) -> HRESULT;
            fn _1(&self) -> HRESULT;
            fn _2(&self) -> HRESULT;
            fn _3(&self) -> HRESULT;
            fn _4(&self) -> HRESULT;
            fn _5(&self) -> HRESULT;
            fn _6(&self) -> HRESULT;
            fn _7(&self) -> HRESULT;
            fn _8(&self) -> HRESULT;
            fn _9(&self) -> HRESULT;
            fn _10(&self) -> HRESULT;
            fn _11(&self) -> HRESULT;
            fn _12(&self) -> HRESULT;
            fn _13(&self) -> HRESULT;
            fn _14(&self) -> HRESULT;
            fn _15(&self) -> HRESULT;
            fn _16(&self) -> HRESULT;
            fn _17(&self) -> HRESULT;
            fn _18(&self) -> HRESULT;
            /// A null `device` goes back to the default device.
            fn SetPersistedDefaultAudioEndpoint(
                &self,
                pid: u32,
                flow: EDataFlow,
                role: ERole,
                device: *mut c_void,
            ) -> HRESULT;
            fn GetPersistedDefaultAudioEndpoint(
                &self,
                pid: u32,
                flow: EDataFlow,
                role: ERole,
                device: *mut HSTRING,
            ) -> HRESULT;
            fn ClearAllPersistedApplicationDefaultEndpoints(&self) -> HRESULT;
        }
    };
}

// Same methods, another IID before Windows 11 (build 21390).
policy_config!(IAudioPolicyConfig, "ab3d4648-e242-459f-b02f-541c70306324");
policy_config!(
    IAudioPolicyConfigDownlevel,
    "2a59116d-6c4f-45e0-a74f-707e3fef9258"
);

enum Policy {
    Current(IAudioPolicyConfig),
    Downlevel(IAudioPolicyConfigDownlevel),
}

impl Policy {
    /// Needs COM (MTA).
    fn get() -> Result<Self> {
        let class = h!("Windows.Media.Internal.AudioPolicyConfig");
        unsafe {
            match RoGetActivationFactory::<IAudioPolicyConfig>(class) {
                Ok(policy) => Ok(Self::Current(policy)),
                Err(_) => Ok(Self::Downlevel(RoGetActivationFactory(class)?)),
            }
        }
    }

    fn set(&self, pid: u32, role: ERole, device: *mut c_void) -> HRESULT {
        unsafe {
            match self {
                Self::Current(p) => p.SetPersistedDefaultAudioEndpoint(pid, eRender, role, device),
                Self::Downlevel(p) => {
                    p.SetPersistedDefaultAudioEndpoint(pid, eRender, role, device)
                }
            }
        }
    }

    fn get_endpoint(&self, pid: u32) -> Result<HSTRING> {
        let mut device = HSTRING::new();
        unsafe {
            match self {
                Self::Current(p) => {
                    p.GetPersistedDefaultAudioEndpoint(pid, eRender, eMultimedia, &raw mut device)
                }
                Self::Downlevel(p) => {
                    p.GetPersistedDefaultAudioEndpoint(pid, eRender, eMultimedia, &raw mut device)
                }
            }
        }
        .ok()?;
        Ok(device)
    }
}

/// The output device chosen for `pid`'s application, as an endpoint id
/// ([`crate::audio_setup::OutputDevice::id`]); `None` when it follows the
/// Windows default device. Needs COM (MTA).
pub fn spotify_endpoint(pid: u32) -> Result<Option<String>> {
    let device = Policy::get()?.get_endpoint(pid)?.to_string();
    Ok(unpack(&device))
}

/// Sends `pid`'s application to the device `endpoint_id`, or back to the
/// default device with `None`. Spotify's open streams move over. Needs
/// COM (MTA).
pub fn route_spotify(pid: u32, endpoint_id: Option<&str>) -> Result<()> {
    let policy = Policy::get()?;
    let device = endpoint_id.map(|id| HSTRING::from(pack(id)));
    let raw = device.as_ref().map_or(std::ptr::null_mut(), |d| unsafe {
        std::mem::transmute_copy(d)
    });
    for role in [eMultimedia, eConsole] {
        policy.set(pid, role, raw).ok()?;
    }
    Ok(())
}

fn pack(endpoint_id: &str) -> String {
    format!("{MMDEVAPI_PREFIX}{endpoint_id}{RENDER_INTERFACE}")
}

fn unpack(device: &str) -> Option<String> {
    let id = device.strip_prefix(MMDEVAPI_PREFIX).unwrap_or(device);
    let id = id.strip_suffix(RENDER_INTERFACE).unwrap_or(id);
    (!id.is_empty()).then(|| id.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_ids_round_trip() {
        let id = "{0.0.0.00000000}.{8b5c2b5e-2c2e-4d4e-9c1a-1f0e2d3c4b5a}";
        assert_eq!(
            pack(id),
            r"\\?\SWD#MMDEVAPI#{0.0.0.00000000}.{8b5c2b5e-2c2e-4d4e-9c1a-1f0e2d3c4b5a}#{e6327cad-dcec-4949-ae8a-991e976a79d2}"
        );
        assert_eq!(unpack(&pack(id)).as_deref(), Some(id));
        assert_eq!(unpack(""), None);
    }
}
