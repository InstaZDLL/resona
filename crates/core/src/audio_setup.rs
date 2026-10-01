//! The Windows audio settings that decide whether a capture can be
//! bit-transparent: the output device's shared-mode format and the volume
//! of Spotify's audio sessions.

use std::collections::HashSet;

use wasapi::{DeviceEnumerator, Direction};
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{
    IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator, ISimpleAudioVolume,
    MMDeviceEnumerator, eConsole, eRender,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
use windows::core::Interface;

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
}

impl OutputDevice {
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
    let (volume_db, volume_scalar, muted) = unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let endpoint: IAudioEndpointVolume = enumerator
            .GetDefaultAudioEndpoint(eRender, eConsole)?
            .Activate(CLSCTX_ALL, None)?;
        (
            endpoint.GetMasterVolumeLevel()?,
            endpoint.GetMasterVolumeLevelScalar()?,
            endpoint.GetMute()?.as_bool(),
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
    })
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
