//! Finding frame documents on disk and checking them.

use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};

use crate::diagnostics::{Diagnostic, Failure};
use crate::document;
use crate::project::Project;
use crate::scene::{self, Resources, Scene};

const MAX_DOCUMENT_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct FrameFile {
    pub path: PathBuf,
    /// Path relative to the frames directory; `None` for files outside it.
    pub relative_path: Option<PathBuf>,
}

impl FrameFile {
    pub fn display_name(&self) -> String {
        self.relative_path
            .as_ref()
            .unwrap_or(&self.path)
            .display()
            .to_string()
    }
}

/// Every `.json` document under the frames directory, sorted by relative path.
pub fn list(project: &Project) -> Result<Vec<FrameFile>> {
    let frames_directory = project.frames_directory();
    let mut paths = Vec::new();
    collect_files(&frames_directory, "json", &mut paths)?;
    let mut frames = paths
        .into_iter()
        .map(|path| FrameFile {
            relative_path: path
                .strip_prefix(&frames_directory)
                .ok()
                .map(Path::to_owned),
            path,
        })
        .collect::<Vec<_>>();
    frames.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(frames)
}

pub(crate) fn collect_files(
    directory: &Path,
    wanted_extension: &str,
    paths: &mut Vec<PathBuf>,
) -> Result<()> {
    let entries = fs::read_dir(directory)
        .with_context(|| format!("cannot read directory {}", directory.display()))?;
    for entry in entries {
        let path = entry?.path();
        if path.is_dir() {
            collect_files(&path, wanted_extension, paths)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == wanted_extension)
        {
            paths.push(path);
        }
    }
    Ok(())
}

/// Bare references live under the frames directory; `./`, `../` and absolute paths do not.
pub fn resolve_reference(project: &Project, reference: &str) -> Result<FrameFile> {
    let reference_path = Path::new(reference);
    let frames_directory = project.frames_directory();
    let is_explicit_path = matches!(
        reference_path.components().next(),
        Some(Component::CurDir | Component::ParentDir | Component::RootDir | Component::Prefix(_))
    );
    if is_explicit_path {
        let path = reference_path
            .canonicalize()
            .map_err(|_| not_found(reference))?;
        return Ok(FrameFile {
            relative_path: path
                .strip_prefix(&frames_directory)
                .ok()
                .map(Path::to_owned),
            path,
        });
    }
    if reference_path
        .components()
        .any(|component| component == Component::ParentDir)
    {
        return Err(Failure::from(Diagnostic::new(
            "invalid_frame_reference",
            format!("\"{reference}\" leaves the frames directory; use an explicit ./ path"),
        ))
        .into());
    }

    let mut candidates = vec![PathBuf::from(reference)];
    if reference_path
        .extension()
        .is_none_or(|extension| extension != "json")
    {
        candidates.push(PathBuf::from(format!("{reference}.json")));
    }
    candidates.retain(|candidate| frames_directory.join(candidate).is_file());
    match candidates.as_slice() {
        [] => Err(not_found(reference)),
        [relative_path] => Ok(FrameFile {
            path: frames_directory.join(relative_path),
            relative_path: Some(relative_path.clone()),
        }),
        ambiguous => Err(Failure::from(Diagnostic::new(
            "ambiguous_frame_reference",
            format!(
                "\"{reference}\" matches several files: {}",
                ambiguous
                    .iter()
                    .map(|candidate| candidate.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ))
        .into()),
    }
}

fn not_found(reference: &str) -> anyhow::Error {
    Failure::from(Diagnostic::new(
        "frame_not_found",
        format!("no frame matches \"{reference}\"; run `forma list` to see available frames"),
    ))
    .into()
}

/// Reads, parses and fully resolves a frame without touching any source file.
pub fn load_scene(file: &FrameFile, resources: &Resources) -> Result<Scene, Failure> {
    let read_failure =
        |message: String| Failure::from(Diagnostic::new("frame_unreadable", message));
    let size = fs::metadata(&file.path)
        .map_err(|error| read_failure(format!("{}: {error}", file.path.display())))?
        .len();
    if size > MAX_DOCUMENT_BYTES {
        return Err(read_failure(format!(
            "{} is larger than {MAX_DOCUMENT_BYTES} bytes",
            file.path.display()
        )));
    }
    let text = fs::read_to_string(&file.path)
        .map_err(|error| read_failure(format!("{}: {error}", file.path.display())))?;
    let frame = document::parse_frame(&text)?;
    scene::build(&frame, resources)
}
