//! Read-only viewer for a Forma project. Agents edit the files; this only looks at them.

mod frame_tree;
mod viewer;

use gpui_kit::*;
use viewer::Viewer;

actions!(forma_desktop, [Quit]);

fn main() {
    gpui_kit::application().run(|cx| {
        gpui_kit::init(cx);
        cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        cx.set_menus(vec![Menu {
            name: "Forma".into(),
            items: vec![MenuItem::action("Quit Forma", Quit)],
            disabled: false,
        }]);
        gpui_kit::open_window(WindowOptions::default(), cx, |_, cx| cx.new(Viewer::new))
            .expect("the main window opens");
        cx.activate(true);
    });
}
