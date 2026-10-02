//! One Direct3D 11 device for the whole viewer, shared by the hardware video decoders (on their
//! threads) and every window's composition surface (on the UI thread): decoded pictures stay on
//! the GPU from the decoder to the screen, as in Moonlight. The device is multithread protected.

use std::sync::OnceLock;
use windows::core::Interface;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;

#[derive(Clone)]
pub struct Gpu {
    pub device: ID3D11Device,
    pub ctx: ID3D11DeviceContext,
    /// a real GPU (hardware decode possible), not WARP
    pub hardware: bool,
}

// The device is created multithread protected (ID3D11Multithread), so it may be used from the
// decode threads and the UI thread.
unsafe impl Send for Gpu {}
unsafe impl Sync for Gpu {}

fn create() -> Option<Gpu> {
    for (driver, hardware) in [(D3D_DRIVER_TYPE_HARDWARE, true), (D3D_DRIVER_TYPE_WARP, false)] {
        let (mut device, mut ctx) = (None, None);
        let flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT;
        let ok = unsafe { D3D11CreateDevice(None, driver, HMODULE::default(), flags, None, D3D11_SDK_VERSION, Some(&mut device), None, Some(&mut ctx)) };
        // WARP has no video support: retry it without the flag
        let ok = ok.or_else(|_| unsafe { D3D11CreateDevice(None, driver, HMODULE::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT, None, D3D11_SDK_VERSION, Some(&mut device), None, Some(&mut ctx)) });
        if let (Ok(()), Some(device), Some(ctx)) = (ok, device, ctx) {
            if let Ok(mt) = device.cast::<ID3D11Multithread>() {
                unsafe {
                    let _ = mt.SetMultithreadProtected(true);
                }
            }
            return Some(Gpu { device, ctx, hardware });
        }
    }
    None
}

/// The shared device (created on first use).
pub fn shared() -> Option<&'static Gpu> {
    static G: OnceLock<Option<Gpu>> = OnceLock::new();
    G.get_or_init(create).as_ref()
}

/// A decoded picture that stays on the GPU: an NV12 texture (coded size, maybe a few rows
/// larger than the picture) and the visible size.
pub struct GpuPic {
    pub tex: ID3D11Texture2D,
    pub width: u32,
    pub height: u32,
}

unsafe impl Send for GpuPic {}

impl std::fmt::Debug for GpuPic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GpuPic({}x{})", self.width, self.height)
    }
}

/// A few NV12 textures the decoder copies its output into, in turn (the decoder's own surfaces
/// go back to it at once; the UI may still be showing the previous copy).
pub struct Ring {
    texs: Vec<ID3D11Texture2D>,
    size: (u32, u32),
    next: usize,
}

impl Ring {
    pub fn new() -> Self {
        Self { texs: vec![], size: (0, 0), next: 0 }
    }

    pub fn next(&mut self, g: &Gpu, w: u32, h: u32) -> Option<ID3D11Texture2D> {
        if self.size != (w, h) {
            self.texs.clear();
            self.size = (w, h);
            for _ in 0..4 {
                let desc = D3D11_TEXTURE2D_DESC {
                    Width: w,
                    Height: h,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_NV12,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                    ..Default::default()
                };
                let mut t = None;
                unsafe { g.device.CreateTexture2D(&desc, None, Some(&mut t)).ok()? };
                self.texs.push(t?);
            }
        }
        let t = self.texs.get(self.next)?.clone();
        self.next = (self.next + 1) % self.texs.len();
        Some(t)
    }
}

impl Default for Ring {
    fn default() -> Self {
        Self::new()
    }
}
