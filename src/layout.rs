//! Rows and columns as taffy flexbox styles. The scene builder resolves tokens, measures content
//! and reads the results back.

use taffy::style::{
    AlignItems, Dimension, Display, FlexDirection, JustifyContent, LengthPercentage,
    LengthPercentageAuto, Style,
};

use crate::document::{CrossAlign, Direction, MainAlign};

/// A layout group's settings with tokens resolved.
#[derive(Debug, Clone, Copy)]
pub struct ResolvedLayout {
    pub direction: Direction,
    pub gap: f64,
    pub padding: Sides,
    pub align: MainAlign,
    pub cross_align: CrossAlign,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Sides {
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    pub left: f64,
}

/// One axis of a node's size as authored, with tokens resolved.
#[derive(Debug, Clone, Copy)]
pub enum AxisSize {
    Length(f64),
    Hug,
    Fill,
    /// `50.0` for `"50%"`.
    Percent(f64),
}

/// How a node sits in its parent's flow; `parent_direction` is `None` for the outermost group.
pub fn item_style(
    width: AxisSize,
    height: AxisSize,
    width_limits: (f64, f64),
    height_limits: (f64, f64),
    parent_direction: Option<Direction>,
) -> Style {
    let mut style = Style {
        // A zero minimum stops fill children from refusing to shrink below their content.
        min_size: taffy::Size {
            width: LengthPercentageAuto::length(width_limits.0 as f32),
            height: LengthPercentageAuto::length(height_limits.0 as f32),
        },
        max_size: taffy::Size {
            width: maximum(width_limits.1),
            height: maximum(height_limits.1),
        },
        // Children keep their size and overflow, as in Figma, rather than squashing each other.
        flex_shrink: 0.0,
        ..Style::default()
    };
    let main_is_width = matches!(parent_direction, Some(Direction::Row));
    for (size, is_width) in [(width, true), (height, false)] {
        let is_main = parent_direction.is_some() && is_width == main_is_width;
        let dimension = match size {
            AxisSize::Length(length) => Dimension::length(length as f32),
            AxisSize::Percent(percent) => Dimension::percent((percent / 100.0) as f32),
            AxisSize::Hug => Dimension::auto(),
            AxisSize::Fill if is_main => {
                style.flex_grow = 1.0;
                style.flex_shrink = 1.0;
                style.flex_basis = Dimension::length(0.0);
                Dimension::auto()
            }
            AxisSize::Fill => {
                style.align_self = Some(AlignItems::STRETCH);
                Dimension::auto()
            }
        };
        if is_width {
            style.size.width = dimension;
        } else {
            style.size.height = dimension;
        }
    }
    style
}

/// Makes `item` lay out its own children.
pub fn container_style(item: Style, layout: &ResolvedLayout) -> Style {
    let length = |value: f64| LengthPercentage::length(value as f32);
    let padding = layout.padding;
    Style {
        display: Display::Flex,
        flex_direction: match layout.direction {
            Direction::Row => FlexDirection::Row,
            Direction::Column => FlexDirection::Column,
        },
        gap: taffy::Size {
            width: length(layout.gap),
            height: length(layout.gap),
        },
        padding: taffy::Rect {
            left: length(padding.left),
            right: length(padding.right),
            top: length(padding.top),
            bottom: length(padding.bottom),
        },
        justify_content: match layout.align {
            MainAlign::Start => JustifyContent::START,
            MainAlign::Center => JustifyContent::CENTER,
            MainAlign::End => JustifyContent::END,
            MainAlign::SpaceBetween => JustifyContent::SPACE_BETWEEN,
        },
        align_items: match layout.cross_align {
            CrossAlign::Start => AlignItems::START,
            CrossAlign::Center => AlignItems::CENTER,
            CrossAlign::End => AlignItems::END,
            CrossAlign::Stretch => AlignItems::STRETCH,
        },
        ..item
    }
}

fn maximum(limit: f64) -> LengthPercentageAuto {
    if limit.is_finite() {
        LengthPercentageAuto::length(limit as f32)
    } else {
        LengthPercentageAuto::auto()
    }
}

/// The nearest `f32` not below `value`, so content measured in `f64` still fits the box taffy
/// hands back; rounding down would make text wrap its last word.
pub fn at_least(value: f64) -> f32 {
    let rounded = value as f32;
    if f64::from(rounded) < value {
        rounded.next_up()
    } else {
        rounded
    }
}
