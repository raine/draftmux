The fix is in `Renderer::text`, which now lays out `code` spans itself.

```
cargo test -j 2 -p markup --test github
cargo test -j 2 -p draftmux --bin draftmux markup_view
```

> Quoted review comment with **bold** and `code`.
>
> > A nested quote.

---

Final paragraph with *emphasis*, **strong**, ~~strike~~ and a long inline code span `crates/draftmux/src/markup_view.rs` that may wrap.
