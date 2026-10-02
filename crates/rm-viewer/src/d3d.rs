//! Direct3D 11 presenter: one flip-model DXGI swap chain per window. The decoded BGRA picture is
//! uploaded straight into the back buffer (sized to the remote picture) and DXGI_SCALING_STRETCH
//! lets the compositor scale it to the window, so no shaders are needed. Hardware device first,
//! WARP (software rasteriser) second; if neither works the caller keeps using GDI.

use rm_decode::Picture;
use windows::core::Interface;
use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;

pub struct Presenter {
    ctx: ID3D11DeviceContext,
    swap: IDXGISwapChain1,
    size: (u32, u32),
    pub kind: &'static str,
}

fn create_device() -> Option<(ID3D11Device, ID3D11DeviceContext, &'static str)> {
    let drivers: [(D3D_DRIVER_TYPE, &'static str); 2] = [(D3D_DRIVER_TYPE_HARDWARE, "d3d11-hardware"), (D3D_DRIVER_TYPE_WARP, "d3d11-warp")];
    for (driver, kind) in drivers {
        let mut device = None;
        let mut ctx = None;
        let ok = unsafe {
            D3D11CreateDevice(None, driver, HMODULE::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT, None, D3D11_SDK_VERSION, Some(&mut device), None, Some(&mut ctx))
        };
        if let (Ok(()), Some(d), Some(c)) = (ok, device, ctx) {
            return Some((d, c, kind));
        }
    }
    None
}

impl Presenter {
    pub fn new(hwnd: HWND, w: u32, h: u32) -> Option<Self> {
        let (device, ctx, kind) = create_device()?;
        unsafe {
            let dxgi: IDXGIDevice = device.cast().ok()?;
            let adapter = dxgi.GetAdapter().ok()?;
            let factory: IDXGIFactory2 = adapter.GetParent().ok()?;
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: w.max(2),
                Height: h.max(2),
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                ..Default::default()
            };
            let swap = factory.CreateSwapChainForHwnd(&device, hwnd, &desc, None, None).ok()?;
            let _ = factory.MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER);
            Some(Self { ctx, swap, size: (w.max(2), h.max(2)), kind })
        }
    }

    /// Upload and present. Returns false if the device was lost (caller falls back to GDI).
    pub fn present(&mut self, p: &Picture) -> bool {
        let (w, h) = (p.width as u32, p.height as u32);
        unsafe {
            if (w, h) != self.size {
                if self.swap.ResizeBuffers(0, w, h, DXGI_FORMAT_UNKNOWN, DXGI_SWAP_CHAIN_FLAG(0)).is_err() {
                    return false;
                }
                self.size = (w, h);
            }
            let Ok(back) = self.swap.GetBuffer::<ID3D11Texture2D>(0) else { return false };
            self.ctx.UpdateSubresource(&back, 0, None, p.bgra.as_ptr() as *const _, w * 4, 0);
            // SyncInterval 0: never block the UI thread; the compositor still shows one frame per refresh.
            self.swap.Present(0, DXGI_PRESENT(0)).is_ok()
        }
    }
}
