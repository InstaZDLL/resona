fn main() {
    let config = slint_build::CompilerConfiguration::new()
        .with_style("fluent".into())
        // `translations/<lang>/LC_MESSAGES/resona.po`, built into the binary.
        .with_bundled_translations("translations")
        .with_default_translation_context(slint_build::DefaultTranslationContext::None);
    slint_build::compile_with_config("ui/app-window.slint", config)
        .expect("failed to compile the Slint UI");

    // The icon Explorer, the taskbar and the installer show for resona.exe.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winresource::WindowsResource::new();
        resource
            .set_icon("assets/resona.ico")
            .set("FileDescription", "Resona")
            .set("ProductName", "Resona");
        resource
            .compile()
            .expect("failed to embed the Windows resources");
    }
}
