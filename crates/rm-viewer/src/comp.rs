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
//!
//! With the window's shape from the Mac (`window_mask`) the picture itself is drawn through it,
//! premultiplied: its corners, a menu's outline or the Dock's are exactly the Mac's, with no black
//! where the video has none of the window (the clip then only rounds what the Mac does not draw).

use rm_decode::Picture;
use windows::core::Interface;
use windows::Win32::Foundation::{HMODULE, HWND, POINT};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::DirectComposition::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;

/// The window's shape: alpha per picture pixel, and its texture once made.
struct Mask {
    w: u32,
    h: u32,
    alpha: std::sync::Arc<Vec<u8>>,
    view: Option<ID3D11ShaderResourceView>,
}

pub struct Comp {
    device: IDCompositionDevice,
    _target: IDCompositionTarget,
    root: IDCompositionVisual,
    clip: IDCompositionRectangleClip,
    /// shown until the first picture; under a shaped picture it would show in its clear corners
    bg: IDCompositionVisual,
    bg_surface: IDCompositionSurface,
    bg_shown: bool,
    bg_scale: IDCompositionScaleTransform,
    mask: Option<Mask>,
    /// the clip's corner radii for a picture without its shape, and with it (the shape makes
    /// the picture's own corners), and which is set
    radii: ([f32; 4], [f32; 4]),
    radii_shaped: Option<bool>,
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
    /// picture area (x, y, w, h) inside the window, pixels
    area: (i32, i32, i32, i32),
    pub kind: &'static str,
    /// the step where showing a GPU picture last failed
    pub last_error: &'static str,
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
            Some(Self { device, _target: target, root, clip, bg, bg_surface, bg_shown: true, bg_scale, mask: None, radii: ([0.0; 4], [0.0; 4]), radii_shaped: None, video, video_scale, chrome, chrome_surface: None, overlay, overlay_surface: None, d3d, ctx, swap: None, swap_size: (0, 0), area: (0, 0, 1, 1), kind, last_error: "" })
        }
    }

    /// The window's shape (alpha per picture pixel, `w`x`h`), or none: used for pictures of that
    /// size only (a resized window's new picture waits for its new shape).
    pub fn set_mask(&mut self, mask: Option<(u32, u32, std::sync::Arc<Vec<u8>>)>) {
        self.mask = mask.filter(|(w, h, a)| a.len() == (*w * *h) as usize).map(|(w, h, alpha)| Mask { w, h, alpha, view: None });
        if self.mask.is_none() {
            self.show_bg(true);
        }
    }

    /// Whether a shape for pictures of this size is known.
    pub fn shaped(&self, w: u32, h: u32) -> bool {
        self.mask.as_ref().is_some_and(|m| (m.w, m.h) == (w, h))
    }

    /// The picture shown now is drawn through its shape (`on`) or not: background and clip to match.
    fn show_bg(&mut self, on: bool) {
        self.set_radii(!on);
        if self.bg_shown == on {
            return;
        }
        self.bg_shown = on;
        unsafe {
            let _ = if on { self.bg.SetContent(&self.bg_surface) } else { self.bg.SetContent(None::<&windows::core::IUnknown>) };
            let _ = self.device.Commit();
        }
    }

    /// The shape's texture for a picture `w`x`h`, made on first use.
    fn mask_view(&mut self, w: u32, h: u32) -> Option<ID3D11ShaderResourceView> {
        let m = self.mask.as_mut().filter(|m| (m.w, m.h) == (w, h))?;
        if m.view.is_none() {
            let d = D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_R8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_IMMUTABLE,
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let init = D3D11_SUBRESOURCE_DATA { pSysMem: m.alpha.as_ptr() as *const _, SysMemPitch: w, SysMemSlicePitch: 0 };
            unsafe {
                let mut t = None;
                self.d3d.CreateTexture2D(&d, Some(&init), Some(&mut t)).ok()?;
                let mut v = None;
                self.d3d.CreateShaderResourceView(&t?, None, Some(&mut v)).ok()?;
                m.view = v;
            }
        }
        m.view.clone()
    }

    fn set_radii(&mut self, shaped: bool) {
        if self.radii_shaped == Some(shaped) {
            return;
        }
        self.radii_shaped = Some(shaped);
        unsafe {
            let [tl, tr, br, bl] = if shaped { self.radii.1 } else { self.radii.0 };
            let c = &self.clip;
            let _ = (c.SetTopLeftRadiusX2(tl), c.SetTopLeftRadiusY2(tl), c.SetTopRightRadiusX2(tr), c.SetTopRightRadiusY2(tr));
            let _ = (c.SetBottomRightRadiusX2(br), c.SetBottomRightRadiusY2(br), c.SetBottomLeftRadiusX2(bl), c.SetBottomLeftRadiusY2(bl));
            let _ = self.device.Commit();
        }
    }

    /// Window `w`x`h`, chrome `bar` pixels high (picture below it), corner radii (top-left,
    /// top-right, bottom-right, bottom-left; 0: square) for a picture without its shape
    /// (`plain`) and with it (`shaped`).
    pub fn layout(&mut self, w: i32, h: i32, bar: i32, plain: [f32; 4], shaped: [f32; 4]) {
        self.radii = (plain, shaped);
        let now = self.radii_shaped.unwrap_or(false);
        self.radii_shaped = None;
        self.set_radii(now);
        unsafe {
            let _ = self.clip.SetLeft2(0.0);
            let _ = self.clip.SetTop2(0.0);
            let _ = self.clip.SetRight2(w as f32);
            let _ = self.clip.SetBottom2(h as f32);
            let _ = self.bg_scale.SetScaleX2(w as f32);
            let _ = self.bg_scale.SetScaleY2(h as f32);
            let _ = self.chrome.SetOffsetY2(0.0);
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
        // the picture keeps its shape (letterboxed when the window's shape differs, e.g. a 16:10
        // Mac screen on a 16:9 monitor), as Moonlight draws it; never stretched one way only
        let (ox, oy, w, h) = crate::keymap::fit_rect((self.area.2, self.area.3), (sw, sh));
        // the same size as the window (within rounding): 1:1 with no filtering (any resampling,
        // even by 0.1 %, softens every glyph)
        let exact = (w - sw as i32).abs() <= 2 && (h - sh as i32).abs() <= 2;
        let (sx, sy) = if exact { (1.0, 1.0) } else { (w as f32 / sw as f32, h as f32 / sh as f32) };
        unsafe {
            let _ = self.video.SetOffsetX2((self.area.0 + ox) as f32);
            let _ = self.video.SetOffsetY2((self.area.1 + oy) as f32);
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
                        // premultiplied: a shaped picture is clear outside the window
                        AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
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
        // premultiplied: alpha from the shape (or opaque), colour never above it
        let shaped = self.mask.as_ref().filter(|m| (m.w, m.h) == (w, h)).map(|m| m.alpha.clone());
        let mut px = p.bgra.clone();
        apply_alpha(&mut px, shaped.as_deref().map(|v| v.as_slice()));
        self.show_bg(shaped.is_none());
        unsafe {
            let Some(sc) = self.swap.as_ref() else { return false };
            let Ok(back) = sc.GetBuffer::<ID3D11Texture2D>(0) else { return false };
            self.ctx.UpdateSubresource(&back, 0, None, px.as_ptr() as *const _, w * 4, 0);
            sc.Present(0, DXGI_PRESENT(0)).is_ok()
        }
    }

    /// Show a hardware-decoded picture: a pixel shader converts the NV12 texture into the swap
    /// chain (Moonlight's renderer; the picture never comes back to system memory). On failure
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
        let r = crate::nv12::shared().ok_or("no GPU colour conversion")?;
        let mask = self.mask_view(w, h);
        self.show_bg(mask.is_none());
        // the picture at the size it is shown, resampled here with a sharp cubic filter (the
        // compositor's bilinear scaling softens every glyph); 1:1 when it already fits
        let (_, _, fw, fh) = crate::keymap::fit_rect((self.area.2, self.area.3), (w, h));
        let out = if (fw - w as i32).abs() <= 2 && (fh - h as i32).abs() <= 2 { (w, h) } else { (fw.max(1) as u32, fh.max(1) as u32) };
        if !self.ensure_swap(out.0, out.1) {
            return Err("swap chain");
        }
        unsafe {
            let sc = self.swap.as_ref().ok_or("swap chain")?;
            let back = sc.GetBuffer::<ID3D11Texture2D>(0).map_err(|_| "back buffer")?;
            r.draw(&self.d3d, &self.ctx, &p.tex, (w, h), &back, out, mask.as_ref())?;
            sc.Present(0, DXGI_PRESENT(0)).ok().map_err(|_| "present")
        }
    }
}

/// A BGRA picture as premultiplied pixels: alpha from `mask` (one byte per pixel) or opaque, no
/// colour above its alpha (the video's colour at a window's edge is already mixed with black).
pub fn apply_alpha(bgra: &mut [u8], mask: Option<&[u8]>) {
    match mask {
        Some(m) if m.len() * 4 == bgra.len() => {
            for (px, a) in bgra.chunks_exact_mut(4).zip(m) {
                px[0] = px[0].min(*a);
                px[1] = px[1].min(*a);
                px[2] = px[2].min(*a);
                px[3] = *a;
            }
        }
        _ => bgra.chunks_exact_mut(4).for_each(|px| px[3] = 255),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn pictures_become_premultiplied_through_their_shape() {
        // two pixels: one outside the window (black in the video), one at its edge (its colour
        // already mixed with black by half), and opaque ones without a shape
        let mut px = vec![0, 0, 0, 0, 120, 60, 200, 0];
        super::apply_alpha(&mut px, Some(&[0, 128]));
        assert_eq!(px, vec![0, 0, 0, 0, 120, 60, 128, 128]);
        let mut px = vec![1, 2, 3, 0, 4, 5, 6, 7];
        super::apply_alpha(&mut px, None);
        assert_eq!(px, vec![1, 2, 3, 255, 4, 5, 6, 255]);
        // a shape of another size is not used
        let mut px = vec![9, 9, 9, 0];
        super::apply_alpha(&mut px, Some(&[0, 0]));
        assert_eq!(px, vec![9, 9, 9, 255]);
    }
}
