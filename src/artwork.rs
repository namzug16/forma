//! Runs artworks: sandboxed Lua scripts that draw an image through a small canvas API.

use std::cell::Cell;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

use anyhow::Result;
use mlua::{
    Function, HookTriggers, Lua, LuaOptions, LuaSerdeExt, StdLib, Table, UserData, UserDataMethods,
    Value, VmState,
};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use tiny_skia::Pixmap;

use crate::diagnostics::{Diagnostic, Failure};
use crate::document::{self, Fill, NumberValue, SolidFill};
use crate::frames;
use crate::project::Project;
use crate::renderer::{self, ArtworkCanvas, ArtworkPathSegment};
use crate::scene::{Affine, Bounds, Stroke};
use crate::tokens::{Paint, Tokens};

const SCRIPT_EXTENSION: &str = "lua";
const MAX_SCRIPT_MEMORY_BYTES: usize = 1024 * 1024 * 1024;
/// About twenty seconds, and many times what a 4K noise texture needs.
const MAX_SCRIPT_INSTRUCTIONS: u64 = 5_000_000_000;
const INSTRUCTIONS_BETWEEN_LIMIT_CHECKS: u32 = 1_000_000;

/// Runs before every script. Removes what could reach outside the sandbox, swallow a limit error
/// or vary between runs, and makes the rest deterministic: Lua seeds string hashing per state, so
/// `pairs` order changes between runs, and `table.sort` picks random pivots on uneven input.
const SANDBOX_PRELUDE: &str = r#"
local type, tostring, error, next, setmetatable, rawlen = type, tostring, error, next, setmetatable, rawlen
local move, min = table.move, math.min

load, loadfile, dofile, require, collectgarbage, print, pcall, xpcall = nil
_G.next, string.dump, math.random, math.randomseed = nil

local function default_is_before(left, right) return left < right end

-- A stable merge sort.
function table.sort(list, is_before)
  is_before = is_before or default_is_before
  local count, buffer, width = #list, {}, 1
  while width < count do
    for left = 1, count, width * 2 do
      local middle, stop = min(left + width, count + 1), min(left + width * 2, count + 1)
      local first, second = left, middle
      for position = left, stop - 1 do
        if first < middle and (second >= stop or not is_before(list[second], list[first])) then
          buffer[position], first = list[first], first + 1
        else
          buffer[position], second = list[second], second + 1
        end
      end
    end
    move(buffer, 1, count, 1, list)
    width = width * 2
  end
end

local key_ranks = { boolean = 1, number = 2, string = 3 }

local function is_key_before(left, right)
  local left_rank, right_rank = key_ranks[type(left)], key_ranks[type(right)]
  if left_rank ~= right_rank then return left_rank < right_rank end
  if left_rank == 1 then return not left and right end
  return left < right
end

local readonly_targets = setmetatable({}, { __mode = "k" })

function pairs(subject)
  local target = readonly_targets[subject] or subject
  local keys = {}
  for key in next, target do
    if not key_ranks[type(key)] then
      error("pairs only visits boolean, number and string keys, not " .. type(key), 2)
    end
    keys[#keys + 1] = key
  end
  table.sort(keys, is_key_before)
  local position = 0
  return function()
    position = position + 1
    local key = keys[position]
    if key ~= nil then return key, target[key] end
  end
end

local function readonly(source)
  local target = {}
  for key, value in next, source do
    target[key] = type(value) == "table" and readonly(value) or value
  end
  local proxy = setmetatable({}, {
    __index = target,
    __len = function() return rawlen(target) end,
    __newindex = function(_, key) error("tokens are read-only: " .. tostring(key), 2) end,
    __metatable = false,
  })
  readonly_targets[proxy] = target
  return proxy
end

return readonly
"#;

#[derive(Debug, Clone)]
pub struct ArtworkFile {
    pub path: PathBuf,
    /// Path relative to the artworks directory, which decides where the PNG goes.
    pub relative_path: PathBuf,
}

impl ArtworkFile {
    pub fn display_name(&self) -> String {
        self.relative_path.display().to_string()
    }
}

/// Every script under the artworks directory, sorted by relative path.
pub fn list(project: &Project) -> Result<Vec<ArtworkFile>> {
    let directory = project.artworks_directory();
    let mut paths = Vec::new();
    frames::collect_files(&directory, SCRIPT_EXTENSION, &mut paths)?;
    let mut artworks = paths
        .into_iter()
        .map(|path| ArtworkFile {
            relative_path: path
                .strip_prefix(&directory)
                .expect("listed under this directory")
                .to_owned(),
            path,
        })
        .collect::<Vec<_>>();
    artworks.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(artworks)
}

/// A name inside the artworks directory, with `.lua` optional.
pub fn resolve_reference(project: &Project, reference: &str) -> Result<ArtworkFile> {
    let reference_path = Path::new(reference);
    if !reference_path
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(Failure::from(Diagnostic::new(
            "invalid_artwork_reference",
            format!("\"{reference}\" must be a name inside the artworks directory"),
        ))
        .into());
    }
    let relative_path = reference_path.with_extension(SCRIPT_EXTENSION);
    let path = project.artworks_directory().join(&relative_path);
    if !path.is_file() {
        return Err(Failure::from(Diagnostic::new(
            "artwork_not_found",
            format!("no artwork matches \"{reference}\" in the artworks directory"),
        ))
        .into());
    }
    Ok(ArtworkFile {
        path,
        relative_path,
    })
}

pub struct GeneratedArtwork {
    pub canvas: Pixmap,
    pub seed: i64,
}

/// Runs a script once. The same script, tokens and seed give the same pixels on a given build
/// and platform.
pub fn generate(
    file: &ArtworkFile,
    tokens: Rc<Tokens>,
    seed_override: Option<i64>,
) -> Result<GeneratedArtwork, Failure> {
    let name = file.display_name();
    let at_script = |diagnostic: Diagnostic| Failure::from(diagnostic.at(&name));
    // Lua already starts these messages with the file and line.
    let script_failure =
        |error: mlua::Error| Failure::from(Diagnostic::new("artwork_error", root_message(&error)));

    let source = fs::read_to_string(&file.path).map_err(|error| {
        at_script(Diagnostic::new(
            "artwork_unreadable",
            format!("{}: {error}", file.path.display()),
        ))
    })?;
    let lua = Lua::new_with(
        StdLib::MATH | StdLib::STRING | StdLib::TABLE,
        LuaOptions::default(),
    )
    .map_err(script_failure)?;
    let readonly: Function = lua
        .load(SANDBOX_PRELUDE)
        .set_name("=sandbox")
        .eval()
        .map_err(script_failure)?;
    let resolved_tokens = lua
        .to_value(&tokens.resolved()?)
        .and_then(|value| readonly.call::<Value>(value))
        .map_err(script_failure)?;
    limit_script_resources(&lua, MAX_SCRIPT_INSTRUCTIONS).map_err(script_failure)?;

    let script = lua
        .load(&source)
        .set_name(format!("@{name}"))
        .eval::<Value>()
        .map_err(script_failure)?;
    let Value::Table(script) = script else {
        return Err(at_script(Diagnostic::new(
            "invalid_artwork",
            "an artwork must return a table with width, height, seed and draw",
        )));
    };

    let width = script_dimension(&lua, &script, "width", &tokens).map_err(at_script)?;
    let height = script_dimension(&lua, &script, "height", &tokens).map_err(at_script)?;
    let (width, height) = renderer::checked_raster_size(width, height).map_err(at_script)?;
    let declared_seed = script.get::<Option<i64>>("seed").map_err(|_| {
        at_script(Diagnostic::new(
            "invalid_artwork",
            "seed must be an integer",
        ))
    })?;
    let seed = seed_override.or(declared_seed).ok_or_else(|| {
        at_script(Diagnostic::new(
            "invalid_artwork",
            "seed is missing; declare it in the artwork or pass --seed",
        ))
    })?;
    let draw = script
        .get::<Option<Function>>("draw")
        .ok()
        .flatten()
        .ok_or_else(|| {
            at_script(Diagnostic::new(
                "invalid_artwork",
                "draw must be a function",
            ))
        })?;

    let canvas = lua
        .create_userdata(ScriptCanvas {
            canvas: ArtworkCanvas::new(width, height),
            tokens,
        })
        .map_err(script_failure)?;
    let random = random_function(&lua, seed).map_err(script_failure)?;
    let noise = noise_function(&lua, seed).map_err(script_failure)?;
    draw.call::<()>((&canvas, resolved_tokens, random, noise))
        .map_err(script_failure)?;

    let canvas = canvas
        .take::<ScriptCanvas>()
        .map_err(script_failure)?
        .canvas
        .into_pixmap();
    Ok(GeneratedArtwork { canvas, seed })
}

/// Turns a runaway script into an error. Instructions are counted because a time limit would make
/// whether a script succeeds depend on the machine.
fn limit_script_resources(lua: &Lua, max_instructions: u64) -> mlua::Result<()> {
    lua.set_memory_limit(MAX_SCRIPT_MEMORY_BYTES)?;
    let checks_left = Cell::new(max_instructions / u64::from(INSTRUCTIONS_BETWEEN_LIMIT_CHECKS));
    lua.set_hook(
        HookTriggers::new().every_nth_instruction(INSTRUCTIONS_BETWEEN_LIMIT_CHECKS),
        move |lua, _| {
            checks_left.set(checks_left.get().saturating_sub(1));
            if checks_left.get() == 0 {
                return Err(error_at_stack_level(
                    lua,
                    0,
                    format!("exceeded the limit of {max_instructions} instructions"),
                ));
            }
            Ok(VmState::Continue)
        },
    )
}

/// A callback's own error, with the script line that called it, which Lua only adds for `error`.
fn located_error(lua: &Lua, message: impl std::fmt::Display) -> mlua::Error {
    error_at_stack_level(lua, 1, message)
}

fn error_at_stack_level(
    lua: &Lua,
    stack_level: usize,
    message: impl std::fmt::Display,
) -> mlua::Error {
    let location = lua.inspect_stack(stack_level, |frame| {
        let source = frame.source();
        Some((source.short_src?.into_owned(), frame.current_line()?))
    });
    match location.flatten() {
        Some((source, line)) => mlua::Error::runtime(format!("{source}:{line}: {message}")),
        None => mlua::Error::runtime(message.to_string()),
    }
}

/// The message the author needs, without the traceback layers `mlua` wraps around it.
fn root_message(error: &mlua::Error) -> String {
    match error {
        mlua::Error::CallbackError { cause, .. } => root_message(cause),
        mlua::Error::RuntimeError(message) | mlua::Error::SyntaxError { message, .. } => {
            message.clone()
        }
        mlua::Error::MemoryError(_) => {
            format!("exceeded the limit of {MAX_SCRIPT_MEMORY_BYTES} bytes of memory")
        }
        other => other.to_string(),
    }
}

fn script_dimension(
    lua: &Lua,
    script: &Table,
    field: &'static str,
    tokens: &Tokens,
) -> Result<f64, Diagnostic> {
    let invalid = || {
        Diagnostic::new(
            "invalid_artwork",
            format!("{field} must be a number or a token reference"),
        )
    };
    let value = script.get::<Value>(field).map_err(|_| invalid())?;
    let value: NumberValue = lua.from_value(value).map_err(|_| invalid())?;
    tokens.number(&value, field)
}

/// SplitMix64: small, and independent of the Lua version unlike `math.random`.
fn next_random(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mixed = (*state ^ (*state >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    let mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    mixed ^ (mixed >> 31)
}

/// `random()` is within [0, 1) and `random(minimum, maximum)` within [minimum, maximum).
fn random_function(lua: &Lua, seed: i64) -> mlua::Result<Function> {
    let state = Cell::new(seed as u64);
    lua.create_function(move |lua, range: (Option<f64>, Option<f64>)| {
        let mut current_state = state.get();
        let unit = (next_random(&mut current_state) >> 11) as f64 / (1u64 << 53) as f64;
        state.set(current_state);
        match range {
            (None, None) => Ok(unit),
            (Some(minimum), Some(maximum)) => Ok(minimum + (maximum - minimum) * unit),
            _ => Err(located_error(lua, "random takes no bounds or both bounds")),
        }
    })
}

/// `noise(x, y, z?)` is smooth seeded Perlin noise within [-1, 1].
fn noise_function(lua: &Lua, seed: i64) -> mlua::Result<Function> {
    let noise = PerlinNoise::new(seed);
    lua.create_function(move |_, (x, y, z): (f64, f64, Option<f64>)| {
        Ok(noise.sample(x, y, z.unwrap_or(0.0)))
    })
}

struct PerlinNoise {
    permutation: [u8; 512],
}

impl PerlinNoise {
    fn new(seed: i64) -> Self {
        let mut shuffled: [u8; 256] = std::array::from_fn(|index| index as u8);
        let mut state = seed as u64;
        for index in (1..shuffled.len()).rev() {
            let other = next_random(&mut state) % (index as u64 + 1);
            shuffled.swap(index, other as usize);
        }
        Self {
            permutation: std::array::from_fn(|index| shuffled[index % 256]),
        }
    }

    fn sample(&self, x: f64, y: f64, z: f64) -> f64 {
        let cell = |value: f64| (value.floor().rem_euclid(256.0)) as usize;
        let fade = |t: f64| t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
        let lerp = |t: f64, from: f64, to: f64| from + t * (to - from);
        let (cell_x, cell_y, cell_z) = (cell(x), cell(y), cell(z));
        let (x, y, z) = (x - x.floor(), y - y.floor(), z - z.floor());
        let (u, v, w) = (fade(x), fade(y), fade(z));

        let hash = |offset_x: usize, offset_y: usize, offset_z: usize| {
            let table = &self.permutation;
            let row = usize::from(table[cell_x + offset_x]) + cell_y + offset_y;
            table[usize::from(table[row]) + cell_z + offset_z]
        };
        lerp(
            w,
            lerp(
                v,
                lerp(
                    u,
                    gradient(hash(0, 0, 0), x, y, z),
                    gradient(hash(1, 0, 0), x - 1.0, y, z),
                ),
                lerp(
                    u,
                    gradient(hash(0, 1, 0), x, y - 1.0, z),
                    gradient(hash(1, 1, 0), x - 1.0, y - 1.0, z),
                ),
            ),
            lerp(
                v,
                lerp(
                    u,
                    gradient(hash(0, 0, 1), x, y, z - 1.0),
                    gradient(hash(1, 0, 1), x - 1.0, y, z - 1.0),
                ),
                lerp(
                    u,
                    gradient(hash(0, 1, 1), x, y - 1.0, z - 1.0),
                    gradient(hash(1, 1, 1), x - 1.0, y - 1.0, z - 1.0),
                ),
            ),
        )
    }
}

/// Dots the offset with one of the twelve cube-edge directions Perlin's improved noise uses.
fn gradient(hash: u8, x: f64, y: f64, z: f64) -> f64 {
    let hash = hash & 15;
    let first = if hash < 8 { x } else { y };
    let second = match hash {
        0..=3 => y,
        12 | 14 => x,
        _ => z,
    };
    let signed = |value: f64, is_negative: bool| if is_negative { -value } else { value };
    signed(first, hash & 1 != 0) + signed(second, hash & 2 != 0)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RectOptions {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    corner_radius: Option<f64>,
    fill: Option<Fill>,
    stroke: Option<document::Stroke>,
    opacity: Option<f64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CircleOptions {
    x: f64,
    y: f64,
    radius: f64,
    fill: Option<Fill>,
    stroke: Option<document::Stroke>,
    opacity: Option<f64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LineOptions {
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
    stroke: document::Stroke,
    opacity: Option<f64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathOptions {
    segments: Vec<Vec<SegmentPart>>,
    fill: Option<Fill>,
    stroke: Option<document::Stroke>,
    opacity: Option<f64>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum SegmentPart {
    Number(f64),
    Command(String),
}

fn path_segment(parts: &[SegmentPart]) -> Result<ArtworkPathSegment, String> {
    let Some((SegmentPart::Command(command), coordinates)) = parts.split_first() else {
        return Err("a path segment starts with its command".to_owned());
    };
    let coordinates = coordinates
        .iter()
        .map(|part| match part {
            SegmentPart::Number(number) => Ok(*number),
            SegmentPart::Command(text) => Err(format!("\"{text}\" is not a coordinate")),
        })
        .collect::<Result<Vec<f64>, String>>()?;
    match (command.as_str(), coordinates.as_slice()) {
        ("move", &[x, y]) => Ok(ArtworkPathSegment::MoveTo(x, y)),
        ("line", &[x, y]) => Ok(ArtworkPathSegment::LineTo(x, y)),
        ("quad", &[x1, y1, x, y]) => Ok(ArtworkPathSegment::QuadTo(x1, y1, x, y)),
        ("cubic", &[x1, y1, x2, y2, x, y]) => Ok(ArtworkPathSegment::CubicTo(x1, y1, x2, y2, x, y)),
        ("close", []) => Ok(ArtworkPathSegment::Close),
        (command, coordinates) => Err(format!(
            "\"{command}\" with {} coordinates is not a path segment; use move (2), line (2), \
             quad (4), cubic (6) or close (0)",
            coordinates.len()
        )),
    }
}

/// Multiplies a paint's alpha, which keeps per-shape opacity cheap at any shape count.
fn with_opacity(mut paint: Paint, opacity: f64) -> Paint {
    let fade = |alpha: &mut u8| *alpha = (f64::from(*alpha) * opacity).round() as u8;
    match &mut paint {
        Paint::Solid { color } => fade(&mut color.alpha),
        Paint::LinearGradient { stops, .. } | Paint::RadialGradient { stops, .. } => {
            for stop in stops {
                fade(&mut stop.color.alpha);
            }
        }
    }
    paint
}

struct ScriptCanvas {
    canvas: ArtworkCanvas,
    tokens: Rc<Tokens>,
}

impl ScriptCanvas {
    /// Frames need `{ "color": ... }`; a bare `#RRGGBB` is unambiguous here and shorter in a loop.
    fn paint(&self, fill: Fill) -> Result<Paint, Diagnostic> {
        let fill = match fill {
            Fill::Token(text) if !text.starts_with('$') => Fill::Solid(SolidFill { color: text }),
            other => other,
        };
        self.tokens.paint(&fill, "fill")
    }

    fn stroke(&self, stroke: &document::Stroke, opacity: f64) -> Result<Stroke, Diagnostic> {
        let invalid = |field: &str, message: &str| {
            Diagnostic::new("invalid_value", message.to_owned()).at(format!("stroke.{field}"))
        };
        let width = self.tokens.number(&stroke.width, "stroke.width")?;
        if width < 0.0 {
            return Err(invalid("width", "must not be negative"));
        }
        let stroke_opacity = stroke.opacity.unwrap_or(1.0);
        if !(0.0..=1.0).contains(&stroke_opacity) {
            return Err(invalid("opacity", "must be within [0, 1]"));
        }
        let dash = stroke.dash.clone().unwrap_or_default();
        if dash.iter().any(|length| *length < 0.0)
            || (!dash.is_empty() && dash.iter().sum::<f64>() <= 0.0)
        {
            return Err(invalid(
                "dash",
                "lengths must not be negative and must have a positive sum",
            ));
        }
        Ok(Stroke {
            color: self.tokens.color(&stroke.color, "stroke.color")?,
            width,
            opacity: stroke_opacity * opacity,
            cap: stroke.cap.unwrap_or_default(),
            join: stroke.join.unwrap_or_default(),
            dash,
        })
    }

    fn style(
        &self,
        fill: Option<Fill>,
        stroke: Option<&document::Stroke>,
        opacity: Option<f64>,
    ) -> Result<(Option<Paint>, Option<Stroke>), Diagnostic> {
        let opacity = opacity.unwrap_or(1.0);
        if !(0.0..=1.0).contains(&opacity) {
            return Err(Diagnostic::new("invalid_value", "must be within [0, 1]").at("opacity"));
        }
        let fill = fill.map(|fill| self.paint(fill)).transpose()?;
        let stroke = stroke
            .map(|stroke| self.stroke(stroke, opacity))
            .transpose()?;
        Ok((fill.map(|fill| with_opacity(fill, opacity)), stroke))
    }
}

fn parse_options<T: DeserializeOwned>(lua: &Lua, options: Value) -> mlua::Result<T> {
    lua.from_value(options)
        .map_err(|error| located_error(lua, root_message(&error)))
}

impl UserData for ScriptCanvas {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method_mut("clear", |lua, this, fill: Value| {
            let fill: Option<Fill> = parse_options(lua, fill)?;
            let paint = fill
                .map(|fill| this.paint(fill))
                .transpose()
                .map_err(|diagnostic| located_error(lua, diagnostic))?;
            this.canvas.clear(paint.as_ref());
            Ok(())
        });
        methods.add_method_mut("rect", |lua, this, options: Value| {
            let options: RectOptions = parse_options(lua, options)?;
            let (fill, stroke) = this
                .style(options.fill, options.stroke.as_ref(), options.opacity)
                .map_err(|diagnostic| located_error(lua, diagnostic))?;
            let bounds = Bounds {
                x: options.x,
                y: options.y,
                width: options.width,
                height: options.height,
            };
            let corner_radius = options.corner_radius.unwrap_or(0.0);
            this.canvas
                .rect(bounds, corner_radius, fill.as_ref(), stroke.as_ref());
            Ok(())
        });
        methods.add_method_mut("circle", |lua, this, options: Value| {
            let options: CircleOptions = parse_options(lua, options)?;
            let (fill, stroke) = this
                .style(options.fill, options.stroke.as_ref(), options.opacity)
                .map_err(|diagnostic| located_error(lua, diagnostic))?;
            let bounds = Bounds {
                x: options.x - options.radius,
                y: options.y - options.radius,
                width: options.radius * 2.0,
                height: options.radius * 2.0,
            };
            this.canvas.ellipse(bounds, fill.as_ref(), stroke.as_ref());
            Ok(())
        });
        methods.add_method_mut("line", |lua, this, options: Value| {
            let options: LineOptions = parse_options(lua, options)?;
            let (_, stroke) = this
                .style(None, Some(&options.stroke), options.opacity)
                .map_err(|diagnostic| located_error(lua, diagnostic))?;
            let segments = [
                ArtworkPathSegment::MoveTo(options.x1, options.y1),
                ArtworkPathSegment::LineTo(options.x2, options.y2),
            ];
            this.canvas.path(&segments, None, stroke.as_ref());
            Ok(())
        });
        methods.add_method_mut("path", |lua, this, options: Value| {
            let options: PathOptions = parse_options(lua, options)?;
            let segments = options
                .segments
                .iter()
                .map(|parts| path_segment(parts))
                .collect::<Result<Vec<_>, String>>()
                .map_err(|message| located_error(lua, message))?;
            let (fill, stroke) = this
                .style(options.fill, options.stroke.as_ref(), options.opacity)
                .map_err(|diagnostic| located_error(lua, diagnostic))?;
            this.canvas.path(&segments, fill.as_ref(), stroke.as_ref());
            Ok(())
        });
        methods.add_method_mut("push", |_, this, ()| {
            this.canvas.save_transform();
            Ok(())
        });
        methods.add_method_mut("pop", |lua, this, ()| {
            if this.canvas.restore_transform() {
                Ok(())
            } else {
                Err(located_error(lua, "pop without a matching push"))
            }
        });
        methods.add_method_mut("translate", |_, this, (x, y): (f64, f64)| {
            this.canvas.apply_transform(Affine::translate(x, y));
            Ok(())
        });
        methods.add_method_mut("rotate", |_, this, degrees: f64| {
            this.canvas.apply_transform(Affine::rotate(degrees));
            Ok(())
        });
        methods.add_method_mut("scale", |_, this, (x, y): (f64, Option<f64>)| {
            this.canvas
                .apply_transform(Affine::scale(x, y.unwrap_or(x)));
            Ok(())
        });
        methods.add_method_mut("shade", |lua, this, color_at: Function| {
            this.canvas.shade(|x, y| {
                let (red, green, blue, alpha) =
                    color_at.call::<(f64, f64, f64, Option<f64>)>((x, y))?;
                let color = [red, green, blue, alpha.unwrap_or(1.0)];
                if color.iter().any(|channel| channel.is_nan()) {
                    return Err(located_error(lua, "shade returned a channel that is NaN"));
                }
                Ok(color)
            })
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKENS: &str = r##"{ "colors": { "ink": "#102030", "alias": "$colors.ink" },
        "sizes": { "tile": 8 } }"##;

    fn run(source: &str, seed_override: Option<i64>) -> Result<GeneratedArtwork, Failure> {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("test.lua");
        fs::write(&path, source).unwrap();
        let file = ArtworkFile {
            path,
            relative_path: "test.lua".into(),
        };
        generate(
            &file,
            Rc::new(Tokens::parse(TOKENS).unwrap()),
            seed_override,
        )
    }

    fn script(draw_body: &str) -> String {
        format!(
            "return {{ width = 8, height = 8, seed = 1, \
             draw = function(canvas, tokens, random, noise)\n{draw_body}\nend }}"
        )
    }

    fn failure_message(source: &str) -> String {
        match run(source, None) {
            Ok(_) => panic!("expected a failure from {source}"),
            Err(failure) => failure.to_string(),
        }
    }

    #[test]
    fn draws_token_colors_and_keeps_the_declared_size() {
        let generated = run(
            r#"return { width = "$sizes.tile", height = 4, seed = 1,
                draw = function(canvas) canvas:clear("$colors.alias") end }"#,
            None,
        )
        .unwrap();
        assert_eq!(
            (generated.canvas.width(), generated.canvas.height()),
            (8, 4)
        );
        let pixel = generated.canvas.pixel(3, 3).unwrap();
        assert_eq!(
            (pixel.red(), pixel.green(), pixel.blue(), pixel.alpha()),
            (0x10, 0x20, 0x30, 255)
        );
    }

    #[test]
    fn the_same_seed_gives_the_same_pixels_across_lua_states() {
        // Twenty keys make an order change from Lua's per-state hash seed all but certain.
        let source = script(
            r##"
            local bag = {}
            for index = 1, 20 do bag["key" .. index] = index end
            local x = 0
            for _, value in pairs(bag) do
              canvas:rect { x = x % 8, y = value % 8, width = 1, height = 1,
                            fill = "#FFFFFF", opacity = random() }
              x = x + 1
            end
            canvas:shade(function(x, y)
              return noise(x / 3, y / 3), random(), 0, 0.5
            end)
            "##,
        );
        let first = run(&source, None).unwrap().canvas;
        for _ in 0..5 {
            assert_eq!(run(&source, None).unwrap().canvas.data(), first.data());
        }
        let reseeded = run(&source, Some(2)).unwrap();
        assert_eq!(reseeded.seed, 2);
        assert_ne!(reseeded.canvas.data(), first.data());
    }

    #[test]
    fn sorting_is_stable() {
        run(
            &script(
                r#"
                local items = {}
                for index = 1, 500 do items[index] = { rank = index % 3, order = index } end
                table.sort(items, function(left, right) return left.rank < right.rank end)
                for index = 2, 500 do
                  local before, after = items[index - 1], items[index]
                  assert(before.rank < after.rank
                    or (before.rank == after.rank and before.order < after.order))
                end
                "#,
            ),
            None,
        )
        .unwrap();
    }

    #[test]
    fn noise_is_smooth_bounded_and_seeded() {
        let noise = PerlinNoise::new(1);
        for step in 0..2000 {
            let x = f64::from(step) * 0.037 - 30.0;
            let value = noise.sample(x, x * 0.7, 0.3);
            assert!((-1.0..=1.0).contains(&value), "{value}");
            let nearby = noise.sample(x + 0.001, x * 0.7, 0.3);
            assert!((value - nearby).abs() < 0.01);
        }
        assert_eq!(noise.sample(2.0, 3.0, 4.0), 0.0);
        assert_ne!(
            noise.sample(0.4, 0.4, 0.4),
            PerlinNoise::new(2).sample(0.4, 0.4, 0.4)
        );
    }

    #[test]
    fn opacity_and_transforms_reach_the_pixels() {
        let generated = run(
            &script(
                r##"
                canvas:push()
                canvas:translate(4, 4)
                canvas:rect { x = 0, y = 0, width = 4, height = 4, fill = "#FFFFFF" }
                canvas:pop()
                canvas:rect { x = 0, y = 0, width = 4, height = 4, fill = "#FFFFFF", opacity = 0.5 }
                "##,
            ),
            None,
        )
        .unwrap();
        assert_eq!(generated.canvas.pixel(6, 6).unwrap().alpha(), 255);
        assert_eq!(generated.canvas.pixel(1, 1).unwrap().alpha(), 128);
        assert_eq!(generated.canvas.pixel(6, 1).unwrap().alpha(), 0);
    }

    #[test]
    fn paths_gradients_and_shade_draw() {
        let generated = run(
            &script(
                r##"
                canvas:path {
                  segments = { { "move", 0, 0 }, { "line", 8, 0 }, { "quad", 8, 8, 0, 8 },
                               { "cubic", 0, 6, 0, 2, 0, 0 }, { "close" } },
                  fill = { type = "linear-gradient", angle = 90, stops = {
                    { offset = 0, color = "#000000" }, { offset = 1, color = "$colors.ink" } } },
                }
                canvas:line { x1 = 0, y1 = 7, x2 = 8, y2 = 7,
                              stroke = { color = "#FFFFFF", width = 2 } }
                "##,
            ),
            None,
        )
        .unwrap();
        assert_eq!(generated.canvas.pixel(1, 1).unwrap().alpha(), 255);
        assert_eq!(generated.canvas.pixel(4, 7).unwrap().red(), 255);

        let shaded = run(
            &script(
                r##"canvas:clear("#000000")
                   canvas:shade(function(x, y) return 1, 1, 1, x == 0 and 1 or 0 end)"##,
            ),
            None,
        )
        .unwrap();
        assert_eq!(shaded.canvas.pixel(0, 5).unwrap().red(), 255);
        assert_eq!(shaded.canvas.pixel(1, 5).unwrap().red(), 0);
    }

    #[test]
    fn the_sandbox_hides_everything_that_escapes_or_varies() {
        for name in [
            "os",
            "io",
            "debug",
            "package",
            "require",
            "load",
            "loadfile",
            "dofile",
            "print",
            "pcall",
            "xpcall",
            "next",
            "collectgarbage",
            "coroutine",
        ] {
            run(&script(&format!("assert({name} == nil, '{name}')")), None).unwrap();
        }
        for name in ["math.random", "math.randomseed", "string.dump"] {
            run(&script(&format!("assert({name} == nil, '{name}')")), None).unwrap();
        }
    }

    #[test]
    fn tokens_are_read_only_but_iterable() {
        run(
            &script(
                r##"
                assert(tokens.colors.ink == "#102030" and tokens.colors.alias == "#102030")
                local names = {}
                for name in pairs(tokens.colors) do names[#names + 1] = name end
                assert(table.concat(names, ",") == "alias,ink")
                "##,
            ),
            None,
        )
        .unwrap();
        assert!(
            failure_message(&script("tokens.colors.ink = 1")).contains("test.lua:2:"),
            "the error must name the line"
        );
    }

    #[test]
    fn errors_name_the_script_line() {
        assert!(failure_message(&script("local a = nil + 1")).contains("test.lua:2:"));
        assert!(
            failure_message(&script(r#"canvas:clear("$colors.nope")"#)).contains("test.lua:2:")
        );
        assert!(failure_message(&script("canvas:circle { x = 1 }")).contains("test.lua:2:"));
        assert!(failure_message(&script("canvas:pop()")).contains("test.lua:2:"));
        assert!(failure_message("return {").contains("test.lua:1:"));
    }

    #[test]
    fn invalid_scripts_fail_with_their_reason() {
        for (source, expected) in [
            ("return 1", "must return a table"),
            ("return { height = 1, seed = 1, draw = print }", "width"),
            (
                "return { width = 1.5, height = 1, seed = 1, draw = function() end }",
                "whole number",
            ),
            (
                "return { width = 99999, height = 1, seed = 1, draw = function() end }",
                "limits",
            ),
            (
                "return { width = 1, height = 1, draw = function() end }",
                "--seed",
            ),
            ("return { width = 1, height = 1, seed = 1 }", "draw must be"),
        ] {
            let message = failure_message(source);
            assert!(message.contains(expected), "{source}: {message}");
        }
    }

    #[test]
    fn runaway_loops_stop_at_their_line() {
        let lua = Lua::new_with(StdLib::NONE, LuaOptions::default()).unwrap();
        limit_script_resources(&lua, 5_000_000).unwrap();
        let error = lua
            .load("\nwhile true do end")
            .set_name("@test.lua")
            .exec()
            .unwrap_err();
        let message = root_message(&error);
        assert!(message.contains("test.lua:2:"), "{message}");
        assert!(message.contains("5000000 instructions"), "{message}");
    }

    #[test]
    fn memory_is_limited() {
        let message = failure_message(&script(
            r#"local parts = {}
               for index = 1, 1e9 do parts[index] = string.rep("x", 1024 * 1024) .. index end"#,
        ));
        assert!(message.contains("memory"), "{message}");
    }

    #[test]
    fn missing_seed_is_fine_with_an_override() {
        let source = "return { width = 1, height = 1, draw = function() end }";
        assert_eq!(run(source, Some(9)).unwrap().seed, 9);
    }
}
