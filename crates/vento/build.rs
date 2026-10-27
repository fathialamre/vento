fn main() {
    // Text in `.slint` files is translated from the same catalogs the Rust
    // code reads; a plain `@tr` has no context unless it gives one.
    let config = slint_build::CompilerConfiguration::new()
        .with_bundled_translations("../../lang")
        .with_default_translation_context(slint_build::DefaultTranslationContext::None);
    slint_build::compile_with_config("ui/app.slint", config).expect("failed to compile app.slint");
}
