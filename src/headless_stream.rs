//! Bidirectional headless live stream client (`CAMF` video out + `CAMI` input in)
//! for Camoufox's `HeadlessStreamServer` (`zz-headless-live-stream.patch`).
//!
//! When [`HeadlessStreamOptions`] is attached to [`crate::CamoufoxOptions`], the launcher sets
//! `CAMOU_HEADLESS_STREAM_SOCKET`, `CAMOU_HEADLESS_STREAM_FPS`, and
//! `CAMOU_HEADLESS_STREAM_FORMAT` before spawning Firefox in `--headless` mode.
//!
//! Because `HeadlessStreamServer` uses `AF_UNIX` domain sockets (`#if defined(XP_UNIX)`),
//! socket transport is active on Linux and macOS.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
#[cfg(unix)]
use tokio::io::{AsyncReadExt, AsyncWriteExt};
#[cfg(unix)]
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
#[cfg(unix)]
use tokio::net::UnixStream;

use crate::exceptions::{CamoufoxError, Result};
use crate::warnings::warn;

/// Environment variable naming the `AF_UNIX` socket bound by `HeadlessStreamServer`.
pub const CAMOU_STREAM_SOCKET_ENV: &str = "CAMOU_HEADLESS_STREAM_SOCKET";
/// Environment variable setting the target stream FPS cap (`1..=120`).
pub const CAMOU_STREAM_FPS_ENV: &str = "CAMOU_HEADLESS_STREAM_FPS";
/// Environment variable selecting the frame pixel format (`"nv12"` or `"bgra"`).
pub const CAMOU_STREAM_FORMAT_ENV: &str = "CAMOU_HEADLESS_STREAM_FORMAT";

/// Little-endian magic bytes `"CAMF"` (`0x464D4143`) starting each video frame header.
pub const CAMF_MAGIC: u32 = 0x464D_4143;
/// Little-endian magic bytes `"CAMI"` (`0x494D4143`) starting each reverse input packet.
pub const CAMI_MAGIC: u32 = 0x494D_4143;
/// Wire protocol version shared by `CAMF` and `CAMI`.
pub const STREAM_PROTOCOL_VERSION: u32 = 1;
/// Size in bytes of a `CAMF` video header.
pub const CAMF_HEADER_SIZE: usize = 32;
/// Size in bytes of a `CAMI` reverse input header.
pub const CAMI_HEADER_SIZE: usize = 36;
/// Maximum UTF-8 payload accepted per `CAMI` input packet by the browser.
pub const MAX_INPUT_PAYLOAD_BYTES: usize = 65_536;
const MAX_FRAME_PAYLOAD_BYTES: usize = 128 * 1024 * 1024;
const UNIX_SUN_PATH_SAFE_MAX: usize = 100;

static SOCKET_SEQ: AtomicU64 = AtomicU64::new(1);

/// Pixel format emitted by `HeadlessStreamServer`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[repr(u32)]
pub enum StreamFormat {
    /// Raw 32-bit BGRA (`width * height * 4` bytes).
    Bgra = 1,
    /// Planar Y + interleaved UV (`width * height * 3 / 2` bytes, even dimensions).
    #[default]
    Nv12 = 2,
}

impl StreamFormat {
    /// Returns the `CAMOU_HEADLESS_STREAM_FORMAT` environment variable string.
    pub fn as_env_str(self) -> &'static str {
        match self {
            Self::Bgra => "bgra",
            Self::Nv12 => "nv12",
        }
    }

    /// Parses a wire `pixel_format` integer (`1` = `BGRA`, `2` = `NV12`).
    pub fn from_wire(code: u32) -> Result<Self> {
        match code {
            1 => Ok(Self::Bgra),
            2 => Ok(Self::Nv12),
            _ => Err(CamoufoxError::ValueError(format!(
                "Unknown CAMF pixel_format code: {code}"
            ))),
        }
    }

    /// Computes expected byte size for `(width, height)`.
    pub fn expected_payload_len(self, width: u32, height: u32) -> Result<usize> {
        let pixels = (width as usize)
            .checked_mul(height as usize)
            .ok_or_else(|| CamoufoxError::ValueError("Frame dimensions overflow usize".into()))?;
        match self {
            Self::Bgra => pixels
                .checked_mul(4)
                .ok_or_else(|| CamoufoxError::ValueError("BGRA payload size overflows usize".into())),
            Self::Nv12 => {
                if (width & 1) != 0 || (height & 1) != 0 {
                    return Err(CamoufoxError::ValueError(format!(
                        "NV12 requires even dimensions, got {width}x{height}"
                    )));
                }
                pixels
                    .checked_mul(3)
                    .map(|v| v / 2)
                    .ok_or_else(|| CamoufoxError::ValueError("NV12 payload size overflows usize".into()))
            }
        }
    }
}

/// Launch options enabling `HeadlessStreamServer` for `--headless` browser instances.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeadlessStreamOptions {
    /// Path to the `AF_UNIX` socket. When `None`, a short temporary `.sock` path is chosen.
    pub socket_path: Option<PathBuf>,
    /// Maximum frame rate cap (`1..=120`). Defaults to browser default (`60`) when `None`.
    pub fps: Option<u8>,
    /// Output frame pixel format (`Nv12` default or `Bgra`).
    pub format: Option<StreamFormat>,
}

impl HeadlessStreamOptions {
    /// Creates default stream options (auto socket path, browser default FPS and `NV12` format).
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets an explicit `AF_UNIX` socket path.
    pub fn with_socket(mut self, path: impl Into<PathBuf>) -> Self {
        self.socket_path = Some(path.into());
        self
    }

    /// Sets the target maximum frame rate (`1..=120`).
    pub fn with_fps(mut self, fps: u8) -> Self {
        self.fps = Some(fps);
        self
    }

    /// Sets the emitted frame pixel format (`StreamFormat::Nv12` or `StreamFormat::Bgra`).
    pub fn with_format(mut self, format: StreamFormat) -> Self {
        self.format = Some(format);
        self
    }
}

/// Decoded video frame read from a `CAMF` stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamFrame {
    pub width: u32,
    pub height: u32,
    pub format: StreamFormat,
    pub pts_us: u64,
    pub data: Vec<u8>,
}

impl StreamFrame {
    /// Parses the 32-byte little-endian `CAMF` header and returns `(width, height, format, payload_size, pts_us)`.
    pub fn decode_header(buf: &[u8; CAMF_HEADER_SIZE]) -> Result<(u32, u32, StreamFormat, usize, u64)> {
        let magic = u32::from_le_bytes(buf[0..4].try_into().unwrap());
        if magic != CAMF_MAGIC {
            return Err(CamoufoxError::ValueError(format!(
                "Invalid CAMF magic: 0x{magic:08x} (expected 0x{CAMF_MAGIC:08x})"
            )));
        }
        let version = u32::from_le_bytes(buf[4..8].try_into().unwrap());
        if version != STREAM_PROTOCOL_VERSION {
            return Err(CamoufoxError::ValueError(format!(
                "Unsupported CAMF protocol version: {version}"
            )));
        }
        let width = u32::from_le_bytes(buf[8..12].try_into().unwrap());
        let height = u32::from_le_bytes(buf[12..16].try_into().unwrap());
        let pixel_format = u32::from_le_bytes(buf[16..20].try_into().unwrap());
        let payload_size = u32::from_le_bytes(buf[20..24].try_into().unwrap()) as usize;
        let pts_us = u64::from_le_bytes(buf[24..32].try_into().unwrap());

        let format = StreamFormat::from_wire(pixel_format)?;
        if width == 0 || height == 0 {
            return Err(CamoufoxError::ValueError(format!(
                "Invalid CAMF dimensions: {width}x{height}"
            )));
        }
        if payload_size > MAX_FRAME_PAYLOAD_BYTES {
            return Err(CamoufoxError::ValueError(format!(
                "CAMF payload_size {payload_size} exceeds cap ({MAX_FRAME_PAYLOAD_BYTES} bytes)"
            )));
        }
        let expected = format.expected_payload_len(width, height)?;
        if payload_size != expected {
            return Err(CamoufoxError::ValueError(format!(
                "CAMF payload_size {payload_size} does not match {width}x{height} {format:?} ({expected} bytes)"
            )));
        }
        Ok((width, height, format, payload_size, pts_us))
    }

    /// Converts the frame payload to tightly packed 8-bit `RGBA` (`width * height * 4` bytes).
    pub fn to_rgba(&self) -> Result<Vec<u8>> {
        let expected = self.format.expected_payload_len(self.width, self.height)?;
        if self.data.len() != expected {
            return Err(CamoufoxError::ValueError(format!(
                "Frame byte length {} does not match expected {expected}",
                self.data.len()
            )));
        }
        let w = self.width as usize;
        let h = self.height as usize;
        let mut rgba = vec![0u8; w * h * 4];
        match self.format {
            StreamFormat::Bgra => {
                for (src, dst) in self.data.chunks_exact(4).zip(rgba.chunks_exact_mut(4)) {
                    dst[0] = src[2];
                    dst[1] = src[1];
                    dst[2] = src[0];
                    dst[3] = src[3];
                }
            }
            StreamFormat::Nv12 => {
                let y_plane = &self.data[..w * h];
                let uv_plane = &self.data[w * h..];
                for row in 0..h {
                    let y_row = &y_plane[row * w..(row + 1) * w];
                    let uv_row = &uv_plane[(row / 2) * w..((row / 2) + 1) * w];
                    let out_row = &mut rgba[row * w * 4..(row + 1) * w * 4];
                    for (col, (&y_px, out_px)) in
                        y_row.iter().zip(out_row.chunks_exact_mut(4)).enumerate()
                    {
                        let c = i32::from(y_px) - 16;
                        let uv_idx = (col / 2) * 2;
                        let d = i32::from(uv_row[uv_idx]) - 128;
                        let e = i32::from(uv_row[uv_idx + 1]) - 128;
                        out_px[0] = ((298 * c + 409 * e + 128) >> 8).clamp(0, 255) as u8;
                        out_px[1] = ((298 * c - 100 * d - 208 * e + 128) >> 8).clamp(0, 255) as u8;
                        out_px[2] = ((298 * c + 516 * d + 128) >> 8).clamp(0, 255) as u8;
                        out_px[3] = 255;
                    }
                }
            }
        }
        Ok(rgba)
    }
}

/// Event discriminator for `CAMI` reverse input packets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum StreamInputType {
    MouseMove = 1,
    MouseDown = 2,
    MouseUp = 3,
    Wheel = 4,
    KeyDown = 5,
    KeyUp = 6,
    Text = 7,
}

/// Mouse button indices accepted by `CAMI` pointer events.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum MouseButton {
    #[default]
    Left = 0,
    Middle = 1,
    Right = 2,
    Back = 3,
    Forward = 4,
}

/// Keyboard modifier bitmask accepted by `CAMI` events.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Modifiers(pub u32);

impl Modifiers {
    pub const NONE: Self = Self(0);
    pub const SHIFT: u32 = 1;
    pub const CONTROL: u32 = 2;
    pub const ALT: u32 = 4;
    pub const META: u32 = 8;

    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    pub const fn bits(self) -> u32 {
        self.0
    }
}

/// Reverse input event serialized into a `CAMI` packet for `HeadlessWidget`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamInputEvent {
    pub kind: StreamInputType,
    pub modifiers: Modifiers,
    /// Pixel X for Mouse/Wheel, or `is_repeat` (`0`/`1`) for Key events.
    pub x: i32,
    /// Pixel Y for Mouse/Wheel, or `location` (`0` = auto) for Key events.
    pub y: i32,
    /// Mouse button (`0..=4`) for Mouse events, or `delta_x` (px) for Wheel.
    pub button: i32,
    /// Wheel `delta_y` (px), or DOM `keyCode` (`0` = auto) for Key events.
    pub data: i32,
    /// Optional UTF-8 payload (`"key\tcode"` for KeyDown/Up, UTF-8 string for Text).
    pub payload: String,
}

impl StreamInputEvent {
    /// Creates a `MouseMove` event at device pixel coordinates `(x, y)`.
    pub fn mouse_move(x: i32, y: i32, modifiers: Modifiers) -> Self {
        Self {
            kind: StreamInputType::MouseMove,
            modifiers,
            x,
            y,
            button: 0,
            data: 0,
            payload: String::new(),
        }
    }

    /// Creates a `MouseDown` event at device pixel coordinates `(x, y)`.
    pub fn mouse_down(x: i32, y: i32, button: MouseButton, modifiers: Modifiers) -> Self {
        Self {
            kind: StreamInputType::MouseDown,
            modifiers,
            x,
            y,
            button: button as i32,
            data: 0,
            payload: String::new(),
        }
    }

    /// Creates a `MouseUp` event at device pixel coordinates `(x, y)`.
    pub fn mouse_up(x: i32, y: i32, button: MouseButton, modifiers: Modifiers) -> Self {
        Self {
            kind: StreamInputType::MouseUp,
            modifiers,
            x,
            y,
            button: button as i32,
            data: 0,
            payload: String::new(),
        }
    }

    /// Creates a `Wheel` scroll event at `(x, y)` with pixel deltas `(delta_x, delta_y)`.
    pub fn wheel(x: i32, y: i32, delta_x: i32, delta_y: i32, modifiers: Modifiers) -> Self {
        Self {
            kind: StreamInputType::Wheel,
            modifiers,
            x,
            y,
            button: delta_x,
            data: delta_y,
            payload: String::new(),
        }
    }

    /// Creates a `KeyDown` event encoding `"key\tcode"` in the UTF-8 payload.
    pub fn key_down(
        key: &str,
        code: &str,
        repeat: bool,
        location: i32,
        key_code: i32,
        modifiers: Modifiers,
    ) -> Self {
        Self {
            kind: StreamInputType::KeyDown,
            modifiers,
            x: i32::from(repeat),
            y: location,
            button: 0,
            data: key_code,
            payload: format!("{key}\t{code}"),
        }
    }

    /// Creates a `KeyUp` event encoding `"key\tcode"` in the UTF-8 payload.
    pub fn key_up(
        key: &str,
        code: &str,
        location: i32,
        key_code: i32,
        modifiers: Modifiers,
    ) -> Self {
        Self {
            kind: StreamInputType::KeyUp,
            modifiers,
            x: 0,
            y: location,
            button: 0,
            data: key_code,
            payload: format!("{key}\t{code}"),
        }
    }

    /// Creates a UTF-8 `Text` composition/paste event.
    pub fn text(text: impl Into<String>, modifiers: Modifiers) -> Self {
        Self {
            kind: StreamInputType::Text,
            modifiers,
            x: 0,
            y: 0,
            button: 0,
            data: 0,
            payload: text.into(),
        }
    }

    /// Serializes this event into a little-endian `CAMI` packet.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let bytes = self.payload.as_bytes();
        if bytes.len() > MAX_INPUT_PAYLOAD_BYTES {
            return Err(CamoufoxError::ValueError(format!(
                "CAMI payload length {} exceeds maximum ({MAX_INPUT_PAYLOAD_BYTES} bytes)",
                bytes.len()
            )));
        }
        let mut out = Vec::with_capacity(CAMI_HEADER_SIZE + bytes.len());
        out.extend_from_slice(&CAMI_MAGIC.to_le_bytes());
        out.extend_from_slice(&STREAM_PROTOCOL_VERSION.to_le_bytes());
        out.extend_from_slice(&(self.kind as u32).to_le_bytes());
        out.extend_from_slice(&self.modifiers.bits().to_le_bytes());
        out.extend_from_slice(&self.x.to_le_bytes());
        out.extend_from_slice(&self.y.to_le_bytes());
        out.extend_from_slice(&self.button.to_le_bytes());
        out.extend_from_slice(&self.data.to_le_bytes());
        out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(bytes);
        Ok(out)
    }
}

/// Generates a short temporary Unix domain socket path safe for `sockaddr_un::sun_path`.
pub(crate) fn allocate_temp_socket_path() -> PathBuf {
    let seq = SOCKET_SEQ.fetch_add(1, Ordering::Relaxed);
    let name = format!("camou-s-{}-{seq}.sock", std::process::id());
    let candidate = std::env::temp_dir().join(&name);
    if candidate.to_string_lossy().len() <= UNIX_SUN_PATH_SAFE_MAX {
        candidate
    } else {
        PathBuf::from("/tmp").join(name)
    }
}

/// Validates [`HeadlessStreamOptions`] and populates `CAMOU_HEADLESS_STREAM_*` in `env`.
pub fn apply_headless_stream_env(
    stream_opts: &HeadlessStreamOptions,
    headless_bool: bool,
    virtual_display: Option<&str>,
    env: &mut BTreeMap<String, String>,
) -> Result<()> {
    #[cfg(not(unix))]
    {
        let _ = (stream_opts, headless_bool, virtual_display, env);
        return Err(CamoufoxError::ValueError(
            "headless_stream uses AF_UNIX domain sockets and is currently supported on Linux and macOS only.".into(),
        ));
    }

    #[cfg(unix)]
    {
        if !headless_bool && virtual_display.is_none() {
            warn(
                "headless_stream captures frames only when launching in headless mode (HeadlessMode::Bool(true)).",
                "UserWarning",
            );
        }
        let socket_path = stream_opts
            .socket_path
            .clone()
            .unwrap_or_else(allocate_temp_socket_path);
        let socket_str = socket_path.to_str().ok_or_else(|| {
            CamoufoxError::ValueError("headless_stream socket_path must be valid UTF-8".into())
        })?;
        if socket_str.len() > UNIX_SUN_PATH_SAFE_MAX {
            return Err(CamoufoxError::ValueError(format!(
                "headless_stream socket_path is too long for sockaddr_un ({} > {UNIX_SUN_PATH_SAFE_MAX} bytes): {socket_str}",
                socket_str.len()
            )));
        }
        if let Some(parent) = socket_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        env.insert(CAMOU_STREAM_SOCKET_ENV.into(), socket_str.to_owned());

        if let Some(fps) = stream_opts.fps {
            if !(1..=120).contains(&fps) {
                return Err(CamoufoxError::ValueError(format!(
                    "headless_stream.fps must be in 1..=120, got {fps}"
                )));
            }
            env.insert(CAMOU_STREAM_FPS_ENV.into(), fps.to_string());
        }
        if let Some(format) = stream_opts.format {
            env.insert(CAMOU_STREAM_FORMAT_ENV.into(), format.as_env_str().to_owned());
        }
        Ok(())
    }
}

/// Read half of a split headless stream connection yielding `CAMF` frames.
#[cfg(unix)]
pub struct HeadlessStreamReader {
    half: OwnedReadHalf,
}

#[cfg(unix)]
impl HeadlessStreamReader {
    /// Reads and validates the next `CAMF` video frame from the stream.
    pub async fn next_frame(&mut self) -> Result<StreamFrame> {
        let mut header = [0u8; CAMF_HEADER_SIZE];
        self.half.read_exact(&mut header).await?;
        let (width, height, format, payload_size, pts_us) = StreamFrame::decode_header(&header)?;
        let mut data = vec![0u8; payload_size];
        self.half.read_exact(&mut data).await?;
        Ok(StreamFrame {
            width,
            height,
            format,
            pts_us,
            data,
        })
    }
}

/// Write half of a split headless stream connection sending `CAMI` input events.
#[cfg(unix)]
pub struct HeadlessStreamWriter {
    half: OwnedWriteHalf,
}

#[cfg(unix)]
impl HeadlessStreamWriter {
    /// Sends a raw [`StreamInputEvent`] over the socket.
    pub async fn send_event(&mut self, event: &StreamInputEvent) -> Result<()> {
        let packet = event.encode()?;
        self.half.write_all(&packet).await?;
        self.half.flush().await?;
        Ok(())
    }

    pub async fn mouse_move(&mut self, x: i32, y: i32, modifiers: Modifiers) -> Result<()> {
        self.send_event(&StreamInputEvent::mouse_move(x, y, modifiers))
            .await
    }

    pub async fn mouse_down(
        &mut self,
        x: i32,
        y: i32,
        button: MouseButton,
        modifiers: Modifiers,
    ) -> Result<()> {
        self.send_event(&StreamInputEvent::mouse_down(x, y, button, modifiers))
            .await
    }

    pub async fn mouse_up(
        &mut self,
        x: i32,
        y: i32,
        button: MouseButton,
        modifiers: Modifiers,
    ) -> Result<()> {
        self.send_event(&StreamInputEvent::mouse_up(x, y, button, modifiers))
            .await
    }

    /// Sends a mouse move followed by press + release at `(x, y)`.
    pub async fn click(
        &mut self,
        x: i32,
        y: i32,
        button: MouseButton,
        modifiers: Modifiers,
    ) -> Result<()> {
        self.mouse_move(x, y, modifiers).await?;
        self.mouse_down(x, y, button, modifiers).await?;
        self.mouse_up(x, y, button, modifiers).await
    }

    /// Sends a native double-click sequence (`move`, `down`, `up`, `down`, `up`).
    pub async fn dblclick(
        &mut self,
        x: i32,
        y: i32,
        button: MouseButton,
        modifiers: Modifiers,
    ) -> Result<()> {
        self.click(x, y, button, modifiers).await?;
        self.mouse_down(x, y, button, modifiers).await?;
        self.mouse_up(x, y, button, modifiers).await
    }

    pub async fn wheel(
        &mut self,
        x: i32,
        y: i32,
        delta_x: i32,
        delta_y: i32,
        modifiers: Modifiers,
    ) -> Result<()> {
        self.send_event(&StreamInputEvent::wheel(x, y, delta_x, delta_y, modifiers))
            .await
    }

    pub async fn key_down(
        &mut self,
        key: &str,
        code: &str,
        repeat: bool,
        location: i32,
        key_code: i32,
        modifiers: Modifiers,
    ) -> Result<()> {
        self.send_event(&StreamInputEvent::key_down(
            key, code, repeat, location, key_code, modifiers,
        ))
        .await
    }

    pub async fn key_up(
        &mut self,
        key: &str,
        code: &str,
        location: i32,
        key_code: i32,
        modifiers: Modifiers,
    ) -> Result<()> {
        self.send_event(&StreamInputEvent::key_up(
            key, code, location, key_code, modifiers,
        ))
        .await
    }

    pub async fn press_key(
        &mut self,
        key: &str,
        code: &str,
        key_code: i32,
        modifiers: Modifiers,
    ) -> Result<()> {
        self.key_down(key, code, false, 0, key_code, modifiers).await?;
        self.key_up(key, code, 0, key_code, modifiers).await
    }

    pub async fn type_text(&mut self, text: impl Into<String>, modifiers: Modifiers) -> Result<()> {
        self.send_event(&StreamInputEvent::text(text, modifiers)).await
    }
}

/// Async client connected to Camoufox's `HeadlessStreamServer` (`AF_UNIX`).
#[cfg(unix)]
pub struct HeadlessStreamClient {
    reader: HeadlessStreamReader,
    writer: HeadlessStreamWriter,
}

#[cfg(unix)]
impl HeadlessStreamClient {
    /// Connects immediately to an already-bound `CAMOU_HEADLESS_STREAM_SOCKET`.
    pub async fn connect(path: impl AsRef<Path>) -> Result<Self> {
        let stream = UnixStream::connect(path.as_ref()).await?;
        let (read_half, write_half) = stream.into_split();
        Ok(Self {
            reader: HeadlessStreamReader { half: read_half },
            writer: HeadlessStreamWriter { half: write_half },
        })
    }

    /// Connects with polling backoff until `timeout` elapses (for use right after launch).
    pub async fn connect_with_retry(path: impl AsRef<Path>, timeout: Duration) -> Result<Self> {
        let path = path.as_ref();
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            match Self::connect(path).await {
                Ok(client) => return Ok(client),
                Err(err) => {
                    if tokio::time::Instant::now() >= deadline {
                        return Err(CamoufoxError::Io(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            format!(
                                "Timed out connecting to headless stream socket '{}': {err}",
                                path.display()
                            ),
                        )));
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            }
        }
    }

    /// Splits into independent reader (`CAMF`) and writer (`CAMI`) halves for concurrent tasks.
    pub fn into_split(self) -> (HeadlessStreamReader, HeadlessStreamWriter) {
        (self.reader, self.writer)
    }

    pub async fn next_frame(&mut self) -> Result<StreamFrame> {
        self.reader.next_frame().await
    }

    pub async fn send_event(&mut self, event: &StreamInputEvent) -> Result<()> {
        self.writer.send_event(event).await
    }

    pub async fn mouse_move(&mut self, x: i32, y: i32, modifiers: Modifiers) -> Result<()> {
        self.writer.mouse_move(x, y, modifiers).await
    }

    pub async fn mouse_down(
        &mut self,
        x: i32,
        y: i32,
        button: MouseButton,
        modifiers: Modifiers,
    ) -> Result<()> {
        self.writer.mouse_down(x, y, button, modifiers).await
    }

    pub async fn mouse_up(
        &mut self,
        x: i32,
        y: i32,
        button: MouseButton,
        modifiers: Modifiers,
    ) -> Result<()> {
        self.writer.mouse_up(x, y, button, modifiers).await
    }

    pub async fn click(
        &mut self,
        x: i32,
        y: i32,
        button: MouseButton,
        modifiers: Modifiers,
    ) -> Result<()> {
        self.writer.click(x, y, button, modifiers).await
    }

    pub async fn dblclick(
        &mut self,
        x: i32,
        y: i32,
        button: MouseButton,
        modifiers: Modifiers,
    ) -> Result<()> {
        self.writer.dblclick(x, y, button, modifiers).await
    }

    pub async fn wheel(
        &mut self,
        x: i32,
        y: i32,
        delta_x: i32,
        delta_y: i32,
        modifiers: Modifiers,
    ) -> Result<()> {
        self.writer.wheel(x, y, delta_x, delta_y, modifiers).await
    }

    pub async fn key_down(
        &mut self,
        key: &str,
        code: &str,
        repeat: bool,
        location: i32,
        key_code: i32,
        modifiers: Modifiers,
    ) -> Result<()> {
        self.writer
            .key_down(key, code, repeat, location, key_code, modifiers)
            .await
    }

    pub async fn key_up(
        &mut self,
        key: &str,
        code: &str,
        location: i32,
        key_code: i32,
        modifiers: Modifiers,
    ) -> Result<()> {
        self.writer
            .key_up(key, code, location, key_code, modifiers)
            .await
    }

    pub async fn press_key(
        &mut self,
        key: &str,
        code: &str,
        key_code: i32,
        modifiers: Modifiers,
    ) -> Result<()> {
        self.writer.press_key(key, code, key_code, modifiers).await
    }

    pub async fn type_text(&mut self, text: impl Into<String>, modifiers: Modifiers) -> Result<()> {
        self.writer.type_text(text, modifiers).await
    }
}

/// Stub reader on non-Unix platforms matching `#if defined(XP_UNIX)` in the browser patch.
#[cfg(not(unix))]
pub struct HeadlessStreamReader;

/// Stub writer on non-Unix platforms matching `#if defined(XP_UNIX)` in the browser patch.
#[cfg(not(unix))]
pub struct HeadlessStreamWriter;

/// Stub client on non-Unix platforms matching `#if defined(XP_UNIX)` in the browser patch.
#[cfg(not(unix))]
pub struct HeadlessStreamClient;

#[cfg(not(unix))]
impl HeadlessStreamClient {
    pub async fn connect(_path: impl AsRef<Path>) -> Result<Self> {
        Err(CamoufoxError::ValueError(
            "HeadlessStreamClient requires Unix domain sockets (Linux/macOS).".into(),
        ))
    }

    pub async fn connect_with_retry(_path: impl AsRef<Path>, _timeout: Duration) -> Result<Self> {
        Self::connect("").await
    }
}
