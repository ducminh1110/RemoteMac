//! Sound from the Mac on this PC. Packets (UDP or the Audio channel) go into a jitter buffer
//! (`rm_protocol::audio::JitterBuffer`); on Windows a render thread plays from it through
//! WASAPI in shared mode on the default output device, follows a change of that device, and
//! starts over on its own when the device goes away (headphones unplugged).
//!
//! Volume is applied here (the slider; heard as even steps). Mute also tells the Mac to stop
//! sending, so a muted session costs no bandwidth.

use rm_protocol::audio::{AudioPacket, AudioStats, JitterBuffer};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// What the output device is doing, for the stats overlay and the settings.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Device {
    #[default]
    Idle,
    Playing(String),
    /// no output device, or it failed (retried every second)
    Failed(String),
}

pub struct Audio {
    jb: Mutex<JitterBuffer>,
    epoch: Instant,
    device: Mutex<Device>,
    /// what the Mac said about its side ("playing", "stopped", "unavailable: …")
    mac: Mutex<Option<String>>,
    /// the Mac can send sound (both sides have the "audio" feature)
    supported: AtomicBool,
    player: AtomicBool,
}

static AUDIO: OnceLock<Audio> = OnceLock::new();

pub fn audio() -> &'static Audio {
    AUDIO.get_or_init(|| {
        let s = crate::settings::Settings::load();
        let mut jb = JitterBuffer::new();
        jb.set_volume(s.volume as f32 / 100.0);
        jb.set_muted(!s.audio);
        Audio { jb: Mutex::new(jb), epoch: Instant::now(), device: Mutex::new(Device::Idle), mac: Mutex::new(None), supported: AtomicBool::new(false), player: AtomicBool::new(false) }
    })
}

impl Audio {
    /// A packet arrived from the Mac.
    pub fn push(&'static self, p: &AudioPacket) {
        self.jb.lock().unwrap().push(p, self.epoch.elapsed().as_micros() as u64);
        self.ensure_player();
    }

    /// The next `out.len() / 2` stereo frames to play.
    pub fn pull(&self, out: &mut [i16]) {
        self.jb.lock().unwrap().pull(out);
    }

    /// A new connection: nothing old is played.
    pub fn reset(&self) {
        self.jb.lock().unwrap().reset();
        *self.mac.lock().unwrap() = None;
    }

    pub fn set_supported(&self, s: bool) {
        self.supported.store(s, Ordering::SeqCst);
    }

    pub fn supported(&self) -> bool {
        self.supported.load(Ordering::SeqCst)
    }

    pub fn set_volume(&self, v: f32) {
        self.jb.lock().unwrap().set_volume(v);
    }

    pub fn set_muted(&self, m: bool) {
        self.jb.lock().unwrap().set_muted(m);
    }

    pub fn muted(&self) -> bool {
        self.jb.lock().unwrap().muted()
    }

    pub fn stats(&self) -> AudioStats {
        self.jb.lock().unwrap().stats.clone()
    }

    pub fn device(&self) -> Device {
        self.device.lock().unwrap().clone()
    }

    pub fn set_mac_status(&self, state: &str, reason: Option<&str>) {
        let s = match reason {
            Some(r) => format!("{state}: {r}"),
            None => state.to_string(),
        };
        eprintln!("sound on the Mac: {s}");
        *self.mac.lock().unwrap() = Some(s);
    }

    /// One line for the stats overlay.
    pub fn summary(&self) -> String {
        if !self.supported() {
            return "sound: not offered by this Mac".into();
        }
        if self.muted() {
            return "sound: muted".into();
        }
        let st = self.stats();
        let dev = match self.device() {
            Device::Idle => "no output yet".to_string(),
            Device::Playing(_) => "playing".to_string(),
            Device::Failed(e) => format!("output failed ({e})"),
        };
        let mac = self.mac.lock().unwrap().clone().unwrap_or_else(|| "asked".into());
        format!(
            "sound: {dev}; Mac {mac}; buffer {:.0}/{:.0} ms, jitter {:.1} ms; lost {} late {} dry {} trimmed {}",
            st.buffered_ms, st.target_ms, st.jitter_ms, st.lost, st.late, st.underruns, st.trimmed
        )
    }

    fn ensure_player(&'static self) {
        let started = self.player.swap(true, Ordering::SeqCst);
        #[cfg(windows)]
        if !started {
            let _ = std::thread::Builder::new().name("rm-audio".into()).spawn(move || wasapi::run(self));
        }
        #[cfg(not(windows))]
        let _ = started;
    }
}

/// Mute or unmute: the buffer, the setting, and the Mac (it stops sending while muted).
pub fn set_muted(link: Option<&crate::net::Link>, muted: bool) {
    audio().set_muted(muted);
    let mut s = crate::settings::Settings::load();
    s.audio = !muted;
    let _ = s.save();
    if let (Some(l), true) = (link, audio().supported()) {
        l.send(&rm_protocol::Message::AudioControl { enabled: !muted });
    }
}

#[cfg(windows)]
mod wasapi {
    use super::{Audio, Device};
    use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
    use windows::Win32::Media::Audio::*;
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED};
    use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

    /// Play until the process ends; a failed or changed device is opened again.
    pub fn run(a: &'static Audio) {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let mut last_error = String::new();
        loop {
            match play(a) {
                Ok(()) => last_error.clear(),
                Err(e) => {
                    let e = e.to_string();
                    if e != last_error {
                        eprintln!("sound output: {e}; trying again");
                        last_error = e.clone();
                    }
                    *a.device.lock().unwrap() = Device::Failed(e);
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
            }
        }
    }

    fn default_id(en: &IMMDeviceEnumerator) -> Option<String> {
        unsafe {
            let d = en.GetDefaultAudioEndpoint(eRender, eConsole).ok()?;
            let p = d.GetId().ok()?;
            let s = p.to_string().ok();
            CoTaskMemFree(Some(p.0 as *const _));
            s
        }
    }

    struct Event(HANDLE);
    impl Drop for Event {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    /// Open the default output and play from the buffer. Returns Ok when the default device
    /// changed (to be opened again), Err when the device failed.
    fn play(a: &Audio) -> windows::core::Result<()> {
        unsafe {
            let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let dev = en.GetDefaultAudioEndpoint(eRender, eConsole)?;
            let id = default_id(&en).unwrap_or_default();
            let client: IAudioClient = dev.Activate(CLSCTX_ALL, None)?;
            let ch = rm_protocol::audio::CHANNELS as u16;
            let wf = WAVEFORMATEX {
                wFormatTag: WAVE_FORMAT_PCM as u16,
                nChannels: ch,
                nSamplesPerSec: rm_protocol::audio::SAMPLE_RATE,
                nAvgBytesPerSec: rm_protocol::audio::SAMPLE_RATE * ch as u32 * 2,
                nBlockAlign: ch * 2,
                wBitsPerSample: 16,
                cbSize: 0,
            };
            // 20 ms of device buffer, refilled on each period's event; Windows converts the rate
            // and format to the device's own (AUTOCONVERTPCM)
            client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_EVENTCALLBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                200_000,
                0,
                &wf,
                None,
            )?;
            let ev = Event(CreateEventW(None, false, false, None)?);
            client.SetEventHandle(ev.0)?;
            let size = client.GetBufferSize()?;
            let render: IAudioRenderClient = client.GetService()?;
            client.Start()?;
            let latency_ms = client.GetStreamLatency().unwrap_or(0) / 10_000;
            eprintln!("sound output: {id} ({size} frames of buffer, {latency_ms} ms device latency)");
            *a.device.lock().unwrap() = Device::Playing(id.clone());
            let mut scratch = vec![0i16; size as usize * 2];
            let mut checked = std::time::Instant::now();
            let result = loop {
                if WaitForSingleObject(ev.0, 200) != WAIT_OBJECT_0 {
                    // no period event: the device may be gone; padding below tells
                }
                let padding = match client.GetCurrentPadding() {
                    Ok(p) => p,
                    Err(e) => break Err(e),
                };
                let free = size.saturating_sub(padding);
                if free > 0 {
                    let buf = match render.GetBuffer(free) {
                        Ok(b) => b,
                        Err(e) => break Err(e),
                    };
                    let out = &mut scratch[..free as usize * 2];
                    a.pull(out);
                    std::ptr::copy_nonoverlapping(out.as_ptr() as *const u8, buf, out.len() * 2);
                    if let Err(e) = render.ReleaseBuffer(free, 0) {
                        break Err(e);
                    }
                }
                // a new default device (headphones plugged in): move to it
                if checked.elapsed().as_secs() >= 2 {
                    checked = std::time::Instant::now();
                    if default_id(&en).is_some_and(|d| d != id) {
                        eprintln!("sound output: the default device changed");
                        break Ok(());
                    }
                }
            };
            let _ = client.Stop();
            result
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packets_play_through_the_shared_buffer() {
        let a = Audio { jb: Mutex::new(JitterBuffer::new()), epoch: Instant::now(), device: Mutex::new(Device::Idle), mac: Mutex::new(None), supported: AtomicBool::new(true), player: AtomicBool::new(true) };
        for s in 0..12u32 {
            a.jb.lock().unwrap().push(&AudioPacket { seq: s, pts_us: s as u64 * 5000, channels: 2, samples: vec![1000; 480] }, s as u64 * 5000);
        }
        let mut out = vec![0i16; 480];
        a.pull(&mut out);
        assert_eq!(out[0], 1000);
        assert!(a.summary().contains("buffer"), "{}", a.summary());
        a.set_muted(true);
        assert_eq!(a.summary(), "sound: muted");
    }
}
