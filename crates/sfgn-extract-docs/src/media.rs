use std::fs::File;
use std::io::Read;
use std::path::Path;

/// High-level media category for binary proxy vertices in `doc.sfgn` / `media.sfgn`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaCategory {
    Image,
    Audio,
    Video,
    Document,
    Archive,
    Binary,
}

impl MediaCategory {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Audio => "audio",
            Self::Video => "video",
            Self::Document => "document",
            Self::Archive => "archive",
            Self::Binary => "binary",
        }
    }
}

/// Lightweight metadata extracted from a binary media file's header without loading the full blob.
#[derive(Debug, Clone)]
pub struct MediaProxyMeta {
    pub category: MediaCategory,
    pub mime_type: &'static str,
    pub size_bytes: u64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_secs: Option<f64>,
    pub sample_rate_hz: Option<u32>,
    pub channels: Option<u16>,
    pub detail: Option<String>,
}

/// Classifies a file extension into `(MediaCategory, mime_type)` if it is a binary media/asset file.
pub fn classify_media_extension(ext: &str) -> Option<(MediaCategory, &'static str)> {
    match ext.to_ascii_lowercase().as_str() {
        "png" => Some((MediaCategory::Image, "image/png")),
        "jpg" | "jpeg" => Some((MediaCategory::Image, "image/jpeg")),
        "gif" => Some((MediaCategory::Image, "image/gif")),
        "webp" => Some((MediaCategory::Image, "image/webp")),
        "bmp" => Some((MediaCategory::Image, "image/bmp")),
        "ico" => Some((MediaCategory::Image, "image/x-icon")),
        "wav" => Some((MediaCategory::Audio, "audio/wav")),
        "mp3" => Some((MediaCategory::Audio, "audio/mpeg")),
        "ogg" => Some((MediaCategory::Audio, "audio/ogg")),
        "flac" => Some((MediaCategory::Audio, "audio/flac")),
        "mp4" => Some((MediaCategory::Video, "video/mp4")),
        "webm" => Some((MediaCategory::Video, "video/webm")),
        "mov" => Some((MediaCategory::Video, "video/quicktime")),
        "pdf" => Some((MediaCategory::Document, "application/pdf")),
        "zip" => Some((MediaCategory::Archive, "application/zip")),
        "tar" => Some((MediaCategory::Archive, "application/x-tar")),
        "gz" | "tgz" => Some((MediaCategory::Archive, "application/gzip")),
        "wasm" => Some((MediaCategory::Binary, "application/wasm")),
        "sqlite" | "db" => Some((MediaCategory::Binary, "application/vnd.sqlite3")),
        _ => None,
    }
}

/// Reads up to the first 64 KiB of `path` to parse dimensions, audio timing, or document headers.
pub fn inspect_media_file(path: &Path, size_bytes: u64) -> Option<MediaProxyMeta> {
    let ext = path.extension()?.to_str()?;
    let (category, mime_type) = classify_media_extension(ext)?;

    let mut header = Vec::new();
    if let Ok(file) = File::open(path) {
        let _ = file.take(65_536).read_to_end(&mut header);
    }

    let mut meta = MediaProxyMeta {
        category,
        mime_type,
        size_bytes,
        width: None,
        height: None,
        duration_secs: None,
        sample_rate_hz: None,
        channels: None,
        detail: None,
    };

    match mime_type {
        "image/png" => {
            if let Some((w, h)) = parse_png_dimensions(&header) {
                meta.width = Some(w);
                meta.height = Some(h);
            }
        }
        "image/gif" => {
            if let Some((w, h)) = parse_gif_dimensions(&header) {
                meta.width = Some(w);
                meta.height = Some(h);
            }
        }
        "image/jpeg" => {
            if let Some((w, h)) = parse_jpeg_dimensions(&header) {
                meta.width = Some(w);
                meta.height = Some(h);
            }
        }
        "image/webp" => {
            if let Some((w, h)) = parse_webp_dimensions(&header) {
                meta.width = Some(w);
                meta.height = Some(h);
            }
        }
        "audio/wav" => {
            if let Some((ch, sr, dur)) = parse_wav_header(&header) {
                meta.channels = Some(ch);
                meta.sample_rate_hz = Some(sr);
                meta.duration_secs = dur;
            }
        }
        "application/pdf" => {
            meta.detail = parse_pdf_header(&header);
        }
        _ => {}
    }

    Some(meta)
}

fn parse_png_dimensions(buf: &[u8]) -> Option<(u32, u32)> {
    const PNG_SIG: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    if buf.len() < 24 || &buf[..8] != PNG_SIG || &buf[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(buf[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(buf[20..24].try_into().ok()?);
    Some((width, height))
}

fn parse_gif_dimensions(buf: &[u8]) -> Option<(u32, u32)> {
    if buf.len() < 10 || (&buf[..6] != b"GIF87a" && &buf[..6] != b"GIF89a") {
        return None;
    }
    let width = u16::from_le_bytes(buf[6..8].try_into().ok()?) as u32;
    let height = u16::from_le_bytes(buf[8..10].try_into().ok()?) as u32;
    Some((width, height))
}

fn parse_jpeg_dimensions(buf: &[u8]) -> Option<(u32, u32)> {
    if buf.len() < 4 || buf[0] != 0xFF || buf[1] != 0xD8 {
        return None;
    }
    let mut i = 2;
    while i + 8 < buf.len() {
        if buf[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = buf[i + 1];
        if marker == 0xD9 || marker == 0xDA {
            break;
        }
        if matches!(marker, 0x01 | 0xD0..=0xD7) {
            i += 2;
            continue;
        }
        let seg_len = u16::from_be_bytes([buf[i + 2], buf[i + 3]]) as usize;
        if seg_len < 2 || i + 2 + seg_len > buf.len() {
            break;
        }
        if matches!(marker, 0xC0..=0xC3) && seg_len >= 7 {
            let height = u16::from_be_bytes([buf[i + 5], buf[i + 6]]) as u32;
            let width = u16::from_be_bytes([buf[i + 7], buf[i + 8]]) as u32;
            return Some((width, height));
        }
        i += 2 + seg_len;
    }
    None
}

fn parse_webp_dimensions(buf: &[u8]) -> Option<(u32, u32)> {
    if buf.len() < 30 || &buf[..4] != b"RIFF" || &buf[8..12] != b"WEBP" {
        return None;
    }
    match &buf[12..16] {
        b"VP8X" if buf.len() >= 30 => {
            let w_minus_1 = u32::from_le_bytes([buf[24], buf[25], buf[26], 0]);
            let h_minus_1 = u32::from_le_bytes([buf[27], buf[28], buf[29], 0]);
            Some((w_minus_1 + 1, h_minus_1 + 1))
        }
        b"VP8 " if buf.len() >= 30 && buf[23..26] == [0x9D, 0x01, 0x2A] => {
            let w = (u16::from_le_bytes([buf[26], buf[27]]) & 0x3FFF) as u32;
            let h = (u16::from_le_bytes([buf[28], buf[29]]) & 0x3FFF) as u32;
            Some((w, h))
        }
        b"VP8L" if buf.len() >= 25 && buf[20] == 0x2F => {
            let bits = u32::from_le_bytes([buf[21], buf[22], buf[23], buf[24]]);
            let w = (bits & 0x3FFF) + 1;
            let h = ((bits >> 14) & 0x3FFF) + 1;
            Some((w, h))
        }
        _ => None,
    }
}

fn parse_wav_header(buf: &[u8]) -> Option<(u16, u32, Option<f64>)> {
    if buf.len() < 36 || &buf[..4] != b"RIFF" || &buf[8..12] != b"WAVE" {
        return None;
    }
    let mut pos = 12;
    let mut channels = None;
    let mut sample_rate = None;
    let mut byte_rate = None;
    let mut data_size = None;

    while pos + 8 <= buf.len() {
        let chunk_id = &buf[pos..pos + 4];
        let chunk_size = u32::from_le_bytes(buf[pos + 4..pos + 8].try_into().ok()?) as usize;
        let body_start = pos + 8;

        if chunk_id == b"fmt " && body_start + 16 <= buf.len() {
            channels = Some(u16::from_le_bytes(
                buf[body_start + 2..body_start + 4].try_into().ok()?,
            ));
            sample_rate = Some(u32::from_le_bytes(
                buf[body_start + 4..body_start + 8].try_into().ok()?,
            ));
            byte_rate = Some(u32::from_le_bytes(
                buf[body_start + 8..body_start + 12].try_into().ok()?,
            ));
        } else if chunk_id == b"data" {
            data_size = Some(chunk_size as u64);
        }

        let padded = chunk_size + (chunk_size & 1);
        pos = body_start.saturating_add(padded);
    }

    let ch = channels?;
    let sr = sample_rate?;
    let duration = match (data_size, byte_rate) {
        (Some(bytes), Some(rate)) if rate > 0 => {
            let secs = (bytes as f64 / rate as f64 * 1000.0).round() / 1000.0;
            Some(secs)
        }
        _ => None,
    };

    Some((ch, sr, duration))
}

fn parse_pdf_header(buf: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(buf);
    let version = text
        .lines()
        .next()
        .and_then(|l| l.strip_prefix("%PDF-"))
        .map(str::trim)?;
    Some(format!("PDF {}", version))
}

