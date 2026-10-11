fn main() {
    // Speaker separation links sherpa-onnx and ONNX Runtime as shared
    // libraries (Cargo.toml explains why not static). Their build script
    // copies them next to the binaries in target/<profile>, but a
    // dependency's link args never reach this package's binaries, so the
    // run-time search path is set here: beside the executable (a cargo
    // build, a Windows install dir, a portable unzip) and in the place the
    // installers put bundle resources (`/usr/lib/tiro` for the deb/rpm,
    // `usr/lib/tiro` inside the AppImage, `Contents/Resources` in the app
    // bundle). Windows finds DLLs beside the exe on its own.
    if std::env::var("CARGO_FEATURE_DIARIZE").is_ok() {
        match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
            Ok("linux") => {
                println!("cargo:rustc-link-arg-bins=-Wl,-rpath,$ORIGIN");
                println!("cargo:rustc-link-arg-bins=-Wl,-rpath,$ORIGIN/../lib/tiro");
            }
            Ok("macos") => {
                println!("cargo:rustc-link-arg-bins=-Wl,-rpath,@executable_path");
                println!("cargo:rustc-link-arg-bins=-Wl,-rpath,@executable_path/../Resources");
            }
            _ => {}
        }
    }
    tauri_build::build()
}
