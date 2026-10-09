//! Records a real agent session for replay elsewhere: opens each application in turn, asks for
//! its menu bar and icon once its first window is up, and stores everything the agent sends for
//! that app (windows, titles, menus, icons, H.264 video) in an `.rmrec` file.

use crate::Session;
use rm_protocol::recording::{self, Record};
use rm_protocol::{encode, encode_video, Frame, Message, ProtocolError, WindowRole};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::time::{Duration, Instant};

pub struct Plan {
    pub apps: Vec<String>,
    /// Keep recording this long after the app's first window appeared.
    pub settle: Duration,
    /// Give up on an app without a window after this long.
    pub max: Duration,
    /// Called at the end of each app's segment (e.g. to take a Mac screenshot).
    pub on_segment_end: Box<dyn FnMut(&str)>,
}

#[derive(Debug, Default)]
pub struct Summary {
    /// (app, windows, video frames, bytes, seconds to first window)
    pub apps: Vec<(String, usize, usize, usize, Option<f64>)>,
}

fn is_timeout(e: &ProtocolError) -> bool {
    matches!(e, ProtocolError::Io(io) if matches!(io.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut))
}

/// The stream needs a short read timeout (the caller sets ~1 s).
pub fn record<S: Read + Write, W: Write>(sess: &mut Session<S>, out: &mut W, mut plan: Plan) -> Result<Summary, ProtocolError> {
    recording::write_header(out)?;
    let put = |out: &mut W, app: &str, t: Instant, frame: Vec<u8>| -> Result<(), ProtocolError> {
        Ok(recording::write_record(out, &Record { app: app.into(), t_ms: t.elapsed().as_millis() as u32, frame })?)
    };
    let t0 = Instant::now();
    put(out, recording::SESSION, t0, encode(&Message::CapabilityReport(sess.capabilities.clone()))?)?;
    sess.send(&Message::ListApps)?;
    let mut summary = Summary::default();
    let mut owner: HashMap<u64, String> = HashMap::new();
    let mut got_apps = false;
    let deadline = Instant::now() + Duration::from_secs(20);
    while !got_apps && Instant::now() < deadline {
        match sess.recv() {
            Ok(Some(Frame::Msg(m @ Message::Apps { .. }))) => {
                put(out, recording::SESSION, t0, encode(&m)?)?;
                got_apps = true;
            }
            Ok(Some(_)) => {}
            Ok(None) => return Err(ProtocolError::Malformed("agent closed the session".into())),
            Err(e) if is_timeout(&e) => {}
            Err(e) => return Err(e),
        }
    }
    for app in plan.apps.clone() {
        let start = Instant::now();
        sess.send(&Message::AppLaunch { application_id: app.clone(), arguments: vec![], working_directory: None, environment: Default::default() })?;
        let (mut windows, mut frames, mut bytes, mut first, mut asked) = (0usize, 0usize, 0usize, None::<Instant>, false);
        loop {
            let done = match first {
                Some(f) => f.elapsed() >= plan.settle,
                None => start.elapsed() >= plan.max,
            };
            if done {
                break;
            }
            let f = match sess.recv() {
                Ok(Some(f)) => f,
                Ok(None) => return Err(ProtocolError::Malformed("agent closed the session".into())),
                Err(e) if is_timeout(&e) => continue,
                Err(e) => return Err(e),
            };
            // which app a frame belongs to
            let tag = match &f {
                Frame::Video(v) => owner.get(&v.window_id).cloned(),
                Frame::Audio(_) => None,
                Frame::Msg(Message::WindowCreated { window_id, application_id, role, .. }) => {
                    owner.insert(*window_id, application_id.clone());
                    if *application_id == app {
                        windows += 1;
                        if *role == WindowRole::Window && first.is_none() {
                            first = Some(Instant::now());
                        }
                    }
                    Some(application_id.clone())
                }
                Frame::Msg(Message::WindowDestroyed { window_id } | Message::WindowMoved { window_id, .. } | Message::WindowTitleChanged { window_id, .. }) => owner.get(window_id).cloned(),
                Frame::Msg(Message::AppLaunched { application_id, .. } | Message::AppExited { application_id, .. } | Message::AppIcon { application_id, .. } | Message::MenuBar { application_id, .. }) => Some(application_id.clone()),
                Frame::Msg(_) => None,
            };
            if tag.as_deref() != Some(app.as_str()) {
                continue; // other apps' leftovers, pongs, errors
            }
            let wire = match &f {
                Frame::Video(v) => {
                    frames += 1;
                    bytes += v.data.len();
                    encode_video(v)?
                }
                Frame::Msg(m) => encode(m)?,
                Frame::Audio(a) => rm_protocol::encode_audio(a)?,
            };
            put(out, &app, start, wire)?;
            if first.is_some() && !asked {
                asked = true;
                sess.send(&Message::GetMenuBar { application_id: app.clone() })?;
                sess.send(&Message::GetAppIcon { application_id: app.clone() })?;
            }
        }
        (plan.on_segment_end)(&app);
        summary.apps.push((app.clone(), windows, frames, bytes, first.map(|f| (f - start).as_secs_f64())));
        eprintln!("recorded {app}: windows={windows} videoFrames={frames} bytes={bytes} firstWindowAfter={:?}", first.map(|f| f - start));
        // close it before the next app (not recorded: the replay keeps the windows open)
        sess.send(&Message::AppTerminate { application_id: app.clone() })?;
        let until = Instant::now() + Duration::from_secs(4);
        while Instant::now() < until {
            match sess.recv() {
                Ok(Some(Frame::Msg(Message::AppExited { application_id, .. }))) if application_id == app => break,
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(e) if is_timeout(&e) => {}
                Err(e) => return Err(e),
            }
        }
    }
    out.flush()?;
    Ok(summary)
}
