# Forma

An agent-first, headless 2D design engine. Define one frame in JSON, compose it from layers and
shared design tokens, and render it to PNG with a local CLI. No browser, account or network needed.

```sh
cargo install --path .
forma init                  # create forma/ in the current directory
forma                       # list frames and whether they are valid
forma validate example      # check a frame, with field-level errors
forma render example        # writes forma/output/example.png
forma inspect example --json
```

## Workflow

1. Read `forma/tokens.json`, the frames, and `forma schema`.
2. Create or edit `forma/frames/name.json`.
3. Run `forma validate name.json`, then `forma render name.json --json`.
4. Look at the PNG and `forma inspect name.json --json`, then edit and repeat.

Commit `forma/` except `forma/output/`, which `forma init` adds to `.gitignore`.

## Commands

| Command | Purpose |
|---|---|
| `forma init [--path DIR]` | Create a project. Never overwrites existing files. |
| `forma`, `forma list` | List frames with sizes and validity. |
| `forma render FRAME [--output PATH] [--scale N] [--clipboard]` | Render one frame. |
| `forma render --all` | Render every frame. |
| `forma validate FRAME`, `forma validate --all` | Check frames without rendering. |
| `forma inspect FRAME [--layer ID]` | Report hierarchy, transforms, bounds and resolved values. |
| `forma icons search QUERY` | Search Lucide icons. |
| `forma icons info NAME` | Show an icon's source and ISC license details. |
| `forma tokens` | Print tokens with references resolved. |
| `forma schema` | Print the document JSON Schema. |
| `forma version` | Print the version. |

`--project PATH` selects a project instead of searching upward. `--json` prints a stable result on
stdout and leaves diagnostics on stderr. Failures exit nonzero, and errors carry a stable `code`,
a JSON `path` and the `layer` id where they apply.

A bare frame reference is looked up in the frames directory, with `.json` optional. `./`, `../` and
absolute references are filesystem paths. If both `post` and `post.json` exist, `post` is an error.

## Frames

```json
{
  "version": 1,
  "id": "post",
  "width": 1080,
  "height": 1080,
  "background": "$gradients.brand",
  "children": [
    {
      "id": "headline",
      "type": "text",
      "text": "DESIGN\nAS CODE",
      "x": 64,
      "y": 64,
      "width": 900,
      "font": "$typography.heading",
      "fill": { "color": "$colors.foreground" }
    }
  ]
}
```

Node types are `rect`, `ellipse`, `line`, `text`, `image`, `svg` and `group`. Children draw back to
front and the frame always clips. Unknown properties are errors; `forma schema` lists them all.

## Format semantics

**Colors** are `#RGB`, `#RGBA`, `#RRGGBB`, `#RRGGBBAA` or a `$token`.

**Transforms.** The node's box is placed so its untransformed `anchor` sits at `(x, y)`, then it is
scaled and rotated around `pivot`. `anchor` defaults to `[0, 0]` and `pivot` to `anchor`, so with a
separate pivot the anchored point can move. Rotation is clockwise degrees.

**Lines** run from the top-left to the bottom-right of their box, so a horizontal line has
`height: 0`.

**Groups** without a size take their bounds from their children. They keep their own origin at
`(x, y)` unless an `anchor` is set, which then refers to those bounds. `"clip": true` needs an
explicit `width` and `height`. Group opacity applies after the children are composited. `mask` is a
node in the group's coordinates; its alpha is multiplied into the composited group, so shapes fade
or cut out the group's contents. Only groups accept a mask.

**Linear gradients.** Angle `0` points up and `90` points right. Stops `0` and `1` reach the corners
of the box at any angle, as in CSS. Interpolation is in sRGB, on straight alpha.

**Radial gradients.** `center` is normalized to the node's box. `radius` is normalized per axis, so
the gradient ellipse has radii `radius × width` and `radius × height`.

**Text.** The box height comes from the laid-out lines. Omit `width` to break only at newlines;
that box is as wide as the longest line. Text is left-to-right in V1. `lineHeight` is a multiple of
`size`, and `letterSpacing` is pixels added after each glyph.

**Fonts.** `file` is relative to the assets directory and takes precedence over `family`; prefer it
for reproducible builds. `family` selects a system font. A missing font or glyph is an error unless
`fallbacks` lists families to try. One font is used per text node, and using a fallback or a
different weight is reported as a warning.

**Images** accept PNG, JPEG and WebP with `fit` of `cover` (default), `contain` or `fill`.

**SVG** assets are fitted inside the box, centered, without distortion. External references are
ignored, only `data:` images load, and `<text>` is rejected; convert it to paths. `color` sets
`currentColor`, which is black by default; colors hard-coded in the file stay as they are.

**Scale.** `--scale` must produce whole pixels. Rasters are limited to 16384 pixels per side and
100 million pixels in total, assets to 64 MB, and group nesting to 64 levels.

## Tokens

`forma/tokens.json` is shared by every frame. Reference a value as `$category.name`. Tokens can
reference other tokens, a cycle is an error, and a token must match the type of the property using
it.

## Reproducibility

The same inputs, fonts and Forma version on the same platform produce identical bytes. System fonts
vary between machines, so use font files for output that must match everywhere.

## License

MIT. `tests/fixtures/Tuffy.ttf` is public domain; see `tests/fixtures/Tuffy-LICENSE.txt`.
