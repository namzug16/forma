//! Drives the real binary through the agent workflow from the spec.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{Value, json};

const FRAME: &str = r##"{
  "version": 1,
  "id": "post",
  "width": 1080,
  "height": 1080,
  "background": "$gradients.brand",
  "children": [
    { "id": "headline", "type": "text", "text": "DESIGN AS CODE", "x": 64, "y": 64, "width": 600,
      "font": "$typography.body", "fill": { "color": "$colors.foreground" } },
    { "id": "logo", "type": "svg", "src": "svg/logo.svg", "x": 64, "y": 500,
      "width": 120, "height": 120 },
    { "id": "photo", "type": "image", "src": "images/photo.png", "x": 300, "y": 500,
      "width": 200, "height": 100, "fit": "cover" },
    { "id": "overflowing", "type": "rect", "x": 1080, "y": 540, "width": 200, "height": 200,
      "anchor": [0.5, 0.5], "rotation": 45, "fill": { "color": "$colors.primary" } }
  ]
}"##;

fn forma(directory: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_forma"))
        .args(arguments)
        .current_dir(directory)
        .output()
        .unwrap()
}

fn json_output(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap()
}

fn png_size(path: &Path) -> (u32, u32) {
    let bytes = fs::read(path).unwrap();
    let be_u32 = |offset: usize| u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap());
    (be_u32(16), be_u32(20))
}

fn add_project_files(project: &Path) {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    fs::copy(
        fixtures.join("Tuffy.ttf"),
        project.join("assets/fonts/Tuffy.ttf"),
    )
    .unwrap();
    fs::write(
        project.join("assets/svg/logo.svg"),
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect width="10" height="10"/></svg>"#,
    )
    .unwrap();
    let mut photo = tiny_skia::Pixmap::new(4, 4).unwrap();
    photo.fill(tiny_skia::Color::WHITE);
    photo
        .save_png(project.join("assets/images/photo.png"))
        .unwrap();

    let tokens_path = project.join("tokens.json");
    let mut tokens: Value = serde_json::from_slice(&fs::read(&tokens_path).unwrap()).unwrap();
    tokens["typography"]["body"] = json!({ "file": "fonts/Tuffy.ttf", "size": 72 });
    fs::write(&tokens_path, tokens.to_string()).unwrap();
    fs::write(project.join("frames/post.json"), FRAME).unwrap();
}

#[test]
fn installed_icons_and_fonts_render_in_any_frame_from_local_files() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    assert!(forma(&root, &["init"]).status.success());
    let project = root.join("forma");

    // The layout `forma icons add` and `forma fonts add` leave behind.
    let icons = project.join("assets/icons/lucide");
    fs::create_dir_all(&icons).unwrap();
    fs::write(
        icons.join("star.svg"),
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="1em" height="1em" viewBox="0 0 24 24"><path fill="none" stroke="currentColor" stroke-width="2" d="M2 2h20v20z"/></svg>"#,
    )
    .unwrap();
    let font_directory = project.join("assets/fonts/inter");
    fs::create_dir_all(&font_directory).unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for subset in ["latin", "cyrillic"] {
        let name = format!("{subset}-400-normal.ttf");
        fs::copy(
            fixtures.join(format!("Inter-{name}")),
            font_directory.join(name),
        )
        .unwrap();
    }

    for frame in ["one", "two"] {
        let source = format!(
            r##"{{ "version": 1, "id": "{frame}", "width": 200, "height": 100, "children": [
                {{ "id": "star", "type": "svg", "src": "icons/lucide/star.svg", "width": 48,
                   "height": 48, "color": "#FF0000" }},
                {{ "id": "title", "type": "text", "text": "Hello Привет", "y": 50,
                   "font": {{ "family": "Inter", "size": 20 }},
                   "fill": {{ "color": "#000000" }} }} ] }}"##
        );
        fs::write(project.join(format!("frames/{frame}.json")), source).unwrap();
        let render = forma(&root, &["render", frame, "--json"]);
        assert!(
            render.status.success(),
            "{}",
            String::from_utf8_lossy(&render.stderr)
        );
    }

    let report = json_output(&forma(
        &root,
        &["inspect", "one", "--layer", "title", "--json"],
    ));
    let font_source = report["layers"][0]["fontSource"].as_str().unwrap();
    assert!(font_source.contains("assets/fonts/inter"), "{font_source}");

    let found = json_output(&forma(&root, &["fonts", "check", "Inter", "--json"]));
    assert_eq!(found["font"]["source"], "project");
    let missing = forma(
        &root,
        &["fonts", "check", "No Such Family Anywhere", "--json"],
    );
    assert!(!missing.status.success());
    assert_eq!(json_output(&missing)["errors"][0]["code"], "font_not_found");
}

#[test]
fn a_frame_goes_from_init_through_validate_render_and_inspect() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let project: PathBuf = root.join("forma");

    assert!(forma(&root, &["init"]).status.success());
    add_project_files(&project);

    // Repeating init must keep the user's edits.
    let tokens_before = fs::read(project.join("tokens.json")).unwrap();
    assert!(forma(&root, &["init"]).status.success());
    assert_eq!(
        fs::read(project.join("tokens.json")).unwrap(),
        tokens_before
    );
    assert_eq!(
        fs::read_to_string(root.join(".gitignore")).unwrap(),
        "/forma/output/\n"
    );

    // Discovery from a nested directory, with a broken frame listed rather than dropped.
    let broken = r#"{ "version": 1, "id": "b", "width": 10, "height": 10,
        "children": [{ "id": "r", "type": "rect", "width": 1, "height": "$spacing.nope" }] }"#;
    fs::write(project.join("frames/broken.json"), broken).unwrap();
    let listing = json_output(&forma(&project.join("assets/images"), &["list", "--json"]));
    let statuses: Vec<_> = listing["frames"]
        .as_array()
        .unwrap()
        .iter()
        .map(|frame| {
            (
                frame["frame"].as_str().unwrap(),
                frame["valid"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        statuses,
        [
            ("broken.json", false),
            ("example.json", true),
            ("post.json", true)
        ]
    );

    // Field-level validation feedback.
    let invalid = forma(&root, &["validate", "broken", "--json"]);
    assert!(!invalid.status.success());
    let error = &json_output(&invalid)["frames"][0]["errors"][0];
    assert_eq!(error["code"], "unresolved_token");
    assert_eq!(error["path"], "children[0].height");
    assert_eq!(error["layer"], "r");

    // Rendering is repeatable byte for byte.
    let first = forma(&root, &["render", "post", "--json"]);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let result = json_output(&first);
    let output_path = project.join("output/post.png");
    assert_eq!(result["output"], output_path.to_str().unwrap());
    assert_eq!(
        (result["width"].as_u64(), result["height"].as_u64()),
        (Some(1080), Some(1080))
    );
    let first_bytes = fs::read(&output_path).unwrap();
    assert!(forma(&root, &["render", "post"]).status.success());
    assert_eq!(fs::read(&output_path).unwrap(), first_bytes);

    // Scale changes raster size only, and sources are protected.
    let scaled = root.join("scaled.png");
    let scaled_render = forma(
        &root,
        &["render", "post", "--scale", "2", "--output", "scaled.png"],
    );
    assert!(scaled_render.status.success());
    assert_eq!(png_size(&scaled), (2160, 2160));
    let tokens_target = project.join("tokens.json");
    let overwrite = forma(
        &root,
        &[
            "render",
            "post",
            "--output",
            tokens_target.to_str().unwrap(),
        ],
    );
    assert!(!overwrite.status.success());
    assert_eq!(fs::read(&tokens_target).unwrap(), tokens_before);

    // Inspection reports resolved values and the rotated overflow.
    let report = json_output(&forma(&root, &["inspect", "post", "--json"]));
    let layers = report["layers"].as_array().unwrap();
    assert_eq!(layers[0]["id"], "headline");
    assert_eq!(layers[0]["fill"]["color"], "#FFFFFFFF");
    assert_eq!(layers[3]["drawOrder"], 3);
    assert_eq!(layers[3]["overflowsClip"], true);
    let world = layers[3]["worldBounds"]["width"].as_f64().unwrap();
    let clipped = layers[3]["clippedBounds"]["width"].as_f64().unwrap();
    assert!((world - 282.84).abs() < 0.01, "{world}");
    assert!((clipped - 141.42).abs() < 0.01, "{clipped}");

    let single = json_output(&forma(
        &root,
        &["inspect", "post", "--layer", "logo", "--json"],
    ));
    assert_eq!(single["layers"].as_array().unwrap().len(), 1);
    assert_eq!(single["layers"][0]["id"], "logo");
}
