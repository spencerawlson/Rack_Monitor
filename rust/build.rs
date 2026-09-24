//! Compiles the rack icon into the executable's resource section, so Explorer,
//! the taskbar and Task Manager show it rather than the default Rust icon. The
//! same file is served to browsers by `web.rs`.

fn main() {
    println!("cargo:rerun-if-changed=app.rc");
    println!("cargo:rerun-if-changed=web/favicon.ico");
    // `compile` is a no-op on non-Windows targets. `manifest_optional` fails
    // the build on a real resource-compiler error but tolerates a toolchain
    // that cannot run one, which keeps cross-checking other targets simple.
    embed_resource::compile("app.rc", embed_resource::NONE).manifest_optional().unwrap();
}
