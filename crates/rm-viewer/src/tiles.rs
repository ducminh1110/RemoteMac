//! Live cards for Stage Manager: each parked window is captured with Windows.Graphics.Capture into
//! a Composition drawing surface and shown, tilted in perspective, on composition-only windows:
//! the sidebar, and the overlay the cards fly across between the sidebar and the stage (a card
//! moves from one to the other: one compositor holds both, as `LiveCardHost` borrows a tile).
//! Ported from StageManagerForWindows (MIT, depoledna / Andreas Wäscher): `CaptureSession`,
//! `D3DDeviceHolder`, `CompositorFactory`, `CompositionHost`. Where they host one HWND per card,
//! one window holds every card's visual tree (root → content → sprite, as theirs).
//! Everything here is on the UI thread except the frame callback, which is free-threaded.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use windows::core::{Interface, IInspectable};
use windows_numerics::{Matrix4x4, Vector2, Vector3};
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::{DirectXAlphaMode, DirectXPixelFormat};
use windows::Graphics::SizeInt32;
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{CompositionGeometricClip, CompositionGraphicsDevice, CompositionRoundedRectangleGeometry, CompositionStretch, CompositionSurfaceBrush, Compositor, ContainerVisual, SpriteVisual};
use windows::Win32::Foundation::{HWND, POINT, SIZE};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::System::WinRT::Composition::{ICompositionDrawingSurfaceInterop, ICompositorDesktopInterop, ICompositorInterop};
use windows::Win32::System::WinRT::Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::System::WinRT::{CreateDispatcherQueueController, DispatcherQueueOptions, DQTAT_COM_NONE, DQTYPE_THREAD_CURRENT};

use crate::cards::{self, Mat};

fn matrix(m: &Mat) -> Matrix4x4 {
    Matrix4x4 { M11: m[0], M12: m[1], M13: m[2], M14: m[3], M21: m[4], M22: m[5], M23: m[6], M24: m[7], M31: m[8], M32: m[9], M33: m[10], M34: m[11], M41: m[12], M42: m[13], M43: m[14], M44: m[15] }
}

/// The compositor, its target on the sidebar window and the D3D devices behind the captures.
pub struct Host {
    compositor: Compositor,
    _target: DesktopWindowTarget,
    root: ContainerVisual,
    /// the transition overlay's target and root, once attached
    overlay: Option<(DesktopWindowTarget, ContainerVisual)>,
    gdev: CompositionGraphicsDevice,
    winrt: IDirect3DDevice,
    d3d: ID3D11Device,
    ctx: ID3D11DeviceContext,
}

/// A COM pointer moved to the capture thread; the D3D context is guarded by `ID3D11Multithread`.
struct Agile<T>(T);
unsafe impl<T> Send for Agile<T> {}
unsafe impl<T> Sync for Agile<T> {}

impl Host {
    /// Fails (and Stage Manager keeps its DWM thumbnails) where capture or composition are missing.
    pub fn new(hwnd: HWND) -> Option<Host> {
        unsafe {
            if !GraphicsCaptureSession::IsSupported().ok()? {
                return None;
            }
            // a compositor needs a dispatcher queue on its thread
            let opts = DispatcherQueueOptions { dwSize: std::mem::size_of::<DispatcherQueueOptions>() as u32, threadType: DQTYPE_THREAD_CURRENT, apartmentType: DQTAT_COM_NONE };
            let controller = CreateDispatcherQueueController(opts).ok()?;
            std::mem::forget(controller); // lives as long as the viewer
            let compositor = Compositor::new().ok()?;

            let (mut d3d, mut ctx) = (None, None);
            D3D11CreateDevice(None, D3D_DRIVER_TYPE_HARDWARE, Default::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT, None, D3D11_SDK_VERSION, Some(&mut d3d), None, Some(&mut ctx)).ok()?;
            let (d3d, ctx) = (d3d?, ctx?);
            if let Ok(mt) = d3d.cast::<ID3D11Multithread>() {
                let _ = mt.SetMultithreadProtected(true);
            }
            let winrt: IDirect3DDevice = CreateDirect3D11DeviceFromDXGIDevice(&d3d.cast::<IDXGIDevice>().ok()?).ok()?.cast().ok()?;
            let gdev = compositor.cast::<ICompositorInterop>().ok()?.CreateGraphicsDevice(&d3d).ok()?;
            let target = compositor.cast::<ICompositorDesktopInterop>().ok()?.CreateDesktopWindowTarget(hwnd, false).ok()?;
            let root = compositor.CreateContainerVisual().ok()?;
            target.SetRoot(&root).ok()?;
            Some(Host { compositor, _target: target, root, overlay: None, gdev, winrt, d3d, ctx })
        }
    }

    /// The overlay window the cards fly over (`TransitionOverlayWindow`).
    pub fn attach_overlay(&mut self, hwnd: HWND) -> bool {
        unsafe {
            let made = (|| {
                let target = self.compositor.cast::<ICompositorDesktopInterop>().ok()?.CreateDesktopWindowTarget(hwnd, true).ok()?;
                let root = self.compositor.CreateContainerVisual().ok()?;
                target.SetRoot(&root).ok()?;
                Some((target, root))
            })();
            self.overlay = made;
            self.overlay.is_some()
        }
    }

    /// A card onto the overlay, in front of what is there (it leaves the sidebar).
    pub fn lift(&self, t: &Tile) {
        if let Some((_, root)) = &self.overlay {
            if let Ok(c) = self.root.Children() {
                let _ = c.Remove(&t.root);
            }
            if let Ok(c) = root.Children() {
                let _ = c.Remove(&t.root);
                let _ = c.InsertAtTop(&t.root);
            }
        }
    }

    /// A card off the overlay (it goes back to the sidebar at the next `set_order`, or away).
    pub fn drop_card(&self, t: &Tile) {
        if let Some((_, root)) = &self.overlay {
            if let Ok(c) = root.Children() {
                let _ = c.Remove(&t.root);
            }
        }
    }

    /// The cards in this order, the first at the back.
    pub fn set_order(&self, back_to_front: &[&Tile]) {
        if let Ok(children) = self.root.Children() {
            let _ = children.RemoveAll();
            for t in back_to_front {
                let _ = children.InsertAtTop(&t.root);
            }
        }
    }
}

/// One live card (`CaptureSession`): root (pure perspective) → content (affine) → sprite (rotation).
pub struct Tile {
    root: ContainerVisual,
    content: ContainerVisual,
    sprite: SpriteVisual,
    clip: Option<(CompositionGeometricClip, CompositionRoundedRectangleGeometry)>,
    compositor: Compositor,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    item: GraphicsCaptureItem,
    token: i64,
    /// source size, as the card's size is solved from it
    pub source: (i32, i32),
    /// the hover scale (1 at rest), anchored on the card's left edge
    pub scale: f64,
    /// a captured frame has reached the surface (it starts blank): a card stands in for its
    /// window only once it has one
    frame: Arc<AtomicBool>,
}

impl Tile {
    pub fn new(host: &Host, hwnd: HWND, source: (i32, i32)) -> Option<Tile> {
        unsafe {
            let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>().ok()?;
            let item: GraphicsCaptureItem = interop.CreateForWindow(hwnd).ok()?;
            let size = item.Size().ok().filter(|s| s.Width > 0 && s.Height > 0).unwrap_or(SizeInt32 { Width: 1, Height: 1 });

            let surface = host.gdev.CreateDrawingSurface(windows::Foundation::Size { Width: size.Width as f32, Height: size.Height as f32 }, DirectXPixelFormat::B8G8R8A8UIntNormalized, DirectXAlphaMode::Premultiplied).ok()?;
            let surface_interop: ICompositionDrawingSurfaceInterop = surface.cast().ok()?;
            let brush: CompositionSurfaceBrush = host.compositor.CreateSurfaceBrushWithSurface(&surface).ok()?;
            let _ = brush.SetStretch(CompositionStretch::Uniform);
            let _ = brush.SetHorizontalAlignmentRatio(0.5);
            let _ = brush.SetVerticalAlignmentRatio(0.5);

            let sprite = host.compositor.CreateSpriteVisual().ok()?;
            sprite.SetBrush(&brush).ok()?;
            let root = host.compositor.CreateContainerVisual().ok()?;
            let content = host.compositor.CreateContainerVisual().ok()?;
            content.Children().ok()?.InsertAtTop(&sprite).ok()?;
            root.Children().ok()?.InsertAtTop(&content).ok()?;

            let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(&host.winrt, DirectXPixelFormat::B8G8R8A8UIntNormalized, 2, size).ok()?;
            // the frame callback: blit the captured frame into the drawing surface
            let st = Arc::new(Mutex::new((size, size)));
            let frame_flag = Arc::new(AtomicBool::new(false));
            let got = frame_flag.clone();
            let (dev, ctx, sf) = (Agile(host.winrt.clone()), Agile(host.ctx.clone()), Agile(surface_interop));
            let _keep = Agile(host.d3d.clone());
            let token = pool
                .FrameArrived(&TypedEventHandler::<Direct3D11CaptureFramePool, IInspectable>::new(move |sender, _| {
                    // capture the wrappers whole (edition 2021 would capture their non-Send fields)
                    let (dev, ctx, sf) = (&dev, &ctx, &sf);
                    let Some(pool) = sender.as_ref() else { return Ok(()) };
                    let Ok(frame) = pool.TryGetNextFrame() else { return Ok(()) };
                    let cs = frame.ContentSize()?;
                    if cs.Width <= 0 || cs.Height <= 0 {
                        return Ok(());
                    }
                    let mut s = st.lock().unwrap();
                    if (cs.Width, cs.Height) != (s.0.Width, s.0.Height) {
                        if pool.Recreate(&dev.0, DirectXPixelFormat::B8G8R8A8UIntNormalized, 2, cs).is_err() {
                            return Ok(());
                        }
                        s.0 = cs;
                    }
                    if (cs.Width, cs.Height) != (s.1.Width, s.1.Height) {
                        if sf.0.Resize(SIZE { cx: cs.Width, cy: cs.Height }).is_err() {
                            return Ok(());
                        }
                        s.1 = cs;
                    }
                    let mut off = POINT::default();
                    let Ok(dst_surface) = sf.0.BeginDraw::<windows::Win32::Graphics::Dxgi::IDXGISurface>(None, &mut off) else {
                        return Ok(());
                    };
                    let _ = (|| -> windows::core::Result<()> {
                        let dst_tex: ID3D11Texture2D = dst_surface.cast()?;
                        let src_tex: ID3D11Texture2D = frame.Surface()?.cast::<IDirect3DDxgiInterfaceAccess>()?.GetInterface()?;
                        ctx.0.CopySubresourceRegion(&dst_tex, 0, off.x as u32, off.y as u32, 0, &src_tex, 0, None);
                        got.store(true, Ordering::Release);
                        Ok(())
                    })();
                    let _ = sf.0.EndDraw();
                    Ok(())
                }))
                .ok()?;
            let session = pool.CreateCaptureSession(&item).ok()?;
            let _ = session.SetIsCursorCaptureEnabled(false);
            let _ = session.SetIsBorderRequired(false); // needs consent on some builds; ignored then
            session.StartCapture().ok()?;
            Some(Tile { root, content, sprite, clip: None, compositor: host.compositor.clone(), pool, session, item, token, source, scale: 1.0, frame: frame_flag })
        }
    }

    /// Where the card is and how it leans (`RecomputePixelSize` + `ApplyTransform` +
    /// `ApplyCornerRadius`). `x`,`y`: the card's top-left in the host (px); `w`,`h`: its size (px);
    /// `top`,`bottom`: the slopes of its edges, in degrees.
    #[allow(clippy::too_many_arguments)]
    pub fn place(&mut self, x: f64, y: f64, w: f64, h: f64, top: f64, bottom: f64, radius: f64) {
        let (hw, hh) = (w * (1.0 + 2.0 * cards::HOVER_HEADROOM), h * (1.0 + 2.0 * cards::HOVER_HEADROOM));
        let _ = self.root.SetOffset(Vector3::new((x - w * cards::HOVER_HEADROOM) as f32, (y - h * cards::HOVER_HEADROOM) as f32, 0.0));
        let _ = self.root.SetSize(Vector2::new(hw as f32, hh as f32));
        let _ = self.content.SetSize(Vector2::new(hw as f32, hh as f32));
        let _ = self.sprite.SetSize(Vector2::new(w as f32, h as f32));
        let _ = self.sprite.SetOffset(Vector3::new(((hw - w) / 2.0) as f32, ((hh - h) / 2.0) as f32, 0.0));
        let _ = self.sprite.SetCenterPoint(Vector3::new((w / 2.0) as f32, (h / 2.0) as f32, 0.0));
        let _ = self.root.SetTransformMatrix(matrix(&cards::perspective(hw, hh)));
        let (shear, rotation) = cards::solve_edge_angles(top, bottom, h);
        let _ = self.sprite.SetRotationAxis(Vector3::new(0.0, 1.0, 0.0));
        let _ = self.sprite.SetRotationAngleInDegrees(rotation as f32);
        let _ = self.content.SetTransformMatrix(matrix(&cards::compose_transform(shear, self.scale, w, hw, hh)));
        if self.clip.is_none() {
            if let (Ok(g), Ok(c)) = (self.compositor.CreateRoundedRectangleGeometry(), self.compositor.CreateGeometricClip()) {
                let _ = c.SetGeometry(&g);
                let _ = self.sprite.SetClip(&c);
                self.clip = Some((c, g));
            }
        }
        if let Some((_, g)) = &self.clip {
            let _ = g.SetCornerRadius(Vector2::new(radius as f32, radius as f32));
            let _ = g.SetSize(Vector2::new(w as f32, h as f32));
        }
    }

    pub fn set_visible(&self, on: bool) {
        let _ = self.root.SetIsVisible(on);
    }

    pub fn set_opacity(&self, a: f32) {
        let _ = self.root.SetOpacity(a.clamp(0.0, 1.0));
    }

    /// A captured frame is on the card (`CaptureSession.HasFrame`).
    pub fn has_frame(&self) -> bool {
        self.frame.load(Ordering::Acquire)
    }
}

impl Drop for Tile {
    /// `Dispose` of the capture objects, each wrapped: the session may already be gone.
    fn drop(&mut self) {
        let _ = self.pool.RemoveFrameArrived(self.token);
        let _ = self.session.Close();
        let _ = self.pool.Close();
        let _ = &self.item;
    }
}
