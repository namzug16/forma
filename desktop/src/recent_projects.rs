//! The list behind the welcome window. It lives in the app's own folder, never in a project.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use forma::project::Project;

const MAX_RECENT_PROJECTS: usize = 10;

fn home_directory() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
}

fn store_path() -> Option<PathBuf> {
    Some(home_directory()?.join("Library/Application Support/Forma/recent-projects.json"))
}

fn read_store(path: &Path) -> Vec<PathBuf> {
    let text = fs::read_to_string(path).unwrap_or_default();
    serde_json::from_str(&text).unwrap_or_default()
}

/// Most recent first. Projects that were moved or deleted are left out.
pub fn load() -> Vec<PathBuf> {
    let Some(path) = store_path() else {
        return Vec::new();
    };
    let mut roots = read_store(&path);
    roots.retain(|root| Project::locate(Some(root), root).is_ok());
    roots
}

pub fn remember(root: &Path) -> Result<()> {
    let path = store_path().context("HOME is not set")?;
    let roots = with_most_recent(read_store(&path), root);
    let parent = path.parent().expect("the store path has a parent");
    fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
    fs::write(&path, serde_json::to_string_pretty(&roots)?)
        .with_context(|| format!("cannot write {}", path.display()))
}

fn with_most_recent(mut roots: Vec<PathBuf>, root: &Path) -> Vec<PathBuf> {
    roots.retain(|existing| existing != root);
    roots.insert(0, root.to_owned());
    roots.truncate(MAX_RECENT_PROJECTS);
    roots
}

/// The folder a project sits in, as `~/projects`.
pub fn location_label(root: &Path) -> String {
    let parent = root.parent().unwrap_or(root);
    match home_directory().and_then(|home| parent.strip_prefix(home).ok().map(Path::to_owned)) {
        Some(relative_path) if relative_path.as_os_str().is_empty() => "~".to_owned(),
        Some(relative_path) => format!("~/{}", relative_path.display()),
        None => parent.display().to_string(),
    }
}

pub fn project_name(root: &Path) -> String {
    root.file_name()
        .unwrap_or(root.as_os_str())
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reopening_a_project_moves_it_to_the_top_without_a_duplicate() {
        let roots = vec![PathBuf::from("/a"), PathBuf::from("/b")];

        assert_eq!(
            with_most_recent(roots, Path::new("/b")),
            [PathBuf::from("/b"), PathBuf::from("/a")]
        );
    }

    #[test]
    fn the_oldest_project_falls_off_the_list() {
        let roots = (0..MAX_RECENT_PROJECTS)
            .map(|index| PathBuf::from(format!("/{index}")))
            .collect();

        let roots = with_most_recent(roots, Path::new("/new"));

        assert_eq!(roots.len(), MAX_RECENT_PROJECTS);
        assert_eq!(roots[0], PathBuf::from("/new"));
        assert!(!roots.contains(&PathBuf::from(format!("/{}", MAX_RECENT_PROJECTS - 1))));
    }

    #[test]
    fn a_missing_or_damaged_store_is_an_empty_list() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("recent-projects.json");

        assert!(read_store(&path).is_empty());
        fs::write(&path, "not json").unwrap();
        assert!(read_store(&path).is_empty());
    }

    #[test]
    fn locations_outside_home_stay_absolute() {
        assert_eq!(
            location_label(Path::new("/Volumes/work/site")),
            "/Volumes/work"
        );
    }

    #[test]
    fn projects_are_named_after_their_folder() {
        assert_eq!(project_name(Path::new("/work/site")), "site");
    }
}
