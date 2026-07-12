fn main() {
    // Embed the app icon + version info into the Windows exe. Works when
    // cross-compiling too (winres shells out to <target>-windres for gnu).
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winres::WindowsResource::new();
        // Cross-compiling (e.g. from macOS): use the mingw-prefixed tools.
        if std::env::var("HOST").map(|h| !h.contains("windows")).unwrap_or(false) {
            res.set_windres_path("x86_64-w64-mingw32-windres");
            res.set_ar_path("x86_64-w64-mingw32-ar");
        }
        res.set_icon("../../assets/icons/kayiver.ico");
        res.set("ProductName", "Kayıver");
        res.set("FileDescription", "Kayıver — tek klavye & fare, bütün ekranlar");
        res.set("LegalCopyright", "MIT");
        match res.compile() {
            Ok(()) => {
                // The GNU linker drops the resource from libresource.a because
                // nothing references it; feeding the object file directly
                // forces the .rsrc section into the exe.
                if let Ok(out) = std::env::var("OUT_DIR") {
                    println!("cargo:rustc-link-arg-bins={out}/resource.o");
                }
            }
            // Non-fatal: a missing windres just means a plain exe.
            Err(e) => println!("cargo:warning=winres failed ({e}); building without icon"),
        }
    }
}
