//! Ordered, echo-guarded text and image clipboard sharing. Clipboard work runs
//! on its own worker, never in capture callbacks or the network reader.
use crate::platform;
use kayiver_core::proto::Msg;
use std::{
    collections::VecDeque,
    hash::{Hash, Hasher},
    io::Cursor,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
const MAX_PNG: usize = 16 * 1024 * 1024;
const MAX_RGBA: usize = 64 * 1024 * 1024;
const CHUNK: usize = 32 * 1024;
const RETRIES: usize = 6;
#[derive(Clone, PartialEq, Eq)]
struct Image {
    width: usize,
    height: usize,
    rgba: Vec<u8>,
}
#[derive(Clone, PartialEq, Eq)]
enum Fingerprint {
    Text(String),
    Image(u64),
}
enum Command {
    Text(String),
    Image(Vec<u8>),
    Replay,
    ReplayImage,
}
#[derive(Default)]
pub struct ClipboardState {
    last: Option<Fingerprint>,
    commands: VecDeque<Command>,
    sent_images: u64,
    received_images: u64,
    image_size: Option<(usize, usize)>,
    error: Option<String>,
    source_kind: Option<&'static str>,
}
pub type ClipState = Arc<Mutex<ClipboardState>>;
static ACTIVE: OnceLock<ClipState> = OnceLock::new();
pub fn new_state() -> ClipState {
    Arc::new(Mutex::new(ClipboardState::default()))
}
pub fn status() -> serde_json::Value {
    let Some(state) = ACTIVE.get() else {
        return serde_json::json!({"running":false});
    };
    let s = state.lock().unwrap();
    serde_json::json!({"running":true,"sent_images":s.sent_images,"received_images":s.received_images,"image_size":s.image_size,"error":s.error,"source_kind":s.source_kind})
}
pub fn request_sync(image_only: bool) -> bool {
    let Some(state) = ACTIVE.get() else {
        return false;
    };
    enqueue(
        state,
        if image_only {
            Command::ReplayImage
        } else {
            Command::Replay
        },
    );
    true
}
fn enqueue(state: &ClipState, command: Command) {
    let mut s = state.lock().unwrap();
    if s.commands.len() >= 8 {
        s.commands.pop_front();
    }
    s.commands.push_back(command);
}
fn image_fingerprint(image: &Image) -> Fingerprint {
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    image.width.hash(&mut hash);
    image.height.hash(&mut hash);
    image.rgba.hash(&mut hash);
    Fingerprint::Image(hash.finish())
}
fn validate_image(image: &Image) -> anyhow::Result<()> {
    let bytes = image
        .width
        .checked_mul(image.height)
        .and_then(|n| n.checked_mul(4));
    anyhow::ensure!(
        image.width > 0
            && image.height > 0
            && bytes == Some(image.rgba.len())
            && image.rgba.len() <= MAX_RGBA,
        "clipboard image exceeds pixel limit or has invalid dimensions"
    );
    Ok(())
}
fn encode_image(image: &Image) -> anyhow::Result<Vec<u8>> {
    validate_image(image)?;
    let mut out = Vec::new();
    {
        let mut encoder =
            png::Encoder::new(&mut out, image.width.try_into()?, image.height.try_into()?);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        encoder.write_header()?.write_image_data(&image.rgba)?;
    }
    anyhow::ensure!(
        out.len() <= MAX_PNG,
        "clipboard image exceeds 16 MiB transfer limit"
    );
    Ok(out)
}
fn decode_image(bytes: &[u8]) -> anyhow::Result<Image> {
    anyhow::ensure!(
        bytes.len() <= MAX_PNG,
        "clipboard image exceeds transfer limit"
    );
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_limits(png::Limits { bytes: MAX_RGBA });
    let mut reader = decoder.read_info()?;
    let info = reader.info();
    let size = (info.width as usize)
        .checked_mul(info.height as usize)
        .and_then(|n| n.checked_mul(4));
    anyhow::ensure!(
        info.color_type == png::ColorType::Rgba
            && info.bit_depth == png::BitDepth::Eight
            && size.is_some_and(|n| n > 0 && n <= MAX_RGBA),
        "invalid clipboard PNG dimensions or pixel format"
    );
    let mut rgba = vec![0; size.unwrap()];
    let frame = reader.next_frame(&mut rgba)?;
    rgba.truncate(frame.buffer_size());
    let image = Image {
        width: frame.width as usize,
        height: frame.height as usize,
        rgba,
    };
    validate_image(&image)?;
    Ok(image)
}
fn decode_copied_image_file(path: &std::path::Path) -> anyhow::Result<Image> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    anyhow::ensure!(
        matches!(
            ext.as_str(),
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "tif" | "tiff" | "bmp"
        ),
        "copied file is not an image"
    );
    anyhow::ensure!(
        std::fs::metadata(path)?.len() <= MAX_PNG as u64,
        "copied image file exceeds size limit"
    );
    let mut reader = image::ImageReader::open(path)?.with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(MAX_RGBA as u64);
    reader.limits(limits);
    let decoded = reader.decode()?.to_rgba8();
    let image = Image {
        width: decoded.width() as usize,
        height: decoded.height() as usize,
        rgba: decoded.into_raw(),
    };
    validate_image(&image)?;
    Ok(image)
}

#[cfg(all(not(feature = "sim"), any(target_os = "macos", target_os = "windows")))]
fn read_image() -> Option<Image> {
    if let Ok(image) = arboard::Clipboard::new().and_then(|mut c| c.get_image()) {
        return Some(Image {
            width: image.width,
            height: image.height,
            rgba: image.bytes.into_owned(),
        });
    }
    let file = platform::get_clipboard_file()?;
    decode_copied_image_file(std::path::Path::new(&file)).ok()
}
#[cfg(feature = "sim")]
fn read_image() -> Option<Image> {
    platform::get_clipboard_image().map(|(width, height, rgba)| Image {
        width,
        height,
        rgba,
    })
}
#[cfg(all(
    not(feature = "sim"),
    not(any(target_os = "macos", target_os = "windows"))
))]
fn read_image() -> Option<Image> {
    None
}
#[cfg(all(not(feature = "sim"), any(target_os = "macos", target_os = "windows")))]
fn write_image(image: &Image) -> anyhow::Result<()> {
    arboard::Clipboard::new()?.set_image(arboard::ImageData {
        width: image.width,
        height: image.height,
        bytes: std::borrow::Cow::Borrowed(&image.rgba),
    })?;
    Ok(())
}
#[cfg(feature = "sim")]
fn write_image(image: &Image) -> anyhow::Result<()> {
    platform::set_clipboard_image(image.width, image.height, image.rgba.clone());
    Ok(())
}
#[cfg(all(
    not(feature = "sim"),
    not(any(target_os = "macos", target_os = "windows"))
))]
fn write_image(_image: &Image) -> anyhow::Result<()> {
    anyhow::bail!("image clipboard unavailable on this backend")
}
fn apply_image(state: &ClipState, png: &[u8]) -> anyhow::Result<()> {
    let image = decode_image(png)?;
    let mut result = Err(anyhow::anyhow!("image clipboard busy"));
    for _ in 0..RETRIES {
        result = write_image(&image);
        if result.is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(75));
    }
    result?;
    // Native backends may normalize alpha. Fingerprint the actual stored pixels
    // so a received image cannot bounce endlessly between platforms.
    let native = read_image().unwrap_or_else(|| image.clone());
    let mut s = state.lock().unwrap();
    s.last = Some(image_fingerprint(&native));
    s.received_images += 1;
    s.image_size = Some((image.width, image.height));
    s.error = None;
    Ok(())
}
pub fn watch(state: ClipState, send: impl Fn(Msg) + Send + 'static) {
    let _ = ACTIVE.set(state.clone());
    std::thread::Builder::new()
        .name("kayiver-clip".into())
        .spawn(move || {
            let first = read_image()
                .map(|i| image_fingerprint(&i))
                .or_else(|| platform::get_clipboard().map(Fingerprint::Text));
            if let Some(payload) = crate::engine::quickshare::detect_payload(
                match &first {
                    Some(Fingerprint::Text(text)) => Some(text.as_str()),
                    _ => None,
                },
                platform::get_clipboard_file().as_deref(),
            ) {
                crate::engine::quickshare::engine().update_candidate(payload);
            }
            state.lock().unwrap().last = first;
            let mut sequence = platform::clipboard_seq();
            let mut transfer = 0u64;
            loop {
                std::thread::sleep(Duration::from_millis(100));
                let command = { state.lock().unwrap().commands.pop_front() };
                let replay = matches!(command, Some(Command::Replay | Command::ReplayImage));
                let image_only = matches!(command, Some(Command::ReplayImage));
                match command {
                    Some(Command::Image(bytes)) => {
                        if let Err(error) = apply_image(&state, &bytes) {
                            state.lock().unwrap().error = Some(error.to_string());
                        }
                        sequence = platform::clipboard_seq();
                        continue;
                    }
                    Some(Command::Text(text)) => {
                        if state.lock().unwrap().last.as_ref()
                            != Some(&Fingerprint::Text(text.clone()))
                        {
                            platform::set_clipboard(&text);
                            if platform::get_clipboard().as_deref() == Some(&text) {
                                let mut s = state.lock().unwrap();
                                s.last = Some(Fingerprint::Text(text));
                                s.error = None;
                            } else {
                                state.lock().unwrap().error =
                                    Some("text clipboard write failed".into());
                            }
                        }
                        sequence = platform::clipboard_seq();
                        continue;
                    }
                    _ => {}
                }
                let seq = platform::clipboard_seq();
                if seq == sequence && !replay {
                    continue;
                }
                let mut image = None;
                let mut text = None;
                for attempt in 0..RETRIES {
                    image = read_image();
                    if image.is_some() {
                        break;
                    }
                    text = platform::get_clipboard();
                    if text.is_some() {
                        break;
                    }
                    if attempt + 1 < RETRIES {
                        std::thread::sleep(Duration::from_millis(75));
                    }
                }
                if platform::clipboard_seq() != seq {
                    continue;
                } // never send a torn copy
                sequence = seq;
                let file = platform::get_clipboard_file();
                state.lock().unwrap().source_kind = Some(if image.is_some() {
                    "image"
                } else if text.is_some() {
                    "text"
                } else if file.is_some() {
                    "file"
                } else {
                    "unknown"
                });
                if image_only && image.is_none() {
                    // An explicit image-only probe finding text is not a
                    // transport failure; keep the source-kind diagnostic only.
                    continue;
                }
                if let Some(payload) =
                    crate::engine::quickshare::detect_payload(text.as_deref(), file.as_deref())
                {
                    crate::engine::quickshare::engine().update_candidate(payload);
                }
                if let Some(image) = image {
                    let fingerprint = image_fingerprint(&image);
                    if !replay && state.lock().unwrap().last.as_ref() == Some(&fingerprint) {
                        continue;
                    }
                    match encode_image(&image) {
                        Ok(png) => {
                            transfer = transfer.wrapping_add(1);
                            send(Msg::ClipboardImageBegin {
                                id: transfer,
                                bytes: png.len() as u32,
                            });
                            for (index, chunk) in png.chunks(CHUNK).enumerate() {
                                send(Msg::ClipboardImageChunk {
                                    id: transfer,
                                    offset: (index * CHUNK) as u32,
                                    data: chunk.to_vec(),
                                });
                                // Yield between bounded chunks; input events can be enqueued
                                // between them without holding the session/capture locks.
                                std::thread::sleep(Duration::from_millis(2));
                            }
                            let mut s = state.lock().unwrap();
                            s.last = Some(fingerprint);
                            s.sent_images += 1;
                            s.image_size = Some((image.width, image.height));
                            s.error = None;
                        }
                        Err(error) => state.lock().unwrap().error = Some(error.to_string()),
                    }
                } else if let Some(text) = text.filter(|t| !t.is_empty()) {
                    let fingerprint = Fingerprint::Text(text.clone());
                    let mut s = state.lock().unwrap();
                    if replay || s.last.as_ref() != Some(&fingerprint) {
                        s.last = Some(fingerprint);
                        s.error = None;
                        drop(s);
                        send(Msg::Clipboard { text });
                    }
                }
            }
        })
        .ok();
}
pub fn apply_remote(state: &ClipState, text: &str) {
    enqueue(state, Command::Text(text.to_owned()));
}
pub fn apply_remote_image(state: &ClipState, bytes: Vec<u8>) {
    enqueue(state, Command::Image(bytes));
}

/// One bounded transfer per authenticated session. Reject missing, duplicate,
/// out-of-order and overlong chunks; discard partial data on session teardown.
#[derive(Default)]
pub struct ImageTransfer {
    pending: Option<(u64, usize, Vec<u8>, Instant)>,
}
impl ImageTransfer {
    pub fn begin(&mut self, id: u64, bytes: usize) {
        self.pending = if bytes > 0 && bytes <= MAX_PNG {
            Some((id, bytes, Vec::new(), Instant::now()))
        } else {
            None
        };
    }
    pub fn chunk(&mut self, id: u64, offset: usize, data: &[u8]) -> Option<Vec<u8>> {
        let valid = self
            .pending
            .as_ref()
            .is_some_and(|(key, total, buf, started)| {
                *key == id
                    && offset == buf.len()
                    && !data.is_empty()
                    && data.len() <= CHUNK
                    && data.len() <= total - buf.len()
                    && started.elapsed() < Duration::from_secs(30)
            });
        if !valid {
            self.pending = None;
            return None;
        }
        let (_, total, buf, _) = self.pending.as_mut()?;
        buf.extend_from_slice(data);
        if buf.len() == *total {
            return self.pending.take().map(|(_, _, buf, _)| buf);
        }
        None
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn copied_rgb_png_file_is_decoded_without_requiring_bitmap_clipboard() {
        let path =
            std::env::temp_dir().join(format!("kayiver-clipboard-file-{}.png", std::process::id()));
        image::RgbImage::from_raw(2, 1, vec![255, 0, 0, 0, 128, 255])
            .unwrap()
            .save(&path)
            .unwrap();
        let decoded = decode_copied_image_file(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        assert_eq!(decoded.rgba, vec![255, 0, 0, 255, 0, 128, 255, 255]);
        assert!(decode_copied_image_file(std::path::Path::new("unrelated.txt")).is_err());
    }
    #[test]
    fn image_png_roundtrip_preserves_rgba_and_alpha() {
        let image = Image {
            width: 2,
            height: 1,
            rgba: vec![255, 0, 0, 255, 0, 128, 255, 128],
        };
        assert_eq!(
            decode_image(&encode_image(&image).unwrap()).unwrap().rgba,
            image.rgba
        );
    }
    #[test]
    fn chunks_reassemble_once_and_fit_encrypted_frame() {
        let data = vec![77; CHUNK + 7];
        let mut transfer = ImageTransfer::default();
        transfer.begin(3, data.len());
        assert!(transfer.chunk(3, 0, &data[..CHUNK]).is_none());
        assert_eq!(transfer.chunk(3, CHUNK, &data[CHUNK..]).unwrap(), data);
        assert!(transfer.chunk(3, CHUNK, &data[CHUNK..]).is_none());
        let frame = Msg::ClipboardImageChunk {
            id: u64::MAX,
            offset: u32::MAX,
            data: vec![255; CHUNK],
        }
        .encode()
        .unwrap();
        assert!(frame.len() + 16 < kayiver_core::wire::MAX_FRAME);
    }
    #[test]
    fn reject_out_of_order_oversized_and_malformed_images() {
        let mut transfer = ImageTransfer::default();
        transfer.begin(4, 10);
        assert!(transfer.chunk(4, 5, &[1]).is_none());
        assert!(transfer.chunk(4, 0, &[1; 10]).is_none());
        transfer.begin(5, MAX_PNG + 1);
        assert!(transfer.chunk(5, 0, &[1]).is_none());
        assert!(decode_image(&[1, 2, 3]).is_err());
        assert!(encode_image(&Image {
            width: 2,
            height: 2,
            rgba: vec![0; 3]
        })
        .is_err());
    }
}
