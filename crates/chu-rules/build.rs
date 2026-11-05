// Builds the referee from vendor/chushogi: libchushogi (the C ABI, for the
// `ffi` backend) with the cc crate, and the `chushogi-gen` binary that the
// `subprocess` backend and the test clients drive. Both land in OUT_DIR, so
// the submodule's tree stays clean.

use std::env;
use std::path::PathBuf;

const LIB_SOURCES: &[&str] = &["types", "position", "sfen", "usi", "movegen", "status", "capi"];
const GEN_SOURCES: &[&str] = &["types", "position", "sfen", "usi", "movegen", "status", "mate", "main"];

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let src = manifest.join("../../vendor/chushogi/src");
    if !src.join("capi.cpp").exists() {
        panic!(
            "{} has no capi.cpp; run `git submodule update --init`",
            src.display()
        );
    }
    println!("cargo:rerun-if-changed={}", src.display());

    if env::var_os("CARGO_FEATURE_FFI").is_some() {
        cc::Build::new()
            .cpp(true)
            .std("c++17")
            .opt_level(2)
            .files(LIB_SOURCES.iter().map(|f| src.join(format!("{f}.cpp"))))
            .compile("chushogi");
    }

    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("chushogi-gen");
    let compiler = cc::Build::new().cpp(true).std("c++17").get_compiler();
    let mut cmd = compiler.to_command();
    cmd.arg("-O2").arg("-o").arg(&out);
    for f in GEN_SOURCES {
        cmd.arg(src.join(format!("{f}.cpp")));
    }
    let status = cmd.status().expect("run C++ compiler");
    assert!(status.success(), "building chushogi-gen failed");
    println!("cargo:rustc-env=CHUSHOGI_GEN={}", out.display());
}
