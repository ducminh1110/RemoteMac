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
            }
            Frame::Msg(Message::WindowCreated { window_id, bounds, title, .. }) => {
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
            Frame::Msg(Message::WindowDestroyed { .. }) => self.destroyed = true,
            Frame::Msg(Message::AppExited { .. }) => self.exited = true,
            Frame::Msg(Message::AppLaunched { pid, .. }) => self.launched_pid = Some(pid),
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
        destroyed: false, exited: false, launched_pid: None, errors: vec![], non_annexb: 0 };

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

    // ---- lifecycle
    c.send(Message::AppTerminate { application_id: app.into() });
    let ok = c.pump(10, |c| c.destroyed && c.exited);
    c.r.check("terminate -> WindowDestroyed + AppExited", ok, format!("destroyed={} exited={}", c.destroyed, c.exited));
    c.r
}
