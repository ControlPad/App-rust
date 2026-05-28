fn main() {
    println!(
        "cargo:rustc-env=SLIDR_BUILD_DATE={}",
        chrono::Utc::now().format("%Y-%m-%d %H:%M UTC")
    );
    println!("cargo:rerun-if-changed=ui/");
    println!("cargo:rerun-if-changed=assets/slidr.rc");
    println!("cargo:rerun-if-changed=assets/logo.ico");
    slint_build::compile_with_config(
        "ui/app.slint",
        slint_build::CompilerConfiguration::new().with_style("material".into()),
    )
    .unwrap();

    // Embed Windows .exe icon
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("assets/slidr.rc", embed_resource::NONE)
            .manifest_optional()
            .unwrap();
    }
}
