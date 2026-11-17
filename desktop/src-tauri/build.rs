use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

fn collect_swift_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    println!("cargo:rerun-if-changed={}", dir.display());
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            collect_swift_sources(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("swift") {
            println!("cargo:rerun-if-changed={}", path.display());
            out.push(path);
        }
    }
}

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
    let swift_sources_dir = manifest_dir.join("../macos/Sources");
    let mut sources = Vec::new();
    collect_swift_sources(&swift_sources_dir, &mut sources);
    assert!(
        !sources.is_empty(),
        "no Swift sources found under {}",
        swift_sources_dir.display()
    );

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let mod_cache = out_dir.join("swift-modcache");
    fs::create_dir_all(&mod_cache).expect("create swift-modcache");
    let obj_path = out_dir.join("sub2ops_macos.o");
    let lib_path = out_dir.join("libsub2ops_macos.a");

    let catalog_path = manifest_dir.join("../../server/src/account_creation_fields.json");
    println!("cargo:rerun-if-changed={}", catalog_path.display());
    let catalog_json =
        fs::read_to_string(&catalog_path).expect("read server/src/account_creation_fields.json");
    let generated_catalog = out_dir.join("AccountCreationCatalogGenerated.swift");
    fs::write(
        &generated_catalog,
        format!(
            "enum AccountCreationCatalogGenerated {{\n    static let json: String = #\"\"\"\n{catalog_json}\n\"\"\"#\n}}\n"
        ),
    )
    .expect("write AccountCreationCatalogGenerated.swift");
    sources.push(generated_catalog);

    let sdk_path = xcrun_output(&["--sdk", "macosx", "--show-sdk-path"]);
    let swiftc_path = PathBuf::from(xcrun_output(&["--find", "swiftc"]));
    let libtool_path = xcrun_output(&["--find", "libtool"]);
    let clang_rt_dir = PathBuf::from(xcrun_output(&["clang", "--print-runtime-dir"]));

    let repo_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .unwrap_or(&manifest_dir)
        .canonicalize()
        .unwrap_or_else(|_| manifest_dir.clone());

    let mut cmd = Command::new(&swiftc_path);
    cmd.args([
        "-O",
        "-whole-module-optimization",
        "-gnone",
        "-swift-version",
        "6",
        "-strict-concurrency=complete",
        "-parse-as-library",
        "-module-name",
        "Sub2OpsMacOS",
        "-target",
        "arm64-apple-macosx14.0",
        "-sdk",
        &sdk_path,
        "-module-cache-path",
    ]);
    cmd.arg(&mod_cache);

    let mut remaps = vec![
        format!("{}=/build/repo", repo_root.display()),
        format!("{}=/build/out", out_dir.display()),
        "/private/var/folders=/build/tmp".to_string(),
        "/var/folders=/build/tmp".to_string(),
        "/Users/runner=/build/home".to_string(),
    ];
    if let Ok(home) = env::var("HOME") {
        if !home.is_empty() {
            remaps.push(format!("{home}=/build/home"));
        }
    }
    for remap in &remaps {
        cmd.args(["-file-prefix-map", remap]);
    }

    cmd.arg("-c");
    for src in &sources {
        cmd.arg(src);
    }
    cmd.arg("-o").arg(&obj_path);

    let status = cmd.status().expect("invoke swiftc");
    assert!(status.success(), "swiftc compilation failed");

    if lib_path.exists() {
        let _ = fs::remove_file(&lib_path);
    }
    let archive_status = Command::new(&libtool_path)
        .arg("-static")
        .arg("-no_warning_for_no_symbols")
        .arg("-o")
        .arg(&lib_path)
        .arg(&obj_path)
        .status()
        .expect("invoke libtool");
    assert!(archive_status.success(), "libtool static archive failed");

    println!("cargo:rustc-link-search=native={}", out_dir.display());
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
    println!("cargo:rustc-link-lib=static=sub2ops_macos");
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
