//! Read-only viewer for a Forma project. Agents edit the files; this only looks at them.

mod frame_render;
mod frame_tree;
mod project_watch;
mod viewer;
mod viewport;

use gpui_kit::*;
use viewer::Viewer;

actions!(forma_desktop, [Quit, FitFrame]);

fn main() {
    gpui_kit::application().run(|cx| {
        gpui_kit::init(cx);
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-0", FitFrame, None),
        ]);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_window_closed(|cx, _| cx.quit()).detach();
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
        let (_, viewer) =
            gpui_kit::open_window(WindowOptions::default(), cx, |_, cx| cx.new(Viewer::new))
                .expect("the main window opens");
        cx.on_action(move |_: &FitFrame, cx| viewer.update(cx, Viewer::fit_frame));
        cx.activate(true);
    });
}
