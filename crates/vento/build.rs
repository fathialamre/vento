fn main() {
    // Text in `.slint` files is translated from the same catalogs the Rust
    // code reads; a plain `@tr` has no context unless it gives one.
    let config = slint_build::CompilerConfiguration::new()
        .with_bundled_translations("../../lang")
        .with_default_translation_context(slint_build::DefaultTranslationContext::None);
    slint_build::compile_with_config("ui/app.slint", config).expect("failed to compile app.slint");

    let target = |key| std::env::var(key).unwrap_or_default();
    if target("CARGO_CFG_TARGET_OS") == "windows" && target("CARGO_CFG_TARGET_ENV") == "msvc" {
        let assets = std::path::Path::new(&target("CARGO_MANIFEST_DIR")).join("assets");
        // The Windows manifest goes into vento.exe through the MSVC linker.
        let manifest = assets.join("vento.exe.manifest");
        println!("cargo:rerun-if-changed={}", manifest.display());
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}",
            manifest.display()
        );
        windows_resources(&assets, &target("OUT_DIR"), &target("CARGO_PKG_VERSION"));
    }
}

/// The app icon and the version Explorer shows (Properties → Details, and
/// installed apps), compiled with the Windows SDK's resource compiler.
#[cfg(windows)]
fn windows_resources(assets: &std::path::Path, out_dir: &str, version: &str) {
    let icon = assets.join("vento.ico");
    println!("cargo:rerun-if-changed={}", icon.display());
    // "0.2.0" as the four numbers a version resource holds.
    let mut parts: Vec<u16> = version
        .split(['.', '-'])
        .map_while(|p| p.parse().ok())
        .collect();
    parts.resize(4, 0);
    let numeric = parts
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let rc = format!(
        r#"1 ICON "{icon}"
1 VERSIONINFO
FILEVERSION {numeric}
PRODUCTVERSION {numeric}
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904b0"
    BEGIN
      VALUE "CompanyName", "Fathi Alamre"
      VALUE "FileDescription", "Vento"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "vento"
      VALUE "LegalCopyright", "Fathi Alamre. GPL-3.0"
      VALUE "OriginalFilename", "vento.exe"
      VALUE "ProductName", "Vento"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        icon = icon.display().to_string().replace('\\', "\\\\"),
    );
    let path = std::path::Path::new(out_dir).join("vento.rc");
    std::fs::write(&path, rc).expect("cannot write vento.rc");
    embed_resource::compile(&path, embed_resource::NONE)
        .manifest_optional()
        .expect("cannot compile vento.rc");
}

/// Only a Windows host builds the Windows resources.
#[cfg(not(windows))]
fn windows_resources(_assets: &std::path::Path, _out_dir: &str, _version: &str) {}
