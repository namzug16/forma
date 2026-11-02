# Artworks

An artwork is one Lua 5.4 file in `forma/artworks/`. Use it when a visual is generated rather than
composed: patterns, particles, noise textures, abstract backgrounds. It cannot draw text or load
images; put those in the frame on top of it.

`forma generate NAME` writes `forma/assets/artworks/NAME.png` at exactly the declared size. Use it
in a frame as `"src": "artworks/NAME.png"`, and commit the PNG. `forma render` never runs scripts,
so generate again after changing the script or a token it reads.

## Script

```lua
return {
  width = 1080,     -- number or token reference such as "$sizes.hero"; required
  height = 1080,    -- same
  seed = 42,        -- integer; required unless `--seed` is passed
  draw = function(canvas, tokens, random, noise) end,  -- required
}
```

- `tokens` is a read-only table of resolved tokens: `tokens.spacing.md`, `pairs(tokens.colors)`.
- `random()` is within [0, 1); `random(minimum, maximum)` within [minimum, maximum). Both are
  floats; use `math.floor` for integers.
- `noise(x, y, z?)` is smooth seeded Perlin noise within [-1, 1]. It is 0 at whole coordinates, so
  scale them, as in `noise(x / 200, y / 200)`.

## Canvas

Every shape call takes one table. Unknown fields are errors.

| Call | Fields |
|---|---|
| `canvas:clear(paint?)` | Resets the canvas to `paint`, or to transparent. |
| `canvas:rect { ... }` | `x`, `y` (top left), `width`, `height`, `cornerRadius?` |
| `canvas:circle { ... }` | `x`, `y` (center), `radius` |
| `canvas:line { ... }` | `x1`, `y1`, `x2`, `y2`, `stroke` (required) |
| `canvas:path { ... }` | `segments` |
| `canvas:shade(function(x, y) return r, g, b, a end)` | One call per pixel. See below. |
| `canvas:push()`, `canvas:pop()` | Save and restore the transform. |
| `canvas:translate(x, y)`, `canvas:rotate(degrees)`, `canvas:scale(x, y?)` | Affect later shapes. |

`rect`, `circle` and `path` also take `fill`, `stroke` and `opacity` (0 to 1). `line` takes
`opacity`.

- **Paint** (`fill`, `clear`): `"#RRGGBB"`, `"$colors.name"`, `"$gradients.name"`, or a gradient
  table as in frames, such as `{ type = "linear-gradient", angle = 90, stops = { ... } }`. Gradient
  geometry is relative to the shape's own box, as in frames.
- **Stroke**: `{ color = "...", width = 2, opacity?, cap?, join?, dash? }`, as in frames.
- **Segments**: `{ "move", x, y }`, `{ "line", x, y }`, `{ "quad", x1, y1, x, y }`,
  `{ "cubic", x1, y1, x2, y2, x, y }`, `{ "close" }`.

`shade` calls the function for every pixel, with `x` and `y` from 0, and composites the result over
the canvas. Return red, green, blue and optional alpha from 0 to 1. It ignores transforms, and
cannot read pixels already drawn. Do not draw shapes inside it.

## Sandbox

Available: basic Lua, `math`, `string` and `table`. Removed: `os`, `io`, `debug`, `coroutine`,
`require`, `load`, `print`, `pcall`, `next` and `math.random`; use the `random` argument. `pairs`
visits keys in sorted order and only takes boolean, number and string keys. `table.sort` is stable.
Printing a table's address with `tostring` varies between runs, so never draw from it.

A script stops with an error, naming its line, after 1 GiB of memory or 5 billion instructions.
