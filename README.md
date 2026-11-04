# Forma

An agent-first, headless 2D design engine. Define one frame in JSON, compose it from layers and
shared design tokens, and render it to PNG with a local CLI. No browser, account or network needed.

**Website: [forma.namzug.dev](https://forma.namzug.dev)**

## Install

Forma runs on Apple silicon Macs for now. Windows and Linux are coming soon.

### With an agent

Paste this into Claude Code, Codex, Cursor or any coding agent:

```text
Install Forma, a headless CLI that renders designs from JSON frames and design tokens to PNG.

1. Check the machine is an Apple silicon Mac; Forma does not support other platforms yet.
2. Install the CLI with Homebrew: brew install namzug16/tap/forma
   If Homebrew is missing, use: curl -fsSL https://raw.githubusercontent.com/namzug16/forma/master/install.sh | sh
3. Confirm it works: forma version
4. Install the Forma agent skill: npx skills add namzug16/forma
5. In this repo, run forma init, then forma render example, and show me forma/output/example.png.
```

### Manually

1. Install the CLI:

   ```sh
   brew install namzug16/tap/forma
   ```

   Or, without Homebrew, to `~/.local/bin` (set `FORMA_INSTALL_DIR` to change that):

   ```sh
   curl -fsSL https://raw.githubusercontent.com/namzug16/forma/master/install.sh | sh
   ```

2. Add the agent skill:

   ```sh
   npx skills add namzug16/forma
   ```

3. Render your first frame:

   ```sh
   forma init
   forma render example
   ```

To build from a checkout, run `cargo install --path .`.

## Quick start

```sh
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
| `forma render FRAME [--output PATH \| --clipboard] [--scale N] [--format png\|svg]` | Render one frame to a file, or to the clipboard without writing a file. SVG outlines text and pastes into Figma. |
| `forma render --all` | Render every frame. |
| `forma generate [ARTWORK] [--seed N]` | Run one artwork, or every artwork, into `forma/assets/artworks/`. |
| `forma validate FRAME`, `forma validate --all` | Check frames without rendering. |
| `forma inspect FRAME [--layer ID]` | Report hierarchy, transforms, bounds and resolved values. |
| `forma fonts list [--source project\|system]` | List families with their weights and styles. |
| `forma fonts search QUERY` | Search project, system and Fontsource fonts. |
| `forma fonts add FAMILY [--weight 400,700] [--style italic] [--subset latin]` | Install a Fontsource font. |
| `forma fonts add FAMILY --from system --accept-license` | Copy a system font's files into the project. |
| `forma fonts info FAMILY` | Show a font's weights, styles, subsets and license. |
| `forma fonts check FAMILY` | Report the font a text layer would use. Exits nonzero if missing. |
| `forma icons search QUERY` | Search Lucide icons. |
| `forma icons add NAME` | Download an icon. Never overwrites an existing file. |
| `forma icons list` | List installed icons. |
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

**Sizes.** `width` and `height` take a number, a `$token`, `"fill"` (the parent's whole size on that
axis) or a percentage of the parent such as `"50%"`. The parent is the frame or the enclosing group.
`"hug"` fits the content and is only valid on groups and text, where it is also the default. Filling
or taking a percentage of a parent that hugs on that axis is an error, because the sizes would depend
on each other. `minWidth`, `maxWidth`, `minHeight` and `maxHeight` apply to every layer after its
size is resolved.

**Layout.** A group with `layout` places its children in a row or column instead of at their `x`
and `y`: `{ "direction": "column", "gap": 12, "padding": 24, "align": "center", "crossAlign":
"stretch" }`. `direction` is `row` (default) or `column`. `padding` is one number,
`[vertical, horizontal]` or `[top, right, bottom, left]`. `align` places children along the
direction (`start`, `center`, `end`, `space-between`) and `crossAlign` across it (`start`, `center`,
`end`, `stretch`). A layout group that hugs is as big as its children plus gaps and padding, so a
card fits its copy without any arithmetic. Inside it, `"fill"` along the direction shares the space
left equally among siblings, and `"fill"` across it stretches, to the largest sibling if the group
hugs. Percentages measure against the box inside the padding. Children keep their size and overflow
rather than shrink, with a `layout_overflow` warning. `x`, `y` and `anchor` are errors on children
in the flow. `"position": "absolute"` takes a child out of it: it is placed with `x` and `y` from
the group's top-left, measures `fill` against the whole group, and does not count toward a hugging
group's size. Hidden children leave no gap. Rotation and scale apply after layout and don't move
siblings.

**Groups** that hug take their bounds from their children, per axis. A group hugging both axes keeps
its own origin at `(x, y)` unless an `anchor` is set, which then refers to those bounds. `"clip":
true` clips to the group's box. Group opacity applies after the children are composited. `mask` is a
node in the group's coordinates; its alpha is multiplied into the composited group, so shapes fade
or cut out the group's contents. Only groups accept a mask. `fill`, `stroke` and `cornerRadius`
paint a background box behind the children, like a rect, and a clip follows the rounded corners.

**Linear gradients.** Angle `0` points up and `90` points right. Stops `0` and `1` reach the corners
of the box at any angle, as in CSS. Interpolation is in sRGB, on straight alpha.

**Radial gradients.** `center` is normalized to the node's box. `radius` is normalized per axis, so
the gradient ellipse has radii `radius × width` and `radius × height`.

**Text.** The box height comes from the laid-out lines. Omit `width` or set `"hug"` to break only
at newlines; that box is as wide as the longest line, and wraps at `maxWidth` if that is narrower. Text is left-to-right in V1. `lineHeight` is a multiple of
`size`, and `letterSpacing` is pixels added after each glyph.

**Fonts.** `file` is relative to the assets directory and takes precedence over `family`. `family`
searches `assets/fonts` first, then system fonts, so a checked-in font renders the same on every
machine. A missing font or glyph is an error unless
`fallbacks` lists families to try. One family is used per text node, and using a fallback or a
different weight is reported as a warning. Files of a family with the same weight and style combine,
so a font split by character subset works as one font, even within a line.

`forma fonts add` puts static TTFs in `assets/fonts/ID/` with the license text and a `font.json`
recording the source. It installs every weight, style and named subset by default, so narrow large
fonts with `--weight` or `--subset`. Adding more of a font later keeps what is there; rendering
never downloads anything.

System fonts work in text layers without being copied. `--from system` is only for sharing a
project: it needs `--accept-license`, because a system font's license usually decides whether
copying is allowed, and it refuses fonts whose embedding bits are restricted.

**Images** accept PNG, JPEG and WebP with `fit` of `cover` (default), `contain` or `fill`.

**SVG** assets are fitted inside the box, centered, without distortion. External references are
ignored, only `data:` images load, and `<text>` is rejected; convert it to paths. `color` sets
`currentColor`, which is black by default; colors hard-coded in the file stay as they are.

**Icons** install to `assets/icons/lucide/NAME.svg` with their original content, and
`manifest.json` beside them records each source and license. Reference one like any SVG:
`{ "type": "svg", "src": "icons/lucide/star.svg", "width": 48, "height": 48, "color": "$colors.primary" }`.

**Scale.** `--scale` must produce whole pixels. Rasters are limited to 16384 pixels per side and
100 million pixels in total, assets to 64 MB, and group nesting to 64 levels.

## Artworks

An artwork is a Lua script that draws an image, for visuals that would take thousands of JSON
layers: patterns, particles, noise textures. `forma/artworks/spiral.lua` becomes
`forma/assets/artworks/spiral.png`, which frames use like any image, with
`"src": "artworks/spiral.png"`. Commit the PNG. `forma render` never runs scripts, so run
`forma generate` again after changing a script or the tokens it reads.

```lua
return {
  width = 1080,          -- or a token, such as "$sizes.hero"
  height = 1080,
  seed = 42,             -- `--seed N` replaces it for one run
  draw = function(canvas, tokens, random, noise)
    canvas:clear("$colors.background")
    for turn = 1, 300 do
      local distance = turn * 1.5
      canvas:circle {
        x = 540 + distance * math.cos(turn / 8),
        y = 540 + distance * math.sin(turn / 8),
        radius = 4 + 12 * random(),
        fill = "$colors.primary",
        opacity = 0.7,
      }
    end
  end,
}
```

The full API is in [skills/forma/references/artworks.md](skills/forma/references/artworks.md).
Scripts run offline in a sandbox with no file, network or clock access, and with limits on memory
and instructions. The same script, tokens and seed give identical pixels on the same Forma build
and platform; floating-point math can differ slightly between platforms.

## Tokens

`forma/tokens.json` is shared by every frame. Reference a value as `$category.name`. Tokens can
reference other tokens, a cycle is an error, and a token must match the type of the property using
it.

## Reproducibility

The same inputs, fonts and Forma version on the same platform produce identical bytes. System fonts
vary between machines, so use font files for output that must match everywhere.

## Desktop viewer

`desktop/` is a read-only macOS app (macOS 15 or later on Apple silicon) for watching a project
while agents work on it. It renders in memory and never writes to the project. Windows and Linux
versions are coming soon.

[Download Forma for macOS](https://github.com/namzug16/forma/releases/latest/download/Forma-macos-arm64.dmg),
open the DMG and drag Forma to Applications. The app is not notarized yet, so macOS blocks the first
launch: try to open it, then choose **Open Anyway** in System Settings > Privacy & Security.

```sh
make desktop      # run from source
make desktop-app  # build target/Forma.app
```

Open a project from the welcome window to browse its frames, tokens and config. Recent projects
are remembered in `~/Library/Application Support/Forma/`. The tree follows the files
that exist, and the selected frame re-renders when it or the tokens and assets change. A frame
that cannot render shows its errors for your agent to fix. Click a layer in the
inspector to outline it on the canvas. The Assets section previews images, SVGs and fonts. The inspector's Export
section copies a frame to the clipboard as SVG, with text as outlines, to paste into tools such as
Figma.

| Input | Action |
| --- | --- |
| Two-finger scroll or drag | Pan |
| Pinch or `Cmd`+scroll | Zoom at the cursor |
| Canvas `-` and `+` buttons | Zoom at the canvas center |
| Canvas reset button or `Cmd-0` | Fit the frame |

## Development

`cargo test` needs internet access: the install tests download from the real Iconify and Fontsource
services, so a change on their side fails a test instead of reaching users.

To release, set `version` under `[workspace.package]` in `Cargo.toml`, add a `## 0.2.0 - DATE`
section to `CHANGELOG.md`, commit, then push a matching tag such as `v0.2.0`.
`.github/workflows/release.yml` tests that commit, publishes the DMG and CLI to a GitHub release
with that changelog section as its notes, and updates the Homebrew tap.

## License

MIT. `tests/fixtures/Tuffy.ttf` is public domain; see `tests/fixtures/Tuffy-LICENSE.txt`.
`tests/fixtures/Inter-*.ttf` are under the SIL Open Font License; see
`tests/fixtures/Inter-LICENSE.txt`.
