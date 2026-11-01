use std::path::{Path, PathBuf};
use std::sync::Arc;

use forma::diagnostics::Failure;
use forma::project::Project;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::*;

pub struct Viewer {
    project: Option<Arc<Project>>,
    /// Why the project cannot be shown; agents fix these, the viewer only reports them.
    project_failure: Vec<String>,
}

impl Viewer {
    pub fn new(_: &mut Context<Self>) -> Self {
        Self {
            project: None,
            project_failure: Vec::new(),
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
            }
            Err(error) => self.project_failure = failure_messages(&error),
        }
        cx.notify();
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
            Some(project) => PathBuf::from(&project.directory)
                .display()
                .to_string()
                .into_any_element(),
        };
        div()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(content)
    }
}
