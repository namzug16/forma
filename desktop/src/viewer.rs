use std::cell::Cell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};

use forma::diagnostics::Failure;
use forma::frames::{self, FrameFile};
use forma::project::Project;
use futures::StreamExt as _;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::resizable::{h_resizable, resizable_panel};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tree::{TreeEvent, TreeState, tree};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use notify::{Event, RecommendedWatcher};
use serde_json::Value;

use crate::asset_preview::{self, AssetKind, AssetPreview};
use crate::frame_render::{self, RenderedFrame};
use crate::frame_tree::{self, Selection};
use crate::project_pages;
use crate::project_watch::{self, WatchedPaths};
use crate::recent_projects;
use crate::style;
use crate::token_sheet::{self, TokenSheet};
use crate::viewport::Viewport;

const ICON_SIZE: f32 = 14.;
const TREE_ROW_HEIGHT: f32 = 28.;
const FRAME_LABEL_OFFSET: f32 = 20.;
const LAYER_ROW_PADDING: f32 = 8.;
const LAYER_TAG_HEIGHT: f32 = 18.;
/// Leaves room for the window's traffic lights above the panels' own content.
const TRAFFIC_LIGHTS_HEIGHT: f32 = 44.;

/// Long enough to gather a burst of writes into one render without feeling late.
const CHANGE_DEBOUNCE: Duration = Duration::from_millis(150);
const COPIED_NOTICE_DURATION: Duration = Duration::from_secs(2);

const ZOOM_BUTTON_FACTOR: f32 = 1.25;
/// Scroll distance that doubles the zoom is about 70 pixels.
const ZOOM_PER_SCROLL_PIXEL: f32 = 0.01;

/// How the last "Export to SVG" went.
enum SvgExport {
    Idle,
    Running,
    Copied,
    Failed(String),
}

/// What the main area shows for the current selection.
enum Content {
    Nothing,
    Loading,
    Frame(RenderedFrame),
    Tokens(TokenSheet),
    Config,
    /// A file forma has no way of drawing.
    NotPreviewable {
        file_name: String,
        byte_size: u64,
    },
    /// Why the selection cannot be shown.
    Failed(Vec<String>),
}

pub struct Viewer {
    project: Arc<Project>,
    /// Why the project cannot be shown; agents fix these, the viewer only reports them.
    project_failure: Vec<String>,
    /// Dropping these stops the change events.
    project_watch: Option<(RecommendedWatcher, WatchedPaths, Task<()>)>,
    /// Set when live updates stop, because what is on screen can then go stale.
    watch_failure: Option<String>,
    frames: Vec<FrameFile>,
    assets: Vec<PathBuf>,
    tree_state: Entity<TreeState>,
    collapsed_folders: HashSet<SharedString>,
    selection: Option<Selection>,
    /// The layer outlined on the canvas; ids are unique within a frame.
    highlighted_layer_id: Option<String>,
    content: Content,
    /// `None` while the frame simply fits the canvas, until the user pans or zooms.
    viewport: Option<Viewport>,
    /// Filled in on paint: input events arrive in window coordinates.
    canvas_bounds: Rc<Cell<Bounds<Pixels>>>,
    drag_position: Option<Point<Pixels>>,
    svg_export: SvgExport,
    /// Dropping it abandons an export, so a late one can't overwrite the clipboard.
    svg_export_task: Option<Task<()>>,
    /// Set while loading. Dropping it cancels a load that a newer change has replaced.
    load_task: Option<Task<()>>,
    _subscriptions: [Subscription; 2],
}

impl Viewer {
    pub fn new(project: Project, cx: &mut Context<Self>) -> Self {
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
        let mut viewer = Self {
            project: Arc::new(project),
            project_failure: Vec::new(),
            project_watch: None,
            watch_failure: None,
            frames: Vec::new(),
            assets: Vec::new(),
            tree_state,
            collapsed_folders: frame_tree::initially_collapsed_folders(),
            selection: None,
            highlighted_layer_id: None,
            content: Content::Nothing,
            viewport: None,
            canvas_bounds: Rc::default(),
            drag_position: None,
            svg_export: SvgExport::Idle,
            svg_export_task: None,
            load_task: None,
            _subscriptions: subscriptions,
        };
        viewer.start_watching(cx);
        viewer.sync_tree(cx);
        viewer
    }

    fn select(&mut self, selection: Selection, cx: &mut Context<Self>) {
        if self.selection.as_ref() != Some(&selection) {
            self.selection = Some(selection);
            self.highlighted_layer_id = None;
            self.reset_svg_export();
            self.viewport = None;
            self.show(Content::Loading, cx);
            self.load_selection(cx);
        }
    }

    /// Reloads what is selected, leaving the current content up until the result arrives.
    fn load_selection(&mut self, cx: &mut Context<Self>) {
        let Some(selection) = self.selection.clone() else {
            return;
        };
        let project = self.project.clone();
        let frames = self.frames.clone();
        let loading = cx.background_spawn(async move { load(&project, &selection, &frames) });
        self.load_task = Some(cx.spawn(async move |viewer, cx| {
            let content = loading
                .await
                .unwrap_or_else(|error| Content::Failed(failure_messages(&error)));
            viewer
                .update(cx, |viewer, cx| {
                    viewer.load_task = None;
                    viewer.show(content, cx);
                })
                .ok();
        }));
        cx.notify();
    }

    fn show(&mut self, content: Content, cx: &mut Context<Self>) {
        let replaced = std::mem::replace(&mut self.content, content);
        // GPUI keeps every image in its atlas until told otherwise.
        match replaced {
            Content::Frame(frame) => cx.drop_image(frame.image, None),
            Content::Tokens(sheet) => {
                for image in sheet.into_images() {
                    cx.drop_image(image, None);
                }
            }
            _ => {}
        }
        if let Content::Frame(frame) = &self.content {
            let layer_still_exists = self
                .highlighted_layer_id
                .as_ref()
                .is_some_and(|id| layer_bounds(&frame.report["layers"], id).is_some());
            if !layer_still_exists {
                self.highlighted_layer_id = None;
            }
        }
        cx.notify();
    }

    fn adopt_project(&mut self, project: Project, cx: &mut Context<Self>) {
        self.project = Arc::new(project);
        self.project_failure.clear();
        self.start_watching(cx);
        self.sync_tree(cx);
        self.load_selection(cx);
    }

    /// The project's own files changed in a way that may invalidate it. A failure leaves the
    /// old watcher running, so the fix is picked up as soon as it is saved.
    fn reload_project(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.project.directory.parent().map(Path::to_owned) else {
            return;
        };
        match Project::locate(Some(&root), &root) {
            Ok(project) => self.adopt_project(project, cx),
            Err(error) => self.project_failure = failure_messages(&error),
        }
        cx.notify();
    }

    fn start_watching(&mut self, cx: &mut Context<Self>) {
        let watched_paths = WatchedPaths::of(&self.project);
        let (watcher, mut changes) = match project_watch::watch(&watched_paths) {
            Ok(watch) => watch,
            Err(error) => {
                self.project_watch = None;
                self.watch_failure = Some(format!("{error:#}"));
                return;
            }
        };
        self.watch_failure = None;
        let forward_changes = cx.spawn(async move |viewer, cx| {
            while let Some(first_change) = changes.next().await {
                let mut batch = vec![first_change];
                cx.background_executor().timer(CHANGE_DEBOUNCE).await;
                while let Ok(change) = changes.try_recv() {
                    batch.push(change);
                }
                let update = viewer.update(cx, |viewer, cx| viewer.apply_changes(batch, cx));
                if update.is_err() {
                    return;
                }
            }
        });
        self.project_watch = Some((watcher, watched_paths, forward_changes));
    }

    fn apply_changes(&mut self, batch: Vec<notify::Result<Event>>, cx: &mut Context<Self>) {
        let mut changed_paths = Vec::new();
        for change in batch {
            match change {
                Ok(event) if !event.kind.is_access() => changed_paths.extend(event.paths),
                Ok(_) => {}
                Err(error) => {
                    self.watch_failure = Some(error.to_string());
                    cx.notify();
                }
            }
        }
        let Some((_, watched_paths, _)) = &self.project_watch else {
            return;
        };
        let selected_frame = match &self.selection {
            Some(Selection::Frame(relative_path)) => Some(relative_path.as_path()),
            _ => None,
        };
        let effects = watched_paths.effects_of(&changed_paths, selected_frame);
        if effects.reload_project || !self.project_failure.is_empty() {
            self.reload_project(cx);
            return;
        }
        if effects.refresh_tree {
            self.sync_tree(cx);
        }
        if effects.reload_selection {
            self.load_selection(cx);
        }
    }

    /// Matches the tree to the frames on disk. Only their existence matters here, so an edit
    /// to a frame leaves the tree alone.
    fn sync_tree(&mut self, cx: &mut Context<Self>) {
        let frames = frames::list(&self.project).unwrap_or_else(|error| {
            self.project_failure = failure_messages(&error);
            Vec::new()
        });
        let assets = asset_preview::list(&self.project).unwrap_or_else(|error| {
            self.project_failure = failure_messages(&error);
            Vec::new()
        });
        let same_frames = frames
            .iter()
            .map(|file| &file.relative_path)
            .eq(self.frames.iter().map(|file| &file.relative_path));
        if same_frames && assets == self.assets && self.tree_state.read(cx).entry(0).is_some() {
            return;
        }
        self.frames = frames;
        self.assets = assets;
        let items = frame_tree::build_items(&self.frames, &self.assets, &self.collapsed_folders);
        self.tree_state.update(cx, |tree_state, cx| {
            let selected_id = tree_state.selected_item().map(|item| item.id.clone());
            tree_state.set_items(items, cx);
            let selected_index = selected_id.and_then(|id| tree_state.index_of(&id));
            tree_state.set_selected_index(selected_index, cx);
        });
        self.clear_selection_if_deleted(cx);
    }

    fn clear_selection_if_deleted(&mut self, cx: &mut Context<Self>) {
        let still_exists = match &self.selection {
            Some(Selection::Frame(selected_path)) => self
                .frames
                .iter()
                .any(|file| file.relative_path.as_ref() == Some(selected_path)),
            Some(Selection::Asset(selected_path)) => self.assets.contains(selected_path),
            _ => true,
        };
        if !still_exists {
            self.selection = None;
            self.highlighted_layer_id = None;
            self.reset_svg_export();
            self.viewport = None;
            self.load_task = None;
            self.show(Content::Nothing, cx);
        }
    }

    fn render_tree(&self) -> impl IntoElement {
        tree(&self.tree_state, |index, entry, is_selected, _, cx| {
            if entry.is_disabled() {
                return ListItem::new(index)
                    .h(px(TREE_ROW_HEIGHT))
                    .child(div().w_full().h(px(1.)).bg(cx.theme().border));
            }
            let secondary_text = cx.theme().muted_foreground;
            let is_frames_root = entry.is_folder() && entry.depth() == 0;
            let item_icon = match Selection::from_item_id(&entry.item().id) {
                Some(Selection::Frame(_)) => Some(IconName::Frame),
                Some(Selection::Asset(relative_path)) => Some(asset_icon(&relative_path)),
                Some(Selection::Tokens) => Some(IconName::Palette),
                Some(Selection::Config) => Some(IconName::Settings2),
                None if is_frames_root => None,
                None if entry.is_expanded() => Some(IconName::FolderOpen),
                None => Some(IconName::Folder),
            };
            let disclosure = entry.is_folder().then(|| {
                icon(
                    if entry.is_expanded() {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    },
                    secondary_text,
                )
            });
            ListItem::new(index)
                .h(px(TREE_ROW_HEIGHT))
                .py_0()
                .px_0()
                .pl(px(8. + 16. * entry.depth() as f32))
                .rounded(cx.theme().radius)
                .text_size(px(13.))
                .child(
                    h_flex()
                        .gap_1p5()
                        .min_w_0()
                        .child(div().flex_none().w(px(ICON_SIZE)).children(disclosure))
                        .children(item_icon.map(|name| icon(name, secondary_text)))
                        .child(
                            div()
                                .truncate()
                                .when(is_frames_root || is_selected, |label| {
                                    label.font_weight(FontWeight::MEDIUM)
                                })
                                .child(entry.item().label.clone()),
                        ),
                )
        })
    }

    fn render_project_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let project_name = self
            .project
            .directory
            .parent()
            .map(recent_projects::project_name)
            .unwrap_or_default();
        let frame_count = match self.frames.len() {
            1 => "1 frame".to_owned(),
            count => format!("{count} frames"),
        };
        h_flex()
            .flex_none()
            .pt(px(TRAFFIC_LIGHTS_HEIGHT))
            .pl_2()
            .pr_4()
            .pb_3()
            .gap_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Button::new("all-projects")
                    .ghost()
                    .small()
                    .icon(IconName::ChevronLeft)
                    .tooltip("All projects")
                    .on_click(|_, window, cx| {
                        // The welcome window comes first: closing the last window quits.
                        crate::open_welcome_window(cx);
                        window.remove_window();
                    }),
            )
            .child(
                div()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(project_name),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(frame_count),
                    ),
            )
    }

    fn render_project(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        h_resizable("panels")
            .child(
                resizable_panel()
                    .size(px(200.))
                    .size_range(px(160.)..px(480.))
                    .child(
                        div()
                            .size_full()
                            .flex()
                            .flex_col()
                            .bg(cx.theme().sidebar)
                            .child(self.render_project_header(cx))
                            .child(div().flex_1().min_h_0().p_2().child(self.render_tree())),
                    ),
            )
            .child(resizable_panel().child(self.render_content(cx)))
            .child(
                resizable_panel()
                    .visible(matches!(self.selection, Some(Selection::Frame(_))))
                    .size(px(260.))
                    .size_range(px(200.)..px(600.))
                    .child(self.render_details(cx)),
            )
    }

    pub fn fit_frame(&mut self, cx: &mut Context<Self>) {
        self.viewport = None;
        cx.notify();
    }

    fn current_viewport(&self) -> Option<Viewport> {
        let frame = self.displayed_frame()?;
        let canvas_size = self.canvas_bounds.get().size;
        Some(self.viewport.unwrap_or_else(|| {
            Viewport::fitting(
                size(frame.width, frame.height),
                size(canvas_size.width.into(), canvas_size.height.into()),
            )
        }))
    }

    fn zoom_at(&mut self, factor: f32, window_position: Point<Pixels>, cx: &mut Context<Self>) {
        let origin = self.canvas_bounds.get().origin;
        if let Some(mut viewport) = self.current_viewport() {
            viewport.zoom_at(
                factor,
                point(
                    (window_position.x - origin.x).into(),
                    (window_position.y - origin.y).into(),
                ),
            );
            self.viewport = Some(viewport);
            cx.notify();
        }
    }

    fn zoom_at_canvas_center(&mut self, factor: f32, cx: &mut Context<Self>) {
        self.zoom_at(factor, self.canvas_bounds.get().center(), cx);
    }

    fn pan(&mut self, delta: Point<Pixels>, cx: &mut Context<Self>) {
        if let Some(mut viewport) = self.current_viewport() {
            viewport.pan(point(delta.x.into(), delta.y.into()));
            self.viewport = Some(viewport);
            cx.notify();
        }
    }

    fn reset_svg_export(&mut self) {
        self.svg_export = SvgExport::Idle;
        self.svg_export_task = None;
    }

    fn selected_frame_file(&self) -> Option<FrameFile> {
        let Some(Selection::Frame(relative_path)) = &self.selection else {
            return None;
        };
        self.frames
            .iter()
            .find(|file| file.relative_path.as_ref() == Some(relative_path))
            .cloned()
    }

    fn copy_svg_to_clipboard(&mut self, cx: &mut Context<Self>) {
        let Some(file) = self.selected_frame_file() else {
            return;
        };
        let project = self.project.clone();
        let exporting =
            cx.background_spawn(async move { frame_render::export_svg(&project, &file) });
        self.svg_export = SvgExport::Running;
        cx.notify();
        self.svg_export_task = Some(cx.spawn(async move |viewer, cx| {
            let exported = exporting.await;
            let update = viewer.update(cx, |viewer, cx| {
                viewer.svg_export = match exported {
                    Ok(markup) => {
                        cx.write_to_clipboard(ClipboardItem::new_string(markup));
                        SvgExport::Copied
                    }
                    Err(error) => SvgExport::Failed(failure_messages(&error).join("\n")),
                };
                cx.notify();
            });
            if update.is_ok() {
                cx.background_executor().timer(COPIED_NOTICE_DURATION).await;
                viewer
                    .update(cx, |viewer, cx| {
                        if matches!(viewer.svg_export, SvgExport::Copied) {
                            viewer.svg_export = SvgExport::Idle;
                            cx.notify();
                        }
                    })
                    .ok();
            }
        }));
    }

    fn render_export_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let is_running = matches!(self.svg_export, SvgExport::Running);
        let (label, button_icon) = match self.svg_export {
            SvgExport::Copied => ("Copied to clipboard", IconName::Check),
            _ => ("Export to SVG", IconName::Copy),
        };
        let note = match &self.svg_export {
            SvgExport::Failed(message) => message.clone(),
            _ => "Copies the SVG to your clipboard, ready to paste into Figma.".to_owned(),
        };
        let note_color = if matches!(self.svg_export, SvgExport::Failed(_)) {
            cx.theme().danger
        } else {
            cx.theme().muted_foreground
        };
        detail_section("Export", cx)
            .child(
                Button::new("export-svg")
                    .primary()
                    .small()
                    .w_full()
                    .icon(button_icon)
                    .label(label)
                    .loading(is_running)
                    .disabled(is_running)
                    .on_click(cx.listener(|viewer, _, _, cx| viewer.copy_svg_to_clipboard(cx))),
            )
            .child(div().text_xs().text_color(note_color).child(note))
    }

    fn toggle_layer_highlight(&mut self, layer_id: String, cx: &mut Context<Self>) {
        let is_highlighted = self.highlighted_layer_id.as_ref() == Some(&layer_id);
        self.highlighted_layer_id = (!is_highlighted).then_some(layer_id);
        cx.notify();
    }

    /// Drawn over the image, so highlighting never re-renders the frame. The outline is clipped
    /// to the frame like the layers are, but the tag is not, so it survives at the frame's edge.
    fn render_layer_highlight(&self, viewport: Viewport) -> Option<impl IntoElement> {
        let frame = self.displayed_frame()?;
        let layer_id = self.highlighted_layer_id.clone()?;
        let bounds = layer_bounds(&frame.report["layers"], &layer_id)?;
        let zoom = viewport.zoom;
        // A layer overhanging the frame keeps its tag on the part that can be seen.
        let tag_left = viewport.offset.x + (bounds.origin.x * zoom).max(0.);
        let tag_top = viewport.offset.y + (bounds.origin.y * zoom).max(0.) - LAYER_TAG_HEIGHT;
        Some(
            div()
                .absolute()
                .size_full()
                .child(
                    div()
                        .absolute()
                        .left(px(viewport.offset.x))
                        .top(px(viewport.offset.y))
                        .w(px(frame.width * zoom))
                        .h(px(frame.height * zoom))
                        .overflow_hidden()
                        .child(
                            div()
                                .absolute()
                                .left(px(bounds.origin.x * zoom))
                                .top(px(bounds.origin.y * zoom))
                                .w(px(bounds.size.width * zoom))
                                .h(px(bounds.size.height * zoom))
                                .border_2()
                                .border_color(style::layer_highlight()),
                        ),
                )
                .child(
                    div()
                        .absolute()
                        .left(px(tag_left))
                        .top(px(tag_top))
                        .h(px(LAYER_TAG_HEIGHT))
                        .px_1()
                        .flex()
                        .items_center()
                        .whitespace_nowrap()
                        .bg(style::layer_highlight())
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(rgb(0xffffff))
                        .child(layer_id),
                ),
        )
    }

    /// The previous image stays up during a re-render, so this is what says it is stale.
    fn render_rendering_badge(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .absolute()
            .top_3()
            .right_3()
            .px_2()
            .py_1()
            .gap_2()
            .rounded(px(8.))
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .text_xs()
            .child(Spinner::new().xsmall().color(cx.theme().muted_foreground))
            .child("Rendering")
    }

    fn render_zoom_controls(&self, zoom: f32, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .absolute()
            .bottom_4()
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(
                h_flex()
                    .id("zoom-controls")
                    .occlude()
                    .p_1()
                    .gap_1()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().background)
                    .shadow_md()
                    .child(
                        Button::new("zoom-out")
                            .ghost()
                            .small()
                            .icon(IconName::Minus)
                            .tooltip("Zoom out")
                            .on_click(cx.listener(|viewer, _, _, cx| {
                                viewer.zoom_at_canvas_center(1. / ZOOM_BUTTON_FACTOR, cx);
                            })),
                    )
                    .child(
                        div()
                            .w(px(44.))
                            .text_center()
                            .text_xs()
                            .child(format!("{:.0}%", zoom * 100.)),
                    )
                    .child(
                        Button::new("zoom-in")
                            .ghost()
                            .small()
                            .icon(IconName::Plus)
                            .tooltip("Zoom in")
                            .on_click(cx.listener(|viewer, _, _, cx| {
                                viewer.zoom_at_canvas_center(ZOOM_BUTTON_FACTOR, cx);
                            })),
                    )
                    .child(div().w(px(1.)).h_4().bg(cx.theme().border))
                    .child(
                        Button::new("zoom-reset")
                            .ghost()
                            .small()
                            .icon(IconName::Maximize)
                            .tooltip("Reset zoom")
                            .on_click(cx.listener(|viewer, _, _, cx| viewer.fit_frame(cx))),
                    ),
            )
    }

    fn render_canvas(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport = self.current_viewport();
        let canvas_bounds = self.canvas_bounds.clone();
        let secondary_text = cx.theme().muted_foreground;
        div()
            .id("canvas")
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(style::canvas_background())
            .child(
                canvas(
                    move |bounds, window, _| {
                        let was_resized = canvas_bounds.replace(bounds).size != bounds.size;
                        if was_resized {
                            // This paint used the old size, so a fitted frame needs another.
                            window.on_next_frame(|window, _| window.refresh());
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .when_some(
                viewport.zip(self.displayed_frame()),
                |canvas, (viewport, frame)| {
                    let frame_width = frame.width * viewport.zoom;
                    canvas
                        .child(
                            div()
                                .absolute()
                                .left(px(viewport.offset.x))
                                .top(px(viewport.offset.y - FRAME_LABEL_OFFSET))
                                .w(px(frame_width))
                                .truncate()
                                .text_xs()
                                .text_color(secondary_text)
                                .child(frame_title(frame)),
                        )
                        .child(
                            img(ImageSource::Render(frame.image.clone()))
                                .absolute()
                                .left(px(viewport.offset.x))
                                .top(px(viewport.offset.y))
                                .w(px(frame_width))
                                .h(px(frame.height * viewport.zoom)),
                        )
                },
            )
            .children(viewport.and_then(|viewport| self.render_layer_highlight(viewport)))
            .when_some(viewport, |canvas, viewport| {
                canvas.child(self.render_zoom_controls(viewport.zoom, cx))
            })
            .when(matches!(self.content, Content::Loading), |canvas| {
                canvas.child(
                    div()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(Spinner::new().large().color(secondary_text)),
                )
            })
            .when(
                self.load_task.is_some() && self.displayed_frame().is_some(),
                |canvas| canvas.child(self.render_rendering_badge(cx)),
            )
            .when(matches!(self.content, Content::Nothing), |canvas| {
                canvas.child(
                    div()
                        .size_full()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .text_color(secondary_text)
                        .child(icon(IconName::Frame, secondary_text).size(px(24.)))
                        .child("Select a frame"),
                )
            })
            .on_scroll(cx.listener(|viewer, event: &ScrollEvent, window, cx| {
                let delta = event.delta.pixel_delta(window.line_height());
                if event.modifiers.platform {
                    let scrolled: f32 = delta.y.into();
                    let factor = (scrolled * ZOOM_PER_SCROLL_PIXEL).exp();
                    viewer.zoom_at(factor, event.position, cx);
                } else {
                    viewer.pan(delta, cx);
                }
            }))
            .on_pinch(cx.listener(|viewer, event: &PinchEvent, _, cx| {
                viewer.zoom_at(1.0 + event.delta, event.position, cx);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|viewer, event: &MouseDownEvent, _, _| {
                    viewer.drag_position = Some(event.position);
                }),
            )
            .on_mouse_move(cx.listener(|viewer, event: &MouseMoveEvent, _, cx| {
                if let Some(previous_position) = viewer.drag_position {
                    viewer.drag_position = Some(event.position);
                    viewer.pan(event.position - previous_position, cx);
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|viewer, _: &MouseUpEvent, _, _| viewer.drag_position = None),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|viewer, _: &MouseUpEvent, _, _| viewer.drag_position = None),
            )
    }

    fn displayed_frame(&self) -> Option<&RenderedFrame> {
        match &self.content {
            Content::Frame(frame) => Some(frame),
            _ => None,
        }
    }

    fn render_content(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if !self.project_failure.is_empty() {
            return self
                .render_failure(&self.project_failure, cx)
                .into_any_element();
        }
        match &self.content {
            Content::Nothing | Content::Loading | Content::Frame(_) => {
                self.render_canvas(cx).into_any_element()
            }
            Content::Tokens(sheet) => project_pages::render_tokens(sheet, cx).into_any_element(),
            Content::Config => project_pages::render_config(&self.project, self.frames.len(), cx)
                .into_any_element(),
            Content::NotPreviewable {
                file_name,
                byte_size,
            } => render_not_previewable(file_name, *byte_size, cx).into_any_element(),
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
            .bg(style::canvas_background())
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .p_6()
            .overflow_y_scrollbar()
            .child(icon(IconName::TriangleAlert, cx.theme().danger).size(px(24.)))
            .child(div().font_weight(FontWeight::MEDIUM).child(subject))
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child("Ask your AI to fix it."),
            )
            .children(messages.iter().map(|message| {
                div()
                    .mt_2()
                    .max_w(px(560.))
                    .p_3()
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().muted)
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .font_family(cx.theme().mono_font_family.clone())
                    .child(message.clone())
            }))
    }

    fn render_details_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let title = match (self.displayed_frame(), &self.selection) {
            (Some(frame), _) => frame_title(frame),
            (None, Some(Selection::Frame(relative_path))) => relative_path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            _ => String::new(),
        };
        div()
            .flex_none()
            .pt(px(TRAFFIC_LIGHTS_HEIGHT))
            .px_4()
            .pb_3()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .truncate()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Frame"),
            )
    }

    fn push_layer_rows(
        &self,
        layers: &Value,
        depth: usize,
        rows: &mut Vec<AnyElement>,
        cx: &mut Context<Self>,
    ) {
        for layer in layers.as_array().map(Vec::as_slice).unwrap_or_default() {
            let layer_id = layer["id"].as_str().unwrap_or_default().to_owned();
            let is_highlighted = self.highlighted_layer_id.as_ref() == Some(&layer_id);
            rows.push(
                h_flex()
                    .id(SharedString::from(format!("layer-{layer_id}")))
                    .h(px(TREE_ROW_HEIGHT))
                    .pl(px(LAYER_ROW_PADDING + 16. * depth as f32))
                    .pr(px(LAYER_ROW_PADDING))
                    .mx(px(-LAYER_ROW_PADDING))
                    .gap_2()
                    .rounded(cx.theme().radius)
                    .text_xs()
                    .cursor_pointer()
                    .when(is_highlighted, |row| row.bg(cx.theme().list_active))
                    .when(!is_highlighted, |row| {
                        row.hover(|row| row.bg(cx.theme().list_hover))
                    })
                    .child(icon(
                        layer_icon(layer["type"].as_str().unwrap_or_default()),
                        if is_highlighted {
                            style::layer_highlight().into()
                        } else {
                            cx.theme().muted_foreground
                        },
                    ))
                    .child(div().min_w_0().truncate().child(layer_id.clone()))
                    .on_click(cx.listener(move |viewer, _, _, cx| {
                        viewer.toggle_layer_highlight(layer_id.clone(), cx);
                    }))
                    .into_any_element(),
            );
            self.push_layer_rows(&layer["children"], depth + 1, rows, cx);
        }
    }

    fn render_details(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut sections = Vec::new();
        if let Some(frame) = self.displayed_frame() {
            let report = &frame.report;
            sections.push(
                detail_section("Size", cx)
                    .child(
                        h_flex()
                            .gap_2()
                            .child(value_chip("W", None, frame.width.to_string(), cx))
                            .child(value_chip("H", None, frame.height.to_string(), cx)),
                    )
                    .into_any_element(),
            );
            let (swatch, description) = describe_paint(&report["background"]);
            sections.push(
                detail_section("Background", cx)
                    .child(value_chip("", swatch, description, cx))
                    .into_any_element(),
            );
            let warnings = report["warnings"].as_array().map(Vec::as_slice);
            if let Some(warnings) = warnings.filter(|warnings| !warnings.is_empty()) {
                sections.push(
                    detail_section("Warnings", cx)
                        .children(warnings.iter().map(|warning| {
                            h_flex()
                                .items_start()
                                .gap_2()
                                .text_xs()
                                .child(icon(IconName::TriangleAlert, cx.theme().warning))
                                .child(div().min_w_0().child(
                                    warning["message"].as_str().unwrap_or_default().to_owned(),
                                ))
                        }))
                        .into_any_element(),
                );
            }
            sections.push(self.render_export_section(cx).into_any_element());
            let mut layer_rows = Vec::new();
            self.push_layer_rows(&report["layers"], 0, &mut layer_rows, cx);
            sections.push(
                detail_section("Layers", cx)
                    .border_b_0()
                    .gap_0()
                    .children(layer_rows)
                    .into_any_element(),
            );
        }
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .child(self.render_details_header(cx))
            .child(
                div()
                    .id("details")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .children(sections),
            )
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
        Selection::Asset(relative_path) => Ok(match asset_preview::load(project, relative_path)? {
            AssetPreview::Drawn(frame) => Content::Frame(frame),
            AssetPreview::NotPreviewable {
                file_name,
                byte_size,
            } => Content::NotPreviewable {
                file_name,
                byte_size,
            },
        }),
        Selection::Tokens => Ok(Content::Tokens(token_sheet::load(project)?)),
        Selection::Config => Ok(Content::Config),
    }
}

pub fn icon(name: IconName, color: Hsla) -> Icon {
    Icon::new(name).size(px(ICON_SIZE)).text_color(color)
}

fn detail_section(title: &str, cx: &App) -> Div {
    div()
        .flex_none()
        .px_4()
        .py_3()
        .flex()
        .flex_col()
        .gap_2()
        .border_b_1()
        .border_color(cx.theme().border)
        .child(
            div()
                .font_weight(FontWeight::MEDIUM)
                .child(title.to_owned()),
        )
}

/// A read-only value drawn like Figma's fields.
fn value_chip(label: &str, swatch: Option<Hsla>, value: String, cx: &App) -> impl IntoElement {
    h_flex()
        .flex_1()
        .min_w_0()
        .h(px(TREE_ROW_HEIGHT))
        .px_2()
        .gap_2()
        .rounded(cx.theme().radius)
        .bg(cx.theme().muted)
        .text_xs()
        .when(!label.is_empty(), |chip| {
            chip.child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child(label.to_owned()),
            )
        })
        .when_some(swatch, |chip, color| {
            chip.child(
                div()
                    .flex_none()
                    .size(px(ICON_SIZE))
                    .rounded(px(2.))
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(color),
            )
        })
        .child(div().truncate().child(value))
}

/// Solid paints show their color; gradients are named, with their first stop as the swatch.
fn describe_paint(paint: &Value) -> (Option<Hsla>, String) {
    let parse_color = |color: &Value| {
        let color = Rgba::try_from(color.as_str()?).ok()?;
        Some(Hsla::from(color))
    };
    match paint["type"].as_str() {
        Some("solid") => (
            parse_color(&paint["color"]),
            opaque_hex(paint["color"].as_str().unwrap_or_default()).to_owned(),
        ),
        Some("linear-gradient") => (
            parse_color(&paint["stops"][0]["color"]),
            "Linear gradient".to_owned(),
        ),
        Some("radial-gradient") => (
            parse_color(&paint["stops"][0]["color"]),
            "Radial gradient".to_owned(),
        ),
        _ => (None, "None".to_owned()),
    }
}

/// Forma writes the alpha even when it is full, which is only noise to a reader.
fn opaque_hex(color: &str) -> &str {
    color
        .strip_suffix("FF")
        .filter(|hex| hex.len() == 7)
        .unwrap_or(color)
}

fn frame_title(frame: &RenderedFrame) -> String {
    frame.report["frame"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

fn asset_icon(relative_path: &Path) -> IconName {
    match asset_preview::kind_of(relative_path) {
        AssetKind::Image => IconName::Image,
        AssetKind::Svg => IconName::Shapes,
        AssetKind::Font => IconName::Type,
        AssetKind::Other => IconName::File,
    }
}

fn render_not_previewable(file_name: &str, byte_size: u64, cx: &App) -> impl IntoElement {
    let secondary_text = cx.theme().muted_foreground;
    div()
        .size_full()
        .bg(style::canvas_background())
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_2()
        .child(icon(IconName::File, secondary_text).size(px(24.)))
        .child(
            div()
                .font_weight(FontWeight::MEDIUM)
                .child(file_name.to_owned()),
        )
        .child(div().text_color(secondary_text).child(format!(
            "{} · No preview for this file type",
            asset_preview::human_size(byte_size)
        )))
}

fn layer_icon(kind: &str) -> IconName {
    match kind {
        "rect" => IconName::Square,
        "ellipse" => IconName::Circle,
        "line" => IconName::Minus,
        "text" => IconName::Type,
        "image" => IconName::Image,
        "svg" => IconName::Shapes,
        _ => IconName::Group,
    }
}

/// The box around a layer in frame units. This is not `clippedBounds`, which is empty for a
/// line with no height even though the line is drawn.
fn layer_bounds(layers: &Value, layer_id: &str) -> Option<Bounds<f32>> {
    layers.as_array()?.iter().find_map(|layer| {
        if layer["id"] != layer_id {
            return layer_bounds(&layer["children"], layer_id);
        }
        let bounds = &layer["worldBounds"];
        let number = |key: &str| bounds[key].as_f64().map(|value| value as f32);
        Some(Bounds::new(
            point(number("x")?, number("y")?),
            size(number("width")?, number("height")?),
        ))
    })
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
        div()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .child(div().flex_1().min_h_0().child(self.render_project(cx)))
                    .when_some(self.watch_failure.clone(), |column, failure| {
                        column.child(
                            h_flex()
                                .flex_none()
                                .px_4()
                                .py_2()
                                .gap_2()
                                .border_t_1()
                                .border_color(cx.theme().border)
                                .text_xs()
                                .child(icon(IconName::TriangleAlert, cx.theme().danger))
                                .child(format!("Live updates stopped: {failure}")),
                        )
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::{Bounds, Hsla, point, rgb, size};
    use serde_json::{Value, json};

    use super::{describe_paint, layer_bounds};

    fn layers() -> Value {
        json!([{
            "id": "badge",
            "worldBounds": { "x": 10, "y": 20, "width": 30, "height": 40 },
            "children": [{
                "id": "rule",
                "worldBounds": { "x": 12, "y": 22, "width": 26, "height": 0 },
                "clippedBounds": null
            }]
        }])
    }

    #[test]
    fn top_level_layers_report_their_bounds() {
        assert_eq!(
            layer_bounds(&layers(), "badge"),
            Some(Bounds::new(point(10., 20.), size(30., 40.)))
        );
    }

    #[test]
    fn nested_lines_with_no_height_still_have_bounds() {
        assert_eq!(
            layer_bounds(&layers(), "rule"),
            Some(Bounds::new(point(12., 22.), size(26., 0.)))
        );
    }

    #[test]
    fn unknown_layers_have_no_bounds() {
        assert_eq!(layer_bounds(&layers(), "missing"), None);
    }

    #[test]
    fn solid_paints_show_their_color() {
        let (swatch, description) = describe_paint(&json!({ "type": "solid", "color": "#FF0000" }));

        assert_eq!(swatch, Some(Hsla::from(rgb(0xff0000))));
        assert_eq!(description, "#FF0000");
    }

    #[test]
    fn full_alpha_is_hidden_but_partial_alpha_is_not() {
        let hex = |color: &str| describe_paint(&json!({ "type": "solid", "color": color })).1;

        assert_eq!(hex("#101014FF"), "#101014");
        assert_eq!(hex("#10101480"), "#10101480");
    }

    #[test]
    fn gradients_are_named_with_their_first_stop() {
        let paint = json!({
            "type": "linear-gradient",
            "stops": [{ "offset": 0, "color": "#00FF00" }, { "offset": 1, "color": "#0000FF" }]
        });

        let (swatch, description) = describe_paint(&paint);

        assert_eq!(swatch, Some(Hsla::from(rgb(0x00ff00))));
        assert_eq!(description, "Linear gradient");
    }

    #[test]
    fn a_missing_background_is_none() {
        assert_eq!(describe_paint(&Value::Null), (None, "None".to_owned()));
    }
}
