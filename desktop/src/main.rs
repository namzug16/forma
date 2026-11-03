//! Read-only viewer for a Forma project. Agents edit the files; this only looks at them.

mod asset_preview;
mod frame_render;
mod frame_tree;
mod layer_details;
mod project_pages;
mod project_watch;
mod recent_projects;
mod render_cache;
mod style;
mod token_sheet;
mod viewer;
mod viewport;
mod welcome;

use forma::project::Project;
use gpui_kit::assets::AllAssets;
use gpui_kit::*;
use viewer::Viewer;
use welcome::Welcome;

actions!(forma_desktop, [Quit, FitFrame]);

/// Weak, so going back to the welcome window really does close the project and its watcher.
struct OpenViewer(WeakEntity<Viewer>);

impl Global for OpenViewer {}

fn main() {
    gpui_kit::application().with_assets(AllAssets).run(|cx| {
        gpui_kit::init(cx);
        style::apply_dark_theme(cx);
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-0", FitFrame, None),
        ]);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_action(|_: &FitFrame, cx| {
            let viewer = cx.try_global::<OpenViewer>();
            if let Some(viewer) = viewer.and_then(|viewer| viewer.0.upgrade()) {
                viewer.update(cx, Viewer::fit_frame);
            }
        });
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        cx.set_menus(vec![
            Menu {
                name: "Forma".into(),
                items: vec![MenuItem::action("Quit Forma", Quit)],
                disabled: false,
            },
            Menu {
                name: "View".into(),
                items: vec![MenuItem::action("Fit Frame", FitFrame)],
                disabled: false,
            },
        ]);
        open_welcome_window(cx);
        cx.activate(true);
    });
}

/// Replaces the welcome window, so one run of the app shows one project.
pub fn open_project_window(project: Project, cx: &mut App) {
    let (_, viewer) = gpui_kit::open_window(
        WindowOptions {
            titlebar: Some(hidden_titlebar()),
            ..Default::default()
        },
        cx,
        |_, cx| cx.new(|cx| Viewer::new(project, cx)),
    )
    .expect("the project window opens");
    cx.set_global(OpenViewer(viewer.downgrade()));
}

pub fn open_welcome_window(cx: &mut App) {
    gpui_kit::open_window(welcome_window_options(cx), cx, |_, cx| cx.new(Welcome::new))
        .expect("the welcome window opens");
}

fn welcome_window_options(cx: &App) -> WindowOptions {
    WindowOptions {
        titlebar: Some(hidden_titlebar()),
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(800.), px(460.)),
            cx,
        ))),
        is_resizable: false,
        ..Default::default()
    }
}

/// Panels run up to the window's edge, with the traffic lights floating over them.
fn hidden_titlebar() -> TitlebarOptions {
    TitlebarOptions {
        title: Some("Forma".into()),
        appears_transparent: true,
        traffic_light_position: Some(point(px(14.), px(14.))),
    }
}
