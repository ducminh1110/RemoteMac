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
/// Raw bytes per upload chunk (base64 grows it by 4/3, well under the bulk frame limit).
pub const UPLOAD_CHUNK: usize = 256 << 10;
/// Largest file a client may upload in one transfer.
pub const MAX_UPLOAD: u64 = 2 << 30;

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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowRole {
    #[default]
    Window,
    /// Dialog, alert, sheet or panel that belongs to `parent_id`.
    Dialog,
    /// A file *open* panel (possibly drawn by an out-of-process helper on the Mac).
    OpenPanel,
    /// A file *save* panel.
    SavePanel,
}

/// One entry of an application's menu bar, as read from the Mac (Accessibility).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MenuNode {
    pub title: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub separator: bool,
    /// Mac shortcut, e.g. "Cmd+Shift+S".
    #[serde(default)]
    pub shortcut: Option<String>,
    #[serde(default)]
    pub children: Vec<MenuNode>,
}

pub const MAX_MENU_ITEMS: usize = 3000;

impl MenuNode {
    /// Total number of nodes (bounded so a hostile agent cannot make the client build huge menus).
    pub fn count(nodes: &[MenuNode]) -> usize {
        nodes.iter().map(|n| 1 + MenuNode::count(&n.children)).sum()
    }
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

    WindowCreated {
        window_id: u64,
        application_id: String,
        title: String,
        bounds: Rect,
        parent_id: Option<u64>,
        /// What kind of window this is, so the client can present it natively
        /// (an owned dialog, or a file panel it may replace with its own picker).
        #[serde(default)]
        role: WindowRole,
    },
    WindowDestroyed { window_id: u64 },
    WindowMoved { window_id: u64, bounds: Rect },
    WindowTitleChanged { window_id: u64, title: String },
    /// Client -> agent: the user closed the local window; ask the remote window to close.
    WindowClose { window_id: u64 },
    /// Client -> agent: the user resized the local window; resize the remote window (points).
    WindowResizeRequest { window_id: u64, width: u32, height: u32 },
    /// Client -> agent: the local window was activated; raise/focus the remote window.
    WindowFocus { window_id: u64 },

    /// Client -> agent: send the icon of a registered application.
    GetAppIcon { application_id: String },
    /// Agent -> client: square RGBA icon (straight alpha), base64 encoded.
    AppIcon { application_id: String, size: u32, rgba_base64: String },

    /// Client -> agent: send this application's menu bar.
    GetMenuBar { application_id: String },
    /// Agent -> client: the application's menu bar (the Apple menu is left out).
    MenuBar { application_id: String, menus: Vec<MenuNode> },
    /// Client -> agent: choose the item at `path` (indices from the top of `MenuBar.menus`).
    MenuInvoke { application_id: String, path: Vec<u32> },

    /// Either direction: the clipboard now holds this text. `seq` lets each side ignore
    /// the echo of a change it applied itself.
    ClipboardSet { seq: u64, text: String },

    /// Client -> agent: start uploading a local file the user picked (Files channel).
    FileUploadBegin { transfer_id: u64, name: String, size: u64 },
    /// Client -> agent: next piece of the file; `offset` must equal the bytes received so far.
    FileUploadChunk { transfer_id: u64, offset: u64, data_base64: String },
    FileUploadEnd { transfer_id: u64 },
    /// Agent -> client: the file is complete on the Mac at `remote_path`.
    FileUploaded { transfer_id: u64, remote_path: String },
    /// Agent -> client: the upload was refused or failed; nothing is kept.
    FileUploadFailed { transfer_id: u64, reason: String },
    /// Client -> agent: choose `remote_path` in this open panel and confirm it.
    PanelChooseFile { window_id: u64, remote_path: String },
    /// Client -> agent: dismiss this panel (the user cancelled the local picker).
    PanelCancel { window_id: u64 },

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
            ClipboardSet { .. } => Channel::Clipboard,
            FileUploadBegin { .. } | FileUploadChunk { .. } | FileUploadEnd { .. } => Channel::Files,
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


// ---------------------------------------------------------------- base64 (icons)

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub fn base64_decode(s: &str) -> Result<Vec<u8>, ProtocolError> {
    let val = |b: u8| -> Result<u32, ProtocolError> {
        B64.iter().position(|&x| x == b).map(|p| p as u32).ok_or_else(|| ProtocolError::Malformed("bad base64".into()))
    };
    let bytes = s.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return Err(ProtocolError::Malformed("bad base64 length".into()));
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for q in bytes.chunks(4) {
        let pad = q.iter().rev().take_while(|&&b| b == b'=').count();
        if pad > 2 || q[..4 - pad].contains(&b'=') {
            return Err(ProtocolError::Malformed("bad base64 padding".into()));
        }
        let mut n = 0u32;
        for &b in &q[..4 - pad] {
            n = n << 6 | val(b)?;
        }
        n <<= 6 * pad as u32;
        let take = 3 - pad;
        out.extend_from_slice(&n.to_be_bytes()[1..1 + take]);
    }
    Ok(out)
}

// ---------------------------------------------------------------- uploads

/// Make a client-supplied file name safe to create inside the upload folder: no directories,
/// no traversal, no control characters, no hidden/empty names, bounded length.
pub fn sanitize_upload_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base.chars().filter(|c| !c.is_control() && *c != ':').collect();
    let trimmed = cleaned.trim().trim_start_matches('.').trim();
    let mut out: String = trimmed.chars().take(200).collect();
    if out.is_empty() || out == "." || out == ".." {
        out = "upload".into();
    }
    out
}

// ---------------------------------------------------------------- video frames

pub const CODEC_H264: u8 = 1;
pub const VIDEO_HEADER_LEN: usize = 8 + 8 + 1 + 1 + 2 + 2;

/// One encoded frame of one remote window. `data` is H.264 in Annex-B form;
/// keyframes carry SPS/PPS in-band so a decoder can start from any keyframe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoFrame {
    pub window_id: u64,
    pub pts_us: u64,
    pub keyframe: bool,
    pub codec: u8,
    pub width: u16,
    pub height: u16,
    pub data: Vec<u8>,
}

impl VideoFrame {
    pub fn encode_payload(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(VIDEO_HEADER_LEN + self.data.len());
        v.extend_from_slice(&self.window_id.to_be_bytes());
        v.extend_from_slice(&self.pts_us.to_be_bytes());
        v.push(self.keyframe as u8);
        v.push(self.codec);
        v.extend_from_slice(&self.width.to_be_bytes());
        v.extend_from_slice(&self.height.to_be_bytes());
        v.extend_from_slice(&self.data);
        v
    }

    pub fn decode_payload(p: &[u8]) -> Result<Self, ProtocolError> {
        if p.len() < VIDEO_HEADER_LEN {
            return Err(ProtocolError::Malformed("video header truncated".into()));
        }
        let be64 = |o: usize| u64::from_be_bytes(p[o..o + 8].try_into().unwrap());
        let be16 = |o: usize| u16::from_be_bytes(p[o..o + 2].try_into().unwrap());
        if p[16] > 1 {
            return Err(ProtocolError::Malformed("bad keyframe flag".into()));
        }
        Ok(Self {
            window_id: be64(0),
            pts_us: be64(8),
            keyframe: p[16] == 1,
            codec: p[17],
            width: be16(18),
            height: be16(20),
            data: p[VIDEO_HEADER_LEN..].to_vec(),
        })
    }

    /// True when the Annex-B payload starts with a start code (cheap sanity check).
    pub fn has_start_code(&self) -> bool {
        self.data.starts_with(&[0, 0, 0, 1])
    }

    /// NAL unit types present (Annex-B, 4-byte start codes), e.g. 7=SPS 8=PPS 5=IDR 1=non-IDR.
    pub fn nal_types(&self) -> Vec<u8> {
        let d = &self.data;
        let mut out = vec![];
        let mut i = 0;
        while i + 4 < d.len() {
            if d[i..i + 4] == [0, 0, 0, 1] {
                out.push(d[i + 4] & 0x1f);
                i += 4;
            } else {
                i += 1;
            }
        }
        out
    }
}

pub fn encode_video(f: &VideoFrame) -> Result<Vec<u8>, ProtocolError> {
    encode_raw(Channel::Video, &f.encode_payload())
}

/// Anything that can arrive on the wire.
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    Msg(Message),
    Video(VideoFrame),
}

/// Blocking read of one frame of either kind. `Ok(None)` on clean EOF.
pub fn read_frame<R: Read>(r: &mut R) -> Result<Option<Frame>, ProtocolError> {
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
    if channel == Channel::Video {
        VideoFrame::decode_payload(&payload).map(|v| Some(Frame::Video(v)))
    } else {
        serde_json::from_slice(&payload).map(|m| Some(Frame::Msg(m))).map_err(|e| ProtocolError::Malformed(e.to_string()))
    }
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

    #[test]
    fn video_roundtrip_and_mixed_stream() {
        let f = VideoFrame { window_id: 42, pts_us: 123_456, keyframe: true, codec: CODEC_H264, width: 480, height: 348,
            data: vec![0, 0, 0, 1, 0x67, 1, 2, 0, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 9, 9] };
        assert_eq!(f.nal_types(), vec![7, 8, 5]);
        assert!(f.has_start_code());
        let mut wire = encode(&Message::ListApps).unwrap();
        wire.extend(encode_video(&f).unwrap());
        wire.extend(encode(&Message::Ping { nonce: 5 }).unwrap());
        let mut cur = std::io::Cursor::new(wire);
        assert_eq!(read_frame(&mut cur).unwrap().unwrap(), Frame::Msg(Message::ListApps));
        assert_eq!(read_frame(&mut cur).unwrap().unwrap(), Frame::Video(f));
        assert_eq!(read_frame(&mut cur).unwrap().unwrap(), Frame::Msg(Message::Ping { nonce: 5 }));
        assert!(read_frame(&mut cur).unwrap().is_none());
    }

    #[test]
    fn video_rejects_truncated_header_and_bad_flag() {
        assert!(VideoFrame::decode_payload(&[0u8; VIDEO_HEADER_LEN - 1]).is_err());
        let mut p = vec![0u8; VIDEO_HEADER_LEN];
        p[16] = 2;
        assert!(VideoFrame::decode_payload(&p).is_err());
        // a video frame over the bulk limit is refused on encode
        let big = VideoFrame { window_id: 1, pts_us: 0, keyframe: false, codec: 1, width: 1, height: 1, data: vec![0; MAX_BULK_FRAME] };
        assert!(matches!(encode_video(&big), Err(ProtocolError::FrameTooLarge(..))));
    }

    #[test]
    fn base64_roundtrip_and_rejects_garbage() {
        for len in 0..20 {
            let data: Vec<u8> = (0..len).map(|i| (i * 37 + 11) as u8).collect();
            assert_eq!(base64_decode(&base64_encode(&data)).unwrap(), data, "len={len}");
        }
        assert_eq!(base64_encode(b"Man"), "TWFu");
        assert_eq!(base64_encode(b"Ma"), "TWE=");
        assert!(base64_decode("TWF").is_err());
        assert!(base64_decode("T=Fu").is_err());
        assert!(base64_decode("TW?u").is_err());
    }

    #[test]
    fn clipboard_rides_its_own_channel() {
        let c = Message::ClipboardSet { seq: 1, text: "x".into() };
        assert_eq!(c.channel(), Channel::Clipboard);
        let bytes = encode(&c).unwrap();
        assert_eq!(decode(&bytes).unwrap().unwrap().0, c);
    }

    #[test]
    fn window_role_defaults_for_older_agents() {
        let j = r#"{"type":"window_created","window_id":1,"application_id":"a","title":"t","bounds":{"x":0,"y":0,"w":1,"h":1},"parent_id":null}"#;
        match serde_json::from_str::<Message>(j).unwrap() {
            Message::WindowCreated { role, .. } => assert_eq!(role, WindowRole::Window),
            m => panic!("{m:?}"),
        }
        let j = r#"{"type":"window_created","window_id":2,"application_id":"a","title":"","bounds":{"x":0,"y":0,"w":1,"h":1},"parent_id":1,"role":"open_panel"}"#;
        assert!(matches!(serde_json::from_str::<Message>(j).unwrap(), Message::WindowCreated { role: WindowRole::OpenPanel, parent_id: Some(1), .. }));
    }

    #[test]
    fn uploads_ride_the_files_channel_and_fit() {
        let c = Message::FileUploadChunk { transfer_id: 1, offset: 0, data_base64: base64_encode(&vec![7u8; UPLOAD_CHUNK]) };
        assert_eq!(c.channel(), Channel::Files);
        assert!(encode(&c).is_ok());
    }

    #[test]
    fn upload_names_cannot_escape_the_folder() {
        assert_eq!(sanitize_upload_name("report.pdf"), "report.pdf");
        assert_eq!(sanitize_upload_name("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_upload_name("C:\\Users\\me\\photo.png"), "photo.png");
        assert_eq!(sanitize_upload_name(".bashrc"), "bashrc");
        assert_eq!(sanitize_upload_name(".."), "upload");
        assert_eq!(sanitize_upload_name("a\u{0}b\nc:d"), "abcd");
        assert_eq!(sanitize_upload_name(""), "upload");
        assert_eq!(sanitize_upload_name(&"x".repeat(500)).len(), 200);
    }

    #[test]
    fn menu_bar_roundtrip() {
        let m = Message::MenuBar { application_id: "xcode".into(), menus: vec![MenuNode { title: "File".into(), enabled: true, children: vec![
            MenuNode { title: "Save".into(), enabled: true, shortcut: Some("Cmd+S".into()), ..Default::default() },
            MenuNode { separator: true, ..Default::default() },
        ], ..Default::default() }] };
        let b = encode(&m).unwrap();
        assert_eq!(decode(&b).unwrap().unwrap().0, m);
        if let Message::MenuBar { menus, .. } = m { assert_eq!(MenuNode::count(&menus), 3) }
    }
}
