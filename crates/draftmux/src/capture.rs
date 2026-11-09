//! `draftmux capture`: renders fixture drafts to PNGs for comparing the
//! preview with GitHub's and Slack's own rendering (docs/design.md, Testing).
//!
//! Each fixture is drawn by [`markup_view::render`] in an offscreen window,
//! light and dark, at a fixed content width on the platform's background,
//! and read back with gpui's `Window::render_to_image`. Slack's conversion
//! notes are left out, since Slack never shows them. Built only with the
//! `capture` feature, which needs gpui's test support.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use draftmux_storage::Target;
use gpui::{
    App, AsyncApp, Bounds, Context, Render, Window, WindowBounds, WindowOptions, div, point,
    prelude::*, px, size,
};
use markup::Document;

use crate::assets;
use crate::markup_view::{self, MarkupStyle};

/// Padding around the body in the image, matching the reference captures.
const PAD: f32 = 16.;

/// Render fixture drafts to PNGs (development tool).
#[derive(clap::Args, Debug)]
pub struct Args {
    /// Target whose preview style to use: github or slack.
    #[arg(long)]
    target: String,
    /// Directory of `.md` fixture drafts.
    fixtures: PathBuf,
    /// Directory to write `<fixture>-<target>-<light|dark>-draftmux.png` to.
    out: PathBuf,
    /// Content width in points.
    #[arg(long, default_value_t = 640.)]
    width: f32,
}

pub fn main(args: Args) -> i32 {
    let target = match args.target.as_str() {
        "github" => Target::Github,
        "slack" => Target::Slack,
        other => {
            eprintln!("draftmux capture: unknown target {other}");
            return 2;
        }
    };
    let fixtures = match fixtures(&args.fixtures) {
        Ok(fixtures) => fixtures,
        Err(err) => {
            eprintln!("draftmux capture: {err:#}");
            return 1;
        }
    };
    std::fs::create_dir_all(&args.out).expect("create the output directory");
    gpui_platform::application()
        .with_assets(assets::Assets)
        .run(move |cx: &mut App| {
            markup_view::init(cx);
            cx.spawn(async move |cx| {
                let code =
                    match capture_all(target, &args.target, &fixtures, &args.out, args.width, cx)
                        .await
                    {
                        Ok(()) => 0,
                        Err(err) => {
                            eprintln!("draftmux capture: {err:#}");
                            1
                        }
                    };
                cx.update(|cx| cx.quit());
                std::process::exit(code);
            })
            .detach();
        });
    0
}

fn fixtures(dir: &Path) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "md") {
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            out.push((name, std::fs::read_to_string(&path)?));
        }
    }
    out.sort();
    Ok(out)
}

async fn capture_all(
    target: Target,
    target_name: &str,
    fixtures: &[(String, String)],
    out: &Path,
    width: f32,
    cx: &mut AsyncApp,
) -> Result<()> {
    for (name, text) in fixtures {
        for dark in [false, true] {
            let doc = markup_view::preview(target, text);
            let mut style = markup_view::style(target, dark);
            style.notes = None;
            let window = cx.update(|cx| {
                cx.open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(Bounds {
                            origin: point(px(-10000.), px(-10000.)),
                            size: size(px(width + 2. * PAD), px(2400.)),
                        })),
                        focus: false,
                        show: true,
                        ..Default::default()
                    },
                    |_, cx| cx.new(|_| Capture { doc, style, width }),
                )
            })?;
            // Two frames: the first lays out, the second paints the result.
            cx.background_executor()
                .timer(Duration::from_millis(300))
                .await;
            let image = window.update(cx, |_, window, _| {
                if window.scale_factor() != 2. {
                    bail!(
                        "scale factor is {}, references are 2x",
                        window.scale_factor()
                    );
                }
                window.render_to_image()
            })??;
            let mode = if dark { "dark" } else { "light" };
            let path = out.join(format!("{name}-{target_name}-{mode}-draftmux.png"));
            crop(image).save(&path)?;
            println!("{}", path.display());
            window.update(cx, |_, window, _| window.remove_window())?;
        }
    }
    Ok(())
}

/// Cuts the image below the last row that isn't background, keeping the
/// bottom padding.
fn crop(image: image::RgbaImage) -> image::RgbaImage {
    let background = *image.get_pixel(0, 0);
    let last = (0..image.height())
        .rev()
        .find(|&y| (0..image.width()).any(|x| *image.get_pixel(x, y) != background))
        .unwrap_or(0);
    let height = (last + 1 + 2 * PAD as u32).min(image.height());
    image::imageops::crop_imm(&image, 0, 0, image.width(), height).to_image()
}

struct Capture {
    doc: Document,
    style: MarkupStyle,
    width: f32,
}

impl Render for Capture {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(self.style.background)
            .p(px(PAD))
            .child(div().w(px(self.width)).child(markup_view::render(
                &self.doc,
                &self.style,
                "capture",
            )))
    }
}
