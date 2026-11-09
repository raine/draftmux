//! Reproducible CPU benchmarks for the synchronous typing pipeline.
//!
//! Run with `cargo test -j 2 -p draftmux --bin draftmux performance::typing
//! -- --ignored --nocapture`. These measure CPU work, not input-to-display latency.

#[path = "../../editor/src/highlight.rs"]
#[allow(dead_code)]
mod highlight;

use std::hint::black_box;
use std::time::Instant;

fn measure(name: &str, mut work: impl FnMut()) {
    for _ in 0..5 {
        work();
    }
    let mut samples = Vec::new();
    for _ in 0..50 {
        let start = Instant::now();
        work();
        samples.push(start.elapsed().as_secs_f64() * 1000.);
    }
    samples.sort_by(f64::total_cmp);
    eprintln!(
        "{name}: median={:.3}ms p95={:.3}ms",
        samples[25], samples[47]
    );
}

#[test]
#[ignore = "manual CPU benchmark"]
fn typing() {
    let section = "## Release readiness\n\nWe reviewed the **deployment plan** with the platform team. The next step is to validate *rollback safety* and [service health](https://example.com/health) before enabling traffic.\n\n- [x] Verify the staging configuration\n- [ ] Confirm ownership and update the runbook\n\n> Keep the previous version available until validation completes.\n\n```rust\nfn ready(count: usize) -> bool { count > 0 }\n```\n\n| Service | Status |\n| --- | --- |\n| API | Ready |\n| Worker | Pending |\n\n";
    for (name, repeats) in [("small", 2), ("large", 200)] {
        let mut text = section.repeat(repeats);
        text.push_str("A final update");
        eprintln!("{name}: {} bytes", text.len());
        measure(&format!("{name}/highlight"), || {
            black_box(highlight::highlight(black_box(&text)));
        });
        for target in [
            draftmux_storage::Target::Github,
            draftmux_storage::Target::Slack,
            draftmux_storage::Target::Rich,
        ] {
            measure(&format!("{name}/{target:?}/parse"), || {
                black_box(crate::markup_view::preview(target, black_box(&text)));
            });
            let doc = crate::markup_view::preview(target, &text);
            let style = crate::markup_view::style(target, false);
            measure(&format!("{name}/{target:?}/element-tree"), || {
                black_box(crate::markup_view::render(&doc, &style, "benchmark", None));
            });
        }
        measure(&format!("{name}/copies"), || {
            black_box((text.clone(), text.clone()));
        });
    }
}
