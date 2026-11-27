# DeckX instructions

## App versions

- For each app update handed over to the user, advance the package version in `Cargo.toml` and keep `Cargo.lock` in sync. The footer and `--version` use the package version.
- End the handoff with the DeckX version. When a build was opened, report the running version.

## Visual acceptance

- Check the rebuilt UI against the supplied reference or existing design before calling a visual change complete. Inspect the affected view and the other views that reuse the changed component.
- Capture the DeckX window at its default size of 840 × 740 and minimum size of 700 × 580 when checking layout. Verify 100%, 125%, and 150% rendering scale when changing pixel-sensitive controls.
- Measure alignment from the rendered pixels. Center the visible glyph or icon inside its own button face on both axes. For symmetric controls, allow at most one physical pixel of difference unless the reference calls for an intentional optical offset. Check spacing, padding, clipping, and hit targets in the relevant states.
- Report the views, sizes, scales, and states you verified. Identify any checks that remain open. Use the actual rendered build as evidence for a visual match.
