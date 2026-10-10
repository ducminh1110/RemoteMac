//! The Settings window (launcher button, or Ctrl+Alt+Shift+P in any remote window): frame
//! rate, bitrate, sharpness, decoder, frame pacing, pointer — as Moonlight's settings page.

use crate::settings::{Settings, BITRATES, DECODERS, DESKTOP_SCALES, FPS, GLASS_LEVELS, KEYBOARD_MODES, MOTION_LEVELS, QUALITY, WINDOW_FRAMES, WORKSPACES};
use std::cell::RefCell;
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub const CLASS: PCWSTR = w!("RmSettings");
const ID_SAVE: usize = 100;
const ID_CANCEL: usize = 101;

struct Ui {
    hwnd: HWND,
    fps: HWND,
    bitrate: HWND,
    quality: HWND,
    desktop: HWND,
    workspace: HWND,
    decoder: HWND,
    pacing: HWND,
    cursor: HWND,
    audio: HWND,
    volume: HWND,
    keyboard: HWND,
    glass: HWND,
    motion: HWND,
    frame: HWND,
    dock: HWND,
    font: HFONT,
    on_save: Box<dyn Fn(Settings)>,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

pub fn register(hinst: HINSTANCE) {
    unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: hinst,
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH((COLOR_WINDOW.0 + 1) as isize as *mut _),
            ..Default::default()
        };
        RegisterClassW(&wc);
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn combo(parent: HWND, hinst: HINSTANCE, x: i32, y: i32, w: i32, items: &[String], sel: usize, font: HFONT) -> HWND {
    let h = CreateWindowExW(WINDOW_EX_STYLE(0), w!("COMBOBOX"), w!(""), WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | WS_VSCROLL.0 | CBS_DROPDOWNLIST as u32), x, y, w, 300, Some(parent), None, Some(hinst), None).unwrap_or_default();
    SendMessageW(h, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
    for it in items {
        let t = HSTRING::from(it.as_str());
        SendMessageW(h, CB_ADDSTRING, None, Some(LPARAM(t.as_ptr() as isize)));
    }
    SendMessageW(h, CB_SETCURSEL, Some(WPARAM(sel)), None);
    h
}

#[allow(clippy::too_many_arguments)]
unsafe fn label(parent: HWND, hinst: HINSTANCE, x: i32, y: i32, w: i32, h: i32, text: &str, font: HFONT) {
    let h = CreateWindowExW(WINDOW_EX_STYLE(0), w!("STATIC"), &HSTRING::from(text), WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0), x, y, w, h, Some(parent), None, Some(hinst), None).unwrap_or_default();
    SendMessageW(h, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
}

#[allow(clippy::too_many_arguments)]
unsafe fn button(parent: HWND, hinst: HINSTANCE, x: i32, y: i32, w: i32, text: &str, id: usize, style: u32, font: HFONT) -> HWND {
    let h = CreateWindowExW(WINDOW_EX_STYLE(0), w!("BUTTON"), &HSTRING::from(text), WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | style), x, y, w, 26, Some(parent), Some(HMENU(id as *mut _)), Some(hinst), None).unwrap_or_default();
    SendMessageW(h, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
    h
}

/// Open the window (or bring it forward). `on_save` gets the new settings.
pub fn show(hinst: HINSTANCE, owner: Option<HWND>, current: Settings, on_save: impl Fn(Settings) + 'static) {
    if let Some(h) = UI.with(|u| u.borrow().as_ref().map(|u| u.hwnd)) {
        unsafe {
            let _ = SetForegroundWindow(h);
        }
        return;
    }
    unsafe {
        let s = crate::native::dpi_scale(owner.unwrap_or_default()).max(1.0);
        let px = |v: i32| (v as f64 * s).round() as i32;
        let Ok(hwnd) = CreateWindowExW(WS_EX_DLGMODALFRAME, CLASS, w!("MacBridge — Settings"), WS_POPUP | WS_CAPTION | WS_SYSMENU | WS_VISIBLE, CW_USEDEFAULT, CW_USEDEFAULT, px(470), px(400), owner, None, Some(hinst), None) else { return };
        let font = CreateFontW(-px(14), 0, 0, 0, 400, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, &HSTRING::from(crate::native::ui_face(400)));
        let (lx, cx, cw) = (px(18), px(160), px(290));
        let mut y = px(18);
        let row = px(38);
        label(hwnd, hinst, lx, y + px(3), px(140), px(22), "Frame rate", font);
        let fps_items: Vec<String> = FPS.iter().map(|f| format!("{f} FPS")).collect();
        let fps = combo(hwnd, hinst, cx, y, cw, &fps_items, FPS.iter().position(|f| *f == current.fps).unwrap_or(1), font);
        y += row;
        label(hwnd, hinst, lx, y + px(3), px(140), px(22), "Bitrate", font);
        let br_items: Vec<String> = BITRATES.iter().map(|b| if *b == 0 { "Auto (adapts to the link)".into() } else { format!("{b} Mbit/s") }).collect();
        let bitrate = combo(hwnd, hinst, cx, y, cw, &br_items, BITRATES.iter().position(|b| *b == current.bitrate_mbps).unwrap_or(0), font);
        y += row;
        label(hwnd, hinst, lx, y + px(3), px(140), px(22), "Sharpness", font);
        let q_items: Vec<String> = QUALITY.iter().map(|x| x.to_string()).collect();
        let quality = combo(hwnd, hinst, cx, y, cw, &q_items, current.quality as usize, font);
        y += row;
        label(hwnd, hinst, lx, y + px(3), px(140), px(22), "Mac screen size", font);
        let (spx, sds) = (crate::net::screen_px(), crate::net::display_scale());
        let ws_items: Vec<String> = (0..WORKSPACES as u8)
            .map(|k| {
                let (w, h) = Settings::points_at(if spx.0 > 0 { spx } else { (1920, 1080) }, sds, k);
                let what = ["as large as this screen", "more space", "even more space", "most space", "pixel for pixel: sharpest, smallest text"][k as usize];
                format!("{w} × {h} ({what})")
            })
            .collect();
        let workspace = combo(hwnd, hinst, cx, y, cw, &ws_items, current.workspace as usize, font);
        y += row;
        label(hwnd, hinst, lx, y + px(3), px(140), px(22), "Mac Desktop scale", font);
        let ds_items: Vec<String> = DESKTOP_SCALES.iter().map(|x| x.to_string()).collect();
        let desktop = combo(hwnd, hinst, cx, y, cw, &ds_items, current.desktop_2x as usize, font);
        y += row;
        label(hwnd, hinst, lx, y + px(3), px(140), px(22), "Video decoder", font);
        let d_items: Vec<String> = DECODERS.iter().map(|x| x.to_string()).collect();
        let decoder = combo(hwnd, hinst, cx, y, cw, &d_items, current.decoder as usize, font);
        y += row;
        let pacing = button(hwnd, hinst, lx, y, px(450), "Frame pacing (smoother motion, up to one frame more delay)", 200, BS_AUTOCHECKBOX as u32, font);
        SendMessageW(pacing, BM_SETCHECK, Some(WPARAM(current.pacing as usize)), None);
        y += px(30);
        let cursor = button(hwnd, hinst, lx, y, px(450), "Use the Windows pointer instead of the Mac's (Ctrl+Alt+Shift+C)", 201, BS_AUTOCHECKBOX as u32, font);
        SendMessageW(cursor, BM_SETCHECK, Some(WPARAM(current.local_cursor as usize)), None);
        y += px(34);
        let audio = button(hwnd, hinst, lx, y, px(450), "Play the Mac's sound on this PC (Ctrl+Alt+Shift+M mutes)", 202, BS_AUTOCHECKBOX as u32, font);
        SendMessageW(audio, BM_SETCHECK, Some(WPARAM(current.audio as usize)), None);
        y += px(32);
        label(hwnd, hinst, lx, y + px(3), px(140), px(22), "Volume", font);
        let volume = CreateWindowExW(WINDOW_EX_STYLE(0), w!("msctls_trackbar32"), w!(""), WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0), cx, y, cw, px(28), Some(hwnd), None, Some(hinst), None).unwrap_or_default();
        SendMessageW(volume, windows::Win32::UI::Controls::TBM_SETRANGE, Some(WPARAM(1)), Some(LPARAM((100 << 16) as isize)));
        SendMessageW(volume, windows::Win32::UI::Controls::TBM_SETPOS, Some(WPARAM(1)), Some(LPARAM(current.volume as isize)));
        y += row;
        label(hwnd, hinst, lx, y + px(3), px(140), px(22), "Keyboard", font);
        let k_items: Vec<String> = KEYBOARD_MODES.iter().map(|x| x.to_string()).collect();
        let keyboard = combo(hwnd, hinst, cx, y, cw, &k_items, current.keyboard as usize, font);
        y += row;
        label(hwnd, hinst, lx, y + px(3), px(140), px(22), "Glass", font);
        let g_items: Vec<String> = GLASS_LEVELS.iter().map(|x| x.to_string()).collect();
        let glass = combo(hwnd, hinst, cx, y, cw, &g_items, current.glass as usize, font);
        y += row;
        label(hwnd, hinst, lx, y + px(3), px(140), px(22), "Animations", font);
        let m_items: Vec<String> = MOTION_LEVELS.iter().map(|x| x.to_string()).collect();
        let motion = combo(hwnd, hinst, cx, y, cw, &m_items, current.motion as usize, font);
        y += row;
        label(hwnd, hinst, lx, y + px(3), px(140), px(22), "Mac windows", font);
        let f_items: Vec<String> = WINDOW_FRAMES.iter().map(|x| x.to_string()).collect();
        let frame = combo(hwnd, hinst, cx, y, cw, &f_items, current.frame as usize, font);
        y += row;
        let dock = button(hwnd, hinst, lx, y, px(450), "Desktop Fusion: the Mac's Dock on this PC's wallpaper (experimental)", 203, BS_AUTOCHECKBOX as u32, font);
        SendMessageW(dock, BM_SETCHECK, Some(WPARAM(current.dock as usize)), None);
        y += px(34);
        label(hwnd, hinst, lx, y, px(445), px(44), "Decoder, frame pacing, Mac Desktop scale and how Mac windows look apply from the next connection or window opened.", font);
        y += px(52);
        button(hwnd, hinst, px(270), y, px(90), "Save", ID_SAVE, BS_DEFPUSHBUTTON as u32, font);
        button(hwnd, hinst, px(370), y, px(90), "Cancel", ID_CANCEL, 0, font);
        // the window as tall as what it holds (nothing cut off at any display scale)
        let mut r = RECT { left: 0, top: 0, right: px(480), bottom: y + px(26) + px(18) };
        let _ = AdjustWindowRectEx(&mut r, WS_POPUP | WS_CAPTION | WS_SYSMENU, false, WS_EX_DLGMODALFRAME);
        let _ = SetWindowPos(hwnd, None, 0, 0, r.right - r.left, r.bottom - r.top, SWP_NOMOVE | SWP_NOZORDER);
        UI.with(|u| *u.borrow_mut() = Some(Ui { hwnd, fps, bitrate, quality, desktop, workspace, decoder, pacing, cursor, audio, volume, keyboard, glass, motion, frame, dock, font, on_save: Box::new(on_save) }));
    }
}

fn read(u: &Ui) -> Settings {
    unsafe {
        let sel = |h: HWND| SendMessageW(h, CB_GETCURSEL, None, None).0.max(0) as usize;
        let checked = |h: HWND| SendMessageW(h, BM_GETCHECK, None, None).0 == 1;
        Settings {
            fps: FPS[sel(u.fps).min(FPS.len() - 1)],
            bitrate_mbps: BITRATES[sel(u.bitrate).min(BITRATES.len() - 1)],
            quality: sel(u.quality).min(QUALITY.len() - 1) as u8,
            decoder: sel(u.decoder).min(2) as u8,
            desktop_2x: sel(u.desktop) == 1,
            workspace: sel(u.workspace).min(WORKSPACES - 1) as u8,
            pacing: checked(u.pacing),
            local_cursor: checked(u.cursor),
            audio: checked(u.audio),
            volume: SendMessageW(u.volume, WM_USER, None, None).0.clamp(0, 100) as u8,
            keyboard: sel(u.keyboard).min(KEYBOARD_MODES.len() - 1) as u8,
            glass: sel(u.glass).min(GLASS_LEVELS.len() - 1) as u8,
            motion: sel(u.motion).min(MOTION_LEVELS.len() - 1) as u8,
            dock: checked(u.dock),
            frame: sel(u.frame).min(WINDOW_FRAMES.len() - 1) as u8,
        }
    }
}

fn close(hwnd: HWND) {
    let ui = UI.with(|u| u.borrow_mut().take());
    if let Some(u) = ui {
        unsafe {
            let _ = DeleteObject(u.font.into());
        }
    }
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            match wp.0 & 0xffff {
                ID_SAVE => {
                    let picked = UI.with(|u| u.borrow().as_ref().map(|u| (read(u), u.hwnd)));
                    if let Some((s, h)) = picked {
                        // take the callback out first: it may touch the viewer's state freely
                        let ui = UI.with(|u| u.borrow_mut().take());
                        if let Some(ui) = ui {
                            (ui.on_save)(s);
                            let _ = DeleteObject(ui.font.into());
                        }
                        let _ = DestroyWindow(h);
                    }
                }
                ID_CANCEL => close(hwnd),
                _ => {}
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            close(hwnd);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
