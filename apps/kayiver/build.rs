fn main() {
    build_display_native();
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

fn build_display_native() {
    use std::{env, path::PathBuf, process::Command};
    println!("cargo:rerun-if-changed=src/display/native");
    println!("cargo:rustc-check-cfg=cfg(display_native)");
    if env::var_os("CARGO_FEATURE_SIM").is_some() { return; }
    let target = env::var("TARGET").unwrap();
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let (compiler, archiver, source) = if target.contains("apple-darwin") {
        ("clang++", "ar", "src/display/native/macos.mm")
    } else if target == "x86_64-pc-windows-gnu" {
        ("x86_64-w64-mingw32-g++", "x86_64-w64-mingw32-ar", "src/display/native/windows.cpp")
    } else { return; };
    let obj = out.join("display-native.o");
    let mut cc = Command::new(compiler);
    cc.args(["-std=c++17", "-O2", "-fno-exceptions", "-fno-rtti", "-c", source, "-o"]).arg(&obj);
    if target.contains("apple") {
        cc.args(["-fobjc-arc", "-fblocks", "-mmacosx-version-min=11.0", "-Wno-unguarded-availability-new"]);
        cc.args(["-arch",if target.starts_with("aarch64"){"arm64"}else{"x86_64"}]);
    }
    assert!(cc.status().expect("native display compiler unavailable").success(), "native display compilation failed");
    assert!(Command::new(archiver).arg("crs").arg(out.join("libkayiver_display.a")).arg(obj).status().unwrap().success());
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=kayiver_display");
    if target.contains("apple") {
        for framework in ["AppKit", "CoreGraphics", "CoreMedia", "CoreVideo", "VideoToolbox"] {
            println!("cargo:rustc-link-lib=framework={framework}");
        }
        println!("cargo:rustc-link-arg=-Wl,-weak_framework,ScreenCaptureKit");
        println!("cargo:rustc-link-lib=c++");
    } else {
        for lib in ["mfplat", "mf", "mfuuid", "wmcodecdspuuid", "strmiids", "d3d11", "dxgi", "ole32", "uuid", "shell32", "user32", "gdi32", "wtsapi32"] {
            println!("cargo:rustc-link-lib={lib}");
        }
    }
    println!("cargo:rustc-cfg=display_native");
}
