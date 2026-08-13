use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use base64::Engine as _;
use sha2::{Digest as _, Sha256};

const PREVIEW_IMAGE_ID: u32 = 2_000_001;
const PREVIEW_PLACEMENT_ID: u32 = 1;
const KITTY_CHUNK_BYTES: usize = 3072;
const CACHE_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct InlinePreviewLayout {
    pub(super) x: u16,
    pub(super) y: u16,
    pub(super) width: u16,
    pub(super) height: u16,
    pub(super) image_x: u16,
    pub(super) image_y: u16,
    pub(super) image_cols: u16,
    pub(super) image_rows: u16,
}

pub(super) fn inline_layout(
    png: &[u8],
    terminal_cols: u16,
    terminal_rows: u16,
    cell_width_px: u32,
    cell_height_px: u32,
) -> Option<InlinePreviewLayout> {
    let (image_width, image_height) = png_dimensions(png)?;
    if terminal_cols < 8 || terminal_rows < 6 {
        return None;
    }
    let max_cols = terminal_cols.saturating_sub(6).max(1);
    let max_rows = terminal_rows.saturating_sub(6).max(1);
    let cell_width_px = cell_width_px.max(1);
    let cell_height_px = cell_height_px.max(1);
    let max_width_px = u64::from(max_cols) * u64::from(cell_width_px);
    let max_height_px = u64::from(max_rows) * u64::from(cell_height_px);

    let width_scale = max_width_px as f64 / image_width as f64;
    let height_scale = max_height_px as f64 / image_height as f64;
    let scale = width_scale.min(height_scale).min(1.0);
    let shown_width_px = (image_width as f64 * scale).ceil().max(1.0) as u64;
    let shown_height_px = (image_height as f64 * scale).ceil().max(1.0) as u64;
    let image_cols = shown_width_px
        .div_ceil(u64::from(cell_width_px))
        .min(u64::from(max_cols)) as u16;
    let image_rows = shown_height_px
        .div_ceil(u64::from(cell_height_px))
        .min(u64::from(max_rows)) as u16;
    let width = image_cols.saturating_add(2);
    let height = image_rows.saturating_add(2);
    let x = terminal_cols.saturating_sub(width) / 2;
    let y = terminal_rows.saturating_sub(height) / 2;

    Some(InlinePreviewLayout {
        x,
        y,
        width,
        height,
        image_x: x + 1,
        image_y: y + 1,
        image_cols,
        image_rows,
    })
}

pub(super) fn encode_inline_png(png: &[u8], layout: InlinePreviewLayout) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"\x1b7");
    bytes.extend_from_slice(clear_inline_preview_bytes().as_slice());
    let _ = write!(bytes, "\x1b[{};{}H", layout.image_y + 1, layout.image_x + 1);
    let control = format!(
        "a=T,t=d,f=100,i={PREVIEW_IMAGE_ID},p={PREVIEW_PLACEMENT_ID},c={},r={},z=100,C=1,q=2",
        layout.image_cols, layout.image_rows
    );
    encode_kitty_data(&mut bytes, &control, png);
    bytes.extend_from_slice(b"\x1b8");
    bytes
}

pub(super) fn clear_inline_preview_bytes() -> Vec<u8> {
    format!("\x1b_Ga=d,d=I,i={PREVIEW_IMAGE_ID},q=2;\x1b\\").into_bytes()
}

pub(super) fn stage_for_local_viewer(
    name: &str,
    extension: &str,
    data: &[u8],
) -> io::Result<PathBuf> {
    let extension = sanitized_extension(extension);
    let dir = ensure_cache_dir()?;
    cleanup_stale(&dir);
    let digest = Sha256::digest(data);
    let stem = sanitized_stem(name);
    let path = dir.join(format!("{stem}-{:x}.{extension}", digest));
    if !path.exists() {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        restrict_file_options(&mut options);
        let mut file = options.open(&path)?;
        file.write_all(data)?;
    }
    Ok(path)
}

fn png_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    if data.len() < 24
        || !data.starts_with(b"\x89PNG\r\n\x1a\n")
        || data.get(12..16) != Some(b"IHDR")
    {
        return None;
    }
    let width = u32::from_be_bytes(data.get(16..20)?.try_into().ok()?);
    let height = u32::from_be_bytes(data.get(20..24)?.try_into().ok()?);
    (width > 0 && height > 0).then_some((width, height))
}

fn encode_kitty_data(out: &mut Vec<u8>, control: &str, data: &[u8]) {
    let mut chunks = data.chunks(KITTY_CHUNK_BYTES).peekable();
    let Some(first) = chunks.next() else {
        return;
    };
    let more = usize::from(chunks.peek().is_some());
    let encoded = base64::engine::general_purpose::STANDARD.encode(first);
    let _ = write!(out, "\x1b_G{control},m={more};{encoded}\x1b\\");
    while let Some(chunk) = chunks.next() {
        let more = usize::from(chunks.peek().is_some());
        let encoded = base64::engine::general_purpose::STANDARD.encode(chunk);
        let _ = write!(out, "\x1b_Gm={more};{encoded}\x1b\\");
    }
}

fn cache_dir() -> PathBuf {
    #[cfg(unix)]
    let user_id = unsafe { libc::geteuid() };
    #[cfg(not(unix))]
    let user_id = std::process::id();
    std::env::temp_dir().join(format!("herdr-image-previews-{user_id}"))
}

fn ensure_cache_dir() -> io::Result<PathBuf> {
    let dir = cache_dir();
    fs::create_dir_all(&dir)?;
    restrict_dir_permissions(&dir)?;
    Ok(dir)
}

fn sanitized_extension(extension: &str) -> &'static str {
    if extension.eq_ignore_ascii_case("png") {
        "png"
    } else if extension.eq_ignore_ascii_case("jpg") || extension.eq_ignore_ascii_case("jpeg") {
        "jpg"
    } else if extension.eq_ignore_ascii_case("gif") {
        "gif"
    } else if extension.eq_ignore_ascii_case("webp") {
        "webp"
    } else if extension.eq_ignore_ascii_case("bmp") {
        "bmp"
    } else {
        "img"
    }
}

fn sanitized_stem(name: &str) -> String {
    let stem = Path::new(name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("image");
    let sanitized: String = stem
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '-'
            }
        })
        .take(48)
        .collect();
    if sanitized.is_empty() {
        "image".to_owned()
    } else {
        sanitized
    }
}

#[cfg(unix)]
fn restrict_file_options(options: &mut fs::OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt as _;
    options.mode(0o600);
}

#[cfg(not(unix))]
fn restrict_file_options(_options: &mut fs::OpenOptions) {}

#[cfg(unix)]
fn restrict_dir_permissions(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn restrict_dir_permissions(_dir: &Path) -> io::Result<()> {
    Ok(())
}

fn cleanup_stale(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let modified = metadata.modified().unwrap_or(SystemTime::now());
        if modified.elapsed().unwrap_or_default() > CACHE_MAX_AGE {
            let _ = fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_header(width: u32, height: u32) -> Vec<u8> {
        let mut data = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        data.extend_from_slice(&width.to_be_bytes());
        data.extend_from_slice(&height.to_be_bytes());
        data
    }

    #[test]
    fn inline_layout_preserves_wide_image_aspect_inside_terminal() {
        let layout = inline_layout(&png_header(1600, 800), 100, 40, 8, 16).expect("layout");
        assert!(layout.image_cols <= 94);
        assert!(layout.image_rows <= 34);
        assert_eq!(layout.width, layout.image_cols + 2);
        assert_eq!(layout.height, layout.image_rows + 2);
        assert_eq!(layout.x + layout.width / 2, 50);
    }

    #[test]
    fn inline_encoder_uploads_png_at_reserved_image_id() {
        let png = png_header(10, 10);
        let layout = inline_layout(&png, 80, 24, 8, 16).expect("layout");
        let encoded = String::from_utf8(encode_inline_png(&png, layout)).expect("utf8 protocol");
        assert!(encoded.contains("a=T,t=d,f=100,i=2000001,p=1"));
        assert!(encoded.contains("z=100,C=1,q=2"));
        assert!(String::from_utf8(clear_inline_preview_bytes())
            .expect("utf8 delete")
            .contains("a=d,d=I,i=2000001"));
    }

    #[test]
    fn local_cache_names_are_sanitized_and_content_addressed() {
        let path = stage_for_local_viewer("../../my image.jpg", "jpg", b"jpeg").expect("stage");
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("name");
        assert!(name.starts_with("my-image-"));
        assert!(name.ends_with(".jpg"));
        assert_eq!(fs::read(&path).expect("read staged"), b"jpeg");
        let _ = fs::remove_file(path);
    }
}
