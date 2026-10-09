//! End-to-end check against a real agent: launch an app, receive its window and
//! H.264 frames, drive it with keyboard/mouse and observe the effect through the
//! window title the test app mirrors ("RM Test App [N chars]").

use crate::Session;
use rm_protocol::{Frame, Message, Modifier, MouseButton, ProtocolError, Rect, VideoFrame};
use std::io::{Read, Write};
use std::time::{Duration, Instant};

#[derive(Default, Debug)]
pub struct Report {
    pub checks: Vec<(String, bool, String)>,
    pub video_frames: usize,
    pub keyframes: usize,
    pub video_bytes: usize,
    pub first_frame_ms: Option<u128>,
    pub first_pts_us: Option<u64>,
    pub last_pts_us: u64,
    /// frames of the first, unbroken stream (a stream restarted at a new size starts its clock over)
    pub fps_frames: usize,
    pub fps_done: bool,
    pub window: Option<(u64, Rect)>,
    pub titles: Vec<String>,
    pub decoded: usize,
    pub decode_errors: usize,
    /// (width, height, distinct colours) of the most recent decoded picture.
    pub last_picture: Option<(usize, usize, usize)>,
}

impl Report {
    /// Frames per second over the span of the received frames' capture timestamps.
    pub fn fps(&self) -> f64 {
        match self.first_pts_us {
            Some(f) if self.last_pts_us > f && self.fps_frames > 1 => (self.fps_frames - 1) as f64 * 1e6 / (self.last_pts_us - f) as f64,
            _ => 0.0,
        }
    }

    fn check(&mut self, name: &str, ok: bool, detail: impl Into<String>) {
        let detail = detail.into();
        eprintln!("[{}] {name}: {detail}", if ok { "PASS" } else { "FAIL" });
        self.checks.push((name.into(), ok, detail));
    }
    pub fn all_ok(&self) -> bool {
        !self.checks.is_empty() && self.checks.iter().all(|c| c.1)
    }
}

struct Ctx<'a, S: Read + Write> {
    sess: &'a mut Session<S>,
    r: Report,
    started: Instant,
    first_video: Option<VideoFrame>,
    last_title: String,
    destroyed: bool,
    exited: bool,
    launched_pid: Option<u32>,
    errors: Vec<String>,
    non_annexb: usize,
    decoder: Option<rm_decode::H264Decoder>,
    clipboard: Option<String>,
    icon: Option<(u32, Vec<u8>)>,
    /// every WindowCreated: (id, role, parent)
    created: Vec<(u64, rm_protocol::WindowRole, Option<u64>)>,
    destroyed_ids: Vec<u64>,
    uploaded: Option<String>,
    menus: Option<Vec<rm_protocol::MenuNode>>,
    display: Option<(bool, u32, u32, Option<String>)>,
    /// last WindowMoved size of the main window, and the last video size seen
    moved: Option<(u32, u32)>,
    video_size: Option<(u16, u16)>,
    /// main window's current content rect (screen points)
    main_rect: Option<rm_protocol::Rect>,
    /// the Mac Desktop window: id, bounds, frames seen, last video size
    desktop: Option<(u64, rm_protocol::Rect)>,
    desktop_frames: usize,
    desktop_video: Option<(u16, u16)>,
    /// Mac Desktop over full GameStream: the client tunnel and the messages it wants sent
    gs_tunnel: Option<std::sync::Arc<rm_gamestream::tunnel::ClientTunnel>>,
    gs_out: Option<std::sync::mpsc::Receiver<Message>>,
    /// sound: the Mac's last audio_status, packets seen, bad packets, loudest sample, last seq
    audio_status: Option<(String, Option<String>)>,
    audio_packets: usize,
    audio_bad: usize,
    audio_peak: i16,
    audio_seq: Option<u32>,
    audio_gaps: usize,
}

fn is_timeout(e: &ProtocolError) -> bool {
    matches!(e, ProtocolError::Io(io) if matches!(io.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut))
}

impl<S: Read + Write> Ctx<'_, S> {
    fn absorb(&mut self, f: Frame) {
        match f {
            Frame::Audio(a) => {
                self.audio_packets += 1;
                if a.channels != 2 || a.frames() != rm_protocol::audio::PACKET_FRAMES {
                    self.audio_bad += 1;
                }
                if self.audio_seq.is_some_and(|s| a.seq != s.wrapping_add(1)) {
                    self.audio_gaps += 1;
                }
                self.audio_seq = Some(a.seq);
                self.audio_peak = self.audio_peak.max(a.samples.iter().map(|s| s.saturating_abs()).max().unwrap_or(0));
            }
            Frame::Msg(Message::AudioStatus { state, reason }) => self.audio_status = Some((state, reason)),
            Frame::Video(v) if self.desktop.map(|d| d.0) == Some(v.window_id) => {
                self.desktop_frames += 1;
                self.desktop_video = Some((v.width, v.height));
            }
            Frame::Video(v) => {
                if self.r.first_frame_ms.is_none() {
                    self.r.first_frame_ms = Some(self.started.elapsed().as_millis());
                    self.first_video = Some(v.clone());
                }
                self.r.first_pts_us.get_or_insert(v.pts_us);
                if !self.r.fps_done && v.pts_us >= self.r.last_pts_us {
                    self.r.last_pts_us = v.pts_us;
                    self.r.fps_frames += 1;
                } else {
                    self.r.fps_done = true;
                }
                self.r.video_frames += 1;
                if self.r.window.map(|w| w.0) == Some(v.window_id) {
                    self.video_size = Some((v.width, v.height));
                }
                self.r.keyframes += v.keyframe as usize;
                self.r.video_bytes += v.data.len();
                if !v.has_start_code() {
                    self.non_annexb += 1;
                }
                if let (Some(d), true) = (self.decoder.as_mut(), self.r.window.is_none_or(|w| w.0 == v.window_id)) {
                    match d.decode(&v.data) {
                        Ok(Some(p)) => {
                            self.r.decoded += 1;
                            self.r.last_picture = Some((p.width, p.height, p.distinct_colors()));
                        }
                        Ok(None) => {}
                        Err(_) => self.r.decode_errors += 1,
                    }
                }
            }
            Frame::Msg(Message::WindowCreated { window_id, bounds, application_id, .. }) if application_id == "desktop" => {
                self.desktop = Some((window_id, bounds));
            }
            Frame::Msg(Message::WindowCreated { window_id, bounds, title, role, parent_id, .. }) => {
                self.created.push((window_id, role, parent_id));
                if role == rm_protocol::WindowRole::Window && self.r.window.is_none() {
                    self.main_rect = Some(bounds);
                }
                if role != rm_protocol::WindowRole::Window {
                    return;
                }
                if self.r.window.is_none() {
                    self.r.window = Some((window_id, bounds));
                }
                self.last_title = title.clone();
                self.r.titles.push(title);
            }
            Frame::Msg(Message::WindowTitleChanged { title, .. }) => {
                self.last_title = title.clone();
                self.r.titles.push(title);
            }
            Frame::Msg(Message::WindowDestroyed { window_id }) => {
                self.destroyed_ids.push(window_id);
                if self.r.window.map(|w| w.0) == Some(window_id) {
                    self.destroyed = true;
                }
            }
            Frame::Msg(Message::FileUploaded { remote_path, .. }) => self.uploaded = Some(remote_path),
            Frame::Msg(Message::FileUploadFailed { reason, .. }) => self.errors.push(format!("upload: {reason}")),
            Frame::Msg(Message::MenuBar { menus, .. }) => self.menus = Some(menus),
            Frame::Msg(Message::DisplayStatus { available, width, height, reason, .. }) => self.display = Some((available, width, height, reason)),
            Frame::Msg(Message::WindowMoved { window_id, bounds }) if self.r.window.map(|w| w.0) == Some(window_id) => {
                self.moved = Some((bounds.w, bounds.h));
                self.main_rect = Some(bounds);
            }
            Frame::Msg(Message::AppExited { .. }) => self.exited = true,
            Frame::Msg(Message::AppLaunched { pid, .. }) => self.launched_pid = Some(pid),
            Frame::Msg(Message::ClipboardSet { text, .. }) => self.clipboard = Some(text),
            Frame::Msg(Message::AppIcon { size, rgba_base64, .. }) => {
                self.icon = Some((size, rm_protocol::base64_decode(&rgba_base64).unwrap_or_default()))
            }
            Frame::Msg(Message::Error { code, message }) => self.errors.push(format!("{code}: {message}")),
            Frame::Msg(Message::GsTunnel { id, op, data_base64 }) => {
                if let Some(t) = &self.gs_tunnel {
                    match op.as_str() {
                        "data" => t.tcp_from_host(id, &rm_protocol::base64_decode(&data_base64).unwrap_or_default()),
                        "close" => t.tcp_close_from_host(id),
                        _ => {}
                    }
                }
            }
            Frame::Msg(_) => {}
        }
    }

    /// Read frames until `done` holds or `secs` elapse. Returns whether `done` held.
    fn pump(&mut self, secs: u64, done: impl Fn(&Self) -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < deadline {
            if done(self) {
                return true;
            }
            // what the GameStream tunnel wants sent to the Mac (RTSP over this connection)
            let out: Vec<Message> = self.gs_out.as_ref().map(|rx| rx.try_iter().collect()).unwrap_or_default();
            for m in out {
                self.send(m);
            }
            match self.sess.recv() {
                Ok(Some(f)) => self.absorb(f),
                Ok(None) => return done(self),
                Err(e) if is_timeout(&e) => {}
                Err(e) => {
                    self.errors.push(format!("recv: {e}"));
                    return false;
                }
            }
        }
        done(self)
    }

    fn send(&mut self, m: Message) {
        if let Err(e) = self.sess.send(&m) {
            self.errors.push(format!("send: {e}"));
        }
    }
}

/// Run the scenario. The stream should have a ~1 s read timeout set by the caller.
pub fn run<S: Read + Write>(sess: &mut Session<S>, app: &str) -> Report {
    let caps_ok = sess.capabilities.can_stream_apps();
    let caps_dump = serde_json::to_string(&sess.capabilities).unwrap_or_default();
    let mut c = Ctx { sess, r: Report::default(), started: Instant::now(), first_video: None, last_title: String::new(),
        destroyed: false, exited: false, launched_pid: None, errors: vec![], non_annexb: 0,
        decoder: rm_decode::H264Decoder::new().ok(), clipboard: None, icon: None, created: vec![], destroyed_ids: vec![], uploaded: None, menus: None, display: None, moved: None, video_size: None, main_rect: None, desktop: None, desktop_frames: 0, desktop_video: None, gs_tunnel: None, gs_out: None, audio_status: None, audio_packets: 0, audio_bad: 0, audio_peak: 0, audio_seq: None, audio_gaps: 0 };

    c.r.check("agent reports capture+input+GUI", caps_ok, caps_dump);

    match c.sess.list_apps() {
        Ok(apps) => {
            let ok = apps.iter().any(|a| a.id == app && a.available);
            c.r.check("app listed and installed", ok, format!("{:?}", apps.iter().map(|a| (&a.id, a.available)).collect::<Vec<_>>()));
        }
        Err(e) => c.r.check("app listed and installed", false, e.to_string()),
    }

    // an id outside the allowlist must be refused
    c.send(Message::AppLaunch { application_id: "/bin/sh".into(), arguments: vec![], working_directory: None, environment: Default::default() });
    let refused = c.pump(5, |c| !c.errors.is_empty());
    c.r.check("non-allowlisted launch refused", refused, format!("{:?}", c.errors));
    c.errors.clear();

    c.started = Instant::now();
    c.send(Message::AppLaunch { application_id: app.into(), arguments: vec![], working_directory: None, environment: Default::default() });
    let launched = c.pump(10, |c| c.launched_pid.is_some());
    c.r.check("app launched", launched, format!("pid={:?} errors={:?}", c.launched_pid, c.errors));
    if !launched {
        return c.r;
    }

    let win = c.pump(25, |c| c.r.window.is_some());
    c.r.check("WindowCreated received", win, format!("{:?}", c.r.window));
    let Some((wid, _)) = c.r.window else { return c.r };

    // ---- video
    c.pump(8, |c| c.r.video_frames >= 60);
    let first_ok = c.first_video.as_ref().map(|v| {
        let n = v.nal_types();
        v.keyframe && n.contains(&7) && n.contains(&8) && n.contains(&5)
    });
    c.r.check("first frame is a keyframe with SPS+PPS+IDR (Annex-B)", first_ok == Some(true),
        format!("{:?}", c.first_video.as_ref().map(|v| (v.keyframe, v.nal_types(), v.width, v.height))));
    let (n, bytes, kf, t) = (c.r.video_frames, c.r.video_bytes, c.r.keyframes, c.r.first_frame_ms);
    c.r.check("video stream flows (>=30 frames, all Annex-B)", n >= 30 && c.non_annexb == 0,
        format!("frames={n} keyframes={kf} bytes={bytes} firstFrameMs={t:?} nonAnnexB={}", c.non_annexb));

    let (dec, errs, pic) = (c.r.decoded, c.r.decode_errors, c.r.last_picture);
    let want = c.r.window.map(|(_, b)| (b.w as usize, b.h as usize));
    let pic_ok = matches!((pic, want), (Some((w, h, colors)), Some((ww, wh))) if (w, h) == (ww, wh) && colors > 8);
    c.r.check("frames decode to window-sized, non-blank pictures", dec >= 30 && pic_ok,
        format!("decoded={dec} decodeErrors={errs} lastPicture(w,h,colors)={pic:?} windowBounds={want:?}"));

    // ---- keyboard: unicode text
    c.send(Message::TextInput { window_id: wid, text: "hello".into() });
    let ok = c.pump(8, |c| c.last_title.contains("[5 chars]"));
    c.r.check("TextInput reaches the app", ok, c.last_title.clone());

    // ---- mouse click, then keyboard still works
    let pt = (20.0, 70.0);
    c.send(Message::MouseMove { window_id: wid, x: pt.0, y: pt.1 });
    std::thread::sleep(Duration::from_millis(150));
    c.send(Message::MouseButton { window_id: wid, button: MouseButton::Left, down: true, x: pt.0, y: pt.1 });
    std::thread::sleep(Duration::from_millis(120));
    c.send(Message::MouseButton { window_id: wid, button: MouseButton::Left, down: false, x: pt.0, y: pt.1 });
    std::thread::sleep(Duration::from_millis(400));
    c.send(Message::TextInput { window_id: wid, text: "X".into() });
    let ok = c.pump(8, |c| c.last_title.contains("[6 chars]"));
    c.r.check("keyboard works after a mouse click", ok, c.last_title.clone());

    // ---- physical key (KeyA) -> "a"
    c.send(Message::Key { window_id: wid, physical_key: "KeyA".into(), modifiers: vec![], down: true });
    c.send(Message::Key { window_id: wid, physical_key: "KeyA".into(), modifiers: vec![], down: false });
    let ok = c.pump(8, |c| c.last_title.contains("[7 chars]"));
    c.r.check("physical key KeyA reaches the app", ok, c.last_title.clone());

    // Cmd+A then Delete empties the field
    c.send(Message::Key { window_id: wid, physical_key: "KeyA".into(), modifiers: vec![Modifier::Command], down: true });
    c.send(Message::Key { window_id: wid, physical_key: "KeyA".into(), modifiers: vec![Modifier::Command], down: false });
    c.send(Message::Key { window_id: wid, physical_key: "Backspace".into(), modifiers: vec![], down: true });
    c.send(Message::Key { window_id: wid, physical_key: "Backspace".into(), modifiers: vec![], down: false });
    let ok = c.pump(8, |c| c.last_title.contains("[0 chars]"));
    c.r.check("Cmd+A, Backspace clears the text (modifiers work)", ok, c.last_title.clone());

    // ---- clipboard, Windows -> Mac: set it, then Cmd+V in the app
    let key = |c: &mut Ctx<S>, k: &str, mods: Vec<Modifier>| {
        c.send(Message::Key { window_id: wid, physical_key: k.into(), modifiers: mods.clone(), down: true });
        c.send(Message::Key { window_id: wid, physical_key: k.into(), modifiers: mods, down: false });
    };
    c.send(Message::ClipboardSet { seq: 1, text: "pasted".into() });
    std::thread::sleep(Duration::from_millis(600));
    key(&mut c, "KeyV", vec![Modifier::Command]);
    let ok = c.pump(8, |c| c.last_title.contains("[6 chars]"));
    c.r.check("clipboard client->Mac (ClipboardSet then Cmd+V pastes 'pasted')", ok, c.last_title.clone());

    // ---- clipboard, Mac -> Windows: Cmd+A, Cmd+C in the app comes back as ClipboardSet
    c.send(Message::TextInput { window_id: wid, text: "!".into() });
    c.pump(5, |c| c.last_title.contains("[7 chars]"));
    c.clipboard = None;
    key(&mut c, "KeyA", vec![Modifier::Command]);
    key(&mut c, "KeyC", vec![Modifier::Command]);
    let ok = c.pump(8, |c| c.clipboard.as_deref() == Some("pasted!"));
    c.r.check("clipboard Mac->client (Cmd+C arrives as ClipboardSet 'pasted!')", ok, format!("{:?}", c.clipboard));

    // ---- app icon for the taskbar
    c.send(Message::GetAppIcon { application_id: app.into() });
    let ok = c.pump(8, |c| c.icon.is_some());
    let detail = c.icon.as_ref().map(|(s, px)| {
        let opaque = px.as_chunks::<4>().0.iter().filter(|p| p[3] > 0).count();
        (ok && *s >= 16 && px.len() == (*s * *s * 4) as usize && opaque > 0, format!("size={s} bytes={} opaquePixels={opaque}", px.len()))
    });
    let (icon_ok, icon_detail) = detail.unwrap_or((false, "no AppIcon".into()));
    c.r.check("app icon delivered (square RGBA, not empty)", icon_ok, icon_detail);
    c.send(Message::WindowFocus { window_id: wid });

    // ---- the menu bar (shown by clients inside the app's windows), and running an item from it
    c.menus = None;
    c.send(Message::GetMenuBar { application_id: app.into() });
    let got = c.pump(10, |c| c.menus.is_some());
    let menus = c.menus.clone().unwrap_or_default();
    let tops: Vec<&str> = menus.iter().map(|m| m.title.as_str()).collect();
    let open_sc = menus.iter().find(|m| m.title == "File").and_then(|f| f.children.iter().find(|i| i.title.starts_with("Open"))).and_then(|i| i.shortcut.clone());
    c.r.check("menu bar read (File > Open… with Cmd+O)", got && open_sc.as_deref() == Some("Cmd+O"), format!("tops={tops:?} open={open_sc:?}"));
    let about = menus.iter().enumerate().find_map(|(t, m)| m.children.iter().position(|i| i.title.starts_with("About")).map(|i| vec![t as u32, i as u32]));
    if let Some(path) = about {
        let before = c.created.len();
        c.send(Message::MenuInvoke { application_id: app.into(), path: path.clone() });
        let found = c.pump(10, move |c| c.created[before..].iter().any(|(_, r, _)| *r == rm_protocol::WindowRole::Dialog));
        let dlg = c.created[before..].iter().find(|(_, r, _)| *r == rm_protocol::WindowRole::Dialog).copied();
        c.r.check("menu item invoked from the client runs on the Mac (About opens its dialog)", found, format!("path={path:?} created={:?} errors={:?}", c.created, c.errors));
        if let Some((did, _, _)) = dlg {
            c.send(Message::WindowClose { window_id: did });
            c.pump(8, |c| c.destroyed_ids.contains(&did));
        }
    } else {
        c.r.check("menu item invoked from the client runs on the Mac (About opens its dialog)", false, "no About item");
    }
    c.send(Message::WindowFocus { window_id: wid });

    // ---- an app dialog appears as a child window of its parent, and can be closed from the client
    let before = c.created.len();
    key(&mut c, "KeyI", vec![Modifier::Command]);
    let found = c.pump(10, move |c| c.created[before..].iter().any(|(_, r, _)| *r == rm_protocol::WindowRole::Dialog));
    let dlg = c.created[before..].iter().find(|(_, r, _)| *r == rm_protocol::WindowRole::Dialog).copied();
    c.r.check("app dialog reported as a child window (role=dialog, parent=main)", found && dlg.map(|d| d.2) == Some(Some(wid)), format!("created={:?}", c.created));
    if let Some((did, _, _)) = dlg {
        c.send(Message::WindowClose { window_id: did });
        let ok = c.pump(8, |c| c.destroyed_ids.contains(&did));
        c.r.check("closing the dialog from the client closes it on the Mac", ok, format!("destroyed={:?}", c.destroyed_ids));
    }

    // ---- the app's file Open panel: upload a file, have the panel open it
    key(&mut c, "KeyO", vec![Modifier::Command]);
    let found = c.pump(12, |c| c.created.iter().any(|(_, r, _)| *r == rm_protocol::WindowRole::OpenPanel));
    let panel = c.created.iter().find(|(_, r, _)| *r == rm_protocol::WindowRole::OpenPanel).copied();
    c.r.check("file Open panel reported (role=open_panel, parent=main)", found && panel.map(|p| p.2) == Some(Some(wid)), format!("created={:?}", c.created));
    let payload: Vec<u8> = (0..716_800u32).map(|i| (i % 251) as u8).collect();
    let tid = 9u64;
    c.send(Message::FileUploadBegin { transfer_id: tid, name: "../rm e2e upload.bin".into(), size: payload.len() as u64 });
    for (i, chunk) in payload.chunks(rm_protocol::UPLOAD_CHUNK).enumerate() {
        c.send(Message::FileUploadChunk { transfer_id: tid, offset: (i * rm_protocol::UPLOAD_CHUNK) as u64, data_base64: rm_protocol::base64_encode(chunk) });
    }
    c.send(Message::FileUploadEnd { transfer_id: tid });
    let ok = c.pump(15, |c| c.uploaded.is_some());
    let path = c.uploaded.clone().unwrap_or_default();
    c.r.check("upload lands in the uploads folder with a sanitised name", ok && path.ends_with("/RemoteMac Uploads/rm e2e upload.bin") && !path.contains(".."), format!("path={path:?} errors={:?}", c.errors));
    if let (Some((pid, _, _)), true) = (panel, ok) {
        c.send(Message::PanelChooseFile { window_id: pid, remote_path: path });
        let ok = c.pump(15, |c| c.last_title.contains("[opened rm e2e upload.bin 716800 bytes]"));
        c.r.check("panel opens the uploaded file in the app", ok, c.last_title.clone());
    }

    // ---- a virtual display the size of the client's monitor; fullscreen fills it exactly
    c.send(Message::DisplayConfigure { width: 1280, height: 720, scale: 1 });
    let got = c.pump(15, |c| c.display.is_some());
    let d = c.display.clone();
    c.r.check("virtual display at the client's monitor size (1280x720)", got && matches!(d, Some((true, 1280, 720, _))), format!("{d:?}"));
    c.send(Message::WindowFullscreen { window_id: wid, on: true });
    let ok = c.pump(12, |c| c.moved == Some((1280, 720)));
    let ok_video = ok && c.pump(12, |c| c.video_size == Some((1280, 720)));
    c.r.check("fullscreen: window content and video are exactly the client's monitor", ok && ok_video, format!("moved={:?} video={:?} errors={:?}", c.moved, c.video_size, c.errors));
    c.send(Message::WindowFullscreen { window_id: wid, on: false });
    let back = c.pump(12, |c| c.moved.is_some_and(|m| m.0 < 1000));
    c.r.check("leaving fullscreen restores the window", back, format!("moved={:?}", c.moved));

    // ---- Mac Desktop: the whole screen as one window; input on it reaches the app under the pointer
    c.send(Message::AppLaunch { application_id: "desktop".into(), arguments: vec![], working_directory: None, environment: Default::default() });
    let up = c.pump(12, |c| c.desktop.is_some() && c.desktop_frames >= 3);
    let d = c.desktop;
    let size_ok = d.is_some_and(|(_, b)| c.desktop_video == Some((b.w as u16, b.h as u16)) && b.w >= 640);
    c.r.check("Mac Desktop streams the whole screen", up && size_ok, format!("desktop={d:?} frames={} video={:?}", c.desktop_frames, c.desktop_video));
    if let (Some((did, db)), Some(r)) = (d, c.main_rect) {
        // click into the test app through the desktop (desktop coordinates = screen points)
        let (x, y) = ((r.x - db.x) as f64 + 40.0, (r.y - db.y) as f64 + 40.0);
        let before = c.last_title.clone();
        for down in [true, false] {
            c.send(Message::MouseButton { window_id: did, button: rm_protocol::MouseButton::Left, down, x, y });
        }
        c.pump(1, |_| false);
        c.send(Message::TextInput { window_id: did, text: "d".into() });
        let ok = c.pump(10, |c| c.last_title != before && c.last_title.contains("chars]"));
        c.r.check("input on the desktop reaches the app under the pointer", ok, format!("before={before:?} after={:?} click=({x},{y})", c.last_title));
        sound(&mut c);
        c.send(Message::AppTerminate { application_id: "desktop".into() });
        let gone = c.pump(8, |c| c.destroyed_ids.contains(&did));
        c.r.check("closing the Mac Desktop stops its stream", gone, format!("destroyed={:?}", c.destroyed_ids));
    } else {
        c.r.check("input on the desktop reaches the app under the pointer", false, "no desktop or main window");
    }

    // ---- Mac Desktop over full GameStream: Moonlight's client core (moonlight-common-c) ->
    // tunnel -> the agent's Sunshine-style host session (Rust, linked into the Swift agent)
    gamestream_desktop(&mut c);

    // ---- lifecycle
    c.send(Message::AppTerminate { application_id: app.into() });
    let ok = c.pump(10, |c| c.destroyed && c.exited);
    c.r.check("terminate -> WindowDestroyed + AppExited", ok, format!("destroyed={} exited={}", c.destroyed, c.exited));
    c.r
}

/// Sound while the Mac Desktop is open (every app is heard): asked for, received as valid PCM
/// packets, stopped when asked. A runner without an audio device may have nothing to capture:
/// then the Mac must say so (`unavailable` with a reason) instead of staying silent.
fn sound<S: Read + Write>(c: &mut Ctx<S>) {
    c.send(Message::AudioControl { enabled: true });
    // something to hear (the Mac's own alert sound)
    #[cfg(target_os = "macos")]
    let player = std::process::Command::new("afplay").args(["-v", "0.3", "/System/Library/Sounds/Submarine.aiff"]).spawn().ok();
    let answered = c.pump(8, |c| c.audio_status.is_some() && (c.audio_packets >= 40 || c.audio_status.as_ref().is_some_and(|s| s.0 != "playing")));
    let status = c.audio_status.clone();
    let ok = answered && status.as_ref().is_some_and(|(s, r)| s == "playing" || (s == "unavailable" && r.as_ref().is_some_and(|r| !r.is_empty())));
    c.r.check("sound: the Mac answers the request (playing, or unavailable with a reason)", ok, format!("{status:?}"));
    eprintln!("[INFO] sound packets: {} (peak {}, gaps {}, malformed {})", c.audio_packets, c.audio_peak, c.audio_gaps, c.audio_bad);
    if c.audio_packets > 0 {
        c.r.check("sound packets are 48 kHz stereo PCM in 5 ms packets", c.audio_bad == 0, format!("packets={} malformed={}", c.audio_packets, c.audio_bad));
    }
    #[cfg(target_os = "macos")]
    if let Some(mut p) = player {
        let _ = p.kill();
        let _ = p.wait();
    }
    if status.is_some_and(|s| s.0 == "playing") {
        c.send(Message::AudioControl { enabled: false });
        let stopped = c.pump(5, |c| c.audio_status.as_ref().is_some_and(|s| s.0 == "stopped"));
        c.pump(1, |_| false);
        let before = c.audio_packets;
        c.pump(1, |_| false);
        c.r.check("sound stops when the viewer asks", stopped && c.audio_packets <= before + 2, format!("status={:?} packets in the last second={}", c.audio_status, c.audio_packets - before));
    }
}

fn gamestream_desktop<S: Read + Write>(c: &mut Ctx<S>) {
    use rm_gamestream::tunnel::{ClientTunnel, ToHost};
    let Some(udp) = c.sess.udp() else {
        c.r.check("Mac Desktop over full GameStream (Moonlight client core through the tunnel)", false, "no UDP path");
        return;
    };
    let key = rm_protocol::udp::random_secret();
    let (tx, rx) = std::sync::mpsc::channel::<Message>();
    let u2 = udp.clone();
    let tunnel = match ClientTunnel::start(std::sync::Arc::new(move |m: ToHost| {
        let msg = match m {
            ToHost::Udp { kind, data } => return u2.send_tunnel(kind, data),
            ToHost::TcpOpen { id } => Message::GsTunnel { id, op: "open".into(), data_base64: String::new() },
            ToHost::TcpData { id, data } => Message::GsTunnel { id, op: "data".into(), data_base64: rm_protocol::base64_encode(data) },
            ToHost::TcpClose { id } => Message::GsTunnel { id, op: "close".into(), data_base64: String::new() },
        };
        let _ = tx.send(msg);
    })) {
        Ok(t) => t,
        Err(e) => {
            c.r.check("Mac Desktop over full GameStream (Moonlight client core through the tunnel)", false, format!("tunnel: {e}"));
            return;
        }
    };
    let t2 = tunnel.clone();
    udp.set_tunnel_handler(move |flow, data| t2.udp_from_host(flow, data));
    c.gs_tunnel = Some(tunnel.clone());
    c.gs_out = Some(rx);
    c.desktop = None;
    c.desktop_frames = 0;
    c.send(Message::AppLaunch { application_id: "desktop".into(), arguments: vec![format!("gamestream={}", rm_protocol::udp::hex(&key))], working_directory: None, environment: Default::default() });
    let up = c.pump(12, |c| c.desktop.is_some());
    // the picture is there at once, the usual way, while Moonlight still connects
    let shown = up && c.pump(8, |c| c.desktop_frames >= 1);
    c.r.check("Mac Desktop shows at once while GameStream connects (usual stream)", shown, format!("desktop={:?} usual frames={}", c.desktop, c.desktop_frames));
    let port = tunnel.rtsp_port;
    let connected = std::sync::Arc::new(std::sync::atomic::AtomicI32::new(i32::MIN));
    let c2 = connected.clone();
    if up {
        std::thread::spawn(move || {
            let p = rm_gamestream::moonlight::Params { rtsp_port: port, key, width: 1280, height: 720, fps: 60, bitrate_kbps: 20_000, packet_size: 1200, remote: true };
            let r = rm_gamestream::moonlight::connect(p, |_, _| {});
            c2.store(r.err().unwrap_or(0), std::sync::atomic::Ordering::SeqCst);
        });
    }
    // the handshake crosses the tunnel through this loop, then frames flow
    let streamed = up && c.pump(25, |_| rm_gamestream::moonlight::frames().0 >= 30);
    let (frames, idrs) = rm_gamestream::moonlight::frames();
    c.r.check(
        "Mac Desktop over full GameStream (Moonlight client core through the tunnel)",
        streamed && idrs >= 1,
        format!("desktop={:?} connect={} frames={frames} idr={idrs}", c.desktop, connected.load(std::sync::atomic::Ordering::SeqCst)),
    );
    if streamed {
        // GameStream's pictures arrive: the viewer says so, and the usual stream stops
        c.send(Message::GsTunnel { id: 0, op: "ready".into(), data_base64: String::new() });
        c.pump(1, |_| false);
        let before = c.desktop_frames;
        c.pump(2, |_| false);
        c.r.check("once GameStream carries the desktop, the usual stream stops", c.desktop_frames <= before + 2, format!("usual frames in 2 s after ready: {}", c.desktop_frames - before));
        // input through Moonlight's encrypted input stream reaches the Mac (the test app, still
        // in front, counts what is typed)
        let before = c.last_title.clone();
        rm_gamestream::moonlight::send_input(&rm_gamestream::Input::Text("m".into()));
        let ok = c.pump(8, |c| c.last_title != before && c.last_title.contains("chars]"));
        c.r.check("GameStream input (ENet) reaches the Mac", ok, format!("before={before:?} after={:?}", c.last_title));
        rm_gamestream::moonlight::stop();
    }
    if let Some(did) = c.desktop.map(|d| d.0) {
        c.send(Message::AppTerminate { application_id: "desktop".into() });
        c.pump(8, |c| c.destroyed_ids.contains(&did));
    }
    c.gs_tunnel = None;
}
