//! Watches a project on disk and decides what each change means for the viewer.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use forma::project::Project;
use futures::channel::mpsc::{UnboundedReceiver, unbounded};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

/// Real paths, because that is what the file system reports events against.
#[derive(Debug)]
pub struct WatchedPaths {
    project_directory: PathBuf,
    frames: PathBuf,
    assets: PathBuf,
    tokens: PathBuf,
    config: PathBuf,
}

/// What a batch of changes requires. Rendering other frames is never one of them.
#[derive(Debug, Default, PartialEq)]
pub struct ChangeEffects {
    pub reload_project: bool,
    pub refresh_tree: bool,
    pub reload_selection: bool,
}

impl WatchedPaths {
    pub fn of(project: &Project) -> Self {
        Self {
            project_directory: real_path(&project.directory),
            frames: real_path(&project.frames_directory()),
            assets: real_path(&project.assets_directory()),
            tokens: real_path(&project.tokens_path()),
            config: real_path(&project.config_path()),
        }
    }

    /// Directories rather than files: saving by rename would end a watch on the file itself.
    fn roots(&self) -> Vec<PathBuf> {
        let tokens_directory = self.tokens.parent().map(Path::to_owned);
        let mut roots = vec![self.project_directory.clone()];
        // Configs may point outside `forma/`; everything else is already covered by the first root.
        for path in [
            Some(&self.frames),
            Some(&self.assets),
            tokens_directory.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            if !path.starts_with(&self.project_directory) && path.is_dir() {
                roots.push(path.clone());
            }
        }
        roots
    }

    /// `selected_frame` is relative to the frames directory.
    pub fn effects_of(
        &self,
        changed_paths: &[PathBuf],
        selected_frame: Option<&Path>,
    ) -> ChangeEffects {
        let selected_frame = selected_frame.map(|relative_path| self.frames.join(relative_path));
        let mut effects = ChangeEffects::default();
        for path in changed_paths {
            effects.reload_project |= touches(path, &self.config);
            effects.refresh_tree |= touches(path, &self.frames) || touches(path, &self.assets);
            effects.reload_selection |= touches(path, &self.tokens)
                || touches(path, &self.assets)
                || selected_frame
                    .as_ref()
                    .is_some_and(|frame| touches(path, frame));
        }
        effects
    }
}

/// True for the path, anything inside it, or a parent: event batches can name just a directory.
fn touches(changed_path: &Path, watched_path: &Path) -> bool {
    changed_path.starts_with(watched_path) || watched_path.starts_with(changed_path)
}

fn real_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_owned())
}

/// Dropping the returned watcher stops the events.
pub fn watch(
    watched_paths: &WatchedPaths,
) -> Result<(RecommendedWatcher, UnboundedReceiver<notify::Result<Event>>)> {
    let (sender, receiver) = unbounded();
    let mut watcher = notify::recommended_watcher(move |event| {
        // The receiver only goes away with the viewer, which owns this watcher too.
        let _ = sender.unbounded_send(event);
    })
    .context("cannot start watching the project")?;
    for root in watched_paths.roots() {
        watcher
            .watch(&root, RecursiveMode::Recursive)
            .with_context(|| format!("cannot watch {}", root.display()))?;
    }
    Ok((watcher, receiver))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{Duration, Instant};

    use super::*;

    fn watched_paths() -> WatchedPaths {
        WatchedPaths {
            project_directory: "/p/forma".into(),
            frames: "/p/forma/frames".into(),
            assets: "/p/forma/assets".into(),
            tokens: "/p/forma/tokens.json".into(),
            config: "/p/forma/config.json".into(),
        }
    }

    fn effects(changed_path: &str, selected_frame: Option<&str>) -> ChangeEffects {
        watched_paths().effects_of(&[changed_path.into()], selected_frame.map(Path::new))
    }

    #[test]
    fn the_selected_frame_reloads_and_refreshes_the_tree() {
        assert_eq!(
            effects("/p/forma/frames/social/post.json", Some("social/post.json")),
            ChangeEffects {
                refresh_tree: true,
                reload_selection: true,
                ..Default::default()
            }
        );
    }

    #[test]
    fn another_frame_only_refreshes_the_tree() {
        assert_eq!(
            effects("/p/forma/frames/other.json", Some("hero.json")),
            ChangeEffects {
                refresh_tree: true,
                ..Default::default()
            }
        );
    }

    #[test]
    fn tokens_reload_the_selection() {
        assert_eq!(
            effects("/p/forma/tokens.json", None),
            ChangeEffects {
                reload_selection: true,
                ..Default::default()
            }
        );
    }

    #[test]
    fn assets_reload_the_selection_and_refresh_the_tree() {
        assert_eq!(
            effects("/p/forma/assets/images/logo.png", None),
            ChangeEffects {
                refresh_tree: true,
                reload_selection: true,
                ..Default::default()
            }
        );
    }

    #[test]
    fn the_config_reloads_the_project() {
        assert_eq!(
            effects("/p/forma/config.json", None),
            ChangeEffects {
                reload_project: true,
                ..Default::default()
            }
        );
    }

    #[test]
    fn a_rendered_output_changes_nothing() {
        assert_eq!(
            effects("/p/forma/output/hero.png", Some("hero.json")),
            ChangeEffects::default()
        );
    }

    #[test]
    fn a_batch_naming_only_the_frames_directory_still_counts() {
        assert_eq!(
            effects("/p/forma/frames", Some("hero.json")),
            ChangeEffects {
                refresh_tree: true,
                reload_selection: true,
                ..Default::default()
            }
        );
    }

    #[test]
    fn a_frame_folder_outside_the_project_is_watched_separately() {
        let directory = tempfile::tempdir().unwrap();
        let frames = directory.path().join("shared-frames");
        fs::create_dir(&frames).unwrap();
        let watched_paths = WatchedPaths {
            frames: frames.clone(),
            ..watched_paths()
        };

        assert_eq!(watched_paths.roots(), [PathBuf::from("/p/forma"), frames]);
    }

    fn wait_for_event(
        receiver: &mut UnboundedReceiver<notify::Result<Event>>,
        expected_path: &Path,
    ) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            match receiver.try_recv() {
                Ok(event) => {
                    if event
                        .unwrap()
                        .paths
                        .iter()
                        .any(|path| path == expected_path)
                    {
                        return;
                    }
                }
                Err(_) => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        panic!("no event for {}", expected_path.display());
    }

    #[test]
    fn real_writes_replacements_and_deletions_are_reported() {
        let root = tempfile::tempdir().unwrap();
        forma::project::initialize(root.path()).unwrap();
        let project = Project::locate(Some(root.path()), root.path()).unwrap();
        let watched_paths = WatchedPaths::of(&project);
        let (_watcher, mut receiver) = watch(&watched_paths).unwrap();
        // shortcut: FSEvents can replay just-created files, so allow it to settle first.
        std::thread::sleep(Duration::from_millis(300));
        let frame = watched_paths.frames.join("new.json");

        fs::write(&frame, "{}").unwrap();
        wait_for_event(&mut receiver, &frame);

        let replacement = watched_paths.frames.join("new.json.tmp");
        fs::write(&replacement, "{ }").unwrap();
        fs::rename(&replacement, &frame).unwrap();
        wait_for_event(&mut receiver, &frame);

        fs::remove_file(&frame).unwrap();
        wait_for_event(&mut receiver, &frame);
    }
}
