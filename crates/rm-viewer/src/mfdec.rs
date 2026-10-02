//! H.264 decoding with Windows' own decoder (Media Foundation), as Moonlight uses the platform
//! decoder: on the GPU (DXVA) when the shared device is a real GPU, the picture then never
//! leaves video memory; otherwise Media Foundation's multithreaded software decoder. Both take
//! High profile, which the portable openh264 fallback cannot.

use crate::gpu::{Gpu, GpuPic, Ring};
use rm_decode::Picture;
use std::mem::ManuallyDrop;
use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};

/// A decoded picture: in video memory, or BGRA in system memory.
pub enum Decoded {
    Gpu(GpuPic),
    Cpu(Picture),
}

pub struct MfDecoder {
    mft: IMFTransform,
    gpu: Option<(&'static Gpu, IMFDXGIDeviceManager)>,
    ring: Ring,
    provides_samples: bool,
    out_size: u32,
    /// decoded (coded) size and row pitch of the output
    size: (u32, u32),
    stride: u32,
    time: i64,
}

// Media Foundation objects live on the decode thread that made them; the struct is moved there
// once, before use.
unsafe impl Send for MfDecoder {}

fn startup() -> windows::core::Result<()> {
    use std::sync::OnceLock;
    static R: OnceLock<bool> = OnceLock::new();
    let ok = *R.get_or_init(|| unsafe { MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET).is_ok() });
    if ok { Ok(()) } else { Err(windows::core::Error::from_hresult(windows::core::HRESULT(0x80004005u32 as i32))) }
}

impl MfDecoder {
    /// `gpu`: decode into the shared device's video memory (DXVA). Fails if Media Foundation or
    /// (with `gpu`) hardware decoding is not available.
    pub fn new(gpu: Option<&'static Gpu>) -> Result<Self, String> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            startup().map_err(|e| format!("MFStartup: {e}"))?;
            let mft: IMFTransform = CoCreateInstance(&CLSID_MSH264DecoderMFT, None, CLSCTX_INPROC_SERVER).map_err(|e| format!("H.264 decoder: {e}"))?;
            let attrs = mft.GetAttributes().ok();
            if let Some(a) = &attrs {
                // no frame reordering delay: output as soon as a frame is decoded
                let _ = a.SetUINT32(&MF_LOW_LATENCY, 1);
            }
            let gpu = match gpu {
                Some(g) => {
                    let aware = attrs.as_ref().and_then(|a| a.GetUINT32(&MF_SA_D3D11_AWARE).ok()).unwrap_or(0) != 0;
                    if !aware || !g.hardware {
                        return Err("no hardware video decoding".into());
                    }
                    let (mut token, mut mgr) = (0u32, None);
                    MFCreateDXGIDeviceManager(&mut token, &mut mgr).map_err(|e| format!("DXGI manager: {e}"))?;
                    let mgr = mgr.ok_or("DXGI manager")?;
                    mgr.ResetDevice(&g.device, token).map_err(|e| format!("DXGI manager device: {e}"))?;
                    mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, mgr.as_raw() as usize).map_err(|e| format!("D3D manager: {e}"))?;
                    Some((g, mgr))
                }
                None => None,
            };
            let t = MFCreateMediaType().map_err(|e| e.to_string())?;
            t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(|e| e.to_string())?;
            t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264).map_err(|e| e.to_string())?;
            t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).map_err(|e| e.to_string())?;
            mft.SetInputType(0, &t, 0).map_err(|e| format!("input type: {e}"))?;
            let mut d = Self { mft, gpu, ring: Ring::new(), provides_samples: false, out_size: 0, size: (0, 0), stride: 0, time: 0 };
            d.choose_output().map_err(|e| format!("output type: {e}"))?;
            let _ = d.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0);
            let _ = d.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0);
            Ok(d)
        }
    }

    pub fn on_gpu(&self) -> bool {
        self.gpu.is_some()
    }

    fn choose_output(&mut self) -> windows::core::Result<()> {
        unsafe {
            let mut i = 0;
            loop {
                let t = self.mft.GetOutputAvailableType(0, i)?;
                if t.GetGUID(&MF_MT_SUBTYPE)? == MFVideoFormat_NV12 {
                    self.mft.SetOutputType(0, &t, 0)?;
                    let fs = t.GetUINT64(&MF_MT_FRAME_SIZE).unwrap_or(0);
                    self.size = ((fs >> 32) as u32, fs as u32);
                    self.stride = t.GetUINT32(&MF_MT_DEFAULT_STRIDE).unwrap_or(self.size.0);
                    break;
                }
                i += 1;
            }
            let info = self.mft.GetOutputStreamInfo(0)?;
            self.provides_samples = info.dwFlags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0 as u32) != 0;
            self.out_size = info.cbSize;
            Ok(())
        }
    }

    /// Decode one access unit (Annex-B); `visible` is the picture size the agent sent (the coded
    /// size is rounded up to 16). Ok(None): no picture out yet.
    pub fn decode(&mut self, annexb: &[u8], visible: (u32, u32)) -> Result<Option<Decoded>, String> {
        unsafe {
            let buf = MFCreateMemoryBuffer(annexb.len() as u32).map_err(|e| e.to_string())?;
            let mut p = std::ptr::null_mut();
            buf.Lock(&mut p, None, None).map_err(|e| e.to_string())?;
            std::ptr::copy_nonoverlapping(annexb.as_ptr(), p, annexb.len());
            let _ = buf.Unlock();
            buf.SetCurrentLength(annexb.len() as u32).map_err(|e| e.to_string())?;
            let sample = MFCreateSample().map_err(|e| e.to_string())?;
            sample.AddBuffer(&buf).map_err(|e| e.to_string())?;
            self.time += 166_666; // 100 ns units, ~60 fps; only the order matters
            let _ = sample.SetSampleTime(self.time);
            let mut fed = self.mft.ProcessInput(0, &sample, 0).is_ok();
            let mut last = None;
            for _ in 0..8 {
                match self.output(visible) {
                    Ok(Some(d)) => last = Some(d),
                    Ok(None) => {
                        if fed {
                            break;
                        }
                        // the decoder was full: it has given its output, try the input again
                        fed = self.mft.ProcessInput(0, &sample, 0).is_ok();
                        if !fed {
                            return Err("decoder refuses input".into());
                        }
                    }
                    Err(e) => return Err(e),
                }
            }
            Ok(last)
        }
    }

    fn output(&mut self, visible: (u32, u32)) -> Result<Option<Decoded>, String> {
        unsafe {
            for _ in 0..4 {
                let own = if self.provides_samples {
                    None
                } else {
                    let s = MFCreateSample().map_err(|e| e.to_string())?;
                    let b = MFCreateMemoryBuffer(self.out_size.max(self.stride * self.size.1 * 3 / 2)).map_err(|e| e.to_string())?;
                    s.AddBuffer(&b).map_err(|e| e.to_string())?;
                    Some(s)
                };
                let mut out = [MFT_OUTPUT_DATA_BUFFER { dwStreamID: 0, pSample: ManuallyDrop::new(own), dwStatus: 0, pEvents: ManuallyDrop::new(None) }];
                let mut status = 0u32;
                let r = self.mft.ProcessOutput(0, &mut out, &mut status);
                let sample = ManuallyDrop::take(&mut out[0].pSample);
                drop(ManuallyDrop::take(&mut out[0].pEvents));
                match r {
                    Ok(()) => {
                        let Some(s) = sample else { return Ok(None) };
                        return self.convert(&s, visible).map(Some);
                    }
                    Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(None),
                    Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                        self.choose_output().map_err(|e| format!("new output type: {e}"))?;
                        continue;
                    }
                    Err(e) => return Err(format!("decode: {e}")),
                }
            }
            Err("decoder keeps changing its output format".into())
        }
    }

    fn convert(&mut self, s: &IMFSample, (vw, vh): (u32, u32)) -> Result<Decoded, String> {
        unsafe {
            let b = s.GetBufferByIndex(0).map_err(|e| e.to_string())?;
            let (cw, ch) = self.size;
            let (w, h) = (vw.clamp(1, cw.max(1)), vh.clamp(1, ch.max(1)));
            if let (Some(g), Ok(dx)) = (self.gpu.as_ref().map(|(g, _)| *g), b.cast::<IMFDXGIBuffer>()) {
                let mut raw = std::ptr::null_mut();
                dx.GetResource(&ID3D11Texture2D::IID, &mut raw).map_err(|e| e.to_string())?;
                let src = ID3D11Texture2D::from_raw(raw);
                let index = dx.GetSubresourceIndex().unwrap_or(0);
                let mut desc = D3D11_TEXTURE2D_DESC::default();
                src.GetDesc(&mut desc);
                let tex = self.ring.next(g, desc.Width, desc.Height).ok_or("texture")?;
                g.ctx.CopySubresourceRegion(&tex, 0, 0, 0, 0, &src, index, None);
                return Ok(Decoded::Gpu(GpuPic { tex, width: w.min(desc.Width), height: h.min(desc.Height) }));
            }
            // system memory NV12 -> BGRA
            let (mut p, mut pitch) = (std::ptr::null_mut::<u8>(), 0i32);
            let two_d = b.cast::<IMF2DBuffer>().ok();
            let locked2d = two_d.as_ref().is_some_and(|t| t.Lock2D(&mut p, &mut pitch).is_ok());
            if !locked2d {
                let mut len = 0u32;
                b.Lock(&mut p, None, Some(&mut len)).map_err(|e| e.to_string())?;
                pitch = self.stride as i32;
            }
            let pitch = pitch.unsigned_abs() as usize;
            let y = std::slice::from_raw_parts(p, pitch * ch as usize * 3 / 2);
            let bgra = nv12_to_bgra(y, pitch, ch as usize, w as usize, h as usize);
            if locked2d {
                let _ = two_d.unwrap().Unlock2D();
            } else {
                let _ = b.Unlock();
            }
            Ok(Decoded::Cpu(Picture { width: w as usize, height: h as usize, bgra }))
        }
    }
}

/// NV12 (Y plane, then interleaved UV at half resolution; `rows` coded rows) to BGRA, BT.709
/// limited range (what VideoToolbox produces for screen content).
pub fn nv12_to_bgra(src: &[u8], pitch: usize, rows: usize, w: usize, h: usize) -> Vec<u8> {
    let mut out = vec![0u8; w * h * 4];
    let uv = &src[pitch * rows..];
    for yy in 0..h {
        let yrow = &src[yy * pitch..yy * pitch + w];
        let uvrow = &uv[(yy / 2) * pitch..];
        let o = &mut out[yy * w * 4..(yy + 1) * w * 4];
        for x in 0..w {
            let yv = (yrow[x] as i32 - 16) * 298;
            let u = uvrow[x & !1] as i32 - 128;
            let v = uvrow[(x & !1) + 1] as i32 - 128;
            let r = (yv + 459 * v + 128) >> 8;
            let g = (yv - 55 * u - 136 * v + 128) >> 8;
            let b = (yv + 541 * u + 128) >> 8;
            o[x * 4] = b.clamp(0, 255) as u8;
            o[x * 4 + 1] = g.clamp(0, 255) as u8;
            o[x * 4 + 2] = r.clamp(0, 255) as u8;
            o[x * 4 + 3] = 255;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use openh264::encoder::Encoder;
    use openh264::formats::{RgbaSliceU8, YUVBuffer};

    /// Media Foundation (where this Windows has it) decodes the same stream as openh264, to
    /// nearly the same pixels (the two differ only in colour matrix rounding).
    #[test]
    fn media_foundation_matches_openh264() {
        let (w, h) = (320usize, 240usize);
        let mut enc = Encoder::new().unwrap();
        let mut frames = vec![];
        for t in 0..10 {
            let rgba: Vec<u8> = (0..w * h).flat_map(|i| [((i % w) * 255 / w) as u8, ((i / w) * 255 / h) as u8, (t * 20) as u8, 255]).collect();
            let yuv = YUVBuffer::from_rgb_source(RgbaSliceU8::new(&rgba, (w, h)));
            frames.push(enc.encode(&yuv).unwrap().to_vec());
        }
        let mut mf = match MfDecoder::new(None) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("skipped: Media Foundation unavailable here ({e})");
                return;
            }
        };
        let mut oh = rm_decode::H264Decoder::new().unwrap();
        let (mut compared, mut worst) = (0, 0.0f64);
        for f in &frames {
            let a = oh.decode(f).unwrap();
            let b = mf.decode(f, (w as u32, h as u32)).unwrap();
            if let (Some(a), Some(Decoded::Cpu(b))) = (a, b) {
                assert_eq!((b.width, b.height), (w, h));
                let diff = a.bgra.iter().zip(&b.bgra).map(|(x, y)| (*x as i32 - *y as i32).unsigned_abs() as f64).sum::<f64>() / a.bgra.len() as f64;
                worst = worst.max(diff);
                compared += 1;
            }
        }
        eprintln!("Media Foundation vs openh264: {compared} frames compared, worst mean difference {worst:.2}");
        assert!(compared >= 5, "Media Foundation produced too few pictures: {compared}");
        assert!(worst < 12.0, "pictures differ too much: {worst}");
    }

    #[test]
    fn nv12_colours() {
        // 2x2 picture, one colour: Y=81 U=90 V=240 is red in BT.709 limited range
        let src = [81u8, 81, 81, 81, 90, 240];
        let out = nv12_to_bgra(&src, 2, 2, 2, 2);
        assert!(out[2] > 200 && out[1] < 60 && out[0] < 80, "{:?}", &out[..4]);
    }
}
