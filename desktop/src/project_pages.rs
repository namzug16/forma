//! The pages that replace the canvas for the project's tokens and config.

use forma::project::Project;
use gpui_kit::assets::IconName;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::style;
use crate::token_sheet::{Token, TokenKind, TokenPreview, TokenSheet};
use crate::viewer::icon;

const COLOR_CARD_WIDTH: f32 = 156.;
const COLOR_TILE_HEIGHT: f32 = 88.;
const GRADIENT_CARD_WIDTH: f32 = 260.;
const GRADIENT_TILE_HEIGHT: f32 = 104.;
const TYPE_PREVIEW_MAX_HEIGHT: f32 = 64.;
const NUMBER_BAR_MAX_WIDTH: f32 = 320.;
const CARD_RADIUS: f32 = 8.;

/// A scrolling page on the canvas background, its content in a centered column.
fn page(
    id: &'static str,
    title: &str,
    subtitle: String,
    sections: impl IntoIterator<Item = AnyElement>,
    cx: &App,
) -> impl IntoElement {
    div()
        .id(id)
        .size_full()
        .bg(style::canvas_background())
        .overflow_y_scrollbar()
        .child(
            div()
                .w_full()
                .max_w(px(960.))
                .mx_auto()
                .p_8()
                .flex()
                .flex_col()
                .gap_8()
                .text_color(cx.theme().foreground)
                .child(
                    div()
                        .child(
                            div()
                                .text_size(px(22.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(title.to_owned()),
                        )
                        .child(
                            div()
                                .text_color(cx.theme().muted_foreground)
                                .child(subtitle),
                        ),
                )
                .children(sections),
        )
}

fn page_section(title: &str) -> Div {
    div().flex().flex_col().gap_3().child(
        div()
            .font_weight(FontWeight::SEMIBOLD)
            .child(title.to_owned()),
    )
}

fn card(cx: &App) -> Div {
    div()
        .rounded(px(CARD_RADIUS))
        .overflow_hidden()
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().background)
}

fn secondary_label(text: String, cx: &App) -> Div {
    div()
        .truncate()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text)
}

fn card_caption(token: &Token, detail: Option<String>, cx: &App) -> Div {
    div()
        .p_2()
        .child(
            div()
                .truncate()
                .font_weight(FontWeight::MEDIUM)
                .child(token.name.clone()),
        )
        .children(detail.map(|detail| secondary_label(detail, cx)))
        .child(secondary_label(token.reference.clone(), cx))
}

fn render_gradient_tile(preview: &Result<TokenPreview, String>, cx: &App) -> AnyElement {
    match preview {
        Ok(preview) => img(preview.image.clone())
            .w(px(GRADIENT_CARD_WIDTH))
            .h(px(GRADIENT_TILE_HEIGHT))
            .rounded_t(px(CARD_RADIUS - 1.))
            .into_any_element(),
        Err(message) => render_preview_failure(message, cx).into_any_element(),
    }
}

fn render_preview_failure(message: &str, cx: &App) -> impl IntoElement {
    h_flex()
        .p_2()
        .gap_2()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(icon(IconName::TriangleAlert, cx.theme().warning))
        .child(div().min_w_0().child(message.to_owned()))
}

fn render_token(token: &Token, cx: &App) -> AnyElement {
    match &token.kind {
        TokenKind::Color { color, text } => card(cx)
            .w(px(COLOR_CARD_WIDTH))
            .child(
                div()
                    .h(px(COLOR_TILE_HEIGHT))
                    .rounded_t(px(CARD_RADIUS - 1.))
                    .bg(*color),
            )
            .child(card_caption(token, Some(text.clone()), cx))
            .into_any_element(),
        TokenKind::Gradient(preview) => card(cx)
            .w(px(GRADIENT_CARD_WIDTH))
            .child(render_gradient_tile(preview, cx))
            .child(card_caption(token, None, cx))
            .into_any_element(),
        TokenKind::Typography { preview, summary } => card(cx)
            .w_full()
            .child(
                div()
                    .h(px(TYPE_PREVIEW_MAX_HEIGHT + 24.))
                    .px_3()
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .child(match preview {
                        Ok(preview) => {
                            let scale = (TYPE_PREVIEW_MAX_HEIGHT / preview.height).min(1.);
                            img(preview.image.clone())
                                .flex_none()
                                .w(px(preview.width * scale))
                                .h(px(preview.height * scale))
                                .into_any_element()
                        }
                        Err(message) => render_preview_failure(message, cx).into_any_element(),
                    }),
            )
            .child(card_caption(token, Some(summary.clone()), cx))
            .into_any_element(),
        TokenKind::Number(value) => h_flex()
            .w_full()
            .h(px(32.))
            .gap_3()
            .child(
                div().w(px(160.)).flex_none().child(
                    div()
                        .truncate()
                        .font_weight(FontWeight::MEDIUM)
                        .child(token.name.clone()),
                ),
            )
            .child(
                div()
                    .flex_none()
                    .w(px((*value as f32).clamp(2., NUMBER_BAR_MAX_WIDTH)))
                    .h(px(10.))
                    .rounded(px(2.))
                    .bg(cx.theme().button_primary),
            )
            .child(secondary_label(value.to_string(), cx))
            .child(div().flex_1())
            .child(secondary_label(token.reference.clone(), cx))
            .into_any_element(),
        TokenKind::Other(text) => h_flex()
            .w_full()
            .h(px(32.))
            .gap_3()
            .child(
                div()
                    .w(px(160.))
                    .flex_none()
                    .truncate()
                    .font_weight(FontWeight::MEDIUM)
                    .child(token.name.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(secondary_label(text.clone(), cx)),
            )
            .child(secondary_label(token.reference.clone(), cx))
            .into_any_element(),
    }
}

pub fn render_tokens(sheet: &TokenSheet, cx: &App) -> impl IntoElement {
    let count = sheet.token_count();
    let subtitle = match count {
        1 => "1 design token".to_owned(),
        count => format!("{count} design tokens"),
    };
    let sections = sheet.groups.iter().map(|group| {
        page_section(&group.title)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_3()
                    .children(group.tokens.iter().map(|token| render_token(token, cx))),
            )
            .into_any_element()
    });
    page("tokens-page", "Tokens", subtitle, sections, cx)
}

fn render_stat(name: IconName, value: String, label: &str, cx: &App) -> impl IntoElement {
    card(cx)
        .flex_1()
        .min_w_0()
        .p_4()
        .flex()
        .flex_col()
        .gap_1()
        .child(icon(name, cx.theme().muted_foreground).size(px(18.)))
        .child(
            div()
                .mt_2()
                .truncate()
                .text_size(px(22.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(value),
        )
        .child(secondary_label(label.to_owned(), cx))
}

fn render_setting_rows<'a>(
    rows: impl IntoIterator<Item = (IconName, &'a str, String)>,
    cx: &App,
) -> impl IntoElement {
    let border = cx.theme().border;
    card(cx).children(
        rows.into_iter()
            .enumerate()
            .map(|(index, (name, label, value))| {
                h_flex()
                    .h(px(40.))
                    .px_3()
                    .gap_3()
                    .when(index > 0, |row| row.border_t_1().border_color(border))
                    .child(icon(name, cx.theme().muted_foreground))
                    .child(div().flex_none().w(px(120.)).child(label.to_owned()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .font_family(cx.theme().mono_font_family.clone())
                            .child(value),
                    )
            }),
    )
}

pub fn render_config(project: &Project, frame_count: usize, cx: &App) -> impl IntoElement {
    let config = &project.config;
    let render_settings = page_section("Rendering")
        .child(
            h_flex()
                .gap_3()
                .child(render_stat(
                    IconName::FileImage,
                    config.render.format.to_uppercase(),
                    "Format",
                    cx,
                ))
                .child(render_stat(
                    IconName::Scaling,
                    format!("{}×", config.render.scale),
                    "Scale",
                    cx,
                ))
                .child(render_stat(
                    IconName::Gauge,
                    format!("{}%", config.render.quality),
                    "Quality",
                    cx,
                ))
                .child(render_stat(
                    IconName::Frame,
                    frame_count.to_string(),
                    "Frames",
                    cx,
                )),
        )
        .into_any_element();
    let paths = page_section("Paths")
        .child(render_setting_rows(
            [
                (
                    IconName::Frame,
                    "Frames",
                    config.frames.display().to_string(),
                ),
                (
                    IconName::Palette,
                    "Tokens",
                    config.tokens.display().to_string(),
                ),
                (
                    IconName::Image,
                    "Assets",
                    config.assets.display().to_string(),
                ),
                (
                    IconName::FolderOutput,
                    "Output",
                    config.output.display().to_string(),
                ),
            ],
            cx,
        ))
        .into_any_element();
    let details = page_section("Project")
        .child(render_setting_rows(
            [
                (
                    IconName::FileCog,
                    "Config file",
                    project.config_path().display().to_string(),
                ),
                (IconName::Hash, "Format version", config.version.to_string()),
            ],
            cx,
        ))
        .into_any_element();
    page(
        "config-page",
        "Config",
        "Project settings".to_owned(),
        [render_settings, paths, details],
        cx,
    )
}
