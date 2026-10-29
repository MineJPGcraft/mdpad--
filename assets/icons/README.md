# Vendored icon assets

These are the **official** icon files, checked in so the conversion into
`src/icon_data.rs` is reproducible and auditable. Nothing here is drawn from
memory or traced by hand: `tools/extract_icons.py` copies the `d` attributes and
`<circle>` elements verbatim.

| Directory | Source | Licence | Used for |
|---|---|---|---|
| `octicons/` | [primer/octicons](https://github.com/primer/octicons) — `icons/*-16.svg` | MIT | GitHub alerts (filled paths) |
| `lucide/` | [lucide-icons/lucide](https://github.com/lucide-icons/lucide) — `icons/*.svg` | ISC | Obsidian callouts (stroke only) |
| `docusaurus/` | [facebook/docusaurus](https://github.com/facebook/docusaurus) — `theme/Admonition/Icon/*.tsx` | MIT | Docusaurus admonitions (even-odd filled) |

Regenerate the Rust table after changing anything here:

```bash
python tools/extract_icons.py
```

## Why these files are committed

The editor is fully offline at runtime, and the previous implementation picked a
Unicode character per callout variant instead of using the real icon. Those
glyphs depend on whichever font happens to carry them, so they never matched the
weight or baseline of the surrounding text — and they were not the icons the
target site shows. Keeping the upstream files in the repository means the data
can be diffed against upstream, and the licence of each set stays attached to the
bytes it covers.
