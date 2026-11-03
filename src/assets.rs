//! Reading project assets, confined to the configured assets directory.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{Cursor, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use image::{ImageReader, Limits};
use tiny_skia::{IntSize, Pixmap};
use usvg::roxmltree::{Document, ParsingOptions};
use usvg::{ImageHrefResolver, Options, Tree};

use crate::diagnostics::Diagnostic;
use crate::tokens::Color;

const MAX_ASSET_BYTES: u64 = 64 * 1024 * 1024;
const MAX_IMAGE_SIDE: u32 = 16384;
const SVG_NAMESPACE: &str = "http://www.w3.org/2000/svg";
const FONT_DIRECTORY: &str = "fonts";
const FONT_EXTENSIONS: [&str; 4] = ["ttf", "otf", "ttc", "otc"];

/// Installs `bytes` at a path that must not exist yet. `Ok(false)` means it already did, and the
/// existing file is untouched. A failed write leaves nothing behind.
pub fn write_new_file(path: &Path, bytes: &[u8]) -> std::io::Result<bool> {
    let parent = path.parent().expect("asset paths have a parent");
    fs::create_dir_all(parent)?;
    let mut file = match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::AlreadyExists => return Ok(false),
        Err(error) => return Err(error),
    };
    file.write_all(bytes).inspect_err(|_| {
        // The write error is the one to report.
        let _ = fs::remove_file(path);
    })?;
    Ok(true)
}

/// Replaces a file through a sibling temporary file, so a failure never leaves a partial one.
pub fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().expect("written paths have a parent");
    fs::create_dir_all(parent)?;
    let file_name = path.file_name().expect("written paths have a file name");
    let temporary_path = parent.join(format!(
        ".{}.{}.tmp",
        file_name.to_string_lossy(),
        std::process::id()
    ));
    let result = fs::write(&temporary_path, bytes).and_then(|()| fs::rename(&temporary_path, path));
    if result.is_err() {
        // Nothing more to do if cleanup fails too; the write error is the one to report.
        let _ = fs::remove_file(&temporary_path);
    }
    result
}

/// Lowercase letters, digits and inner hyphens: safe in a path and a URL.
pub fn is_safe_slug(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// Decoded images kept across renders, so a large photo is decoded once rather than on every
/// edit. A file whose modification time or size changed is decoded again.
// shortcut: entries for deleted or renamed files stay until the cache is dropped, add eviction if
// a long session's memory grows
#[derive(Debug, Default, Clone)]
pub struct ImageCache(Arc<Mutex<HashMap<PathBuf, CachedImage>>>);

#[derive(Debug)]
struct CachedImage {
    modified: SystemTime,
    byte_size: u64,
    image: Arc<Pixmap>,
}

#[derive(Debug)]
pub struct Assets {
    directory: PathBuf,
    image_cache: ImageCache,
}

impl Assets {
    pub fn new(directory: PathBuf) -> Self {
        Self::with_image_cache(directory, ImageCache::default())
    }

    pub fn with_image_cache(directory: PathBuf, image_cache: ImageCache) -> Self {
        Self {
            directory,
            image_cache,
        }
    }

    /// Canonicalizing first means neither `..` nor a symlink can reach outside the directory.
    pub fn resolve(&self, relative_path: &str, path: &str) -> Result<PathBuf, Diagnostic> {
        let not_found = || {
            Diagnostic::new(
                "asset_not_found",
                if relative_path.starts_with("artworks/") {
                    format!(
                        "asset \"{relative_path}\" does not exist; \
                         run `forma generate` to create it"
                    )
                } else {
                    format!("asset \"{relative_path}\" does not exist")
                },
            )
            .at(path)
        };
        let directory = self.directory.canonicalize().map_err(|_| not_found())?;
        let resolved = directory
            .join(relative_path)
            .canonicalize()
            .map_err(|_| not_found())?;
        if !resolved.starts_with(&directory) {
            return Err(Diagnostic::new(
                "asset_outside_project",
                format!("asset \"{relative_path}\" is outside the assets directory"),
            )
            .at(path));
        }
        Ok(resolved)
    }

    pub fn read(&self, relative_path: &str, path: &str) -> Result<(PathBuf, Vec<u8>), Diagnostic> {
        let resolved = self.resolve(relative_path, path)?;
        let unreadable = |error: std::io::Error| {
            Diagnostic::new(
                "asset_unreadable",
                format!("{}: {error}", resolved.display()),
            )
            .at(path)
        };
        let size = fs::metadata(&resolved).map_err(unreadable)?.len();
        if size > MAX_ASSET_BYTES {
            return Err(Diagnostic::new(
                "asset_too_large",
                format!("\"{relative_path}\" is larger than {MAX_ASSET_BYTES} bytes"),
            )
            .at(path));
        }
        let bytes = fs::read(&resolved).map_err(unreadable)?;
        Ok((resolved, bytes))
    }

    /// Font files under `fonts/`, relative to the assets directory, sorted so that resolving a
    /// family does not depend on directory order. Symlinks are skipped.
    pub fn font_files(&self) -> Result<Vec<String>, Diagnostic> {
        let unreadable = |path: &Path, error: std::io::Error| {
            Diagnostic::new("asset_unreadable", format!("{}: {error}", path.display()))
        };
        let mut pending_directories = vec![self.directory.join(FONT_DIRECTORY)];
        let mut files = Vec::new();
        while let Some(directory) = pending_directories.pop() {
            let entries = match fs::read_dir(&directory) {
                Ok(entries) => entries,
                Err(error) if error.kind() == ErrorKind::NotFound => continue,
                Err(error) => return Err(unreadable(&directory, error)),
            };
            for entry in entries {
                let entry = entry.map_err(|error| unreadable(&directory, error))?;
                let path = entry.path();
                let file_type = entry
                    .file_type()
                    .map_err(|error| unreadable(&path, error))?;
                if file_type.is_dir() {
                    pending_directories.push(path);
                } else if file_type.is_file()
                    && path.extension().is_some_and(|extension| {
                        FONT_EXTENSIONS
                            .iter()
                            .any(|known| extension.eq_ignore_ascii_case(known))
                    })
                {
                    let relative = path.strip_prefix(&self.directory);
                    files.push(
                        relative
                            .expect("walked from here")
                            .to_string_lossy()
                            .into_owned(),
                    );
                }
            }
        }
        files.sort();
        Ok(files)
    }

    pub fn load_image(
        &self,
        relative_path: &str,
        path: &str,
    ) -> Result<(PathBuf, Arc<Pixmap>), Diagnostic> {
        let resolved = self.resolve(relative_path, path)?;
        // Read before the file, so a write in between only costs one more decode next time.
        let stamp = fs::metadata(&resolved)
            .and_then(|metadata| Ok((metadata.modified()?, metadata.len())))
            .ok();
        let mut cached_images = self
            .image_cache
            .0
            .lock()
            .expect("no decode panics while locked");
        if let (Some((modified, byte_size)), Some(cached)) = (stamp, cached_images.get(&resolved))
            && cached.modified == modified
            && cached.byte_size == byte_size
        {
            return Ok((resolved, cached.image.clone()));
        }
        drop(cached_images);
        let (resolved, bytes) = self.read(relative_path, path)?;
        let image = Arc::new(
            decode_image(&bytes, &resolved)
                .map_err(|message| Diagnostic::new("invalid_image", message).at(path))?,
        );
        if let Some((modified, byte_size)) = stamp {
            cached_images = self
                .image_cache
                .0
                .lock()
                .expect("no decode panics while locked");
            cached_images.insert(
                resolved.clone(),
                CachedImage {
                    modified,
                    byte_size,
                    image: image.clone(),
                },
            );
        }
        Ok((resolved, image))
    }

    pub fn load_svg(
        &self,
        relative_path: &str,
        current_color: Option<Color>,
        path: &str,
    ) -> Result<(PathBuf, Tree), Diagnostic> {
        let (resolved, bytes) = self.read(relative_path, path)?;
        let tree = parse_svg(&bytes, current_color)
            .map_err(|(code, message)| Diagnostic::new(code, message).at(path))?;
        Ok((resolved, tree))
    }
}

pub fn validate_svg(bytes: &[u8]) -> Result<(), Diagnostic> {
    parse_svg(bytes, None)
        .map(|_| ())
        .map_err(|(code, message)| Diagnostic::new(code, message))
}

/// Scripts are never executed by usvg. External references are refused here; only self-contained
/// `data:` images load. Text is rejected because SVG font selection would bypass frame font rules.
/// `current_color` resolves `currentColor`; the file's bytes stay untouched.
fn parse_svg(bytes: &[u8], current_color: Option<Color>) -> Result<Tree, (&'static str, String)> {
    let invalid = |error: &dyn std::fmt::Display| ("invalid_svg", error.to_string());
    let text = std::str::from_utf8(bytes).map_err(|error| invalid(&error))?;
    let parsing_options = ParsingOptions {
        allow_dtd: true,
        ..ParsingOptions::default()
    };
    let document =
        Document::parse_with_options(text, parsing_options).map_err(|error| invalid(&error))?;
    if document
        .descendants()
        .any(|node| node.has_tag_name((SVG_NAMESPACE, "text")))
    {
        return Err((
            "unsupported_svg_text",
            "SVG <text> is not supported; convert it to paths or use a text node".to_owned(),
        ));
    }
    let options = Options {
        image_href_resolver: ImageHrefResolver {
            resolve_string: Box::new(|_, _| None),
            ..ImageHrefResolver::default()
        },
        style_sheet: current_color.map(|color| format!("svg {{ color: {color} }}")),
        ..Options::default()
    };
    Tree::from_data(bytes, &options).map_err(|error| invalid(&error))
}

/// Premultiplied once here, as the renderer needs, rather than on every draw.
fn decode_image(bytes: &[u8], source: &Path) -> Result<Pixmap, String> {
    let describe = |error: &dyn std::fmt::Display| format!("{}: {error}", source.display());
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| describe(&error))?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_SIDE);
    limits.max_image_height = Some(MAX_IMAGE_SIDE);
    reader.limits(limits);
    let decoded = reader
        .decode()
        .map_err(|error| describe(&error))?
        .into_rgba8();
    let (width, height) = decoded.dimensions();
    let premultiply =
        |channel: u8, alpha: u8| ((u16::from(channel) * u16::from(alpha) + 127) / 255) as u8;
    let mut premultiplied = decoded.into_raw();
    for pixel in premultiplied.as_chunks_mut::<4>().0 {
        let alpha = pixel[3];
        pixel[0] = premultiply(pixel[0], alpha);
        pixel[1] = premultiply(pixel[1], alpha);
        pixel[2] = premultiply(pixel[2], alpha);
    }
    IntSize::from_wh(width, height)
        .and_then(|size| Pixmap::from_vec(premultiplied, size))
        .ok_or_else(|| describe(&"the image has no pixels"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_that_escape_the_assets_directory_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let assets_directory = root.path().join("assets");
        fs::create_dir(&assets_directory).unwrap();
        fs::write(assets_directory.join("inside.png"), "inside").unwrap();
        let secret = root.path().join("secret.png");
        fs::write(&secret, "secret").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&secret, assets_directory.join("link.png")).unwrap();
        let assets = Assets::new(assets_directory);

        assert!(assets.read("inside.png", "src").is_ok());
        let mut escaping_paths = vec!["../secret.png", secret.to_str().unwrap()];
        if cfg!(unix) {
            escaping_paths.push("link.png");
        }
        for escaping_path in escaping_paths {
            let error = assets.read(escaping_path, "src").unwrap_err();
            assert_eq!(error.code, "asset_outside_project", "{escaping_path}");
        }
    }

    #[test]
    fn a_cached_image_is_decoded_again_once_its_file_changes() {
        let assets_directory = tempfile::tempdir().unwrap();
        let write_png = |width| {
            let mut pixmap = Pixmap::new(width, 1).unwrap();
            pixmap.fill(tiny_skia::Color::WHITE);
            pixmap
                .save_png(assets_directory.path().join("photo.png"))
                .unwrap();
        };
        write_png(1);
        let image_cache = ImageCache::default();
        let load = || {
            Assets::with_image_cache(assets_directory.path().into(), image_cache.clone())
                .load_image("photo.png", "src")
                .unwrap()
                .1
        };

        let first = load();
        assert!(Arc::ptr_eq(&first, &load()));

        write_png(2);
        assert_eq!(load().width(), 2);
    }

    #[test]
    fn svg_cannot_load_files_or_bypass_font_rules() {
        let root = tempfile::tempdir().unwrap();
        let secret = root.path().join("secret.png");
        let mut pixmap = tiny_skia::Pixmap::new(1, 1).unwrap();
        pixmap.fill(tiny_skia::Color::WHITE);
        pixmap.save_png(&secret).unwrap();
        let open_tag = r#"<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1">"#;

        let linking = format!(r#"{open_tag}<image href="{}"/></svg>"#, secret.display());
        let tree = parse_svg(linking.as_bytes(), None).unwrap();
        assert!(!tree.root().has_children());

        let drawing = format!(r#"{open_tag}<rect width="1" height="1"/></svg>"#);
        assert!(
            parse_svg(drawing.as_bytes(), None)
                .unwrap()
                .root()
                .has_children()
        );

        let with_text = format!("{open_tag}<text>hi</text></svg>");
        assert_eq!(
            parse_svg(with_text.as_bytes(), None).unwrap_err().0,
            "unsupported_svg_text"
        );
    }
}
