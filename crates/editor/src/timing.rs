//! Opt-in CPU timings. Set `DRAFTMUX_TIMING=1` to emit timings to stderr.
//! Input-to-paint ends at editor paint, not GPU presentation or display scanout.

use std::cell::Cell;
use std::sync::OnceLock;
use std::time::Instant;

thread_local! {
    static INPUT: Cell<Option<(gpui::EntityId, Instant)>> = const { Cell::new(None) };
}

fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("DRAFTMUX_TIMING").is_some())
}

pub struct Span(&'static str, Option<Instant>);

pub fn span(name: &'static str) -> Span {
    Span(name, enabled().then(Instant::now))
}

impl Drop for Span {
    fn drop(&mut self) {
        if let Some(start) = self.1 {
            eprintln!(
                "timing {} {:.3}",
                self.0,
                start.elapsed().as_secs_f64() * 1000.
            );
        }
    }
}

pub fn input(id: gpui::EntityId) {
    if enabled() {
        INPUT.with(|input| {
            if input.get().is_none() {
                input.set(Some((id, Instant::now())));
            }
        });
    }
}

pub fn painted(id: gpui::EntityId) {
    if enabled() {
        INPUT.with(|input| {
            if let Some((pending, start)) = input.get()
                && pending == id
            {
                input.set(None);
                drop(Span("input-to-editor-paint", Some(start)));
            }
        });
    }
}
