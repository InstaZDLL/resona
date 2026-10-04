//! The Windows audio settings that decide whether a capture can be
//! bit-transparent: the output device's shared-mode format and the volume
//! of Spotify's audio sessions.

use std::collections::HashSet;

use wasapi::{DeviceEnumerator, Direction};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::Media::Audio::ENDPOINT_SYSFX_DISABLED;
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{
    IAudioSessionControl2, IAudioSessionManager2, IMMDevice, IMMDeviceEnumerator,
    ISimpleAudioVolume, MMDeviceEnumerator,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
use windows::Win32::System::Registry::{
    HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW,
};
use windows::core::{HSTRING, Interface};

use crate::Result;
use crate::format::{CAPTURE_CHANNELS, CAPTURE_SAMPLE_RATE};

#[derive(Debug, Clone)]
pub struct OutputDevice {
    /// Endpoint id (`{0.0.0.00000000}.{guid}`).
    pub id: String,
    pub name: String,
    pub sample_rate: u32,
    pub bits_per_sample: u16,
    /// More than 2 (a virtual surround device) means Spotify renders a
    /// multichannel stream that Windows downmixes for our stereo capture.
    pub channels: u16,
    /// Speaker positions of those channels (`WAVEFORMATEXTENSIBLE`).
    pub channel_mask: u32,
    /// Master volume of the device (the Windows volume slider), in dB.
    pub volume_db: f32,
    /// Same, as the 0.0 to 1.0 slider position.
    pub volume_scalar: f32,
    pub muted: bool,
    /// An effect is installed and not disabled in the registry, so Windows
    /// "Audio enhancements" *may* process the stream before our capture.
    /// A hint, not a verdict: the registry and the Settings switch have
    /// been seen to disagree (a headset utility rewriting the effect
    /// properties), and an installed effect may be inactive. The per-track
    /// [`crate::analysis::Fidelity`] is what tells. `None` if unreadable.
    pub enhancements: Option<bool>,
}

/// A Windows setting that keeps captures from being lossless.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupIssue {
    /// "Audio enhancements" process the stream before the capture.
    Enhancements,
    /// The device mixes at another rate than Spotify's 44.1 kHz: the
    /// stream is resampled before the capture (measured at 48 kHz).
    SampleRate(u32),
}

impl OutputDevice {
    /// What to change in Windows for lossless captures; empty when the
    /// device is set up right.
    pub fn lossless_issues(&self) -> Vec<SetupIssue> {
        let mut issues = Vec::new();
        if self.enhancements == Some(true) {
            issues.push(SetupIssue::Enhancements);
        }
        if self.resamples() {
            issues.push(SetupIssue::SampleRate(self.sample_rate));
        }
        issues
    }

    /// Spotify streams at 44.1 kHz; a device mixing at another rate means
    /// a resampler somewhere before our capture.
    pub fn resamples(&self) -> bool {
        self.sample_rate != CAPTURE_SAMPLE_RATE
    }

    pub fn downmixes(&self) -> bool {
        self.channels != CAPTURE_CHANNELS
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SessionVolume {
    pub pid: u32,
    /// 0.0 to 1.0, the slider in the Windows volume mixer.
    pub volume: f32,
    pub muted: bool,
}

/// Default render device and its shared-mode mix format. Needs COM (MTA).
pub fn default_output_device() -> Result<OutputDevice> {
    let device = DeviceEnumerator::new()?.get_default_device(&Direction::Render)?;
    output_device(&device.get_id()?)
}

/// The device Spotify (root process `pid`) plays on: the one chosen for it
/// in the Windows volume mixer, else the default device. Needs COM (MTA).
pub fn spotify_output_device(pid: u32) -> Result<OutputDevice> {
    let routed = crate::routing::spotify_endpoint(pid).unwrap_or_else(|e| {
        tracing::warn!("cannot read Spotify's output device: {e}");
        None
    });
    match routed {
        // A device chosen once and unplugged since: Windows falls back to
        // the default one.
        Some(id) => output_device(&id).or_else(|_| default_output_device()),
        None => default_output_device(),
    }
}

/// Active render devices, as `(id, name)`. Needs COM (MTA).
pub fn render_devices() -> Result<Vec<(String, String)>> {
    let devices = DeviceEnumerator::new()?.get_device_collection(&Direction::Render)?;
    let mut list = Vec::new();
    for index in 0..devices.get_nbr_devices()? {
        let device = devices.get_device_at_index(index)?;
        list.push((device.get_id()?, device.get_friendlyname()?));
    }
    Ok(list)
}

/// The playback side of an installed virtual audio cable (VB-Audio's
/// "CABLE Input"), as `(id, name)`. Needs COM (MTA).
pub fn virtual_cable() -> Result<Option<(String, String)>> {
    Ok(render_devices()?
        .into_iter()
        .find(|(_, name)| is_virtual_cable(name)))
}

fn is_virtual_cable(name: &str) -> bool {
    let name = name.to_lowercase();
    name.contains("cable input") || name.contains("virtual audio cable")
}

/// Devices nobody listens on: virtual cables (VB-Audio installs several)
/// and Steam's streaming endpoints.
fn is_virtual(name: &str) -> bool {
    let name = name.to_lowercase();
    is_virtual_cable(&name) || name.contains("vb-audio") || name.contains("steam streaming")
}

/// The Windows default output device is a virtual cable, so nothing is
/// heard (VB-Cable's installer does that).
#[derive(Debug, Clone)]
pub struct CableAsDefault {
    /// A real device to go back to, as `(id, name)`.
    pub replacement: Option<(String, String)>,
}

/// `Some` when the Windows default output device is a virtual cable.
/// Needs COM (MTA).
pub fn cable_as_default() -> Result<Option<CableAsDefault>> {
    let default = DeviceEnumerator::new()?.get_default_device(&Direction::Render)?;
    if !is_virtual(&default.get_friendlyname()?) {
        return Ok(None);
    }
    let replacement = render_devices()?
        .into_iter()
        .find(|(_, name)| !is_virtual(name));
    Ok(Some(CableAsDefault { replacement }))
}

/// The render device `id` and its shared-mode mix format. Needs COM (MTA).
pub fn output_device(id: &str) -> Result<OutputDevice> {
    let device = DeviceEnumerator::new()?.get_device(id)?;
    let format = device.get_iaudioclient()?.get_mixformat()?;
    let (volume_db, volume_scalar, muted) = unsafe {
        let endpoint: IAudioEndpointVolume = immdevice(id)?.Activate(CLSCTX_ALL, None)?;
        (
            endpoint.GetMasterVolumeLevel()?,
            endpoint.GetMasterVolumeLevelScalar()?,
            endpoint.GetMute()?.as_bool(),
        )
    };
    Ok(OutputDevice {
        id: id.to_owned(),
        name: device.get_friendlyname()?,
        sample_rate: format.get_samplespersec(),
        bits_per_sample: format.get_validbitspersample(),
        channels: format.get_nchannels(),
        channel_mask: format.get_dwchannelmask(),
        volume_db,
        volume_scalar,
        muted,
        enhancements: enhancements_active(id),
    })
}

fn immdevice(id: &str) -> windows::core::Result<IMMDevice> {
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        enumerator.GetDevice(&HSTRING::from(id))
    }
}

/// Whether Windows "Audio enhancements" process this device's streams.
///
/// Read from the device's effect properties in the registry
/// (`MMDevices\Audio\Render\{id}\FxProperties`): the property store
/// `IMMDevice::OpenPropertyStore` hands out does not include them (it
/// returned nothing while the registry held the switch, measured
/// 2026-10-01). Active means an effect is installed (`PKEY_FX_*Clsid`,
/// for instance a headset's virtual surround) and the switch is not Off
/// (`PKEY_AudioEndpoint_Disable_SysFx` ≠ 1). `None` if the id has no GUID.
fn enhancements_active(device_id: &str) -> Option<bool> {
    const FX_CLSID: &str = "{d04e05a6-594b-4fb6-a80d-01af5eed7d1d}";
    /// Pre-mix, post-mix, stream, mode and endpoint effects.
    const FX_SLOTS: [u32; 5] = [1, 2, 5, 6, 7];
    const DISABLE_SYSFX: &str = "{1da5d803-d492-4edd-8c23-e0c0ffee7f0e},5";

    let guid = device_id.rsplit('.').next()?;
    let key = HSTRING::from(format!(
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render\{guid}\FxProperties"
    ));
    let read_dword = |name: &str| {
        let mut value = 0_u32;
        let mut size = size_of::<u32>() as u32;
        let status = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                &key,
                &HSTRING::from(name),
                RRF_RT_REG_DWORD,
                None,
                Some((&raw mut value).cast()),
                Some(&raw mut size),
            )
        };
        (status == ERROR_SUCCESS).then_some(value)
    };
    let has_value = |name: &str| {
        let mut size = 0_u32;
        let status = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                &key,
                &HSTRING::from(name),
                RRF_RT_REG_SZ,
                None,
                None,
                Some(&raw mut size),
            )
        };
        status == ERROR_SUCCESS && size > 2
    };

    let installed = FX_SLOTS
        .iter()
        .any(|slot| has_value(&format!("{FX_CLSID},{slot}")));
    let disabled = read_dword(DISABLE_SYSFX) == Some(ENDPOINT_SYSFX_DISABLED);
    Some(installed && !disabled)
}

/// Calls `visit` with the volume control of every audio session owned by
/// `pids`, on every render device (Spotify may be routed to any). Needs
/// COM (MTA).
fn for_each_session(
    pids: &HashSet<u32>,
    mut visit: impl FnMut(u32, &ISimpleAudioVolume) -> windows::core::Result<()>,
) -> Result<()> {
    for (id, _) in render_devices()? {
        visit_sessions(&id, pids, &mut visit)?;
    }
    Ok(())
}

fn visit_sessions(
    device_id: &str,
    pids: &HashSet<u32>,
    visit: &mut impl FnMut(u32, &ISimpleAudioVolume) -> windows::core::Result<()>,
) -> Result<()> {
    unsafe {
        let manager: IAudioSessionManager2 = immdevice(device_id)?.Activate(CLSCTX_ALL, None)?;
        let sessions = manager.GetSessionEnumerator()?;
        for index in 0..sessions.GetCount()? {
            let control = sessions.GetSession(index)?;
            // Cross-process sessions have no single PID; they are not Spotify's.
            let Ok(pid) = control
                .cast::<IAudioSessionControl2>()
                .and_then(|c| c.GetProcessId())
            else {
                continue;
            };
            if pids.contains(&pid) {
                visit(pid, &control.cast::<ISimpleAudioVolume>()?)?;
            }
        }
    }
    Ok(())
}

/// Volume and mute state of Spotify's audio sessions.
pub fn session_volumes(pids: &HashSet<u32>) -> Result<Vec<SessionVolume>> {
    let mut volumes = Vec::new();
    for_each_session(pids, |pid, volume| {
        volumes.push(SessionVolume {
            pid,
            volume: unsafe { volume.GetMasterVolume()? },
            muted: unsafe { volume.GetMute()? }.as_bool(),
        });
        Ok(())
    })?;
    Ok(volumes)
}

/// Mutes or unmutes Spotify in the Windows mixer, as during ads. The
/// recording does not depend on it: ads are not recorded either way.
pub fn set_spotify_muted(pids: &HashSet<u32>, muted: bool) -> Result<()> {
    for_each_session(pids, |_, volume| unsafe {
        volume.SetMute(muted, std::ptr::null())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_vb_cable() {
        assert!(is_virtual_cable("CABLE Input (VB-Audio Virtual Cable)"));
        assert!(!is_virtual_cable(
            "Haut-parleurs (PRO X Wireless Gaming Headset)"
        ));
        assert!(!is_virtual_cable("CABLE In 16ch (VB-Audio Virtual Cable)"));
    }

    #[test]
    fn tells_virtual_devices_from_real_ones() {
        assert!(is_virtual("CABLE In 16ch (VB-Audio Virtual Cable)"));
        assert!(is_virtual("Speakers (Steam Streaming Speakers)"));
        assert!(!is_virtual(
            "Speakers (Logitech PRO X Wireless Gaming Headset)"
        ));
    }
}
