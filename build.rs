//! Compiles OpenBoardView's board file parsers and the C wrapper in `native/`
//! into a static library linked into this crate.
//!
//! The OpenBoardView source tree is taken from `OBV_SRC` when set, otherwise
//! from the `vendor/OpenBoardView` git submodule. Only the parser sources are
//! compiled: no SDL, imgui or OpenGL. The `src/utf8` and `src/mpc` submodules
//! of OpenBoardView must be checked out.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// Parser sources, relative to `<OBV>/src/openboardview`.
const OBV_CXX_SOURCES: &[&str] = &[
    "utils.cpp",
    "FileFormats/BRDFileBase.cpp",
    "FileFormats/ADFile.cpp",
    "FileFormats/ASCFile.cpp",
    "FileFormats/BDVFile.cpp",
    "FileFormats/BRD2File.cpp",
    "FileFormats/BRDFile.cpp",
    "FileFormats/BVRFile.cpp",
    "FileFormats/BVR3File.cpp",
    "FileFormats/CADFile.cpp",
    "FileFormats/CAEFile.cpp",
    "FileFormats/CSTFile.cpp",
    "FileFormats/FZFile.cpp",
    "FileFormats/GenCADFile.cpp",
    "FileFormats/XZZPCBFile.cpp",
];

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());

    println!("cargo:rerun-if-env-changed=OBV_SRC");
    let obv_root =
        env::var_os("OBV_SRC").map(PathBuf::from).unwrap_or_else(|| manifest_dir.join("vendor/OpenBoardView"));
    let obv_src = obv_root.join("src");
    let obv_app = obv_src.join("openboardview");

    check_tree(&obv_root, &obv_src, &obv_app);

    let version = obv_version(&obv_root);
    println!("cargo:rustc-env=OBV_VERSION={version}");

    let generated = out_dir.join("generated");
    generate_gencad_grammar_header(
        &obv_app.join("FileFormats/GenCADFileBnf.h"),
        &generated.join("build-generated/GenCADFileGrammar.h"),
    );

    let zlib_includes: Vec<PathBuf> =
        env::var("DEP_Z_INCLUDE").map(|v| v.split(',').map(PathBuf::from).collect()).unwrap_or_default();

    let shim = manifest_dir.join("native/shim");
    let native = manifest_dir.join("native");

    // C sources: DES (XZZPCB) and mpc (GenCAD parser combinators).
    let mut c = cc::Build::new();
    c.warnings(false)
        .include(&shim)
        .include(&obv_app)
        .include(&obv_src)
        .file(obv_app.join("Crypto/des.c"))
        .file(obv_src.join("mpc/mpc.c"));
    c.compile("obv_c");

    let mut cxx = cc::Build::new();
    cxx.cpp(true)
        .std("c++17")
        .warnings(false)
        .define("WITH_STD_FILESYSTEM", None)
        .define("OBV_VERSION", Some(format!("\"{version}\"").as_str()))
        // The shim must win over any system SDL headers.
        .include(&shim)
        .include(&native)
        .include(&obv_app)
        .include(&obv_src)
        .include(&generated)
        .includes(&zlib_includes)
        // Keep absolute build paths out of __FILE__ in parser error messages.
        .flag_if_supported(format!("-ffile-prefix-map={}/=", obv_app.display()))
        .file(native.join("obv_ffi.cpp"));
    for source in OBV_CXX_SOURCES {
        cxx.file(obv_app.join(source));
    }
    cxx.compile("obv_parsers");

    println!("cargo:rerun-if-changed=native");
    println!("cargo:rerun-if-changed={}", obv_app.join("FileFormats").display());
    println!("cargo:rerun-if-changed={}", obv_app.join("utils.cpp").display());
    println!("cargo:rerun-if-changed={}", obv_app.join("utils.h").display());
}

fn check_tree(root: &Path, src: &Path, app: &Path) {
    if !app.join("FileFormats/BRDFileBase.h").is_file() {
        panic!(
            "OpenBoardView sources not found at {}.\n\
             Run `git submodule update --init vendor/OpenBoardView`, \
             or set OBV_SRC to an OpenBoardView checkout.",
            root.display()
        );
    }
    for (dir, header) in [("utf8", "utf8.h"), ("mpc", "mpc.h")] {
        if !src.join(dir).join(header).is_file() {
            panic!(
                "OpenBoardView submodule src/{dir} is not checked out in {}.\n\
                 Run `git -C {} submodule update --init src/utf8 src/mpc`.",
                root.display(),
                root.display()
            );
        }
    }
}

/// Reads `project(OpenBoardView VERSION x.y.z)` from the top-level CMakeLists.txt.
fn obv_version(root: &Path) -> String {
    let cmake = root.join("CMakeLists.txt");
    println!("cargo:rerun-if-changed={}", cmake.display());
    fs::read_to_string(&cmake)
        .ok()
        .and_then(|text| {
            text.lines()
                .map(str::trim)
                .find(|line| line.starts_with("project(") && line.contains("VERSION"))
                .and_then(|line| line.split("VERSION").nth(1))
                .map(|rest| rest.trim().trim_end_matches(')').trim().to_string())
        })
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Port of OpenBoardView's `utilities/generate_grammar_header.py`: emits an
/// X-macro listing every rule name defined in the GenCAD BNF.
fn generate_gencad_grammar_header(bnf: &Path, out: &Path) {
    println!("cargo:rerun-if-changed={}", bnf.display());
    let text = fs::read_to_string(bnf).unwrap_or_else(|e| panic!("cannot read {}: {e}", bnf.display()));

    let rules: Vec<&str> = text
        .lines()
        .filter(|line| !line.contains("SKIP_LINE_IN_GRAMMAR_GENERATOR"))
        .filter_map(|line| {
            // Python: re.findall(r"^\s*\w+", ..., re.MULTILINE)
            let trimmed = line.trim_start();
            let end = trimmed.find(|c: char| !(c.is_alphanumeric() || c == '_')).unwrap_or(trimmed.len());
            (end > 0).then(|| &trimmed[..end])
        })
        .collect();
    assert!(!rules.is_empty(), "no grammar rules found in {}", bnf.display());

    let mut header = String::from(
        "#pragma once\n\
         /* Generated by build.rs from GenCADFileBnf.h. Do not edit. */\n\
         #define X_MACRO_PARSE_VARS \\\n",
    );
    for rule in rules {
        header.push_str(&format!("X({rule}, \"{rule}\")\\\n"));
    }
    header.push_str(" /*'empty' line to end last continuation*/\n");

    fs::create_dir_all(out.parent().unwrap()).unwrap();
    fs::write(out, header).unwrap();
}
