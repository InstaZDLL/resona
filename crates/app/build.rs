fn main() {
    let config = slint_build::CompilerConfiguration::new()
        .with_style("fluent".into())
        // `translations/<lang>/LC_MESSAGES/spytify.po`, built into the binary.
        .with_bundled_translations("translations")
        .with_default_translation_context(slint_build::DefaultTranslationContext::None);
    slint_build::compile_with_config("ui/app-window.slint", config)
        .expect("failed to compile the Slint UI");
}
