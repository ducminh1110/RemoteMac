//! The wizard's own window on Windows: the same pages, shown in a WebView2 control inside a
//! native window instead of a browser tab. The page stays on the local server (127.0.0.1, the
//! one-time token); the window allows no other origin: links elsewhere open in the default
//! browser, and new windows are never created.

use std::sync::Arc;
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::platform::run_return::EventLoopExtRunReturn;
use tao::window::{Icon, WindowBuilder};
use wry::WebViewBuilder;

/// What closes the window from elsewhere (the page's own shutdown).
#[derive(Debug, Clone, Copy)]
pub enum UserEvent {
    Close,
}

pub struct Shell {
    proxy: tao::event_loop::EventLoopProxy<UserEvent>,
}

impl Shell {
    pub fn close(&self) {
        let _ = self.proxy.send_event(UserEvent::Close);
    }
}

/// Taro's mark for the title bar and the taskbar: a rounded square in Taro's purple with a
/// white screen, drawn here so the binary carries no image file.
fn icon() -> Option<Icon> {
    const N: u32 = 64;
    let (r, g, b) = (0x7c, 0x4a, 0xe8);
    let mut px = Vec::with_capacity((N * N * 4) as usize);
    for y in 0..N {
        for x in 0..N {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            // rounded corners (radius 14)
            let rad = 14.0f32;
            let cx = fx.clamp(rad, N as f32 - rad);
            let cy = fy.clamp(rad, N as f32 - rad);
            let d = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
            let a = (rad + 0.5 - d).clamp(0.0, 1.0);
            let screen = (16..48).contains(&x) && (20..40).contains(&y);
            let stand = (28..36).contains(&x) && (40..46).contains(&y);
            let (pr, pg, pb) = if screen || stand {
                (255, 255, 255)
            } else {
                (r, g, b)
            };
            px.extend_from_slice(&[pr, pg, pb, (a * 255.0) as u8]);
        }
    }
    Icon::from_rgba(px, N, N).ok()
}

/// Run the window until it is closed (or the page shuts the wizard down). `on_ready` receives
/// a handle that closes the window from another thread. Returns an error when the window or
/// WebView2 cannot be created (the caller falls back to the browser).
pub fn run(url: &str, on_ready: impl FnOnce(Shell)) -> Result<(), String> {
    let origin = {
        let u = url::Url::parse(url).map_err(|e| e.to_string())?;
        u.origin().ascii_serialization()
    };
    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    // 1360x880, or less on a small screen (at most 92% x 88% of the primary monitor)
    let (mut w, mut h) = (1360.0f64, 880.0f64);
    if let Some(m) = event_loop.primary_monitor() {
        let size = m.size().to_logical::<f64>(m.scale_factor());
        w = w.min(size.width * 0.92).max(800.0);
        h = h.min(size.height * 0.88).max(560.0);
    }
    let window = WindowBuilder::new()
        .with_title("Taro Enterprise Setup")
        .with_inner_size(LogicalSize::new(w, h))
        .with_min_inner_size(LogicalSize::new(800.0, 560.0))
        .with_window_icon(icon())
        .build(&event_loop)
        .map_err(|e| format!("could not open the window: {e}"))?;

    let allowed = origin.clone();
    let allowed_new = Arc::new(origin);
    let webview = WebViewBuilder::new()
        .with_url(url)
        .with_devtools(cfg!(debug_assertions))
        .with_navigation_handler(move |to: String| {
            if to.starts_with(&format!("{allowed}/"))
                || to == allowed
                || to.starts_with("blob:")
                || to.starts_with("about:")
            {
                return true;
            }
            // anything else (a support link, the GitHub page of a release) opens outside
            if to.starts_with("https://") {
                let _ = crate::platform::open_browser(&to);
            }
            false
        })
        .with_new_window_req_handler(move |to: String, _features| {
            if to.starts_with("https://") && !to.starts_with(allowed_new.as_str()) {
                let _ = crate::platform::open_browser(&to);
            }
            wry::NewWindowResponse::Deny
        })
        .build(&window)
        .map_err(|e| {
            format!(
                "the Microsoft Edge WebView2 Runtime is not available ({e}). It comes with \
                 Windows 11 and current Windows 10; install it from \
                 https://developer.microsoft.com/microsoft-edge/webview2/"
            )
        })?;

    on_ready(Shell {
        proxy: event_loop.create_proxy(),
    });

    event_loop.run_return(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        // keep the WebView alive as long as the loop runs
        let _ = &webview;
        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            }
            | Event::UserEvent(UserEvent::Close) => *control_flow = ControlFlow::Exit,
            _ => {}
        }
    });
    Ok(())
}
