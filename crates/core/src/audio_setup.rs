//! The Windows audio settings that decide whether a capture can be
//! bit-transparent: the output device's shared-mode format and the volume
//! of Spotify's audio sessions.

use std::collections::HashSet;

use wasapi::{DeviceEnumerator, Direction};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::Media::Audio::ENDPOINT_SYSFX_DISABLED;
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{
    IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator, ISimpleAudioVolume,
    MMDeviceEnumerator, eConsole, eRender,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree};
use windows::Win32::System::Registry::{
    HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW,
};
use windows::core::{HSTRING, Interface};

use crate::Result;
use crate::format::{CAPTURE_CHANNELS, CAPTURE_SAMPLE_RATE};

#[derive(Debug, Clone)]
pub struct OutputDevice {
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
    /// Windows "Audio enhancements" are on and an effect is installed: it
    /// processes the stream before our capture, so nothing captured is
    /// lossless. `None` if the device's effect settings could not be read.
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
    let format = device.get_iaudioclient()?.get_mixformat()?;
    let (volume_db, volume_scalar, muted, enhancements) = unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let endpoint_device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let endpoint: IAudioEndpointVolume = endpoint_device.Activate(CLSCTX_ALL, None)?;
        (
            endpoint.GetMasterVolumeLevel()?,
            endpoint.GetMasterVolumeLevelScalar()?,
            endpoint.GetMute()?.as_bool(),
            {
                let id = endpoint_device.GetId()?;
                let text = id.to_string();
                CoTaskMemFree(Some(id.0.cast()));
                text.ok().and_then(|id| enhancements_active(&id))
            },
        )
    };
    Ok(OutputDevice {
        name: device.get_friendlyname()?,
        sample_rate: format.get_samplespersec(),
        bits_per_sample: format.get_validbitspersample(),
        channels: format.get_nchannels(),
        channel_mask: format.get_dwchannelmask(),
        volume_db,
        volume_scalar,
        muted,
        enhancements,
    })
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

/// Volume and mute state of every audio session owned by `pids` on the
/// default render device. Needs COM (MTA).
pub fn session_volumes(pids: &HashSet<u32>) -> Result<Vec<SessionVolume>> {
    let mut volumes = Vec::new();
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let manager: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None)?;
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
            if !pids.contains(&pid) {
                continue;
            }
            let volume = control.cast::<ISimpleAudioVolume>()?;
            volumes.push(SessionVolume {
                pid,
                volume: volume.GetMasterVolume()?,
                muted: volume.GetMute()?.as_bool(),
            });
        }
    }
    Ok(volumes)
}
