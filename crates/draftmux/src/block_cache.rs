//! Measured top-level markdown blocks. After measurement, unchanged offscreen
//! blocks reuse their height and text without building or laying out elements.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use gpui::{
    AnyElement, App, AvailableSpace, Bounds, Element, ElementId, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, Pixels, ScrollHandle, Size, Style, Window, relative,
    size,
};
use markup::Block;

/// Measurements, selection text and scroll state shared by all frames of a block.
pub struct CachedBlock {
    pub block: Block,
    height: Cell<Option<Pixels>>,
    pub texts: crate::selectable::TextRegistry,
    pub scrolls: Rc<RefCell<BTreeMap<u64, ScrollHandle>>>,
}

impl CachedBlock {
    fn new(block: Block) -> Self {
        Self {
            block,
            height: Cell::new(None),
            texts: Default::default(),
            scrolls: Default::default(),
        }
    }
}

#[derive(Default)]
pub struct BlockCache {
    blocks: Vec<Rc<CachedBlock>>,
    width: Option<Pixels>,
}

impl BlockCache {
    /// Invalidates appearance-dependent data without losing horizontal offsets.
    pub fn clear(&mut self) {
        for block in &self.blocks {
            block.height.set(None);
            block.texts.borrow_mut().clear();
        }
        self.width = None;
    }

    pub fn reset(&mut self) {
        self.blocks.clear();
        self.width = None;
    }

    pub fn prepare(&mut self, blocks: &[Block], width: Pixels) {
        let prefix = self
            .blocks
            .iter()
            .zip(blocks)
            .take_while(|(a, b)| a.block == **b)
            .count();
        if prefix != self.blocks.len() || prefix != blocks.len() {
            let suffix = self.blocks[prefix..]
                .iter()
                .rev()
                .zip(blocks[prefix..].iter().rev())
                .take_while(|(a, b)| a.block == **b)
                .count();
            let mut next = self.blocks[..prefix].to_vec();
            next.extend(
                blocks[prefix..blocks.len() - suffix]
                    .iter()
                    .cloned()
                    .map(|block| Rc::new(CachedBlock::new(block))),
            );
            next.extend_from_slice(&self.blocks[self.blocks.len() - suffix..]);
            // An unchanged suffix can acquire a different preceding margin.
            if suffix > 0 {
                next[blocks.len() - suffix].height.set(None);
            }
            self.blocks = next;
        }
        if self.width != Some(width) {
            for block in &self.blocks {
                block.height.set(None);
            }
        }
        self.width = Some(width);
    }

    /// Reindexes cached paragraphs after block insertion or deletion.
    pub fn texts(&self) -> BTreeMap<crate::selectable::Key, gpui::SharedString> {
        let mut texts = BTreeMap::new();
        for (body, block) in self.blocks.iter().enumerate() {
            for (&(_, paragraph), text) in block.texts.borrow().iter() {
                texts.insert((body, paragraph), text.clone());
            }
        }
        texts
    }

    pub fn wrap(
        &self,
        index: usize,
        render: impl FnOnce(&CachedBlock) -> AnyElement + 'static,
    ) -> AnyElement {
        MeasuredBlock {
            block: self.blocks[index].clone(),
            render: Some(Box::new(render)),
            child: None,
            width: self.width.expect("prepare blocks before measuring"),
        }
        .into_any_element()
    }
}

type RenderBlock = Box<dyn FnOnce(&CachedBlock) -> AnyElement>;

struct MeasuredBlock {
    block: Rc<CachedBlock>,
    render: Option<RenderBlock>,
    child: Option<AnyElement>,
    width: Pixels,
}

impl MeasuredBlock {
    fn child(&mut self) -> &mut AnyElement {
        self.child
            .get_or_insert_with(|| self.render.take().expect("block renders once")(&self.block))
    }
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
        let width = self.width;
        let height = self.block.height.get().unwrap_or_else(|| {
            let height = self
                .child()
                .layout_as_root(
                    size(AvailableSpace::Definite(width), AvailableSpace::MinContent),
                    window,
                    cx,
                )
                .height;
            self.block.height.set(Some(height));
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
            self.child()
                .layout_as_root(Size::from(bounds.size), window, cx);
            self.child().prepaint_at(bounds.origin, window, cx);
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
            self.child().paint(window, cx);
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
        for (i, block) in cache.blocks.iter().enumerate() {
            block.height.set(Some(px((i + 1) as f32 * 10.)));
        }
        cache
    }

    fn heights(cache: &BlockCache) -> Vec<Option<Pixels>> {
        cache
            .blocks
            .iter()
            .map(|block| block.height.get())
            .collect()
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
        assert!(cache.blocks.is_empty());
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
        cache.blocks[0].scrolls.borrow_mut().insert(1, scroll);
        cache.prepare(&blocks(&["a", "b"]), px(500.));
        assert_eq!(heights(&cache), vec![None, None]);
        cache.blocks[0].height.set(Some(px(40.)));
        cache.clear();
        cache.prepare(&blocks(&["a", "b"]), px(500.));
        assert_eq!(heights(&cache), vec![None, None]);
        assert_eq!(cache.blocks[0].scrolls.borrow()[&1].offset().x, px(-30.));
    }

    #[test]
    fn reindexes_offscreen_paragraphs_and_retains_each_blocks_scroll_state() {
        let mut cache = measured(&["a", "b"]);
        cache.blocks[0]
            .texts
            .borrow_mut()
            .insert((0, 1), "first".into());
        cache.blocks[1]
            .texts
            .borrow_mut()
            .insert((1, 1), "last 世界".into());
        let scroll = ScrollHandle::new();
        scroll.set_offset(point(px(-30.), px(0.)));
        cache.blocks[1].scrolls.borrow_mut().insert(1, scroll);

        cache.prepare(&blocks(&["new", "a", "b"]), px(400.));
        assert_eq!(
            cache.texts().keys().copied().collect::<Vec<_>>(),
            vec![(1, 1), (2, 1)]
        );
        assert_eq!(cache.texts()[&(2, 1)].as_ref(), "last 世界");
        assert_eq!(cache.blocks[2].scrolls.borrow()[&1].offset().x, px(-30.));

        cache.prepare(&blocks(&["b"]), px(400.));
        assert_eq!(
            cache.texts().keys().copied().collect::<Vec<_>>(),
            vec![(0, 1)]
        );
        assert_eq!(cache.texts()[&(0, 1)].as_ref(), "last 世界");
        cache.clear();
        assert!(cache.texts().is_empty());
        assert_eq!(cache.blocks[0].scrolls.borrow()[&1].offset().x, px(-30.));
        cache.reset();
        assert!(cache.blocks.is_empty());
    }

    #[test]
    fn defers_element_construction_and_builds_a_child_only_once() {
        let cache = measured(&["a"]);
        let calls = Rc::new(Cell::new(0));
        let count = calls.clone();
        let mut element = cache.wrap(0, move |_| {
            count.set(count.get() + 1);
            gpui::div().into_any_element()
        });
        assert_eq!(calls.get(), 0);
        let block = element.downcast_mut::<MeasuredBlock>().unwrap();
        assert!(block.child.is_none());
        block.child();
        block.child();
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn identical_documents_keep_the_same_cached_blocks() {
        let mut cache = measured(&["a", "b"]);
        let first = cache.blocks[0].clone();
        cache.prepare(&blocks(&["a", "b"]), px(400.));
        assert!(Rc::ptr_eq(&first, &cache.blocks[0]));
    }
}
