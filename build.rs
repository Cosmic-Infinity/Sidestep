use embed_manifest::manifest::{DpiAwareness, ExecutionLevel};
use embed_manifest::{embed_manifest, new_manifest};

fn main() {
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        // The `preview` feature builds a layout-check copy that runs without
        // elevation and never starts goodbyedpi.
        let level = if std::env::var_os("CARGO_FEATURE_PREVIEW").is_some() {
            ExecutionLevel::AsInvoker
        } else {
            ExecutionLevel::RequireAdministrator
        };
        embed_manifest(
            new_manifest("Sidestep")
                .requested_execution_level(level)
                // nwg (high-dpi feature) scales by the system DPI at start-up.
                .dpi_awareness(DpiAwareness::System),
        )
        .expect("unable to embed manifest");
        // the version resource takes its numbers from Cargo.toml, so the version is set in one place
        let env = |k: &str| std::env::var(k).unwrap();
        let macros = [
            format!("VER_MAJOR={}", env("CARGO_PKG_VERSION_MAJOR")),
            format!("VER_MINOR={}", env("CARGO_PKG_VERSION_MINOR")),
            format!("VER_PATCH={}", env("CARGO_PKG_VERSION_PATCH")),
            format!("VER_STR=\"{}\"", env("CARGO_PKG_VERSION")),
        ];
        embed_resource::compile("sidestep.rc", &macros);
    }
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=sidestep.rc");
    println!("cargo:rerun-if-changed=icons/app.ico");
}
