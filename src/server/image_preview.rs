use std::fmt;
use std::fs;
use std::io::{self, Read as _};
use std::path::Path;

use crate::protocol::MAX_IMAGE_PREVIEW_PAYLOAD;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImagePreview {
    pub(crate) name: String,
    pub(crate) extension: String,
    pub(crate) data: Vec<u8>,
}

#[derive(Debug)]
pub(crate) enum ImagePreviewError {
    Io(io::Error),
    NotAFile,
    UnsupportedFormat,
    TooLarge,
}

impl fmt::Display for ImagePreviewError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "could not read image: {err}"),
            Self::NotAFile => write!(f, "image target is not a regular file"),
            Self::UnsupportedFormat => write!(f, "unsupported or invalid image format"),
            Self::TooLarge => write!(
                f,
                "image exceeds the {} MiB preview limit",
                MAX_IMAGE_PREVIEW_PAYLOAD / (1024 * 1024)
            ),
        }
    }
}

impl From<io::Error> for ImagePreviewError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

pub(crate) fn load(path: &Path) -> Result<ImagePreview, ImagePreviewError> {
    let metadata = fs::metadata(path)?;
    if !metadata.is_file() {
        return Err(ImagePreviewError::NotAFile);
    }
    if metadata.len() > MAX_IMAGE_PREVIEW_PAYLOAD as u64 {
        return Err(ImagePreviewError::TooLarge);
    }

    let mut data = Vec::with_capacity(metadata.len() as usize);
    fs::File::open(path)?
        .take(MAX_IMAGE_PREVIEW_PAYLOAD as u64 + 1)
        .read_to_end(&mut data)?;
    if data.len() > MAX_IMAGE_PREVIEW_PAYLOAD {
        return Err(ImagePreviewError::TooLarge);
    }
    let extension = detected_extension(&data).ok_or(ImagePreviewError::UnsupportedFormat)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("image")
        .to_owned();

    Ok(ImagePreview {
        name,
        extension: extension.to_owned(),
        data,
    })
}

fn detected_extension(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if data.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("jpg")
    } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        Some("gif")
    } else if data.len() >= 12 && data.starts_with(b"RIFF") && data[8..12] == *b"WEBP" {
        Some("webp")
    } else if data.starts_with(b"BM") {
        Some("bmp")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "herdr-image-preview-{}-{}-{name}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ))
    }

    #[test]
    fn load_uses_signature_instead_of_extension() {
        let path = temp_path("generated.txt");
        fs::write(&path, b"\x89PNG\r\n\x1a\npreview").expect("write fixture");

        let preview = load(&path).expect("load image");
        assert!(preview.name.ends_with("generated.txt"));
        assert_eq!(preview.extension, "png");
        assert_eq!(preview.data, b"\x89PNG\r\n\x1a\npreview");

        let _ = fs::remove_file(path);
    }

    #[test]
    fn load_rejects_non_image_content() {
        let path = temp_path("not-an-image.png");
        fs::write(&path, b"not really a png").expect("write fixture");

        assert!(matches!(
            load(&path),
            Err(ImagePreviewError::UnsupportedFormat)
        ));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn load_rejects_oversized_files_before_reading_them() {
        let path = temp_path("oversized.png");
        let file = fs::File::create(&path).expect("create fixture");
        file.set_len(MAX_IMAGE_PREVIEW_PAYLOAD as u64 + 1)
            .expect("size fixture");

        assert!(matches!(load(&path), Err(ImagePreviewError::TooLarge)));

        let _ = fs::remove_file(path);
    }
}
