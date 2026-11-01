//! Turns the project's frame files into the side tree.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use forma::frames::FrameFile;
use gpui_kit::SharedString;
use gpui_kit::component::tree::TreeItem;

const FOLDER_PREFIX: &str = "folder:";
const FRAME_PREFIX: &str = "frame:";
const TOKENS_ID: &str = "tokens";
const CONFIG_ID: &str = "config";

/// What the main area is showing. Folders only expand, so they are never selected.
#[derive(Clone, Debug, PartialEq)]
pub enum Selection {
    /// Path relative to the frames directory.
    Frame(PathBuf),
    Tokens,
    Config,
}

impl Selection {
    pub fn from_item_id(item_id: &str) -> Option<Self> {
        match item_id {
            TOKENS_ID => Some(Self::Tokens),
            CONFIG_ID => Some(Self::Config),
            _ => item_id
                .strip_prefix(FRAME_PREFIX)
                .map(|relative_path| Self::Frame(relative_path.into())),
        }
    }
}

#[derive(Default)]
struct Folder {
    folders: BTreeMap<String, Folder>,
    frames: Vec<PathBuf>,
}

impl Folder {
    fn insert(&mut self, relative_path: &Path) {
        let mut folder = self;
        for component in relative_path.parent().into_iter().flatten() {
            let name = component.to_string_lossy().into_owned();
            folder = folder.folders.entry(name).or_default();
        }
        folder.frames.push(relative_path.to_owned());
    }

    fn into_items(
        self,
        parent_id: &str,
        collapsed_folders: &HashSet<SharedString>,
    ) -> Vec<TreeItem> {
        let folders = self.folders.into_iter().map(|(name, folder)| {
            let id = format!("{parent_id}/{name}");
            let is_expanded = !collapsed_folders.contains(id.as_str());
            let children = folder.into_items(&id, collapsed_folders);
            TreeItem::new(id, name)
                .children(children)
                .expanded(is_expanded)
        });
        let frames = self.frames.into_iter().map(|relative_path| {
            let label = relative_path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy();
            TreeItem::new(
                format!("{FRAME_PREFIX}{}", relative_path.display()),
                label.into_owned(),
            )
        });
        folders.chain(frames).collect()
    }
}

/// New folders start expanded, so a frame an agent just created is visible.
pub fn build_items(
    frames: &[FrameFile],
    collapsed_folders: &HashSet<SharedString>,
) -> Vec<TreeItem> {
    let mut root = Folder::default();
    for relative_path in frames
        .iter()
        .filter_map(|file| file.relative_path.as_deref())
    {
        root.insert(relative_path);
    }
    let frames_id = FOLDER_PREFIX.to_owned() + "frames";
    let is_expanded = !collapsed_folders.contains(frames_id.as_str());
    let children = root.into_items(&frames_id, collapsed_folders);
    vec![
        TreeItem::new(frames_id, "Frames")
            .children(children)
            .expanded(is_expanded),
        TreeItem::new(TOKENS_ID, "Tokens"),
        TreeItem::new(CONFIG_ID, "Config"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(relative_path: &str) -> FrameFile {
        FrameFile {
            path: PathBuf::from("/project/forma/frames").join(relative_path),
            relative_path: Some(relative_path.into()),
        }
    }

    #[test]
    fn frames_nest_under_their_folders_after_subfolders() {
        let items = build_items(
            &[frame("hero.json"), frame("social/post.json")],
            &HashSet::new(),
        );

        let labels: Vec<_> = items[0]
            .children
            .iter()
            .map(|item| item.label.as_ref())
            .collect();
        assert_eq!(labels, ["social", "hero"]);
        assert_eq!(items[0].children[0].children[0].label.as_ref(), "post");
    }

    #[test]
    fn frame_items_select_their_relative_path() {
        let items = build_items(&[frame("social/post.json")], &HashSet::new());

        let post = &items[0].children[0].children[0];
        assert_eq!(
            Selection::from_item_id(&post.id),
            Some(Selection::Frame("social/post.json".into()))
        );
    }

    #[test]
    fn folders_select_nothing() {
        let items = build_items(&[frame("social/post.json")], &HashSet::new());

        assert_eq!(Selection::from_item_id(&items[0].children[0].id), None);
    }

    #[test]
    fn collapsed_folders_stay_collapsed_when_the_tree_is_rebuilt() {
        let first = build_items(&[frame("social/post.json")], &HashSet::new());
        let collapsed = HashSet::from([first[0].children[0].id.clone()]);

        let rebuilt = build_items(&[frame("social/post.json")], &collapsed);

        assert!(!rebuilt[0].children[0].is_expanded());
        assert!(rebuilt[0].is_expanded());
    }

    #[test]
    fn an_empty_project_still_offers_tokens_and_config() {
        let items = build_items(&[], &HashSet::new());

        assert_eq!(
            Selection::from_item_id(&items[1].id),
            Some(Selection::Tokens)
        );
        assert_eq!(
            Selection::from_item_id(&items[2].id),
            Some(Selection::Config)
        );
    }
}
