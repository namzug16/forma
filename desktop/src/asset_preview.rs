//! Finds the project's assets and draws them for the canvas. Everything is read through forma's
//! `Assets`, so a viewer can't reach further than a frame can.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use forma::assets::Assets;
use forma::diagnostics::Failure;
use forma::project::Project;
use forma::scene::Resources;
use forma::text::FontLibrary;
use forma::tokens::Tokens;
use serde_json::{Value, json};

use crate::frame_render::{RenderedFrame, bgra_render_image};
use crate::token_sheet::render_preview;

const FONT_SPECIMEN: &str = "Aa Bb Cc Dd Ee\nDesign as code\n0123456789";
const FONT_SPECIMEN_SIZE: f64 = 96.;
const FONT_SPECIMEN_WIDTH: f64 = 1000.;
const FONT_SPECIMEN_HEIGHT: f64 = 420.;

#[derive(Debug, PartialEq)]
pub enum AssetKind {
    Image,
    Svg,
    Font,
    Other,
}

pub fn kind_of(path: &Path) -> AssetKind {
    let extension = path.extension().unwrap_or_default().to_string_lossy();
    match extension.to_lowercase().as_str() {
        "png" | "jpg" | "jpeg" | "webp" => AssetKind::Image,
        "svg" => AssetKind::Svg,
        "ttf" | "otf" | "ttc" | "otc" => AssetKind::Font,
        _ => AssetKind::Other,
    }
}

/// Files under the assets directory relative to it, sorted. Hidden files such as `.gitkeep` are
/// left out. Symlinks aren't followed, so one pointing at a parent can't loop.
pub fn list(project: &Project) -> Result<Vec<PathBuf>> {
    let assets_directory = project.assets_directory();
    let mut files = Vec::new();
    let mut pending_directories = vec![assets_directory.clone()];
    while let Some(directory) = pending_directories.pop() {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("cannot read assets in {}", directory.display()));
            }
        };
        for entry in entries {
            let entry = entry?;
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                pending_directories.push(path);
            } else if let Ok(relative_path) = path.strip_prefix(&assets_directory) {
                files.push(relative_path.to_owned());
            }
        }
    }
    files.sort();
    Ok(files)
}

pub enum AssetPreview {
    /// Shown on the canvas like a frame, so it can be zoomed and panned.
    Drawn(RenderedFrame),
    NotPreviewable {
        file_name: String,
        byte_size: u64,
    },
}

pub fn load(project: &Project, relative_path: &Path) -> Result<AssetPreview> {
    let assets = Assets::new(project.assets_directory());
    let source = relative_path.to_string_lossy();
    let file_name = relative_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    match kind_of(relative_path) {
        AssetKind::Image => {
            let (_, image) = assets.load_image(&source, &source).map_err(Failure::from)?;
            let mut bgra = image.rgba;
            for pixel in bgra.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
            Ok(AssetPreview::Drawn(RenderedFrame {
                image: bgra_render_image(image.width, image.height, bgra),
                width: image.width as f32,
                height: image.height as f32,
                report: named_report(&file_name),
            }))
        }
        AssetKind::Svg => {
            let (_, tree) = assets
                .load_svg(&source, None, &source)
                .map_err(Failure::from)?;
            let size = tree.size();
            let frame = json!({
                "version": 1,
                "id": "preview",
                "width": size.width().ceil(),
                "height": size.height().ceil(),
                "children": [{
                    "id": "asset",
                    "type": "svg",
                    "src": source,
                    "width": size.width().ceil(),
                    "height": size.height().ceil(),
                }],
            });
            draw(&frame, project, &file_name)
        }
        AssetKind::Font => {
            let frame = json!({
                "version": 1,
                "id": "preview",
                "width": FONT_SPECIMEN_WIDTH,
                "height": FONT_SPECIMEN_HEIGHT,
                "children": [{
                    "id": "specimen",
                    "type": "text",
                    "text": FONT_SPECIMEN,
                    "width": FONT_SPECIMEN_WIDTH,
                    "font": { "file": source, "size": FONT_SPECIMEN_SIZE },
                    "fill": { "color": "#FFFFFF" },
                }],
            });
            draw(&frame, project, &file_name)
        }
        AssetKind::Other => {
            let resolved = assets.resolve(&source, &source).map_err(Failure::from)?;
            let byte_size = fs::metadata(&resolved)
                .with_context(|| format!("cannot read {}", resolved.display()))?
                .len();
            Ok(AssetPreview::NotPreviewable {
                file_name,
                byte_size,
            })
        }
    }
}

/// Assets don't use tokens, so a broken `tokens.json` shouldn't hide them.
fn draw(frame: &Value, project: &Project, file_name: &str) -> Result<AssetPreview> {
    let resources = Resources {
        tokens: Tokens::parse("{}")?,
        assets: Assets::new(project.assets_directory()),
        fonts: FontLibrary::default(),
    };
    let preview = render_preview(frame, &resources).map_err(anyhow::Error::msg)?;
    Ok(AssetPreview::Drawn(RenderedFrame {
        image: preview.image,
        width: preview.width,
        height: preview.height,
        report: named_report(file_name),
    }))
}

/// The canvas labels what it shows from the report, as it does for a frame.
fn named_report(file_name: &str) -> Value {
    json!({ "frame": file_name })
}

pub fn human_size(byte_size: u64) -> String {
    const KILOBYTE: f64 = 1024.;
    let bytes = byte_size as f64;
    if bytes < KILOBYTE {
        format!("{byte_size} B")
    } else if bytes < KILOBYTE * KILOBYTE {
        format!("{:.1} KB", bytes / KILOBYTE)
    } else {
        format!("{:.1} MB", bytes / (KILOBYTE * KILOBYTE))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> (tempfile::TempDir, Project) {
        let root = tempfile::tempdir().unwrap();
        forma::project::initialize(root.path()).unwrap();
        let project = Project::locate(Some(root.path()), root.path()).unwrap();
        (root, project)
    }

    #[test]
    fn extensions_decide_the_kind_in_any_case() {
        assert_eq!(kind_of(Path::new("images/a.PNG")), AssetKind::Image);
        assert_eq!(kind_of(Path::new("logo.svg")), AssetKind::Svg);
        assert_eq!(kind_of(Path::new("fonts/a.ttf")), AssetKind::Font);
        assert_eq!(kind_of(Path::new("notes.txt")), AssetKind::Other);
    }

    #[test]
    fn listing_skips_hidden_files_and_nests_folders() {
        let (_root, project) = project();
        let assets_directory = project.assets_directory();
        fs::write(assets_directory.join("images/a.png"), "").unwrap();
        fs::write(assets_directory.join("notes.txt"), "").unwrap();

        assert_eq!(
            list(&project).unwrap(),
            [PathBuf::from("images/a.png"), PathBuf::from("notes.txt")]
        );
    }

    #[test]
    fn a_project_without_assets_lists_nothing() {
        let (_root, project) = project();
        fs::remove_dir_all(project.assets_directory()).unwrap();

        assert!(list(&project).unwrap().is_empty());
    }

    #[test]
    fn svgs_keep_their_natural_size() {
        let (_root, project) = project();
        fs::write(
            project.assets_directory().join("svg/box.svg"),
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="30">
                <rect width="40" height="30" fill="red"/></svg>"#,
        )
        .unwrap();

        let AssetPreview::Drawn(preview) = load(&project, Path::new("svg/box.svg")).unwrap() else {
            panic!("the svg should be drawn");
        };

        assert_eq!((preview.width, preview.height), (40., 30.));
        assert_eq!(preview.report["frame"], "box.svg");
    }

    #[test]
    fn images_that_cannot_be_decoded_report_it() {
        let (_root, project) = project();
        fs::write(project.assets_directory().join("images/bad.png"), "nope").unwrap();

        assert!(load(&project, Path::new("images/bad.png")).is_err());
    }

    #[test]
    fn other_files_report_their_size() {
        let (_root, project) = project();
        fs::write(project.assets_directory().join("notes.txt"), "hello").unwrap();

        let AssetPreview::NotPreviewable { byte_size, .. } =
            load(&project, Path::new("notes.txt")).unwrap()
        else {
            panic!("a text file has no preview");
        };

        assert_eq!(byte_size, 5);
    }

    #[test]
    fn a_broken_tokens_file_does_not_hide_assets() {
        let (_root, project) = project();
        fs::write(project.tokens_path(), "{ broken").unwrap();
        fs::write(
            project.assets_directory().join("svg/box.svg"),
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="3"/>"#,
        )
        .unwrap();

        assert!(load(&project, Path::new("svg/box.svg")).is_ok());
    }

    #[test]
    fn sizes_read_naturally() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2.0 KB");
        assert_eq!(human_size(3 * 1024 * 1024), "3.0 MB");
    }
}
