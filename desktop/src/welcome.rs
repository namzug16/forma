//! The first window: pick a recent project or open another one.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use forma::project::Project;
use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::recent_projects;
use crate::style;
use crate::viewer::{failure_messages, icon};

const LOGO_SVG: &[u8] = include_bytes!("../assets/logo.svg");

pub struct Welcome {
    logo: Arc<Image>,
    recent_projects: Vec<PathBuf>,
    /// Why the last directory could not be opened.
    failure: Vec<String>,
}

impl Welcome {
    pub fn new(_: &mut Context<Self>) -> Self {
        Self {
            logo: Arc::new(Image::from_bytes(ImageFormat::Svg, LOGO_SVG.to_vec())),
            recent_projects: recent_projects::load(),
            failure: Vec::new(),
        }
    }

    fn choose_directory(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked_paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open".into()),
        });
        cx.spawn_in(window, async move |welcome, cx| {
            let picked_paths = picked_paths.await;
            welcome
                .update_in(cx, |welcome, window, cx| match picked_paths {
                    Ok(Ok(Some(paths))) => {
                        if let Some(path) = paths.first() {
                            welcome.open(path, window, cx);
                        }
                    }
                    Ok(Ok(None)) => {}
                    Ok(Err(error)) => {
                        welcome.failure = vec![format!("{error:#}")];
                        cx.notify();
                    }
                    Err(error) => {
                        welcome.failure = vec![error.to_string()];
                        cx.notify();
                    }
                })
                .ok();
        })
        .detach();
    }

    fn open(&mut self, directory: &Path, window: &mut Window, cx: &mut Context<Self>) {
        match Project::locate(None, directory) {
            Ok(project) => {
                if let Some(root) = project.directory.parent()
                    && let Err(error) = recent_projects::remember(root)
                {
                    // Only the list suffers, which is no reason to refuse the project.
                    eprintln!("warning: {error:#}");
                }
                crate::open_project_window(project, cx);
                window.remove_window();
            }
            Err(error) => {
                self.failure = failure_messages(&error);
                cx.notify();
            }
        }
    }

    fn render_recent_project(
        &self,
        index: usize,
        root: &Path,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let root_to_open = root.to_owned();
        h_flex()
            .id(("recent-project", index))
            .flex_none()
            .gap_3()
            .p_2()
            .rounded(px(8.))
            .cursor_pointer()
            .hover(|row| row.bg(cx.theme().list_active))
            .child(
                img(self.logo.clone())
                    .flex_none()
                    .size(px(36.))
                    .rounded(px(8.)),
            )
            .child(
                div()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(recent_projects::project_name(root)),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(recent_projects::location_label(root)),
                    ),
            )
            .on_click(cx.listener(move |welcome, _, window, cx| {
                welcome.open(&root_to_open, window, cx);
            }))
            .into_any_element()
    }

    fn render_recent_projects(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut rows = Vec::new();
        for (index, root) in self.recent_projects.iter().enumerate() {
            rows.push(self.render_recent_project(index, root, cx));
        }
        let secondary_text = cx.theme().muted_foreground;
        div()
            .id("recent-projects")
            .w(px(RECENT_PANEL_WIDTH))
            .flex_none()
            .h_full()
            .p_3()
            .flex()
            .flex_col()
            .gap_1()
            .bg(cx.theme().background)
            .overflow_y_scroll()
            .when(rows.is_empty(), |panel| {
                panel
                    .items_center()
                    .justify_center()
                    .child(div().text_color(secondary_text).child("No recent projects"))
            })
            .children(rows)
    }
}

const RECENT_PANEL_WIDTH: f32 = 300.;

impl Render for Welcome {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let secondary_text = cx.theme().muted_foreground;
        h_flex()
            .size_full()
            .text_color(cx.theme().foreground)
            .bg(style::canvas_background())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .px_12()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .child(img(self.logo.clone()).size(px(128.)))
                    .child(
                        div()
                            .mt_4()
                            .text_size(px(34.))
                            .font_weight(FontWeight::BOLD)
                            .child("Forma"),
                    )
                    .child(
                        div()
                            .text_color(secondary_text)
                            .child(concat!("Version ", env!("CARGO_PKG_VERSION"))),
                    )
                    .child(
                        h_flex()
                            .id("open-project")
                            .mt_8()
                            .w_full()
                            .h(px(44.))
                            .px_4()
                            .gap_3()
                            .rounded(px(22.))
                            .bg(cx.theme().muted)
                            .cursor_pointer()
                            .font_weight(FontWeight::SEMIBOLD)
                            .hover(|button| button.bg(cx.theme().list_active))
                            .child(icon(IconName::FolderOpen, secondary_text).size(px(18.)))
                            .child("Open Existing Project…")
                            .on_click(cx.listener(|welcome, _, window, cx| {
                                welcome.choose_directory(window, cx);
                            })),
                    )
                    .children(self.failure.iter().map(|message| {
                        div()
                            .mt_2()
                            .text_xs()
                            .text_color(cx.theme().danger)
                            .child(message.clone())
                    })),
            )
            .child(self.render_recent_projects(cx))
    }
}
