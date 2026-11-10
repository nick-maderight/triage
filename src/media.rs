//! Card images: bytes from the harness store that keeps them, never from a request path.

use crate::events::{Card, image_hash};
use std::path::{Path, PathBuf};

/// The largest image the inbox serves, in bytes.
pub const MAX_BYTES: u64 = 32 * 1024 * 1024;

/// The roots of the two image stores, read from the daemons' home directories at startup.
#[derive(Clone, Debug, Default)]
pub struct Roots {
    pub omp_blobs: PathBuf,
    pub dsh_objects: PathBuf,
}

/// The bytes of one image on `card` and the type to serve them as. `None` when the card has no
/// such image, its name is not a content hash, or the stored bytes are not that kind of image.
pub fn image(roots: &Roots, card: &Card, index: usize) -> Option<(String, Vec<u8>)> {
    let image = card.images.get(index)?;
    if !image_hash(&image.id) {
        return None;
    }
    let path = if card.engine.as_deref() == Some("dsh") {
        roots.dsh_objects.join(&image.id[..2]).join(&image.id)
    } else {
        roots.omp_blobs.join(&image.id)
    };
    read(&path, &image.media_type)
}

fn read(path: &Path, declared: &str) -> Option<(String, Vec<u8>)> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let detected = sniff(&bytes)?;
    (detected == declared.to_ascii_lowercase()).then(|| (detected.to_owned(), bytes))
}

/// The media type the bytes themselves declare. SVG, HTML and every other type are refused.
fn sniff(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some("image/png");
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("image/jpeg");
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{CardImage, CardOption, Kind, Star};
    use std::fs;

    const PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];
    const HASH: &str = "3addf06e255025540c5534d36ea135ec2054c082bb0c9291b9679dcbe1ba950a";

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("triage-media-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn card(engine: Option<&str>, id: &str, media_type: &str) -> Card {
        Card {
            id: "done:%1:a".to_owned(),
            kind: Kind::Info,
            priority: 3,
            created_ms: 0,
            source: crate::events::Source::default(),
            title: "Done".to_owned(),
            star: Star::default(),
            options: Vec::<CardOption>::new(),
            answerable: true,
            allow_other: false,
            questions: Vec::new(),
            body: None,
            images: vec![CardImage {
                id: id.to_owned(),
                media_type: media_type.to_owned(),
                name: Some("shot.png".to_owned()),
                width: None,
                height: None,
            }],
            refs: Vec::new(),
            level: None,
            tool_call_id: None,
            report_due: false,
            engine: engine.map(str::to_owned),
        }
    }

    #[test]
    fn an_image_resolves_from_each_store() {
        let scratch = Scratch::new();
        let roots = Roots {
            omp_blobs: scratch.0.join("blobs"),
            dsh_objects: scratch.0.join("objects"),
        };
        fs::create_dir_all(&roots.omp_blobs).unwrap();
        fs::create_dir_all(roots.dsh_objects.join(&HASH[..2])).unwrap();
        fs::write(roots.omp_blobs.join(HASH), PNG).unwrap();
        fs::write(roots.dsh_objects.join(&HASH[..2]).join(HASH), PNG).unwrap();

        let (media_type, bytes) = image(&roots, &card(None, HASH, "image/png"), 0).unwrap();
        assert_eq!(media_type, "image/png");
        assert_eq!(bytes, PNG);
        let (media_type, bytes) =
            image(&roots, &card(Some("dsh"), HASH, "image/png"), 0).unwrap();
        assert_eq!(media_type, "image/png");
        assert_eq!(bytes, PNG);
    }

    #[test]
    fn a_bad_name_bytes_or_index_is_refused() {
        let scratch = Scratch::new();
        let roots = Roots {
            omp_blobs: scratch.0.join("blobs"),
            dsh_objects: scratch.0.join("objects"),
        };
        fs::create_dir_all(&roots.omp_blobs).unwrap();
        fs::write(roots.omp_blobs.join(HASH), b"not an image").unwrap();

        assert!(image(&roots, &card(None, "../../etc/passwd", "image/png"), 0).is_none());
        assert!(image(&roots, &card(None, HASH, "image/png"), 0).is_none());
        assert!(image(&roots, &card(None, HASH, "image/webp"), 0).is_none());
        assert!(image(&roots, &card(None, HASH, "image/png"), 1).is_none());
        assert_eq!(card(None, HASH, "image/png").images.len(), 1);
    }

    #[test]
    fn an_oversized_image_is_refused_before_it_is_read() {
        let scratch = Scratch::new();
        let roots = Roots {
            omp_blobs: scratch.0.join("blobs"),
            dsh_objects: scratch.0.join("objects"),
        };
        fs::create_dir_all(&roots.omp_blobs).unwrap();
        let file = fs::File::create(roots.omp_blobs.join(HASH)).unwrap();
        file.set_len(MAX_BYTES + 1).unwrap();

        assert!(image(&roots, &card(None, HASH, "image/png"), 0).is_none());
    }
}
