---
name: forma
description: Set up and use Forma, a headless CLI that renders designs from JSON frames and shared design tokens to PNG. Use this skill whenever the user mentions forma, a repo contains forma/config.json, or the user wants to add a design system (colors, type, logos, example images) to a repo and generate on-brand images such as social posts, banners, Open Graph images, thumbnails or ads from it, even if they don't name forma. Not for building web or app UI in code, or for editing existing photos.
---

# Forma

Forma turns JSON frames into PNGs. A project is a `forma/` directory that sits inside any repo, next
to whatever else lives there.

Run `command -v forma` first. If it is missing, stop and tell the user: Forma is installed from a
source checkout with `cargo install --path <checkout>`.

If `forma/config.json` exists in the repo, skip to [Designing](#designing). Otherwise start with
[Setting up](#setting-up).

`forma schema` is the authority on which properties exist. Read
[references/format.md](references/format.md) before writing your first frame: it covers what the
schema can't, such as how anchors, groups, masks, gradients and fonts behave, plus every command.

## Setting up

Keep the repo's own files out of this. `forma init` only adds `forma/` and one `.gitignore` line, so
the rest of the repo stays exactly as it was.

1. **Create the project.** Run `forma init` at the repo root.

2. **Write the tokens.** `forma/tokens.json` holds starter placeholders, so replace them with the
   user's values, copied exactly. Use names that describe the role (`colors.background`,
   `typography.heading`), and add categories when needed (`radii`, for example). Starter tokens and
   `frames/example.json` depend on each other, so delete the example frame once its tokens are gone.

3. **Install the fonts.** Use `forma fonts search`, `forma fonts info` and
   `forma fonts add FAMILY --weight 400,700 --subset latin`, naming only the weights the tokens use,
   because a full install is large. Fonts installed this way render the same on every machine.
   - A commercial font the user supplies goes in `forma/assets/fonts/` and is referenced with `file`.
   - Copying a system font needs `--accept-license`. That is a legal statement about the user's
     license, so ask them instead of passing it yourself.
   - If you can't tell the font from an image, ask. Guessing a similar font silently changes the
     brand.

4. **Store the images.** Sort them by how they will be used:
   - Examples of the style go in `forma/references/`. Forma never renders these; they are there so
     later sessions can look at them too.
   - Logos and photos that will appear in designs go in `forma/assets/images/` or
     `forma/assets/svg/`.

5. **Write `forma/DESIGN.md`.** Tokens hold values; this holds the judgment that makes a design look
   like the brand, and it is the only memory a later session has. Use this structure, and skip
   sections you have nothing to say about:

   ```markdown
   # Design system

   ## Brand
   Who this is for and how it should feel.

   ## Color
   What each color token is for, and which combinations are allowed.

   ## Type
   Which typography token to use where, plus case, weight and length habits.

   ## Layout
   Margins, grid, alignment, density, how much empty space.

   ## Imagery
   Logo placement and clear space, photo or illustration style, icon use.

   ## Avoid
   What would look wrong for this brand.

   ## References
   Each file in forma/references/ and what to take from it.
   ```

   Separate what the user said from what you inferred from the images, and mark inferred rules with
   `(inferred)`. The user should be able to see at a glance which rules are your guess.

6. **Prove it.** Build one small frame using the color, type and logo tokens, render it, and compare
   it with the references. Then show the user the PNG and list the inferred rules for them to
   confirm or correct.

## Designing

1. Read `forma/DESIGN.md`, run `forma tokens`, look at the images in `forma/references/`, and open
   the frames already in `forma/frames/`. Reusing their structure is the fastest way to stay
   consistent.
2. Create or edit `forma/frames/NAME.json`. Its `id` should match the file name.
3. Use tokens for every color, font, gradient and spacing value the system defines. A hard-coded
   value passes validation, but it stops following the system when a token changes.
4. Run `forma validate NAME`, then `forma render NAME --json`. Errors include a `code`, a JSON `path`
   and the `layer`, which point at the exact fix. Treat warnings as problems too: a fallback font or
   substituted weight means the design isn't using the brand's type.
5. Open the PNG and look at it. Validation only proves that the file is well formed, not that the
   design works, so check for text collisions, cramped margins, weak hierarchy and anything that
   breaks `DESIGN.md`. Use `forma inspect NAME --json` for exact bounds, then edit and repeat.
6. Check for values that bypass the tokens:

   ```sh
   grep -nE '#[0-9A-Fa-f]{3,8}"|"(family|file)"' forma/frames/*.json
   ```

   Replace each match with a token, unless the user asked for it or it is a deliberate one-off.
7. Tell the user where the PNG is, and mention any deliberate exception.

If a design needs something the system lacks, add it to `tokens.json` or `DESIGN.md` and say so,
instead of leaving a one-off in the frame. Rules then keep working in the next session.

`forma/output/` is ignored by git; everything else in `forma/` should be committed.
