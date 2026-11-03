//! A folder of frames shown side by side, so related screens can be compared at once.

use std::path::{Path, PathBuf};

use forma::frames::FrameFile;
use gpui_kit::{Point, Size, point, size};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Arrangement {
    Horizontal,
    Vertical,
    Grid,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GroupLayout {
    pub arrangement: Arrangement,
    /// Gap between frames, in design units.
    pub spacing: f32,
    /// `None` keeps the grid about square however many frames there are.
    pub columns: Option<usize>,
}

impl Default for GroupLayout {
    fn default() -> Self {
        Self {
            arrangement: Arrangement::Horizontal,
            spacing: 64.,
            columns: None,
        }
    }
}

impl GroupLayout {
    pub fn columns_for(&self, frame_count: usize) -> usize {
        self.columns
            .unwrap_or_else(|| (frame_count as f32).sqrt().ceil() as usize)
            .max(1)
    }
}

/// Only the folder's own frames; its subfolders are groups of their own.
pub fn frames_in(frames: &[FrameFile], folder: &Path) -> Vec<PathBuf> {
    frames
        .iter()
        .filter_map(|file| file.relative_path.clone())
        .filter(|relative_path| relative_path.parent() == Some(folder))
        .collect()
}

/// The top-left of each frame in design units, and the size of the whole arrangement. Grid cells
/// take the widest frame of their column and the tallest of their row, with frames at the top left.
pub fn arrange(sizes: &[Size<f32>], layout: &GroupLayout) -> (Vec<Point<f32>>, Size<f32>) {
    let columns = match layout.arrangement {
        Arrangement::Horizontal => sizes.len().max(1),
        Arrangement::Vertical => 1,
        Arrangement::Grid => layout.columns_for(sizes.len()),
    };
    let rows = sizes.len().div_ceil(columns);
    let mut column_widths = vec![0f32; columns];
    let mut row_heights = vec![0f32; rows];
    for (index, frame) in sizes.iter().enumerate() {
        column_widths[index % columns] = column_widths[index % columns].max(frame.width);
        row_heights[index / columns] = row_heights[index / columns].max(frame.height);
    }
    let starts = |lengths: &[f32]| {
        lengths
            .iter()
            .scan(0., |start, length| {
                let this_start = *start;
                *start += length + layout.spacing;
                Some(this_start)
            })
            .collect::<Vec<f32>>()
    };
    let (column_starts, row_starts) = (starts(&column_widths), starts(&row_heights));
    let origins = (0..sizes.len())
        .map(|index| point(column_starts[index % columns], row_starts[index / columns]))
        .collect();
    let total = |lengths: &[f32]| {
        lengths.iter().sum::<f32>() + layout.spacing * lengths.len().saturating_sub(1) as f32
    };
    (origins, size(total(&column_widths), total(&row_heights)))
}

#[cfg(test)]
mod tests {
    use super::{Arrangement, GroupLayout, arrange, frames_in};
    use forma::frames::FrameFile;
    use gpui_kit::{point, size};

    fn layout(arrangement: Arrangement) -> GroupLayout {
        GroupLayout {
            arrangement,
            spacing: 10.,
            columns: None,
        }
    }

    #[test]
    fn horizontal_frames_line_up_left_to_right() {
        let sizes = [size(100., 50.), size(30., 80.)];

        let (origins, total) = arrange(&sizes, &layout(Arrangement::Horizontal));

        assert_eq!(origins, [point(0., 0.), point(110., 0.)]);
        assert_eq!(total, size(140., 80.));
    }

    #[test]
    fn vertical_frames_stack_top_to_bottom() {
        let sizes = [size(100., 50.), size(30., 80.)];

        let (origins, total) = arrange(&sizes, &layout(Arrangement::Vertical));

        assert_eq!(origins, [point(0., 0.), point(0., 60.)]);
        assert_eq!(total, size(100., 140.));
    }

    #[test]
    fn grid_cells_fit_the_largest_frame_of_their_row_and_column() {
        let sizes = [size(100., 50.), size(30., 80.), size(60., 20.)];

        let (origins, total) = arrange(&sizes, &layout(Arrangement::Grid));

        assert_eq!(origins, [point(0., 0.), point(110., 0.), point(0., 90.)]);
        assert_eq!(total, size(140., 110.));
    }

    #[test]
    fn grids_stay_about_square_unless_told_otherwise() {
        let mut layout = layout(Arrangement::Grid);
        assert_eq!(layout.columns_for(5), 3);
        assert_eq!(layout.columns_for(0), 1);

        layout.columns = Some(2);
        assert_eq!(layout.columns_for(5), 2);
    }

    #[test]
    fn a_group_holds_only_its_own_frames() {
        let frame = |relative_path: &str| FrameFile {
            path: relative_path.into(),
            relative_path: Some(relative_path.into()),
        };
        let frames = [
            frame("top.json"),
            frame("ads/a.json"),
            frame("ads/wide/b.json"),
        ];

        assert_eq!(
            frames_in(&frames, "ads".as_ref()),
            [std::path::PathBuf::from("ads/a.json")]
        );
    }
}
