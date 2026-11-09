//! Measured top-level markdown blocks. Unchanged blocks retain their height;
//! only blocks intersecting the viewport need layout and prepaint each frame.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use gpui::{
    AnyElement, App, AvailableSpace, Bounds, Element, ElementId, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, Pixels, ScrollHandle, Size, Style, Window, relative,
    size,
};
use markup::Block;

#[derive(Default)]
pub struct BlockCache {
    blocks: Vec<Block>,
    heights: Vec<Rc<Cell<Option<Pixels>>>>,
    width: Option<Pixels>,
    /// Scroll offsets must outlive offscreen elements.
    pub scrolls: Rc<RefCell<BTreeMap<u64, ScrollHandle>>>,
}

impl BlockCache {
    pub fn clear(&mut self) {
        self.blocks.clear();
        self.heights.clear();
        self.width = None;
    }

    pub fn prepare(&mut self, blocks: &[Block], width: Pixels) {
        if self.width != Some(width) {
            self.heights = (0..blocks.len()).map(|_| Rc::default()).collect();
        } else if self.blocks != blocks {
            let prefix = self
                .blocks
                .iter()
                .zip(blocks)
                .take_while(|(a, b)| a == b)
                .count();
            let suffix = self.blocks[prefix..]
                .iter()
                .rev()
                .zip(blocks[prefix..].iter().rev())
                .take_while(|(a, b)| a == b)
                .count();
            let mut heights: Vec<Rc<Cell<Option<Pixels>>>> =
                (0..blocks.len()).map(|_| Rc::default()).collect();
            heights[..prefix].clone_from_slice(&self.heights[..prefix]);
            // The first surviving suffix block can have a different preceding
            // margin, so measure it again even when its own content is unchanged.
            if suffix > 1 {
                heights[blocks.len() - suffix + 1..]
                    .clone_from_slice(&self.heights[self.blocks.len() - suffix + 1..]);
            }
            self.heights = heights;
        }
        if self.blocks != blocks {
            self.blocks = blocks.to_vec();
        }
        self.width = Some(width);
    }

    pub fn wrap(&self, index: usize, child: AnyElement) -> AnyElement {
        MeasuredBlock {
            child,
            width: self.width.expect("prepare blocks before measuring"),
            height: self.heights[index].clone(),
        }
        .into_any_element()
    }
}

struct MeasuredBlock {
    child: AnyElement,
    width: Pixels,
    height: Rc<Cell<Option<Pixels>>>,
}

impl IntoElement for MeasuredBlock {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for MeasuredBlock {
    type RequestLayoutState = ();
    type PrepaintState = bool;

    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let height = self.height.get().unwrap_or_else(|| {
            let height = self
                .child
                .layout_as_root(
                    size(
                        AvailableSpace::Definite(self.width),
                        AvailableSpace::MinContent,
                    ),
                    window,
                    cx,
                )
                .height;
            self.height.set(Some(height));
            height
        });
        let mut style = Style::default();
        style.size = size(relative(1.).into(), height.into());
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> bool {
        let visible = bounds.intersects(&window.content_mask().bounds);
        if visible {
            self.child
                .layout_as_root(Size::from(bounds.size), window, cx);
            self.child.prepaint_at(bounds.origin, window, cx);
        }
        visible
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        visible: &mut bool,
        window: &mut Window,
        cx: &mut App,
    ) {
        if *visible {
            self.child.paint(window, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, px};

    fn blocks(names: &[&str]) -> Vec<Block> {
        names
            .iter()
            .map(|text| Block::Html((*text).to_owned()))
            .collect()
    }

    fn measured(names: &[&str]) -> BlockCache {
        let mut cache = BlockCache::default();
        cache.prepare(&blocks(names), px(400.));
        for (i, height) in cache.heights.iter().enumerate() {
            height.set(Some(px((i + 1) as f32 * 10.)));
        }
        cache
    }

    fn heights(cache: &BlockCache) -> Vec<Option<Pixels>> {
        cache.heights.iter().map(|height| height.get()).collect()
    }

    #[test]
    fn retains_unchanged_heights_and_remeasures_changed_neighbor() {
        let mut cache = measured(&["a", "b", "c", "d"]);
        cache.prepare(&blocks(&["a", "changed", "c", "d"]), px(400.));
        assert_eq!(
            heights(&cache),
            vec![Some(px(10.)), None, None, Some(px(40.))]
        );
    }

    #[test]
    fn preserves_suffix_heights_across_insertions_and_deletions() {
        let mut cache = measured(&["a", "b", "c", "d"]);
        cache.prepare(&blocks(&["a", "new", "b", "c", "d"]), px(400.));
        assert_eq!(
            heights(&cache),
            vec![Some(px(10.)), None, None, Some(px(30.)), Some(px(40.))]
        );
        cache.prepare(&blocks(&["c", "d"]), px(400.));
        assert_eq!(heights(&cache), vec![None, Some(px(40.))]);
        cache.prepare(&[], px(400.));
        assert!(cache.heights.is_empty());
    }

    #[test]
    fn appending_preserves_all_existing_heights() {
        let mut cache = measured(&["a", "b"]);
        cache.prepare(&blocks(&["a", "b", "new"]), px(400.));
        assert_eq!(heights(&cache), vec![Some(px(10.)), Some(px(20.)), None]);
        cache.prepare(&blocks(&["a", "b"]), px(400.));
        assert_eq!(heights(&cache), vec![Some(px(10.)), Some(px(20.))]);
    }

    #[test]
    fn width_and_appearance_invalidate_measurements_but_keep_scroll_offsets() {
        let mut cache = measured(&["a", "b"]);
        let scroll = ScrollHandle::new();
        scroll.set_offset(point(px(-30.), px(0.)));
        cache.scrolls.borrow_mut().insert(1, scroll);
        cache.prepare(&blocks(&["a", "b"]), px(500.));
        assert_eq!(heights(&cache), vec![None, None]);
        cache.heights[0].set(Some(px(40.)));
        cache.clear();
        cache.prepare(&blocks(&["a", "b"]), px(500.));
        assert_eq!(heights(&cache), vec![None, None]);
        assert_eq!(cache.scrolls.borrow()[&1].offset().x, px(-30.));
    }

    #[test]
    fn identical_documents_keep_the_same_measurement_cells() {
        let mut cache = measured(&["a", "b"]);
        let first = cache.heights[0].clone();
        cache.prepare(&blocks(&["a", "b"]), px(400.));
        assert!(Rc::ptr_eq(&first, &cache.heights[0]));
    }
}
