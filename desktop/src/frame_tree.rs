//! Turns the project's frame files into the side tree.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use forma::frames::FrameFile;
use gpui_kit::SharedString;
use gpui_kit::component::tree::TreeItem;

const FOLDER_PREFIX: &str = "folder:";
const FRAME_PREFIX: &str = "frame:";
const ASSET_PREFIX: &str = "asset:";
const TOKENS_ID: &str = "tokens";
const CONFIG_ID: &str = "config";
const SEPARATOR_ID: &str = "separator";

/// What the main area is showing. Folders only expand, so they are never selected.
#[derive(Clone, Debug, PartialEq)]
pub enum Selection {
    /// Path relative to the frames directory.
    Frame(PathBuf),
    /// Path relative to the assets directory.
    Asset(PathBuf),
    Tokens,
    Config,
}

impl Selection {
    pub fn from_item_id(item_id: &str) -> Option<Self> {
        match item_id {
            TOKENS_ID => Some(Self::Tokens),
            CONFIG_ID => Some(Self::Config),
            _ => {
                if let Some(relative_path) = item_id.strip_prefix(FRAME_PREFIX) {
                    Some(Self::Frame(relative_path.into()))
                } else {
                    item_id
                        .strip_prefix(ASSET_PREFIX)
                        .map(|relative_path| Self::Asset(relative_path.into()))
                }
            }
        }
    }
}

#[derive(Default)]
struct Folder {
    folders: BTreeMap<String, Folder>,
    files: Vec<PathBuf>,
}

impl Folder {
    fn insert(&mut self, relative_path: &Path) {
        let mut folder = self;
        for component in relative_path.parent().into_iter().flatten() {
            let name = component.to_string_lossy().into_owned();
            folder = folder.folders.entry(name).or_default();
        }
        folder.files.push(relative_path.to_owned());
    }

    fn into_items(
        self,
        parent_id: &str,
        collapsed_folders: &HashSet<SharedString>,
        file_item: &impl Fn(&Path) -> TreeItem,
    ) -> Vec<TreeItem> {
        let folders = self.folders.into_iter().map(|(name, folder)| {
            let id = format!("{parent_id}/{name}");
            let is_expanded = !collapsed_folders.contains(id.as_str());
            let children = folder.into_items(&id, collapsed_folders, file_item);
            TreeItem::new(id, name)
                .children(children)
                .expanded(is_expanded)
        });
        let files = self.files.iter().map(|path| file_item(path));
        folders.chain(files).collect()
    }
}

fn file_tree<'a>(
    root_id: &str,
    label: &str,
    relative_paths: impl Iterator<Item = &'a Path>,
    collapsed_folders: &HashSet<SharedString>,
    file_item: impl Fn(&Path) -> TreeItem,
) -> TreeItem {
    let mut root = Folder::default();
    for relative_path in relative_paths {
        root.insert(relative_path);
    }
    let is_expanded = !collapsed_folders.contains(root_id);
    let children = root.into_items(root_id, collapsed_folders, &file_item);
    TreeItem::new(root_id.to_owned(), label.to_owned())
        .children(children)
        .expanded(is_expanded)
}

fn assets_root_id() -> String {
    format!("{FOLDER_PREFIX}assets")
}

/// Assets start closed, because frames are what people open a project for.
pub fn initially_collapsed_folders() -> HashSet<SharedString> {
    HashSet::from([assets_root_id().into()])
}

/// New folders otherwise start expanded, so a frame an agent just created is visible.
pub fn build_items(
    frames: &[FrameFile],
    assets: &[PathBuf],
    collapsed_folders: &HashSet<SharedString>,
) -> Vec<TreeItem> {
    let frame_paths = frames
        .iter()
        .filter_map(|file| file.relative_path.as_deref());
    vec![
        TreeItem::new(TOKENS_ID, "Tokens"),
        TreeItem::new(CONFIG_ID, "Config"),
        TreeItem::new(SEPARATOR_ID, "").disabled(true),
        file_tree(
            &format!("{FOLDER_PREFIX}frames"),
            "Frames",
            frame_paths,
            collapsed_folders,
            |relative_path| {
                let label = relative_path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy();
                TreeItem::new(
                    format!("{FRAME_PREFIX}{}", relative_path.display()),
                    label.into_owned(),
                )
            },
        ),
        file_tree(
            &assets_root_id(),
            "Assets",
            assets.iter().map(PathBuf::as_path),
            collapsed_folders,
            |relative_path| {
                let label = relative_path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy();
                TreeItem::new(
                    format!("{ASSET_PREFIX}{}", relative_path.display()),
                    label.into_owned(),
                )
            },
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAMES_ROOT: usize = 3;
    const ASSETS_ROOT: usize = 4;

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
            &[],
            &HashSet::new(),
        );

        let labels: Vec<_> = items[FRAMES_ROOT]
            .children
            .iter()
            .map(|item| item.label.as_ref())
            .collect();
        assert_eq!(labels, ["social", "hero"]);
        assert_eq!(
            items[FRAMES_ROOT].children[0].children[0].label.as_ref(),
            "post"
        );
    }

    #[test]
    fn frame_items_select_their_relative_path() {
        let items = build_items(&[frame("social/post.json")], &[], &HashSet::new());

        let post = &items[FRAMES_ROOT].children[0].children[0];
        assert_eq!(
            Selection::from_item_id(&post.id),
            Some(Selection::Frame("social/post.json".into()))
        );
    }

    #[test]
    fn folders_select_nothing() {
        let items = build_items(&[frame("social/post.json")], &[], &HashSet::new());

        assert_eq!(
            Selection::from_item_id(&items[FRAMES_ROOT].children[0].id),
            None
        );
    }

    #[test]
    fn collapsed_folders_stay_collapsed_when_the_tree_is_rebuilt() {
        let first = build_items(&[frame("social/post.json")], &[], &HashSet::new());
        let collapsed = HashSet::from([first[FRAMES_ROOT].children[0].id.clone()]);

        let rebuilt = build_items(&[frame("social/post.json")], &[], &collapsed);

        assert!(!rebuilt[FRAMES_ROOT].children[0].is_expanded());
        assert!(rebuilt[FRAMES_ROOT].is_expanded());
    }

    #[test]
    fn assets_start_closed_and_select_their_relative_path() {
        let assets = [PathBuf::from("images/logo.png")];

        let items = build_items(&[], &assets, &initially_collapsed_folders());

        let assets_root = &items[ASSETS_ROOT];
        assert!(!assets_root.is_expanded());
        assert_eq!(assets_root.children[0].label.as_ref(), "images");
        let logo = &assets_root.children[0].children[0];
        assert_eq!(logo.label.as_ref(), "logo.png");
        assert_eq!(
            Selection::from_item_id(&logo.id),
            Some(Selection::Asset("images/logo.png".into()))
        );
    }

    #[test]
    fn tokens_and_config_come_first_and_are_separated_from_the_frames() {
        let items = build_items(&[], &[], &HashSet::new());

        assert_eq!(
            Selection::from_item_id(&items[0].id),
            Some(Selection::Tokens)
        );
        assert_eq!(
            Selection::from_item_id(&items[1].id),
            Some(Selection::Config)
        );
        assert!(items[2].is_disabled());
        assert_eq!(Selection::from_item_id(&items[2].id), None);
        assert_eq!(items[FRAMES_ROOT].label.as_ref(), "Frames");
    }
}
