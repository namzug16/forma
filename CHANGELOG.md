# Changelog

Each release's section is published as its GitHub release notes.

## 0.3.0 - 2026-11-04

### Frames

- Groups take `fill`, `stroke` and `cornerRadius` to paint a background behind their children, so a
  card is one group. Clipping follows the rounded corners.
- `width` and `height` accept `"hug"`, `"fill"` and percentages such as `"50%"`, measured against
  the parent group or the frame, and every layer takes `minWidth`, `maxWidth`, `minHeight` and
  `maxHeight`. Text that hugs wraps at its `maxWidth`. A group may hug one axis and set the other,
  and can clip without an explicit size. `forma inspect` reports how each size was authored.
- Groups take `layout` to place their children in a row or column with `gap`, `padding`, `align`
  and `crossAlign`, so a card hugs its copy and agents no longer recompute positions when text
  changes. `"position": "absolute"` takes a child out of the flow, and children that overflow a
  fixed-size layout are reported as a warning.

### Desktop

- Clicking a subfolder of the frames directory shows its frames together, side by side, stacked or
  in a grid, with adjustable spacing and columns.
- Clicking a layer opens its details in a pane below the layers list: what forma resolved it to,
  and every property as written in the frame file, with token references resolved beside them.
- Going back to a recently shown frame shows its last render at once while any change re-renders.
- Edits show sooner: frames render at the size they are shown, decoded images are reused across
  renders, and a burst of file changes runs one render at a time instead of several in parallel.

## 0.2.0 - 2026-11-03

### CLI

- `forma render --format svg` writes a frame as SVG with text as outlines, to paste into tools
  such as Figma. With `--clipboard` the markup is copied as text. `--scale` does not apply to SVG.

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
