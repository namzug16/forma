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

fn error_code(output: &Output) -> Value {
    assert!(!output.status.success());
    json_output(output)["errors"][0]["code"].clone()
}

fn succeed(directory: &Path, arguments: &[&str]) -> Value {
    let output = forma(directory, arguments);
    assert!(
        output.status.success(),
        "{arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    json_output(&output)
}

fn strings(value: &Value) -> Vec<&str> {
    let items = value.as_array().unwrap();
    items.iter().map(|item| item.as_str().unwrap()).collect()
}

/// Rendering must not need the network; the macOS sandbox makes that a real restriction.
fn render_offline(directory: &Path, frame: &str) -> Output {
    let forma_binary = env!("CARGO_BIN_EXE_forma");
    let mut command = if cfg!(target_os = "macos") {
        let mut sandbox = Command::new("sandbox-exec");
        sandbox.args([
            "-p",
            "(version 1)(allow default)(deny network*)",
            forma_binary,
        ]);
        sandbox
    } else {
        Command::new(forma_binary)
    };
    command
        .args(["render", frame, "--json"])
        .current_dir(directory)
        .output()
        .unwrap()
}

/// Talks to the real Iconify and Fontsource services, so it needs internet access.
#[test]
fn an_agent_installs_real_icons_and_fonts_and_renders_them_offline() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    assert!(forma(&root, &["init"]).status.success());
    let project = root.join("forma");

    // Icons: the file on disk is exactly what Iconify serves.
    let found = succeed(&root, &["icons", "search", "star", "--json"]);
    assert!(strings(&found["icons"]).contains(&"star"));
    let added = succeed(&root, &["icons", "add", "star", "--json"]);
    assert_eq!(added["icon"]["src"], "icons/lucide/star.svg");
    let star_path = project.join("assets/icons/lucide/star.svg");
    let served = ureq::get("https://api.iconify.design/lucide/star.svg")
        .call()
        .unwrap()
        .body_mut()
        .read_to_vec()
        .unwrap();
    assert_eq!(fs::read(&star_path).unwrap(), served);
    let installed = succeed(&root, &["icons", "list", "--json"]);
    assert_eq!(installed["icons"][0]["license"], "ISC");

    let readd = forma(&root, &["icons", "add", "star", "--json"]);
    assert_eq!(error_code(&readd), "icon_already_installed");
    assert_eq!(fs::read(&star_path).unwrap(), served);
    let missing = forma(&root, &["icons", "add", "no-such-icon-x", "--json"]);
    assert_eq!(error_code(&missing), "icon_not_found");
    let escaping = forma(&root, &["icons", "add", "../secret", "--json"]);
    assert_eq!(error_code(&escaping), "invalid_icon_name");

    // Fonts: a real install, then a second weight joining it.
    let found = succeed(&root, &["fonts", "search", "Inter", "--json"]);
    let is_catalog_inter =
        |font: &Value| font["family"] == "Inter" && font["source"] == "fontsource";
    assert!(
        found["fonts"]
            .as_array()
            .unwrap()
            .iter()
            .any(is_catalog_inter)
    );
    let info = succeed(&root, &["fonts", "info", "Inter", "--json"]);
    assert_eq!(info["catalog"]["license"], "OFL-1.1");

    let regular = [
        "fonts", "add", "Inter", "--weight", "400", "--style", "normal", "--json",
    ];
    let added = succeed(&root, &regular);
    let installed_files = strings(&added["font"]["installed"]);
    assert!(installed_files.contains(&"latin-400-normal.ttf"));
    assert!(installed_files.contains(&"cyrillic-400-normal.ttf"));
    let inter = project.join("assets/fonts/inter");
    assert!(
        fs::read_to_string(inter.join("LICENSE"))
            .unwrap()
            .contains("Open Font License")
    );
    let record: Value =
        serde_json::from_slice(&fs::read(inter.join("font.json")).unwrap()).unwrap();
    assert_eq!(record["license"], "OFL-1.1");

    assert_eq!(
        error_code(&forma(&root, &regular)),
        "font_already_installed"
    );
    succeed(
        &root,
        &[
            "fonts", "add", "Inter", "--weight", "700", "--style", "normal", "--json",
        ],
    );
    let unavailable = forma(
        &root,
        &["fonts", "add", "Inter", "--weight", "950", "--json"],
    );
    assert_eq!(error_code(&unavailable), "font_variant_unavailable");
    let unknown = forma(
        &root,
        &["fonts", "add", "No Such Family Anywhere", "--json"],
    );
    assert_eq!(error_code(&unknown), "font_not_found");

    let checked = succeed(&root, &["fonts", "check", "Inter", "--json"]);
    assert_eq!(checked["font"]["source"], "project");
    assert_eq!(checked["font"]["weights"], json!([400, 700]));

    // Its files embed the name "Noto Sans JP Thin", and its catalog lists numbered slices.
    let added = succeed(
        &root,
        &[
            "fonts",
            "add",
            "Noto Sans JP",
            "--weight",
            "400",
            "--subset",
            "japanese,latin",
            "--json",
        ],
    );
    assert_eq!(
        strings(&added["font"]["installed"]),
        ["japanese-400-normal.ttf", "latin-400-normal.ttf"]
    );

    // Everything installed works in every frame with the network gone.
    for frame in ["one", "two"] {
        let source = format!(
            r##"{{ "version": 1, "id": "{frame}", "width": 400, "height": 200, "children": [
                {{ "id": "star", "type": "svg", "src": "icons/lucide/star.svg", "width": 48,
                   "height": 48, "color": "#FF0000" }},
                {{ "id": "latin", "type": "text", "text": "Hello Привет", "y": 60,
                   "font": {{ "family": "Inter", "size": 20, "weight": 700 }},
                   "fill": {{ "color": "#000000" }} }},
                {{ "id": "japanese", "type": "text", "text": "こんにちは Noto", "y": 100,
                   "font": {{ "family": "Noto Sans JP", "size": 20 }},
                   "fill": {{ "color": "#000000" }} }} ] }}"##
        );
        fs::write(project.join(format!("frames/{frame}.json")), source).unwrap();
        let render = render_offline(&root, frame);
        assert!(
            render.status.success(),
            "{}",
            String::from_utf8_lossy(&render.stderr)
        );
        assert_eq!(json_output(&render)["warnings"], json!([]));
    }

    let canvas = tiny_skia::Pixmap::load_png(project.join("output/one.png")).unwrap();
    let is_red = |pixel: &tiny_skia::PremultipliedColorU8| {
        pixel.red() > 200 && pixel.green() == 0 && pixel.alpha() == 255
    };
    assert!(canvas.pixels().iter().any(is_red));

    let report = succeed(&root, &["inspect", "one", "--json"]);
    let font_source = report["layers"][2]["fontSource"].as_str().unwrap();
    assert!(
        font_source.contains("assets/fonts/noto-sans-jp"),
        "{font_source}"
    );
}

/// Copies a real system font, which only macOS guarantees by name.
#[cfg(target_os = "macos")]
#[test]
fn a_system_font_is_copied_only_after_accepting_its_license() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    assert!(forma(&root, &["init"]).status.success());
    let add = [
        "fonts",
        "add",
        "Helvetica Neue",
        "--from",
        "system",
        "--json",
    ];

    assert_eq!(error_code(&forma(&root, &add)), "font_license_not_accepted");
    let source = succeed(&root, &["fonts", "check", "Helvetica Neue", "--json"]);
    assert_eq!(source["font"]["source"], "system");

    let accepted = [&add[..], &["--accept-license"]].concat();
    let copied = succeed(&root, &accepted);
    assert_eq!(strings(&copied["font"]["installed"]), ["HelveticaNeue.ttc"]);
    assert!(
        copied["font"]["license"]
            .as_str()
            .unwrap()
            .contains("Copyright")
    );
    let source = succeed(&root, &["fonts", "check", "Helvetica Neue", "--json"]);
    assert_eq!(source["font"]["source"], "project");

    assert_eq!(
        error_code(&forma(&root, &accepted)),
        "font_already_installed"
    );
}

#[test]
fn an_artwork_goes_from_init_through_generate_into_a_frame() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let project = root.join("forma");
    assert!(forma(&root, &["init"]).status.success());
    fs::write(
        project.join("frames/poster.json"),
        r#"{ "version": 1, "id": "poster", "width": 100, "height": 100, "children": [
            { "id": "art", "type": "image", "src": "artworks/example.png",
              "x": 0, "y": 0, "width": 100, "height": 100 } ] }"#,
    )
    .unwrap();

    // Rendering never runs a script, it only says what to do.
    let missing = forma(&root, &["render", "poster"]);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("forma generate"));

    let generated = forma(&root, &["generate", "example", "--json"]);
    assert!(generated.status.success());
    let result = json_output(&generated);
    assert_eq!(result["seed"], json!(7));
    let output = project.join("assets/artworks/example.png");
    assert_eq!(result["output"], json!(output));
    assert_eq!(png_size(&output), (1080, 1080));
    let first_generation = fs::read(&output).unwrap();

    assert!(forma(&root, &["generate"]).status.success());
    assert_eq!(fs::read(&output).unwrap(), first_generation);
    assert!(forma(&root, &["generate", "--seed", "8"]).status.success());
    assert_ne!(fs::read(&output).unwrap(), first_generation);
    assert!(forma(&root, &["render", "poster"]).status.success());

    // One broken artwork must not hide the others, and must fail the run.
    fs::write(project.join("artworks/broken.lua"), "return 1").unwrap();
    let batch = forma(&root, &["generate", "--json"]);
    assert!(!batch.status.success());
    let batch = json_output(&batch);
    assert_eq!(batch["results"][0]["artwork"], json!("broken.lua"));
    assert_eq!(batch["results"][0]["success"], json!(false));
    assert_eq!(batch["results"][1]["success"], json!(true));

    let outside = forma(&root, &["generate", "../frames/poster"]);
    assert!(!outside.status.success());
    assert!(String::from_utf8_lossy(&outside.stderr).contains("invalid_artwork_reference"));
}

#[test]
fn rendering_every_frame_as_json_prints_only_json() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    assert!(forma(&root, &["init"]).status.success());

    let rendered = forma(&root, &["render", "--all", "--json"]);
    assert!(rendered.status.success());
    let result = json_output(&rendered);
    assert_eq!(result["success"], json!(true));
    assert_eq!(result["results"][0]["frame"], json!("example"));
}

#[test]
fn projects_created_before_artworks_still_load() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    assert!(forma(&root, &["init"]).status.success());
    let config_path = root.join("forma/config.json");
    let config = fs::read_to_string(&config_path).unwrap();
    fs::write(
        &config_path,
        config.replace("  \"artworks\": \"./artworks\",\n", ""),
    )
    .unwrap();
    assert!(forma(&root, &["list"]).status.success());
    assert!(forma(&root, &["generate", "example"]).status.success());
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

    // The clipboard replaces the file, whether or not this machine has a clipboard.
    fs::remove_dir_all(project.join("output")).unwrap();
    forma(&root, &["render", "post", "--clipboard"]);
    assert!(!project.join("output").exists());
    let both_destinations = forma(
        &root,
        &["render", "post", "--clipboard", "--output", "both.png"],
    );
    assert!(!both_destinations.status.success());
    assert!(!root.join("both.png").exists());

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
