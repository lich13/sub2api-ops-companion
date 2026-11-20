use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

fn xcrun_output(args: &[&str]) -> String {
    let output = Command::new("xcrun")
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("failed to run xcrun {args:?}: {e}"));
    assert!(
        output.status.success(),
        "xcrun {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("xcrun output must be UTF-8")
        .trim()
        .to_string()
}

fn build_macos_swift_shell() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let package = manifest_dir.join("../macos");
    for input in ["Package.swift", "Package.resolved", "Plugins", "Sources"] {
        println!("cargo:rerun-if-changed={}", package.join(input).display());
    }
    println!("cargo:rerun-if-changed=../../server/src/account_creation_fields.json");
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let sdk_path = xcrun_output(&["--sdk", "macosx", "--show-sdk-path"]);
    let swiftc_path = PathBuf::from(xcrun_output(&["--find", "swiftc"]));
    let clang_rt_dir = PathBuf::from(xcrun_output(&["clang", "--print-runtime-dir"]));
    let repo = manifest_dir
        .join("../..")
        .canonicalize()
        .expect("repository");
    let mut args = vec![
        "build".to_owned(),
        "--package-path".into(),
        package.display().to_string(),
        "--scratch-path".into(),
        out_dir.join("swift-package").display().to_string(),
        "--only-use-versions-from-resolved-file".into(),
        "--product".into(),
        "Sub2OpsMacOS".into(),
        "--configuration".into(),
        "release".into(),
        "--triple".into(),
        "arm64-apple-macosx14.0".into(),
        "--sdk".into(),
        sdk_path.clone(),
        "-debug-info-format".into(),
        "none".into(),
        "-Xswiftc".into(),
        "-strict-concurrency=complete".into(),
    ];
    let mut remaps = vec![
        format!("{}=/build/repo", repo.display()),
        format!("{}=/build/out", out_dir.display()),
        "/private/var/folders=/build/tmp".to_string(),
        "/var/folders=/build/tmp".to_string(),
        "/Users/runner=/build/home".to_string(),
    ];
    if let Ok(home) = env::var("HOME") {
        remaps.push(format!("{home}=/build/home"));
    }
    for remap in remaps {
        args.extend([
            "-Xswiftc".into(),
            "-file-prefix-map".into(),
            "-Xswiftc".into(),
            remap.clone(),
        ]);
        args.extend(["-Xcc".into(), format!("-ffile-prefix-map={remap}")]);
    }
    let build = Command::new("swift")
        .args(&args)
        .status()
        .expect("invoke swift build");
    assert!(build.success(), "Swift package build failed");
    let output = Command::new("swift")
        .args(&args)
        .arg("--show-bin-path")
        .output()
        .expect("Swift product directory");
    assert!(
        output.status.success(),
        "Swift product directory unavailable"
    );
    let lib_dir = String::from_utf8(output.stdout).expect("UTF-8 product directory");
    println!("cargo:rustc-link-search=native={}", lib_dir.trim());
    let sdk_swift = Path::new(&sdk_path).join("usr/lib/swift");
    if sdk_swift.exists() {
        println!("cargo:rustc-link-search=native={}", sdk_swift.display());
    }
    if let Some(toolchain_usr) = swiftc_path.parent().and_then(Path::parent) {
        let toolchain_swift = toolchain_usr.join("lib/swift/macosx");
        if toolchain_swift.exists() {
            println!(
                "cargo:rustc-link-search=native={}",
                toolchain_swift.display()
            );
        }
    }
    println!("cargo:rustc-link-lib=static=Sub2OpsMacOS");
    if clang_rt_dir.join("libclang_rt.osx.a").exists() {
        println!("cargo:rustc-link-search=native={}", clang_rt_dir.display());
        println!("cargo:rustc-link-lib=static=clang_rt.osx");
    }
    for framework in ["AppKit", "SwiftUI", "Foundation", "UniformTypeIdentifiers"] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
}

fn main() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        build_macos_swift_shell();
    }
    tauri_build::build()
}
