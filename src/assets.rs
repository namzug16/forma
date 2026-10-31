//! Reading project assets, confined to the configured assets directory.

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use image::{ImageReader, Limits};
use usvg::roxmltree::{Document, ParsingOptions};
use usvg::{ImageHrefResolver, Options, Tree};

use crate::diagnostics::Diagnostic;

const MAX_ASSET_BYTES: u64 = 64 * 1024 * 1024;
const MAX_IMAGE_SIDE: u32 = 16384;
const SVG_NAMESPACE: &str = "http://www.w3.org/2000/svg";

/// Decoded pixels in straight (not premultiplied) RGBA.
#[derive(Debug)]
pub struct DecodedImage {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug)]
pub struct Assets {
    directory: PathBuf,
}

impl Assets {
    pub fn new(directory: PathBuf) -> Self {
        Self { directory }
    }

    /// Canonicalizing first means neither `..` nor a symlink can reach outside the directory.
    pub fn resolve(&self, relative_path: &str, path: &str) -> Result<PathBuf, Diagnostic> {
        let not_found = || {
            Diagnostic::new(
                "asset_not_found",
                format!("asset \"{relative_path}\" does not exist"),
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

    pub fn load_image(
        &self,
        relative_path: &str,
        path: &str,
    ) -> Result<(PathBuf, DecodedImage), Diagnostic> {
        let (resolved, bytes) = self.read(relative_path, path)?;
        let image = decode_image(&bytes, &resolved)
            .map_err(|message| Diagnostic::new("invalid_image", message).at(path))?;
        Ok((resolved, image))
    }

    pub fn load_svg(&self, relative_path: &str, path: &str) -> Result<(PathBuf, Tree), Diagnostic> {
        let (resolved, bytes) = self.read(relative_path, path)?;
        let tree =
            parse_svg(&bytes).map_err(|(code, message)| Diagnostic::new(code, message).at(path))?;
        Ok((resolved, tree))
    }
}

/// Scripts are never executed by usvg. External references are refused here; only self-contained
/// `data:` images load. Text is rejected because SVG font selection would bypass frame font rules.
fn parse_svg(bytes: &[u8]) -> Result<Tree, (&'static str, String)> {
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
        ..Options::default()
    };
    Tree::from_data(bytes, &options).map_err(|error| invalid(&error))
}

fn decode_image(bytes: &[u8], source: &Path) -> Result<DecodedImage, String> {
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
    Ok(DecodedImage {
        rgba: decoded.into_raw(),
        width,
        height,
    })
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
    fn svg_cannot_load_files_or_bypass_font_rules() {
        let root = tempfile::tempdir().unwrap();
        let secret = root.path().join("secret.png");
        let mut pixmap = tiny_skia::Pixmap::new(1, 1).unwrap();
        pixmap.fill(tiny_skia::Color::WHITE);
        pixmap.save_png(&secret).unwrap();
        let open_tag = r#"<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1">"#;

        let linking = format!(r#"{open_tag}<image href="{}"/></svg>"#, secret.display());
        let tree = parse_svg(linking.as_bytes()).unwrap();
        assert!(!tree.root().has_children());

        let drawing = format!(r#"{open_tag}<rect width="1" height="1"/></svg>"#);
        assert!(parse_svg(drawing.as_bytes()).unwrap().root().has_children());

        let with_text = format!("{open_tag}<text>hi</text></svg>");
        assert_eq!(
            parse_svg(with_text.as_bytes()).unwrap_err().0,
            "unsupported_svg_text"
        );
    }
}
