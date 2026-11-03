# Changelog

Each release's section is published as its GitHub release notes.

## 0.1.0 - 2026-11-03

The first release, for Apple silicon Macs. Windows and Linux are coming soon.

### CLI

- Install with `brew install namzug16/tap/forma` or the curl script in the README.
- `forma init` creates a project; `forma` lists its frames and whether they are valid.
- Frames are JSON documents of `rect`, `ellipse`, `line`, `text`, `image`, `svg` and `group`
  layers, with solid fills, strokes, linear and radial gradients, group opacity, clipping and
  masks.
- Shared design tokens, referenced as `$category.name`, with typed and cycle-checked references.
- `forma render` writes PNGs or copies them to the clipboard, at any whole-pixel scale.
- `forma validate` and `forma inspect` report field-level errors, hierarchy, bounds and resolved
  values. `--json` gives stable output for agents.
- Fonts from the project, the system or Fontsource, including fonts split into subset files.
- Lucide icons with `forma icons`, recolored through `currentColor`.
- Artworks: sandboxed Lua scripts that draw patterns, particles and noise into images, run with
  `forma generate`.
- An agent skill in `skills/forma`.

### Desktop

- A read-only viewer for macOS 15 or later that follows a project live while agents work on it.
- Browse frames, tokens, config and assets, with previews of images, SVGs and fonts.
- Zoom and pan the canvas, and highlight layers from the inspector.
- Copy a frame to the clipboard as SVG, with text as outlines, to paste into tools such as Figma.
- The app is not notarized yet: choose **Open Anyway** in System Settings > Privacy & Security on
  first launch.
