# Validation

CPU is shared between parallel agents and gpui builds are slow. Unless
explicitly asked:

- Never run workspace-wide `cargo test`, `clippy` or `build`, release builds,
  or `scripts/bundle`.
- Run only tests covering the changed code, scoped to a package and target
  with `-j 2`: `cargo test -j 2 -p markup --test github` (`--lib` for unit
  tests). A bare name filter still builds every test target.
- Skip tests for trivial edits with no logic change.
- Run real pi/oxfmt smoke tests (`-- --ignored`) at most once per change.
- Don't rerun after commit, rebase or merge unless relevant code changed; read
  the saved log after a failure instead of rerunning.
- Report what ran and what was deferred.
