//! DirectComposition window surface: the whole remote window (Mac chrome + picture) is a small
//! visual tree under an anti-aliased rounded-rectangle clip, so windows get real macOS-like
//! rounded corners (which neither GDI regions nor DWM's fixed small radius can give).
//!
//! root (clip: rounded rect)
//!  ├─ background  1x1 surface scaled to the window (shows before the first frame)
//!  ├─ video       composition swap chain, scaled to the picture area
//!  └─ chrome      surface with the bar drawn by GDI (traffic lights, menus, title)
//!
//! The top-level window is created with WS_EX_NOREDIRECTIONBITMAP: the corners outside the
//! clip are truly transparent. Child windows (input / focus) stay, they just do not draw.

use rm_decode::Picture;
use windows::core::Interface;
use windows::Win32::Foundation::{HMODULE, HWND, POINT};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::DirectComposition::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;

pub struct Comp {
    device: IDCompositionDevice,
    _target: IDCompositionTarget,
    root: IDCompositionVisual,
    clip: IDCompositionRectangleClip,
    bg_scale: IDCompositionScaleTransform,
    video: IDCompositionVisual,
    video_scale: IDCompositionScaleTransform,
    chrome: IDCompositionVisual,
    chrome_surface: Option<(IDCompositionSurface, i32, i32)>,
    /// stats overlay (Ctrl+Alt+Shift+S), above everything
    overlay: IDCompositionVisual,
    overlay_surface: Option<(IDCompositionSurface, i32, i32)>,
    d3d: ID3D11Device,
    ctx: ID3D11DeviceContext,
    swap: Option<IDXGISwapChain1>,
    swap_size: (u32, u32),
    /// GPU colour conversion (NV12 -> the swap chain), for hardware-decoded pictures
    vp: Option<VideoProc>,
    /// picture area (x, y, w, h) inside the window, pixels
    area: (i32, i32, i32, i32),
    pub kind: &'static str,
    /// the step where showing a GPU picture last failed
    pub last_error: &'static str,
}

struct VideoProc {
    dev: ID3D11VideoDevice,
    ctx: ID3D11VideoContext,
    en: ID3D11VideoProcessorEnumerator,
    vp: ID3D11VideoProcessor,
    /// (input texture size, output size)
    sizes: ((u32, u32), (u32, u32)),
}

fn create_device() -> Option<(ID3D11Device, ID3D11DeviceContext, &'static str)> {
    // the viewer's shared device: hardware-decoded pictures live on it
    if let Some(g) = crate::gpu::shared() {
        return Some((g.device.clone(), g.ctx.clone(), if g.hardware { "dcomp-d3d11-hardware" } else { "dcomp-d3d11-warp" }));
    }
    let drivers: [(D3D_DRIVER_TYPE, &'static str); 2] = [(D3D_DRIVER_TYPE_HARDWARE, "dcomp-d3d11-hardware"), (D3D_DRIVER_TYPE_WARP, "dcomp-d3d11-warp")];
    for (driver, kind) in drivers {
        let (mut device, mut ctx) = (None, None);
        let ok = unsafe {
            D3D11CreateDevice(None, driver, HMODULE::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT, None, D3D11_SDK_VERSION, Some(&mut device), None, Some(&mut ctx))
        };
        if let (Ok(()), Some(d), Some(c)) = (ok, device, ctx) {
            return Some((d, c, kind));
        }
    }
    None
}

/// Whether composition windows can be made on this machine (decided before creating windows,
/// because WS_EX_NOREDIRECTIONBITMAP cannot be taken back).
pub fn available() -> bool {
    create_device().is_some_and(|(d, _, _)| unsafe { d.cast::<IDXGIDevice>().ok().and_then(|x| DCompositionCreateDevice::<_, IDCompositionDevice>(&x).ok()).is_some() })
}

/// Upload a BGRA picture into a texture (rows `pitch` apart) at `offset`.
unsafe fn upload(ctx: &ID3D11DeviceContext, tex: &ID3D11Texture2D, offset: POINT, w: i32, h: i32, bgra: &[u8]) {
    let b = D3D11_BOX { left: offset.x as u32, top: offset.y as u32, front: 0, right: (offset.x + w) as u32, bottom: (offset.y + h) as u32, back: 1 };
    ctx.UpdateSubresource(tex, 0, Some(&b), bgra.as_ptr() as *const _, (w * 4) as u32, 0);
}

impl Comp {
    pub fn new(hwnd: HWND) -> Option<Self> {
        let (d3d, ctx, kind) = create_device()?;
        unsafe {
            let dxgi: IDXGIDevice = d3d.cast().ok()?;
            let device: IDCompositionDevice = DCompositionCreateDevice(&dxgi).ok()?;
            let target = device.CreateTargetForHwnd(hwnd, true).ok()?;
            let root = device.CreateVisual().ok()?;
            let clip = device.CreateRectangleClip().ok()?;
            root.SetClip(&clip).ok()?;
            // soft borders: the rounded clip is anti-aliased (hard is the default)
            root.SetBorderMode(DCOMPOSITION_BORDER_MODE_SOFT).ok()?;
            // background: one opaque pixel, stretched
            let bg = device.CreateVisual().ok()?;
            let bg_surface = device.CreateSurface(1, 1, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_ALPHA_MODE_PREMULTIPLIED).ok()?;
            let mut off = POINT::default();
            let tex: ID3D11Texture2D = bg_surface.BeginDraw(None, &mut off).ok()?;
            upload(&ctx, &tex, off, 1, 1, &[250, 250, 250, 255]);
            bg_surface.EndDraw().ok()?;
            bg.SetContent(&bg_surface).ok()?;
            let bg_scale = device.CreateScaleTransform().ok()?;
            bg.SetTransform(&bg_scale).ok()?;
            let video = device.CreateVisual().ok()?;
            let video_scale = device.CreateScaleTransform().ok()?;
            video.SetTransform(&video_scale).ok()?;
            video.SetBitmapInterpolationMode(DCOMPOSITION_BITMAP_INTERPOLATION_MODE_LINEAR).ok()?;
            let chrome = device.CreateVisual().ok()?;
            root.AddVisual(&bg, false, None).ok()?;
            root.AddVisual(&video, true, &bg).ok()?;
            root.AddVisual(&chrome, true, &video).ok()?;
            let overlay = device.CreateVisual().ok()?;
            root.AddVisual(&overlay, true, &chrome).ok()?;
            target.SetRoot(&root).ok()?;
            device.Commit().ok()?;
            Some(Self { device, _target: target, root, clip, bg_scale, video, video_scale, chrome, chrome_surface: None, overlay, overlay_surface: None, d3d, ctx, swap: None, swap_size: (0, 0), vp: None, area: (0, 0, 1, 1), kind, last_error: "" })
        }
    }

    /// Window `w`x`h`, chrome `bar` pixels high (picture below it), corner `radius` (0: square).
    pub fn layout(&mut self, w: i32, h: i32, bar: i32, radius: f32) {
        unsafe {
            let _ = self.clip.SetLeft2(0.0);
            let _ = self.clip.SetTop2(0.0);
            let _ = self.clip.SetRight2(w as f32);
            let _ = self.clip.SetBottom2(h as f32);
            for set in [
                IDCompositionRectangleClip::SetTopLeftRadiusX2, IDCompositionRectangleClip::SetTopLeftRadiusY2,
                IDCompositionRectangleClip::SetTopRightRadiusX2, IDCompositionRectangleClip::SetTopRightRadiusY2,
                IDCompositionRectangleClip::SetBottomLeftRadiusX2, IDCompositionRectangleClip::SetBottomLeftRadiusY2,
                IDCompositionRectangleClip::SetBottomRightRadiusX2, IDCompositionRectangleClip::SetBottomRightRadiusY2,
            ] {
                let _ = set(&self.clip, radius);
            }
            let _ = self.bg_scale.SetScaleX2(w as f32);
            let _ = self.bg_scale.SetScaleY2(h as f32);
            let _ = self.chrome.SetOffsetY2(0.0);
            let _ = self.video.SetOffsetY2(bar as f32);
            self.area = (0, bar, w.max(1), (h - bar).max(1));
            self.scale_video();
            let _ = self.root.SetOffsetX2(0.0);
            let _ = self.device.Commit();
        }
    }

    fn scale_video(&self) {
        let (sw, sh) = self.swap_size;
        if sw == 0 || sh == 0 {
            return;
        }
        // within a couple of pixels of the window: draw 1:1 with no filtering (any resampling,
        // even by 0.1 %, softens every glyph); otherwise scale smoothly
        let exact = (self.area.2 - sw as i32).abs() <= 2 && (self.area.3 - sh as i32).abs() <= 2;
        let (sx, sy) = if exact { (1.0, 1.0) } else { (self.area.2 as f32 / sw as f32, self.area.3 as f32 / sh as f32) };
        unsafe {
            let _ = self.video_scale.SetScaleX2(sx);
            let _ = self.video_scale.SetScaleY2(sy);
            let _ = self.video.SetBitmapInterpolationMode(if exact { DCOMPOSITION_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR } else { DCOMPOSITION_BITMAP_INTERPOLATION_MODE_LINEAR });
        }
    }

    /// The chrome bar as drawn (BGRA, `w`x`h`, top-down); `h` 0 hides it.
    pub fn set_chrome(&mut self, w: i32, h: i32, bgra: &[u8]) -> bool {
        unsafe {
            if w <= 0 || h <= 0 {
                let _ = self.chrome.SetContent(None::<&windows::core::IUnknown>);
                self.chrome_surface = None;
                return self.device.Commit().is_ok();
            }
            if self.chrome_surface.as_ref().map(|s| (s.1, s.2)) != Some((w, h)) {
                let Ok(s) = self.device.CreateSurface(w as u32, h as u32, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_ALPHA_MODE_PREMULTIPLIED) else { return false };
                let _ = self.chrome.SetContent(&s);
                self.chrome_surface = Some((s, w, h));
            }
            let Some((s, _, _)) = self.chrome_surface.as_ref() else { return false };
            let mut off = POINT::default();
            let Ok(tex) = s.BeginDraw::<ID3D11Texture2D>(None, &mut off) else { return false };
            upload(&self.ctx, &tex, off, w, h, bgra);
            let ok = s.EndDraw().is_ok();
            ok && self.device.Commit().is_ok()
        }
    }

    /// The stats overlay (premultiplied BGRA, `w`x`h`) at (`x`, `y`); `w` 0 hides it.
    pub fn set_overlay(&mut self, x: i32, y: i32, w: i32, h: i32, bgra: &[u8]) -> bool {
        unsafe {
            if w <= 0 || h <= 0 {
                let _ = self.overlay.SetContent(None::<&windows::core::IUnknown>);
                self.overlay_surface = None;
                return self.device.Commit().is_ok();
            }
            if self.overlay_surface.as_ref().map(|s| (s.1, s.2)) != Some((w, h)) {
                let Ok(s) = self.device.CreateSurface(w as u32, h as u32, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_ALPHA_MODE_PREMULTIPLIED) else { return false };
                let _ = self.overlay.SetContent(&s);
                self.overlay_surface = Some((s, w, h));
            }
            let _ = self.overlay.SetOffsetX2(x as f32);
            let _ = self.overlay.SetOffsetY2(y as f32);
            let Some((s, _, _)) = self.overlay_surface.as_ref() else { return false };
            let mut off = POINT::default();
            let Ok(tex) = s.BeginDraw::<ID3D11Texture2D>(None, &mut off) else { return false };
            upload(&self.ctx, &tex, off, w, h, bgra);
            let ok = s.EndDraw().is_ok();
            ok && self.device.Commit().is_ok()
        }
    }

    /// The swap chain at `w`x`h` (made or resized). False if the device was lost.
    fn ensure_swap(&mut self, w: u32, h: u32) -> bool {
        unsafe {
            if self.swap.is_some() && self.swap_size == (w, h) {
                return true;
            }
            match self.swap.as_ref() {
                Some(sc) => {
                    if sc.ResizeBuffers(0, w, h, DXGI_FORMAT_UNKNOWN, DXGI_SWAP_CHAIN_FLAG(0)).is_err() {
                        return false;
                    }
                }
                None => {
                    let Ok(dxgi) = self.d3d.cast::<IDXGIDevice>() else { return false };
                    let Ok(adapter) = dxgi.GetAdapter() else { return false };
                    let Ok(factory) = adapter.GetParent::<IDXGIFactory2>() else { return false };
                    let desc = DXGI_SWAP_CHAIN_DESC1 {
                        Width: w,
                        Height: h,
                        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                        BufferCount: 2,
                        Scaling: DXGI_SCALING_STRETCH,
                        SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                        AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                        ..Default::default()
                    };
                    let Ok(sc) = factory.CreateSwapChainForComposition(&self.d3d, &desc, None) else { return false };
                    if self.video.SetContent(&sc).is_err() {
                        return false;
                    }
                    self.swap = Some(sc);
                }
            }
            self.swap_size = (w, h);
            self.scale_video();
            let _ = self.device.Commit();
            true
        }
    }

    /// Upload and show a decoded picture. False if the device was lost.
    pub fn present(&mut self, p: &Picture) -> bool {
        let (w, h) = (p.width as u32, p.height as u32);
        if !self.ensure_swap(w, h) {
            return false;
        }
        unsafe {
            let Some(sc) = self.swap.as_ref() else { return false };
            let Ok(back) = sc.GetBuffer::<ID3D11Texture2D>(0) else { return false };
            self.ctx.UpdateSubresource(&back, 0, None, p.bgra.as_ptr() as *const _, w * 4, 0);
            sc.Present(0, DXGI_PRESENT(0)).is_ok()
        }
    }

    /// Show a hardware-decoded picture: the GPU video processor converts NV12 into the swap
    /// chain directly (the picture never comes back to system memory). On failure
    /// `last_error` names the step.
    pub fn present_gpu(&mut self, p: &crate::gpu::GpuPic) -> bool {
        match self.present_gpu_inner(p) {
            Ok(()) => true,
            Err(e) => {
                self.last_error = e;
                false
            }
        }
    }

    fn present_gpu_inner(&mut self, p: &crate::gpu::GpuPic) -> Result<(), &'static str> {
        let (w, h) = (p.width, p.height);
        if !self.ensure_swap(w, h) {
            return Err("swap chain");
        }
        unsafe {
            let mut td = D3D11_TEXTURE2D_DESC::default();
            p.tex.GetDesc(&mut td);
            let sizes = ((td.Width, td.Height), (w, h));
            if self.vp.as_ref().map(|v| v.sizes) != Some(sizes) {
                self.vp = None;
                let dev = self.d3d.cast::<ID3D11VideoDevice>().map_err(|_| "no video device")?;
                let vctx = self.ctx.cast::<ID3D11VideoContext>().map_err(|_| "no video context")?;
                let rate = DXGI_RATIONAL { Numerator: 60, Denominator: 1 };
                let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                    InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                    InputFrameRate: rate,
                    InputWidth: td.Width,
                    InputHeight: td.Height,
                    OutputFrameRate: rate,
                    OutputWidth: w,
                    OutputHeight: h,
                    Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
                };
                let en = dev.CreateVideoProcessorEnumerator(&desc).map_err(|_| "video processor enumerator")?;
                let vp = dev.CreateVideoProcessor(&en, 0).map_err(|_| "video processor")?;
                // BT.709 limited-range YCbCr in, full-range RGB out
                let cs_in = D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: 0x14 };
                let cs_out = D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: 0 };
                vctx.VideoProcessorSetStreamColorSpace(&vp, 0, &cs_in);
                vctx.VideoProcessorSetOutputColorSpace(&vp, &cs_out);
                vctx.VideoProcessorSetStreamFrameFormat(&vp, 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
                vctx.VideoProcessorSetStreamAutoProcessingMode(&vp, 0, false);
                let r = windows::Win32::Foundation::RECT { left: 0, top: 0, right: w as i32, bottom: h as i32 };
                vctx.VideoProcessorSetStreamSourceRect(&vp, 0, true, Some(&r));
                vctx.VideoProcessorSetStreamDestRect(&vp, 0, true, Some(&r));
                vctx.VideoProcessorSetOutputTargetRect(&vp, true, Some(&r));
                self.vp = Some(VideoProc { dev, ctx: vctx, en, vp, sizes });
            }
            let v = self.vp.as_ref().ok_or("video processor")?;
            let sc = self.swap.as_ref().ok_or("swap chain")?;
            let back = sc.GetBuffer::<ID3D11Texture2D>(0).map_err(|_| "back buffer")?;
            let od = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC { ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D, Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } } };
            let mut ov = None;
            v.dev.CreateVideoProcessorOutputView(&back, &v.en, &od, Some(&mut ov)).map_err(|_| "output view")?;
            let id = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC { FourCC: 0, ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D, Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 } } };
            let mut iv = None;
            v.dev.CreateVideoProcessorInputView(&p.tex, &v.en, &id, Some(&mut iv)).map_err(|_| "input view (NV12 texture)")?;
            let (Some(ov), Some(iv)) = (ov, iv) else { return Err("views") };
            let mut stream = D3D11_VIDEO_PROCESSOR_STREAM { Enable: true.into(), pInputSurface: std::mem::ManuallyDrop::new(Some(iv)), ..Default::default() };
            let ok = v.ctx.VideoProcessorBlt(&v.vp, &ov, 0, std::slice::from_ref(&stream)).is_ok();
            std::mem::ManuallyDrop::drop(&mut stream.pInputSurface);
            if !ok {
                return Err("video processor blit");
            }
            sc.Present(0, DXGI_PRESENT(0)).ok().map_err(|_| "present")
        }
    }
}
