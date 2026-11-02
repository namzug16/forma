//! Read-only viewer for a Forma project. Agents edit the files; this only looks at them.

mod frame_render;
mod frame_tree;
mod project_watch;
mod recent_projects;
mod style;
mod viewer;
mod viewport;
mod welcome;

use forma::project::Project;
use gpui_kit::assets::AllAssets;
use gpui_kit::*;
use viewer::Viewer;
use welcome::Welcome;

actions!(forma_desktop, [Quit, FitFrame]);

fn main() {
    gpui_kit::application().with_assets(AllAssets).run(|cx| {
        gpui_kit::init(cx);
        style::apply_dark_theme(cx);
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-0", FitFrame, None),
        ]);
        cx.on_action(|_: &Quit, cx| cx.quit());
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
        gpui_kit::open_window(welcome_window_options(cx), cx, |_, cx| cx.new(Welcome::new))
            .expect("the welcome window opens");
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
    cx.on_action(move |_: &FitFrame, cx| viewer.update(cx, Viewer::fit_frame));
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
