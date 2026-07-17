fn main() {
    // No JS-facing commands: the app's Rust core is the only caller
    // (run_mobile_plugin bypasses the webview permission system), so the
    // webview can never touch key material even if XSS'd.
    tauri_plugin::Builder::new(&[])
        .android_path("android")
        .ios_path("ios")
        .build();
}
