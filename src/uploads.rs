//! Images the inbox attaches to a prompt: stored once on the daemon, then handed to the engine
//! natively (an `@` reference for omp, an encoded attachment for dsh).

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// The most one prompt image may weigh before base64. The engines normalize it down.
pub const MAX_BYTES: usize = 10 * 1024 * 1024;

/// One image saved for a prompt.
pub struct Upload {
    /// Absolute path of the saved copy, which omp takes as an `@` reference.
    pub path: PathBuf,
    /// Canonical base64 of the bytes, which dsh takes as an encoded attachment.
    pub base64: String,
    pub media_type: String,
    /// The display name the session sees. Never a path.
    pub name: String,
}

impl Upload {
    /// The image block a dsh launch or prompt carries.
    pub fn dsh_image(&self) -> serde_json::Value {
        serde_json::json!({
            "mediaType": self.media_type,
            "data": self.base64,
            "name": self.name,
        })
    }
}

/// Save an encoded image under `<db file directory>/uploads` and return it. The declared type
/// must be one the engines take, the bytes must match it, and the image must fit `MAX_BYTES`.
pub fn store(
    db_file: &Path,
    media_type: &str,
    data: &str,
    name: Option<&str>,
) -> Result<Upload, String> {
    let media_type = media_type.trim().to_ascii_lowercase();
    let extension = match media_type.as_str() {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => return Err("image must be PNG, JPEG, WebP or GIF".to_owned()),
    };
    // Base64 is four thirds of the bytes; refuse an oversized string before decoding it.
    if data.len() > MAX_BYTES / 3 * 4 + 8 {
        return Err("image is too large".to_owned());
    }
    let bytes = STANDARD
        .decode(data.trim())
        .map_err(|_| "image is not valid base64".to_owned())?;
    if bytes.len() > MAX_BYTES {
        return Err("image is too large".to_owned());
    }
    if sniff(&bytes) != Some(media_type.as_str()) {
        return Err("image bytes do not match its type".to_owned());
    }
    let directory = db_file.with_file_name("uploads");
    create_private_dir(&directory)?;
    let path = directory.join(format!("{}.{extension}", uuid::Uuid::new_v4()));
    write_private(&path, &bytes)?;
    let path = fs::canonicalize(&path).map_err(|error| error.to_string())?;
    Ok(Upload {
        path,
        base64: STANDARD.encode(&bytes),
        name: display_name(name, extension),
        media_type,
    })
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

fn create_private_dir(directory: &Path) -> Result<(), String> {
    if let Some(parent) = directory.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(directory) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(directory).map_err(|error| error.to_string())?;
            if !metadata.file_type().is_dir() {
                return Err(error.to_string());
            }
        }
        Err(error) => return Err(error.to_string()),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|error| error.to_string())?;
    file.write_all(bytes).map_err(|error| error.to_string())?;
    Ok(())
}

/// The original file name when it has one, else a name from the type. Only the base name is
/// kept and control characters are dropped; the name is display-only.
fn display_name(name: Option<&str>, extension: &str) -> String {
    let base = name
        .map(|name| name.rsplit(['/', '\\']).next().unwrap_or_default())
        .map(|name| name.chars().filter(|c| !c.is_control()).take(80).collect::<String>())
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty());
    base.unwrap_or_else(|| format!("image.{extension}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("triage-uploads-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn store_png(scratch: &Scratch, name: Option<&str>) -> Upload {
        store(
            &scratch.0.join("triage.db"),
            "IMAGE/PNG",
            &STANDARD.encode(PNG),
            name,
        )
        .unwrap()
    }

    #[test]
    fn a_png_is_saved_privately_and_canonicalized() {
        let scratch = Scratch::new();
        let upload = store_png(&scratch, Some("/tmp/shots/../shot.png"));
        assert_eq!(upload.media_type, "image/png");
        assert_eq!(upload.name, "shot.png");
        assert_eq!(fs::read(&upload.path).unwrap(), PNG);
        assert!(upload.path.starts_with(scratch.0.join("uploads")));
        assert_eq!(
            STANDARD.decode(&upload.base64).unwrap(),
            PNG,
            "the dsh copy is canonical base64"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&upload.path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn a_name_without_a_file_part_falls_back_to_the_type() {
        let scratch = Scratch::new();
        assert_eq!(store_png(&scratch, None).name, "image.png");
        assert_eq!(store_png(&scratch, Some("   ")).name, "image.png");
    }

    #[test]
    fn bad_bytes_or_a_bad_type_are_refused() {
        let scratch = Scratch::new();
        let db = scratch.0.join("triage.db");
        let png = STANDARD.encode(PNG);
        assert!(store(&db, "image/svg+xml", &png, None).is_err());
        assert!(store(&db, "image/png", "not base64", None).is_err());
        assert!(store(&db, "image/png", &STANDARD.encode(b"not an image"), None).is_err());
        assert!(store(&db, "image/webp", &png, None).is_err());
        assert!(store(&db, "image/jpeg", &png, None).is_err());
        assert!(store(&db, "image/png", &"A".repeat(MAX_BYTES / 3 * 4 + 9), None).is_err());
    }
}
