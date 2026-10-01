//! Wire protocol shared by the macOS agent, the relay-facing client and tests.
//!
//! Framing: `u32 BE length | u8 channel | payload`, where `length` covers the
//! channel byte plus the payload. Control-plane payloads are JSON. Bulk media
//! will get its own binary framing once the capture gate (see docs/SPEC.md §2)
//! passes; nothing here pretends to carry video yet.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{Read, Write};

pub const PROTOCOL_VERSION: u16 = 1;
pub const MIN_PROTOCOL_VERSION: u16 = 1;

/// Hard cap per frame so a hostile peer cannot make us allocate arbitrarily.
pub const MAX_CONTROL_FRAME: usize = 1 << 20; // 1 MiB
pub const MAX_BULK_FRAME: usize = 16 << 20; // 16 MiB

#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("frame of {0} bytes exceeds limit {1}")]
    FrameTooLarge(usize, usize),
    #[error("empty frame")]
    EmptyFrame,
    #[error("unknown channel {0}")]
    UnknownChannel(u8),
    #[error("malformed message: {0}")]
    Malformed(String),
    #[error("no common protocol version (our max {ours}, peer supports {theirs_min}..={theirs_max})")]
    VersionMismatch { ours: u16, theirs_min: u16, theirs_max: u16 },
}

/// Logical channels. Lower priority number = scheduled first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum Channel {
    Input = 0,
    Control = 1,
    WindowMetadata = 2,
    Video = 3,
    Clipboard = 4,
    Files = 5,
    Telemetry = 6,
}

impl Channel {
    pub fn priority(self) -> u8 {
        self as u8
    }
    pub fn from_u8(v: u8) -> Result<Self, ProtocolError> {
        Ok(match v {
            0 => Channel::Input,
            1 => Channel::Control,
            2 => Channel::WindowMetadata,
            3 => Channel::Video,
            4 => Channel::Clipboard,
            5 => Channel::Files,
            6 => Channel::Telemetry,
            n => return Err(ProtocolError::UnknownChannel(n)),
        })
    }
    pub fn max_frame(self) -> usize {
        match self {
            Channel::Video | Channel::Files | Channel::Clipboard => MAX_BULK_FRAME,
            _ => MAX_CONTROL_FRAME,
        }
    }
}

// ---------------------------------------------------------------- capabilities

/// Result of a runtime capability probe. Never assume — report why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Capability {
    Available { detail: String },
    Unavailable { reason: String },
    Unknown { reason: String },
}

impl Capability {
    pub fn is_available(&self) -> bool {
        matches!(self, Capability::Available { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityReport {
    pub gui_session: Capability,
    pub capture: Capability,
    pub input: Capability,
    pub accessibility: Capability,
    pub hardware_encode: Capability,
}

impl CapabilityReport {
    pub fn unknown(reason: &str) -> Self {
        let u = || Capability::Unknown { reason: reason.into() };
        Self { gui_session: u(), capture: u(), input: u(), accessibility: u(), hardware_encode: u() }
    }
    /// Minimum needed to stream a window and drive it.
    pub fn can_stream_apps(&self) -> bool {
        self.gui_session.is_available() && self.capture.is_available() && self.input.is_available()
    }
}

// ---------------------------------------------------------------- hello

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub min_version: u16,
    pub max_version: u16,
    pub codecs: Vec<String>,
    pub features: Vec<String>,
    pub max_surface: (u32, u32),
    /// Free-form peer description, e.g. "remote-agent 0.1.0 macos arm64".
    pub agent: String,
}

impl Hello {
    pub fn ours(agent: &str, codecs: &[&str], features: &[&str]) -> Self {
        Self {
            min_version: MIN_PROTOCOL_VERSION,
            max_version: PROTOCOL_VERSION,
            codecs: codecs.iter().map(|s| s.to_string()).collect(),
            features: features.iter().map(|s| s.to_string()).collect(),
            max_surface: (3840, 2160),
            agent: agent.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Negotiated {
    pub version: u16,
    pub codec: Option<String>,
    pub features: Vec<String>,
}

/// Pick highest common version, first mutually supported codec (our order of
/// preference), and the feature intersection.
pub fn negotiate(ours: &Hello, theirs: &Hello) -> Result<Negotiated, ProtocolError> {
    let hi = ours.max_version.min(theirs.max_version);
    let lo = ours.min_version.max(theirs.min_version);
    if hi < lo {
        return Err(ProtocolError::VersionMismatch {
            ours: ours.max_version,
            theirs_min: theirs.min_version,
            theirs_max: theirs.max_version,
        });
    }
    let codec = ours.codecs.iter().find(|c| theirs.codecs.contains(c)).cloned();
    let features = ours.features.iter().filter(|f| theirs.features.contains(f)).cloned().collect();
    Ok(Negotiated { version: hi, codec, features })
}

// ---------------------------------------------------------------- messages

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppInfo {
    pub id: String,
    pub name: String,
    pub available: bool,
    #[serde(default)]
    pub version: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Modifier {
    Command,
    Option,
    Control,
    Shift,
    Fn,
    CapsLock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    ClientHello(Hello),
    ServerHello(Hello),

    CapabilityReport(CapabilityReport),
    /// Sent instead of silently ignoring a request the host cannot honour.
    CapabilityUnavailable { capability: String, reason: String },

    ListApps,
    Apps { apps: Vec<AppInfo> },
    AppLaunch {
        application_id: String,
        #[serde(default)]
        arguments: Vec<String>,
        #[serde(default)]
        working_directory: Option<String>,
        #[serde(default)]
        environment: BTreeMap<String, String>,
    },
    AppLaunched { application_id: String, pid: u32 },
    AppTerminate { application_id: String },
    AppExited { application_id: String, code: Option<i32> },

    WindowCreated { window_id: u64, application_id: String, title: String, bounds: Rect, parent_id: Option<u64> },
    WindowDestroyed { window_id: u64 },
    WindowMoved { window_id: u64, bounds: Rect },
    WindowTitleChanged { window_id: u64, title: String },

    MouseMove { window_id: u64, x: f64, y: f64 },
    MouseButton { window_id: u64, button: MouseButton, down: bool, x: f64, y: f64 },
    Scroll { window_id: u64, dx: f64, dy: f64 },
    /// Physical key (layout independent) + modifiers; never raw characters only.
    Key { window_id: u64, physical_key: String, modifiers: Vec<Modifier>, down: bool },
    TextInput { window_id: u64, text: String },

    Error { code: String, message: String },
    Ping { nonce: u64 },
    Pong { nonce: u64 },
}

impl Message {
    pub fn channel(&self) -> Channel {
        use Message::*;
        match self {
            MouseMove { .. } | MouseButton { .. } | Scroll { .. } | Key { .. } | TextInput { .. } => Channel::Input,
            WindowCreated { .. } | WindowDestroyed { .. } | WindowMoved { .. } | WindowTitleChanged { .. } => {
                Channel::WindowMetadata
            }
            Ping { .. } | Pong { .. } => Channel::Telemetry,
            _ => Channel::Control,
        }
    }
}

// ---------------------------------------------------------------- framing

pub fn encode(msg: &Message) -> Result<Vec<u8>, ProtocolError> {
    let payload = serde_json::to_vec(msg).map_err(|e| ProtocolError::Malformed(e.to_string()))?;
    encode_raw(msg.channel(), &payload)
}

pub fn encode_raw(channel: Channel, payload: &[u8]) -> Result<Vec<u8>, ProtocolError> {
    let len = payload.len() + 1;
    if len > channel.max_frame() {
        return Err(ProtocolError::FrameTooLarge(len, channel.max_frame()));
    }
    let mut out = Vec::with_capacity(4 + len);
    out.extend_from_slice(&(len as u32).to_be_bytes());
    out.push(channel as u8);
    out.extend_from_slice(payload);
    Ok(out)
}

/// Try to decode one frame from a buffer. `Ok(None)` = need more bytes.
/// Returns the message and the number of bytes consumed.
pub fn decode(buf: &[u8]) -> Result<Option<(Message, usize)>, ProtocolError> {
    if buf.len() < 4 {
        return Ok(None);
    }
    let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if len == 0 {
        return Err(ProtocolError::EmptyFrame);
    }
    // Reject before waiting for the body: validates the channel byte early.
    if buf.len() >= 5 {
        let ch = Channel::from_u8(buf[4])?;
        if len > ch.max_frame() {
            return Err(ProtocolError::FrameTooLarge(len, ch.max_frame()));
        }
    } else if len > MAX_BULK_FRAME {
        return Err(ProtocolError::FrameTooLarge(len, MAX_BULK_FRAME));
    }
    if buf.len() < 4 + len {
        return Ok(None);
    }
    let payload = &buf[5..4 + len];
    let msg = serde_json::from_slice(payload).map_err(|e| ProtocolError::Malformed(e.to_string()))?;
    Ok(Some((msg, 4 + len)))
}

pub fn write_message<W: Write>(w: &mut W, msg: &Message) -> Result<(), ProtocolError> {
    w.write_all(&encode(msg)?)?;
    w.flush()?;
    Ok(())
}

/// Blocking read of exactly one message. `Ok(None)` on clean EOF at a frame boundary.
pub fn read_message<R: Read>(r: &mut R) -> Result<Option<Message>, ProtocolError> {
    let mut head = [0u8; 4];
    match r.read(&mut head[..1])? {
        0 => return Ok(None),
        _ => r.read_exact(&mut head[1..])?,
    }
    let len = u32::from_be_bytes(head) as usize;
    if len == 0 {
        return Err(ProtocolError::EmptyFrame);
    }
    if len > MAX_BULK_FRAME {
        return Err(ProtocolError::FrameTooLarge(len, MAX_BULK_FRAME));
    }
    let mut ch = [0u8; 1];
    r.read_exact(&mut ch)?;
    let channel = Channel::from_u8(ch[0])?;
    if len > channel.max_frame() {
        return Err(ProtocolError::FrameTooLarge(len, channel.max_frame()));
    }
    let mut payload = vec![0u8; len - 1];
    r.read_exact(&mut payload)?;
    serde_json::from_slice(&payload).map(Some).map_err(|e| ProtocolError::Malformed(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<Message> {
        vec![
            Message::ClientHello(Hello::ours("c", &["h264"], &["clipboard"])),
            Message::ListApps,
            Message::AppLaunch {
                application_id: "textedit".into(),
                arguments: vec!["/tmp/a.txt".into()],
                working_directory: None,
                environment: BTreeMap::new(),
            },
            Message::Key {
                window_id: 7,
                physical_key: "KeyA".into(),
                modifiers: vec![Modifier::Command, Modifier::Shift],
                down: true,
            },
            Message::MouseMove { window_id: 1, x: 1.5, y: 2.5 },
            Message::CapabilityUnavailable { capability: "capture".into(), reason: "no TCC".into() },
        ]
    }

    #[test]
    fn roundtrip() {
        for m in sample() {
            let bytes = encode(&m).unwrap();
            let (back, used) = decode(&bytes).unwrap().unwrap();
            assert_eq!(back, m);
            assert_eq!(used, bytes.len());
            let mut cur = std::io::Cursor::new(bytes);
            assert_eq!(read_message(&mut cur).unwrap().unwrap(), m);
        }
    }

    #[test]
    fn partial_frames_need_more() {
        let bytes = encode(&Message::ListApps).unwrap();
        for cut in 0..bytes.len() {
            assert!(decode(&bytes[..cut]).unwrap().is_none(), "cut={cut}");
        }
    }

    #[test]
    fn two_frames_in_one_buffer() {
        let mut b = encode(&Message::ListApps).unwrap();
        let first = b.len();
        b.extend(encode(&Message::Ping { nonce: 1 }).unwrap());
        let (m, used) = decode(&b).unwrap().unwrap();
        assert_eq!(m, Message::ListApps);
        assert_eq!(used, first);
    }

    #[test]
    fn rejects_oversize_before_buffering() {
        let mut b = ((MAX_CONTROL_FRAME + 1) as u32).to_be_bytes().to_vec();
        b.push(Channel::Control as u8);
        assert!(matches!(decode(&b), Err(ProtocolError::FrameTooLarge(..))));
        let mut cur = std::io::Cursor::new(b);
        assert!(matches!(read_message(&mut cur), Err(ProtocolError::FrameTooLarge(..))));
    }

    #[test]
    fn rejects_huge_length_without_channel_byte() {
        let b = u32::MAX.to_be_bytes().to_vec();
        assert!(matches!(decode(&b), Err(ProtocolError::FrameTooLarge(..))));
    }

    #[test]
    fn rejects_empty_unknown_channel_and_garbage() {
        assert!(matches!(decode(&[0, 0, 0, 0]), Err(ProtocolError::EmptyFrame)));
        assert!(matches!(decode(&[0, 0, 0, 2, 99, b'x']), Err(ProtocolError::UnknownChannel(99))));
        let mut b = vec![0, 0, 0, 4, Channel::Control as u8];
        b.extend_from_slice(b"{no");
        assert!(matches!(decode(&b), Err(ProtocolError::Malformed(_))));
    }

    #[test]
    fn unknown_message_type_is_error_not_panic() {
        let payload = br#"{"type":"format_c_drive"}"#;
        let b = encode_raw(Channel::Control, payload).unwrap();
        assert!(matches!(decode(&b), Err(ProtocolError::Malformed(_))));
    }

    #[test]
    fn eof_semantics() {
        let mut empty = std::io::Cursor::new(Vec::<u8>::new());
        assert!(read_message(&mut empty).unwrap().is_none());
        let mut trunc = std::io::Cursor::new(vec![0, 0, 0, 9, 1, b'{']);
        assert!(read_message(&mut trunc).is_err());
    }

    #[test]
    fn input_goes_on_highest_priority_channel() {
        let k = Message::Key { window_id: 1, physical_key: "KeyA".into(), modifiers: vec![], down: true };
        assert_eq!(k.channel(), Channel::Input);
        assert!(Channel::Input.priority() < Channel::Video.priority());
        assert!(Channel::Video.priority() < Channel::Files.priority());
    }

    #[test]
    fn negotiation() {
        let a = Hello { min_version: 1, max_version: 3, codecs: vec!["hevc".into(), "h264".into()], features: vec!["a".into(), "b".into()], max_surface: (1, 1), agent: "a".into() };
        let b = Hello { min_version: 2, max_version: 5, codecs: vec!["h264".into()], features: vec!["b".into(), "c".into()], max_surface: (1, 1), agent: "b".into() };
        let n = negotiate(&a, &b).unwrap();
        assert_eq!(n.version, 3);
        assert_eq!(n.codec.as_deref(), Some("h264"));
        assert_eq!(n.features, vec!["b"]);

        let old = Hello { min_version: 9, max_version: 9, ..b.clone() };
        assert!(matches!(negotiate(&a, &old), Err(ProtocolError::VersionMismatch { .. })));
    }

    #[test]
    fn capability_report_gating() {
        let mut r = CapabilityReport::unknown("not probed");
        assert!(!r.can_stream_apps());
        let ok = Capability::Available { detail: "x".into() };
        r.gui_session = ok.clone();
        r.capture = ok.clone();
        assert!(!r.can_stream_apps());
        r.input = ok;
        assert!(r.can_stream_apps());
    }
}
