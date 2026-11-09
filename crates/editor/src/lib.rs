//! Draftmux's markdown source editor: a soft-wrapping, undoable plain-text
//! editor with light markdown highlighting.

#[macro_use]
pub mod platform;
mod buffer;
mod editor;
mod element;
mod highlight;
mod movement;
pub mod timing;

pub use editor::{
    ContextMenuItem, ContextMenuRenderer, DismissMenu, Editor, EditorEvent, EditorStyle, init,
};
