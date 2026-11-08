// The GNU Windows toolchain loads Microsoft's WebView2 loader dynamically.
// Obtain it from the locked official SDK dependency, never from a system PATH.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows")
        || std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("gnu")
    {
        return;
    }
    let architecture = match std::env::var("CARGO_CFG_TARGET_ARCH").unwrap().as_str() {
        "x86_64" => "x64",
        "x86" => "x86",
        "aarch64" => "arm64",
        _ => panic!("Unsupported WebView2 architecture"),
    };
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let build = out.parent().unwrap().parent().unwrap();
    let profile = build.parent().unwrap();
    let loader = std::fs::read_dir(build)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("webview2-com-sys-")
        })
        .map(|entry| {
            entry
                .path()
                .join("out")
                .join(architecture)
                .join("WebView2Loader.dll")
        })
        .find(|path| path.is_file())
        .expect("Microsoft WebView2Loader.dll missing from SDK build output");
    for directory in [profile.to_path_buf(), profile.join("deps")] {
        std::fs::create_dir_all(&directory).unwrap();
        let destination = directory.join("WebView2Loader.dll");
        // Avoid touching the loader while a running instance has it mapped.
        if std::fs::read(&destination).ok() != std::fs::read(&loader).ok() {
            std::fs::copy(&loader, &destination).expect("Could not package WebView2Loader.dll");
        }
    }
}
