//! The canvas transform: where a frame sits and how large it is drawn.

use gpui_kit::{Point, Size, point};

const MIN_ZOOM: f32 = 0.02;
const MAX_ZOOM: f32 = 32.0;
/// Share of the panel a fitted frame fills, leaving a margin around it.
const FIT_MARGIN: f32 = 0.9;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    pub zoom: f32,
    /// Top-left of the frame, relative to the canvas.
    pub offset: Point<f32>,
}

impl Viewport {
    /// Centres the frame and shrinks it to fit, but never enlarges it past 100%.
    pub fn fitting(frame: Size<f32>, canvas: Size<f32>) -> Self {
        let zoom = (canvas.width * FIT_MARGIN / frame.width)
            .min(canvas.height * FIT_MARGIN / frame.height)
            .clamp(MIN_ZOOM, 1.0);
        let offset = point(
            (canvas.width - frame.width * zoom) / 2.0,
            (canvas.height - frame.height * zoom) / 2.0,
        );
        Self { zoom, offset }
    }

    pub fn pan(&mut self, delta: Point<f32>) {
        self.offset = point(self.offset.x + delta.x, self.offset.y + delta.y);
    }

    /// Keeps the point of the frame under `cursor` in place.
    pub fn zoom_at(&mut self, factor: f32, cursor: Point<f32>) {
        let zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        let applied_factor = zoom / self.zoom;
        self.offset = point(
            cursor.x - (cursor.x - self.offset.x) * applied_factor,
            cursor.y - (cursor.y - self.offset.y) * applied_factor,
        );
        self.zoom = zoom;
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::size;

    use super::*;

    #[test]
    fn a_large_frame_shrinks_and_centres() {
        let viewport = Viewport::fitting(size(1000.0, 1000.0), size(500.0, 1000.0));

        assert_eq!(viewport.zoom, 0.45);
        assert_eq!(viewport.offset, point(25.0, 275.0));
    }

    #[test]
    fn a_small_frame_stays_at_full_size() {
        let viewport = Viewport::fitting(size(100.0, 100.0), size(500.0, 500.0));

        assert_eq!(viewport.zoom, 1.0);
        assert_eq!(viewport.offset, point(200.0, 200.0));
    }

    #[test]
    fn zooming_keeps_the_point_under_the_cursor() {
        let mut viewport = Viewport {
            zoom: 1.0,
            offset: point(10.0, 20.0),
        };
        let cursor = point(110.0, 70.0);

        viewport.zoom_at(2.0, cursor);

        assert_eq!(viewport.zoom, 2.0);
        assert_eq!(viewport.offset, point(-90.0, -30.0));
    }

    #[test]
    fn zoom_stops_at_its_limit_without_drifting() {
        let mut viewport = Viewport {
            zoom: MAX_ZOOM,
            offset: point(5.0, 5.0),
        };

        viewport.zoom_at(2.0, point(100.0, 100.0));

        assert_eq!(viewport.zoom, MAX_ZOOM);
        assert_eq!(viewport.offset, point(5.0, 5.0));
    }
}
