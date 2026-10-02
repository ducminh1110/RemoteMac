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
            Some(f) if self.last_pts_us > f && self.video_frames > 1 => (self.video_frames - 1) as f64 * 1e6 / (self.last_pts_us - f) as f64,
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
}

fn is_timeout(e: &ProtocolError) -> bool {
    matches!(e, ProtocolError::Io(io) if matches!(io.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut))
}

impl<S: Read + Write> Ctx<'_, S> {
    fn absorb(&mut self, f: Frame) {
        match f {
            Frame::Video(v) => {
                if self.r.first_frame_ms.is_none() {
                    self.r.first_frame_ms = Some(self.started.elapsed().as_millis());
                    self.first_video = Some(v.clone());
                }
                self.r.first_pts_us.get_or_insert(v.pts_us);
                self.r.last_pts_us = v.pts_us;
                self.r.video_frames += 1;
                self.r.keyframes += v.keyframe as usize;
                self.r.video_bytes += v.data.len();
                if !v.has_start_code() {
                    self.non_annexb += 1;
                }
                if let Some(d) = self.decoder.as_mut() {
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
            Frame::Msg(Message::WindowCreated { window_id, bounds, title, role, parent_id, .. }) => {
                self.created.push((window_id, role, parent_id));
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
            Frame::Msg(Message::AppExited { .. }) => self.exited = true,
            Frame::Msg(Message::AppLaunched { pid, .. }) => self.launched_pid = Some(pid),
            Frame::Msg(Message::ClipboardSet { text, .. }) => self.clipboard = Some(text),
            Frame::Msg(Message::AppIcon { size, rgba_base64, .. }) => {
                self.icon = Some((size, rm_protocol::base64_decode(&rgba_base64).unwrap_or_default()))
            }
            Frame::Msg(Message::Error { code, message }) => self.errors.push(format!("{code}: {message}")),
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
        decoder: rm_decode::H264Decoder::new().ok(), clipboard: None, icon: None, created: vec![], destroyed_ids: vec![], uploaded: None };

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

    // ---- an app dialog appears as a child window of its parent, and can be closed from the client
    key(&mut c, "KeyI", vec![Modifier::Command]);
    let found = c.pump(10, |c| c.created.iter().any(|(_, r, _)| *r == rm_protocol::WindowRole::Dialog));
    let dlg = c.created.iter().find(|(_, r, _)| *r == rm_protocol::WindowRole::Dialog).copied();
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

    // ---- lifecycle
    c.send(Message::AppTerminate { application_id: app.into() });
    let ok = c.pump(10, |c| c.destroyed && c.exited);
    c.r.check("terminate -> WindowDestroyed + AppExited", ok, format!("destroyed={} exited={}", c.destroyed, c.exited));
    c.r
}
