use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::assets::Assets;
use crate::diagnostics::{Diagnostic, Failure};
use crate::frames::FrameFile;
use crate::scene::Resources;
use crate::text::FontLibrary;
use crate::tokens::Tokens;

const PROJECT_DIRECTORY: &str = "forma";
const CONFIG_FILE: &str = "config.json";

const DEFAULT_CONFIG: &str = r#"{
  "version": 1,
  "frames": "./frames",
  "tokens": "./tokens.json",
  "assets": "./assets",
  "output": "./output",
  "render": {
    "format": "png",
    "scale": 1,
    "quality": 95
  }
}
"#;

const DEFAULT_TOKENS: &str = r##"{
  "colors": {
    "background": "#101014",
    "foreground": "#FFFFFF",
    "primary": "#E8FF57"
  },
  "spacing": { "sm": 16, "md": 32, "lg": 64 },
  "sizes": { "logo": 120 },
  "typography": {
    "heading": {
      "family": "Inter",
      "size": 96,
      "weight": 800,
      "lineHeight": 1.05
    }
  },
  "gradients": {
    "brand": {
      "type": "linear-gradient",
      "angle": 135,
      "stops": [
        { "offset": 0, "color": "$colors.primary" },
        { "offset": 1, "color": "#914DFF" }
      ]
    }
  }
}
"##;

const EXAMPLE_FRAME: &str = r##"{
  "version": 1,
  "id": "example",
  "width": 1080,
  "height": 1080,
  "background": { "color": "$colors.background" },
  "children": [
    {
      "id": "panel",
      "type": "rect",
      "x": 540,
      "y": 540,
      "width": 640,
      "height": 640,
      "anchor": [0.5, 0.5],
      "rotation": 12,
      "cornerRadius": 48,
      "fill": "$gradients.brand"
    }
  ]
}
"##;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    pub frames: PathBuf,
    pub tokens: PathBuf,
    pub assets: PathBuf,
    pub output: PathBuf,
    pub render: RenderConfig,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenderConfig {
    pub format: String,
    pub scale: f64,
    /// Reserved for lossy formats; PNG ignores it.
    #[allow(dead_code)]
    pub quality: u8,
}

#[derive(Debug)]
pub struct Project {
    /// The `forma/` directory; every configured path resolves from it.
    pub directory: PathBuf,
    pub config: Config,
}

impl Project {
    /// Uses `explicit_root` when given, otherwise the nearest valid project upward from `start`.
    pub fn locate(explicit_root: Option<&Path>, start: &Path) -> Result<Self> {
        if let Some(root) = explicit_root {
            return Self::load(root);
        }
        for directory in start.ancestors() {
            if !config_path(directory).is_file() {
                continue;
            }
            match Self::load(directory) {
                Ok(project) => return Ok(project),
                Err(error) => eprintln!("warning: skipping invalid project: {error:#}"),
            }
        }
        Err(Failure::from(Diagnostic::new(
            "project_not_found",
            format!(
                "no forma/config.json in {} or its parents; run `forma init` to create one",
                start.display()
            ),
        ))
        .into())
    }

    /// Joins a configured path, dropping the `./` that configs conventionally start with.
    fn resolve(&self, configured_path: &Path) -> PathBuf {
        self.directory.join(configured_path).components().collect()
    }

    pub fn assets_directory(&self) -> PathBuf {
        self.resolve(&self.config.assets)
    }

    pub fn frames_directory(&self) -> PathBuf {
        self.resolve(&self.config.frames)
    }

    pub fn tokens_path(&self) -> PathBuf {
        self.resolve(&self.config.tokens)
    }

    pub fn config_path(&self) -> PathBuf {
        self.directory.join(CONFIG_FILE)
    }

    /// The source tree that rendering must never write into.
    pub fn is_source_path(&self, path: &Path) -> bool {
        let frames = self.frames_directory();
        let assets = self.assets_directory();
        path.starts_with(frames)
            || path.starts_with(assets)
            || path == self.tokens_path()
            || path == self.config_path()
    }

    pub fn default_output_path(&self, file: &FrameFile) -> PathBuf {
        let relative_path = match &file.relative_path {
            Some(relative_path) => relative_path.as_path(),
            None => Path::new(file.path.file_name().unwrap_or_default()),
        };
        self.resolve(&self.config.output)
            .join(relative_path)
            .with_extension("png")
    }

    pub fn tokens(&self) -> Result<Tokens> {
        Tokens::load(&self.tokens_path())
    }

    pub fn resources(&self) -> Result<Resources> {
        Ok(Resources {
            tokens: self.tokens()?,
            assets: Assets::new(self.assets_directory()),
            fonts: FontLibrary::default(),
        })
    }

    fn load(root: &Path) -> Result<Self> {
        let path = config_path(root);
        let text =
            fs::read_to_string(&path).with_context(|| format!("cannot read {}", path.display()))?;
        let config: Config = serde_json::from_str(&text)
            .with_context(|| format!("invalid config {}", path.display()))?;
        anyhow::ensure!(
            config.version == 1,
            "unsupported config version {} in {}",
            config.version,
            path.display()
        );
        let directory = root.join(PROJECT_DIRECTORY);
        let directory = directory
            .canonicalize()
            .with_context(|| format!("cannot resolve {}", directory.display()))?;
        Ok(Self { directory, config })
    }
}

fn config_path(root: &Path) -> PathBuf {
    root.join(PROJECT_DIRECTORY).join(CONFIG_FILE)
}

#[derive(Debug, Default)]
pub struct InitReport {
    pub created: Vec<PathBuf>,
    pub already_present: Vec<PathBuf>,
}

/// Creates the project skeleton. Existing files are reported, never overwritten.
pub fn initialize(root: &Path) -> Result<InitReport> {
    let mut report = InitReport::default();
    let directory = root.join(PROJECT_DIRECTORY);
    let starter_files = [
        ("config.json", DEFAULT_CONFIG),
        ("tokens.json", DEFAULT_TOKENS),
        ("frames/example.json", EXAMPLE_FRAME),
        ("assets/fonts/.gitkeep", ""),
        ("assets/images/.gitkeep", ""),
        ("assets/svg/.gitkeep", ""),
    ];
    for (relative_path, contents) in starter_files {
        let path = directory.join(relative_path);
        if path.exists() {
            report.already_present.push(path);
            continue;
        }
        let parent = path.parent().expect("starter paths have a parent");
        fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
        fs::write(&path, contents).with_context(|| format!("cannot write {}", path.display()))?;
        report.created.push(path);
    }
    ignore_output_directory(root, &mut report)?;
    Ok(report)
}

fn ignore_output_directory(root: &Path, report: &mut InitReport) -> Result<()> {
    const IGNORE_RULE: &str = "/forma/output/";
    let path = root.join(".gitignore");
    let existing = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(error).with_context(|| format!("cannot read {}", path.display()));
        }
    };
    if existing.lines().any(|line| line.trim() == IGNORE_RULE) {
        report.already_present.push(path);
        return Ok(());
    }
    let separator = if existing.is_empty() || existing.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    fs::write(&path, format!("{existing}{separator}{IGNORE_RULE}\n"))
        .with_context(|| format!("cannot write {}", path.display()))?;
    report.created.push(path);
    Ok(())
}
