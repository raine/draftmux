Proposed change to the design doc step:
```diff
diff --git a/skills/design/SKILL.md b/skills/design/SKILL.md
--- a/skills/design/SKILL.md
+++ b/skills/design/SKILL.md
@@ -12,3 +12,4 @@ ## Saving
-Save it as `$WORKSPACE/designs/<slug>.md` and upload it with
+Date the document with the day of its first version and save it as
+`$WORKSPACE/designs/<date>-<slug>.md`. Upload it with
 the comment below.
! changed
\ No newline at end of file
```
Run it with:
```sh
cargo test -j 2 -p markup --test slack
```
```rs
fn main() {
    println!("a line long enough to scroll sideways in a Slack code block, past the edge of the message");
}
```
```text
plain text keeps its line numbers
```
```foobar
an unknown language has no line numbers
```
```
no language: the old code block
```
