# Fix streaming popover flicker

The popover re-rendered on every token, so it **flickered** while the
response streamed. This change *batches* updates per frame and ~~drops~~
keeps the `requestAnimationFrame` path. See [the issue](https://github.com/raine/steer/issues/42)
or https://example.com/very/long/autolinked/path/that/should/wrap/nicely.

## Changes

- Batch popover updates per animation frame
- Keep selection anchoring stable
  - nested bullet with `inline code`
    - third level
- Remove the debounce workaround

1. Parse the stream
2. Coalesce updates
3. Paint once

### Testing

- [x] Unit tests for the batching queue
- [x] Manual run against LM Studio
- [ ] Check on a slow machine

> Quoted text from the original report, with a
> second line and **bold** inside.

> [!NOTE]
> Alerts render with a colored bar and title.

> [!TIP]
> Helpful advice.

> [!IMPORTANT]
> Key information.

> [!WARNING]
> Needs attention.

> [!CAUTION]
> Negative consequences.

```rust
fn coalesce(updates: &mut Vec<Update>) -> Option<Update> {
    updates.drain(..).reduce(|a, b| a.merge(b)) // a long trailing comment that should scroll sideways rather than wrap
}
```

| Backend | Latency | Notes |
|:--------|--------:|:-----:|
| LM Studio | 0.8s | local |
| claude-code-proxy | 1.4s | `--fast` |
| ollama | 2.1s | |

---

#### Heading four
##### Heading five
###### Heading six

<details>
<summary>Raw HTML stays literal</summary>
</details>
