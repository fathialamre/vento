fn main() {
    // Text in `.slint` files is translated from the same catalogs the Rust
    // code reads; a plain `@tr` has no context unless it gives one.
    let config = slint_build::CompilerConfiguration::new()
        .with_bundled_translations("../../lang")
        .with_default_translation_context(slint_build::DefaultTranslationContext::None);
    slint_build::compile_with_config("ui/app.slint", config).expect("failed to compile app.slint");

    // The Windows manifest goes into vento.exe through the MSVC linker, so
    // no resource compiler is needed.
    let target = |key| std::env::var(key).unwrap_or_default();
    if target("CARGO_CFG_TARGET_OS") == "windows" && target("CARGO_CFG_TARGET_ENV") == "msvc" {
        let manifest =
            std::path::Path::new(&target("CARGO_MANIFEST_DIR")).join("assets/vento.exe.manifest");
        println!("cargo:rerun-if-changed={}", manifest.display());
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}",
            manifest.display()
        );
    }
}
