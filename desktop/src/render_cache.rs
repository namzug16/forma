//! Keeps the last render of recently shown frames, so going back to one shows it at once.

use std::sync::Arc;

use crate::frame_render::RenderedFrame;
use crate::frame_tree::Selection;

/// Each entry holds a display-sized image in GPUI's atlas, so this bounds that memory.
const CAPACITY: usize = 24;

pub struct CachedRender {
    pub frame: Arc<RenderedFrame>,
    /// Its file changed since it was rendered; still worth showing until the new render lands.
    pub is_stale: bool,
}

#[derive(Default)]
pub struct RenderCache {
    /// Least recently used first.
    entries: Vec<(Selection, CachedRender)>,
    /// The render running now, and whether its file changed after it was read.
    in_flight: Option<(Selection, bool)>,
}

impl RenderCache {
    /// Marks the entry as just used.
    pub fn get(&mut self, key: &Selection) -> Option<&CachedRender> {
        let index = self
            .entries
            .iter()
            .position(|(entry_key, _)| entry_key == key)?;
        let entry = self.entries.remove(index);
        self.entries.push(entry);
        self.entries.last().map(|(_, cached)| cached)
    }

    pub fn start_render(&mut self, key: &Selection) {
        self.in_flight = Some((key.clone(), false));
    }

    /// Returns the frames that left the cache, whose images the caller must free.
    pub fn insert(
        &mut self,
        key: Selection,
        frame: Arc<RenderedFrame>,
        is_pinned: impl Fn(&Selection) -> bool,
    ) -> Vec<Arc<RenderedFrame>> {
        let is_stale = match self.in_flight.take() {
            Some((in_flight_key, changed_while_rendering)) if in_flight_key == key => {
                changed_while_rendering
            }
            other => {
                self.in_flight = other;
                false
            }
        };
        let mut evicted = self.remove(|entry_key| entry_key == &key);
        self.entries.push((key, CachedRender { frame, is_stale }));
        while self.entries.len() > CAPACITY {
            let Some(index) = self.entries.iter().position(|(key, _)| !is_pinned(key)) else {
                break;
            };
            evicted.push(self.entries.remove(index).1.frame);
        }
        evicted
    }

    pub fn mark_stale(&mut self, is_affected: impl Fn(&Selection) -> bool) {
        for (key, cached) in &mut self.entries {
            cached.is_stale |= is_affected(key);
        }
        if let Some((key, changed_while_rendering)) = &mut self.in_flight {
            *changed_while_rendering |= is_affected(key);
        }
    }

    /// Returns the removed frames, whose images the caller must free.
    pub fn remove(&mut self, is_removed: impl Fn(&Selection) -> bool) -> Vec<Arc<RenderedFrame>> {
        let (removed, kept) = std::mem::take(&mut self.entries)
            .into_iter()
            .partition(|(key, _)| is_removed(key));
        self.entries = kept;
        removed
            .into_iter()
            .map(|(_, cached)| cached.frame)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;
    use tiny_skia::Pixmap;

    use super::*;
    use crate::frame_render::to_render_image;

    fn frame() -> Arc<RenderedFrame> {
        Arc::new(RenderedFrame {
            image: to_render_image(&Pixmap::new(1, 1).unwrap()),
            width: 1.,
            height: 1.,
            report: Value::Null,
            resolution: None,
        })
    }

    fn key(index: usize) -> Selection {
        Selection::Frame(format!("{index}.json").into())
    }

    #[test]
    fn the_least_recently_used_unpinned_frame_is_evicted() {
        let mut cache = RenderCache::default();
        for index in 0..CAPACITY {
            cache.insert(key(index), frame(), |_| false);
        }
        cache.get(&key(0));

        let evicted = cache.insert(key(CAPACITY), frame(), |key| key == &self::key(1));

        assert_eq!(evicted.len(), 1);
        assert!(cache.get(&key(0)).is_some());
        assert!(cache.get(&key(1)).is_some());
        assert!(cache.get(&key(2)).is_none());
    }

    #[test]
    fn replacing_a_frame_frees_the_old_one() {
        let mut cache = RenderCache::default();
        let old = frame();
        cache.insert(key(0), old.clone(), |_| false);

        let evicted = cache.insert(key(0), frame(), |_| false);

        assert!(Arc::ptr_eq(&evicted[0], &old));
    }

    #[test]
    fn a_change_during_its_render_leaves_the_result_stale() {
        let mut cache = RenderCache::default();
        cache.start_render(&key(0));
        cache.mark_stale(|key| key == &self::key(0));
        cache.insert(key(0), frame(), |_| false);
        cache.start_render(&key(1));
        cache.insert(key(1), frame(), |_| false);

        assert!(cache.get(&key(0)).unwrap().is_stale);
        assert!(!cache.get(&key(1)).unwrap().is_stale);
    }
}
