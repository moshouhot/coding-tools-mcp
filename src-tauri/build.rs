fn main() {
    if std::env::var_os("CARGO_FEATURE_UI_SMOKE").is_some()
        && std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
    {
        // Tauri embeds this in the app EXE, but Cargo's native test EXE also
        // needs Common Controls v6 (TaskDialogIndirect is absent from v5).
        println!("cargo:rustc-link-arg-tests=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-tests=/MANIFESTDEPENDENCY:type='win32' name='Microsoft.Windows.Common-Controls' version='6.0.0.0' processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'");
    }
    tauri_build::build()
}
