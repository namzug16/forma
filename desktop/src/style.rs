//! The viewer's look: a dark neutral palette after Figma's editor.

use gpui_kit::component::{Theme, ThemeMode, ThemeTokens};
use gpui_kit::{App, Rgba, px, rgb, rgba};

const PANEL: u32 = 0x2c2c2c;
const CONTROL: u32 = 0x383838;
const SELECTED: u32 = 0x3e3e3e;
const DIVIDER: u32 = 0x444444;
const ACCENT: u32 = 0x0d99ff;
const ACCENT_HOVER: u32 = 0x007be5;
const DANGER: u32 = 0xf24822;
const WARNING: u32 = 0xffc21a;
const TEXT_PRIMARY: u32 = 0xffffffe6;
const TEXT_SECONDARY: u32 = 0xffffffb3;

/// Darker than the panels so frames stand out, as on Figma's default page.
pub fn canvas_background() -> Rgba {
    rgb(0x1e1e1e)
}

/// Vivid enough to stay visible on most designs.
pub fn layer_highlight() -> Rgba {
    rgb(0xff2d9b)
}

/// The app is always dark, whatever the system appearance is.
pub fn apply_dark_theme(cx: &mut App) {
    Theme::change(ThemeMode::Dark, None, cx);
    let theme = Theme::global_mut(cx);
    let colors = &mut theme.colors;
    for panel_color in [
        &mut colors.background,
        &mut colors.sidebar,
        &mut colors.list,
        &mut colors.list_even,
    ] {
        *panel_color = rgb(PANEL).into();
    }
    for text_color in [
        &mut colors.foreground,
        &mut colors.sidebar_foreground,
        &mut colors.accent_foreground,
    ] {
        *text_color = rgba(TEXT_PRIMARY).into();
    }
    colors.muted = rgb(CONTROL).into();
    colors.muted_foreground = rgba(TEXT_SECONDARY).into();
    colors.accent = rgb(CONTROL).into();
    colors.border = rgb(DIVIDER).into();
    colors.sidebar_border = rgb(DIVIDER).into();
    colors.list_hover = rgb(CONTROL).into();
    colors.list_active = rgb(SELECTED).into();
    colors.list_active_border = rgba(0x00000000).into();
    colors.button_primary = rgb(ACCENT).into();
    colors.button_primary_hover = rgb(ACCENT_HOVER).into();
    colors.button_primary_active = rgb(ACCENT_HOVER).into();
    colors.button_primary_foreground = rgb(0xffffff).into();
    colors.danger = rgb(DANGER).into();
    colors.warning = rgb(WARNING).into();
    theme.tokens = ThemeTokens::from(*colors);
    theme.font_size = px(13.);
    theme.radius = px(5.);
}
