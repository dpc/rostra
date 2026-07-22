#[test]
fn built_in_mutation_surfaces_are_private() {
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/ui/fail/*.rs");
}

#[test]
fn extension_macro_needs_no_direct_storage_dependencies() {
    use std::fs;
    use std::process::Command;

    let fixture = tempfile::tempdir().expect("create isolated downstream fixture");
    let crate_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let crate_path = crate_dir
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    fs::write(
        fixture.path().join("Cargo.toml"),
        format!(
            r#"[package]
name = "rostra-client-db-extension-consumer"
version = "0.0.0"
edition = "2024"

[dependencies]
rostra-client-db = {{ path = "{crate_path}" }}

[workspace]
"#
        ),
    )
    .expect("write downstream manifest");
    fs::create_dir(fixture.path().join("src")).expect("create fixture source directory");
    fs::write(
        fixture.path().join("src/main.rs"),
        include_str!("fixtures/extension_macro.rs"),
    )
    .expect("write fixture source");
    fs::copy(
        crate_dir.join("../../Cargo.lock"),
        fixture.path().join("Cargo.lock"),
    )
    .expect("seed fixture lock for vendored/offline dependency resolution");

    let target_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| crate_dir.join("../../target"))
        .join("extension-api-fixture");
    let output = Command::new(env!("CARGO"))
        .args([
            "check",
            "--offline",
            "--quiet",
            "--manifest-path",
            fixture
                .path()
                .join("Cargo.toml")
                .to_str()
                .expect("UTF-8 fixture path"),
        ])
        .env("CARGO_TARGET_DIR", target_dir)
        .output()
        .expect("run cargo check for downstream fixture");
    assert!(
        output.status.success(),
        "minimal downstream fixture failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
