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
    println!("cargo:rerun-if-env-changed=VOICEFOX_RC");
    let target = env::var("TARGET").expect("TARGET is set by Cargo");
    let mut compiler = env::var_os("VOICEFOX_RC")
        .map(Command::new)
        .or_else(|| find_msvc_tools::find(&target, "rc.exe"))
        .unwrap_or_else(|| Command::new("rc.exe"));
    let status = compiler
        .arg("/nologo")
        .arg(format!("/fo{}", output.display()))
        .arg(resource)
        .status()
        .expect("Cannot run rc.exe; install the Windows SDK or set VOICEFOX_RC to its resource compiler path");
    assert!(
        status.success(),
        "rc.exe failed to compile the application icon"
    );
    output
}

fn compile_gnu(resource: &Path, out_dir: &Path) -> PathBuf {
    let output = out_dir.join("voicefox-resource.o");
    println!("cargo:rerun-if-env-changed=WINDRES");
    let target = env::var("TARGET").expect("TARGET is set by Cargo");
    let (prefixed, format) = match target.as_str() {
        "x86_64-pc-windows-gnu" => (Some("x86_64-w64-mingw32-windres"), Some("pe-x86-64")),
        "i686-pc-windows-gnu" => (Some("i686-w64-mingw32-windres"), Some("pe-i386")),
        _ => (None, None),
    };
    let compile = |program| {
        let mut command = Command::new(program);
        command
            .arg("--input")
            .arg(resource)
            .arg("--output")
            .arg(&output)
            .arg("--output-format=coff");
        if let Some(format) = format {
            command.arg("--target").arg(format);
        }
        command.status()
    };
    let result = if let Some(program) = env::var_os("WINDRES") {
        compile(program)
    } else if env::var("HOST").as_deref() != Ok(target.as_str()) {
        match prefixed.map(|program| compile(program.into())) {
            Some(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                compile("windres".into())
            }
            Some(result) => result,
            None => compile("windres".into()),
        }
    } else {
        compile("windres".into())
    };
    let status = result.expect(
        "Cannot run windres; install MinGW binutils or set WINDRES to its resource compiler path",
    );
    assert!(
        status.success(),
        "windres failed to compile the application icon"
    );
    output
}
