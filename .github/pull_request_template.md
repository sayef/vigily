## What and why

<!-- What does this change, and why is it needed? Link the issue if there is one. -->

## How it was tested

<!-- Tests added or run. For UI changes, attach screenshots in light and dark (npm run qa). -->

## Checklist

- [ ] `cargo fmt --all` and `cargo clippy --all-targets -- -D warnings` are clean; `cargo nextest run` passes
- [ ] `npm run lint`, `npm run typecheck` and `npm run build` pass in `crates/vigily-ui`
- [ ] `uv run --no-sync pytest` passes after `uv run --no-sync maturin develop`
- [ ] UI changes follow `brand/DESIGN.md` and were checked in a browser at desktop and phone widths
- [ ] README, docs and CHANGELOG are updated if commands, the API or the SDK changed
