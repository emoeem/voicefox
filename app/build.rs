use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=../icons/voicefox.ico");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo"));
    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory is set"));
    let icon = manifest_dir.join("../icons/voicefox.ico");
    let resource = out_dir.join("voicefox.rc");
    let icon_path = icon.to_string_lossy().replace('\\', "/");
    fs::write(&resource, format!("1 ICON \"{icon_path}\"\n"))
        .expect("Windows resource script can be written");

    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let output = if target_env == "msvc" {
        compile_msvc(&resource, &out_dir)
    } else {
        compile_gnu(&resource, &out_dir)
    };
    println!("cargo:rustc-link-arg-bin=voicefox={}", output.display());
}

fn compile_msvc(resource: &Path, out_dir: &Path) -> PathBuf {
    let output = out_dir.join("voicefox.res");
    let status = Command::new("rc.exe")
        .arg("/nologo")
        .arg(format!("/fo{}", output.display()))
        .arg(resource)
        .status()
        .expect("rc.exe is required to embed the Windows application icon");
    assert!(
        status.success(),
        "rc.exe failed to compile the application icon"
    );
    output
}

fn compile_gnu(resource: &Path, out_dir: &Path) -> PathBuf {
    let output = out_dir.join("voicefox-resource.o");
    let status = Command::new("windres")
        .arg("--input")
        .arg(resource)
        .arg("--output")
        .arg(&output)
        .arg("--output-format=coff")
        .status()
        .expect("windres is required to embed the Windows application icon");
    assert!(
        status.success(),
        "windres failed to compile the application icon"
    );
    output
}
