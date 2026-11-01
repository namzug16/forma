use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use forma::diagnostics::Failure;
use forma::frames::{self, FrameFile};
use forma::project::Project;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::resizable::{h_resizable, resizable_panel};
use gpui_kit::component::tree::{TreeEvent, TreeState, tree};
use gpui_kit::*;

use crate::frame_tree::{self, Selection};

pub struct Viewer {
    project: Option<Arc<Project>>,
    /// Why the project cannot be shown; agents fix these, the viewer only reports them.
    project_failure: Vec<String>,
    frames: Vec<FrameFile>,
    tree_state: Entity<TreeState>,
    collapsed_folders: HashSet<SharedString>,
    selection: Option<Selection>,
    _subscriptions: [Subscription; 2],
}

impl Viewer {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let tree_state = cx.new(|cx| TreeState::new(cx));
        let subscriptions = [
            cx.observe(&tree_state, |viewer, tree_state, cx| {
                let selected_item = tree_state.read(cx).selected_item();
                let selection = selected_item.and_then(|item| Selection::from_item_id(&item.id));
                if let Some(selection) = selection {
                    viewer.select(selection, cx);
                }
            }),
            cx.subscribe(&tree_state, |viewer, _, event, _| match event {
                TreeEvent::Expanded(id) => {
                    viewer.collapsed_folders.remove(id);
                }
                TreeEvent::Collapsed(id) => {
                    viewer.collapsed_folders.insert(id.clone());
                }
            }),
        ];
        Self {
            project: None,
            project_failure: Vec::new(),
            frames: Vec::new(),
            tree_state,
            collapsed_folders: HashSet::new(),
            selection: None,
            _subscriptions: subscriptions,
        }
    }

    fn select(&mut self, selection: Selection, cx: &mut Context<Self>) {
        if self.selection.as_ref() != Some(&selection) {
            self.selection = Some(selection);
            cx.notify();
        }
    }

    fn choose_directory(&mut self, cx: &mut Context<Self>) {
        let picked_paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open".into()),
        });
        cx.spawn(async move |viewer, cx| {
            let picked_paths = picked_paths.await;
            viewer
                .update(cx, |viewer, cx| match picked_paths {
                    Ok(Ok(Some(paths))) => {
                        if let Some(path) = paths.into_iter().next() {
                            viewer.open_project(&path, cx);
                        }
                    }
                    Ok(Ok(None)) => {}
                    Ok(Err(error)) => {
                        viewer.project_failure = vec![format!("{error:#}")];
                        cx.notify();
                    }
                    Err(error) => {
                        viewer.project_failure = vec![error.to_string()];
                        cx.notify();
                    }
                })
                .ok();
        })
        .detach();
    }

    fn open_project(&mut self, picked_directory: &Path, cx: &mut Context<Self>) {
        match Project::locate(None, picked_directory) {
            Ok(project) => {
                self.project = Some(Arc::new(project));
                self.project_failure.clear();
                self.rebuild_tree(cx);
            }
            Err(error) => self.project_failure = failure_messages(&error),
        }
        cx.notify();
    }

    /// Re-reads which frames exist, keeping the tree's highlight and folder state.
    fn rebuild_tree(&mut self, cx: &mut Context<Self>) {
        let Some(project) = &self.project else {
            return;
        };
        match frames::list(project) {
            Ok(frames) => self.frames = frames,
            Err(error) => {
                self.frames.clear();
                self.project_failure = failure_messages(&error);
            }
        }
        let items = frame_tree::build_items(&self.frames, &self.collapsed_folders);
        self.tree_state.update(cx, |tree_state, cx| {
            let selected_id = tree_state.selected_item().map(|item| item.id.clone());
            tree_state.set_items(items, cx);
            let selected_index = selected_id.and_then(|id| tree_state.index_of(&id));
            tree_state.set_selected_index(selected_index, cx);
        });
    }

    fn render_tree(&self) -> impl IntoElement {
        tree(&self.tree_state, |index, entry, _, _, _| {
            let disclosure = match (entry.is_folder(), entry.is_expanded()) {
                (false, _) => "",
                (true, true) => "▾ ",
                (true, false) => "▸ ",
            };
            ListItem::new(index)
                .pl(px(8. + 14. * entry.depth() as f32))
                .child(format!("{disclosure}{}", entry.item().label))
        })
    }

    fn render_project(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_resizable("panels")
            .child(
                resizable_panel()
                    .size(px(240.))
                    .size_range(px(160.)..px(480.))
                    .child(
                        div()
                            .size_full()
                            .bg(cx.theme().sidebar)
                            .child(self.render_tree()),
                    ),
            )
            .child(resizable_panel())
    }

    fn render_start_screen(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_4()
            .child("Open a project to view its frames")
            .child(
                Button::new("open-directory")
                    .primary()
                    .label("Open directory…")
                    .on_click(cx.listener(|viewer, _, _, cx| viewer.choose_directory(cx))),
            )
            .children(self.project_failure.iter().map(|message| {
                div()
                    .max_w(px(560.))
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .child(message.clone())
            }))
    }
}

/// One line per diagnostic, in the same words the CLI uses.
pub fn failure_messages(error: &anyhow::Error) -> Vec<String> {
    match error.downcast_ref::<Failure>() {
        Some(failure) => failure.0.iter().map(ToString::to_string).collect(),
        None => vec![format!("{error:#}")],
    }
}

impl Render for Viewer {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match &self.project {
            None => self.render_start_screen(cx).into_any_element(),
            Some(_) => self.render_project(cx).into_any_element(),
        };
        div()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(content)
    }
}
