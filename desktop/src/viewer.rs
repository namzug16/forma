use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context as _, Result};

use forma::diagnostics::Failure;
use forma::frames::{self, FrameFile};
use forma::project::Project;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::resizable::{h_resizable, resizable_panel};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::tree::{TreeEvent, TreeState, tree};
use gpui_kit::*;
use serde_json::Value;

use crate::frame_render::{self, RenderedFrame};
use crate::frame_tree::{self, Selection};

/// What the main area shows for the current selection.
enum Content {
    Nothing,
    Loading,
    Frame(RenderedFrame),
    Text(String),
    /// Why the selection cannot be shown.
    Failed(Vec<String>),
}

pub struct Viewer {
    project: Option<Arc<Project>>,
    /// Why the project cannot be shown; agents fix these, the viewer only reports them.
    project_failure: Vec<String>,
    frames: Vec<FrameFile>,
    tree_state: Entity<TreeState>,
    collapsed_folders: HashSet<SharedString>,
    selection: Option<Selection>,
    content: Content,
    /// Dropping it cancels a load that a newer selection or change has replaced.
    load_task: Option<Task<()>>,
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
            content: Content::Nothing,
            load_task: None,
            _subscriptions: subscriptions,
        }
    }

    fn select(&mut self, selection: Selection, cx: &mut Context<Self>) {
        if self.selection.as_ref() != Some(&selection) {
            self.selection = Some(selection);
            self.show(Content::Loading, cx);
            self.load_selection(cx);
        }
    }

    /// Reloads what is selected, leaving the current content up until the result arrives.
    fn load_selection(&mut self, cx: &mut Context<Self>) {
        let (Some(project), Some(selection)) = (self.project.clone(), self.selection.clone())
        else {
            return;
        };
        let frames = self.frames.clone();
        let loading = cx.background_spawn(async move { load(&project, &selection, &frames) });
        self.load_task = Some(cx.spawn(async move |viewer, cx| {
            let content = loading
                .await
                .unwrap_or_else(|error| Content::Failed(failure_messages(&error)));
            viewer
                .update(cx, |viewer, cx| viewer.show(content, cx))
                .ok();
        }));
    }

    fn show(&mut self, content: Content, cx: &mut Context<Self>) {
        let replaced = std::mem::replace(&mut self.content, content);
        if let Content::Frame(frame) = replaced {
            // GPUI keeps every image in its atlas until told otherwise.
            cx.drop_image(frame.image, None);
        }
        cx.notify();
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
            .child(resizable_panel().child(self.render_content(cx)))
            .child(
                resizable_panel()
                    .size(px(300.))
                    .size_range(px(200.)..px(600.))
                    .child(self.render_details(cx)),
            )
    }

    fn render_content(&self, cx: &mut Context<Self>) -> AnyElement {
        match &self.content {
            Content::Nothing | Content::Loading => div().into_any_element(),
            Content::Frame(frame) => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(img(ImageSource::Render(frame.image.clone())).size_full())
                .into_any_element(),
            Content::Text(text) => div()
                .id("text-content")
                .size_full()
                .overflow_y_scrollbar()
                .child(
                    div()
                        .p_4()
                        .text_sm()
                        .font_family(cx.theme().mono_font_family.clone())
                        .child(text.clone()),
                )
                .into_any_element(),
            Content::Failed(messages) => self.render_failure(messages, cx).into_any_element(),
        }
    }

    fn render_failure(&self, messages: &[String], cx: &mut Context<Self>) -> impl IntoElement {
        let subject = match self.selection {
            Some(Selection::Frame(_)) => "This frame has errors and can't be rendered.",
            _ => "This file has errors and can't be shown.",
        };
        div()
            .id("failure")
            .size_full()
            .p_6()
            .flex()
            .flex_col()
            .gap_2()
            .overflow_y_scrollbar()
            .child(div().text_color(cx.theme().danger).child(subject))
            .child("Ask your AI to fix it.")
            .children(messages.iter().map(|message| {
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .font_family(cx.theme().mono_font_family.clone())
                    .child(message.clone())
            }))
    }

    fn render_details(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut rows = Vec::new();
        if let Content::Frame(frame) = &self.content {
            let report = &frame.report;
            rows.push(detail_heading(report["frame"].as_str().unwrap_or_default()));
            rows.push(detail_row(
                format!("{} × {}", frame.width, frame.height),
                0,
                cx,
            ));
            rows.push(detail_row(
                format!("background {}", report["background"]),
                0,
                cx,
            ));
            let warnings = report["warnings"].as_array().map(Vec::as_slice);
            for warning in warnings.unwrap_or_default() {
                let message = warning["message"].as_str().unwrap_or_default();
                rows.push(
                    div()
                        .text_sm()
                        .text_color(cx.theme().warning)
                        .child(message.to_owned())
                        .into_any_element(),
                );
            }
            rows.push(detail_heading("Layers"));
            push_layer_rows(&report["layers"], 0, &mut rows, cx);
        }
        div()
            .id("details")
            .size_full()
            .p_3()
            .flex()
            .flex_col()
            .gap_1()
            .overflow_y_scrollbar()
            .children(rows)
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

fn load(project: &Project, selection: &Selection, frames: &[FrameFile]) -> Result<Content> {
    match selection {
        Selection::Frame(relative_path) => {
            let file = frames
                .iter()
                .find(|file| file.relative_path.as_ref() == Some(relative_path))
                .context("this frame no longer exists")?;
            Ok(Content::Frame(frame_render::render_frame(project, file)?))
        }
        Selection::Tokens => {
            let tokens = project.tokens()?.resolved()?;
            Ok(Content::Text(serde_json::to_string_pretty(&tokens)?))
        }
        Selection::Config => {
            let path = project.config_path();
            let text = fs::read_to_string(&path)
                .with_context(|| format!("cannot read {}", path.display()))?;
            Ok(Content::Text(text))
        }
    }
}

fn detail_heading(text: &str) -> AnyElement {
    div()
        .pt_2()
        .font_weight(FontWeight::SEMIBOLD)
        .child(text.to_owned())
        .into_any_element()
}

fn detail_row(text: String, depth: usize, cx: &App) -> AnyElement {
    div()
        .pl(px(12. * depth as f32))
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text)
        .into_any_element()
}

fn push_layer_rows(layers: &Value, depth: usize, rows: &mut Vec<AnyElement>, cx: &App) {
    for layer in layers.as_array().map(Vec::as_slice).unwrap_or_default() {
        let kind = layer["type"].as_str().unwrap_or_default();
        let id = layer["id"].as_str().unwrap_or_default();
        let bounds = &layer["clippedBounds"];
        let placement = if bounds.is_null() {
            "clipped".to_owned()
        } else {
            format!(
                "{:.0}, {:.0}  {:.0} × {:.0}",
                bounds["x"].as_f64().unwrap_or_default(),
                bounds["y"].as_f64().unwrap_or_default(),
                bounds["width"].as_f64().unwrap_or_default(),
                bounds["height"].as_f64().unwrap_or_default(),
            )
        };
        rows.push(detail_row(format!("{kind} {id}  {placement}"), depth, cx));
        push_layer_rows(&layer["children"], depth + 1, rows, cx);
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
