use std::{env, fs, path::Path};

fn copy_directory(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_directory(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn main() {
    println!("cargo:rerun-if-changed=vendor/windows/platform-tools");
    println!("cargo:rerun-if-changed=vendor/windows/scrcpy");
    println!("cargo:rerun-if-changed=assets/androidtool.ico");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    winresource::WindowsResource::new()
        .set_icon("assets/androidtool.ico")
        .set("ProductName", "AndroidTool")
        .set("FileDescription", "AndroidTool - Android device toolkit")
        .set("InternalName", "androidtool")
        .set("OriginalFilename", "androidtool.exe")
        .compile()
        .expect("compile AndroidTool Windows icon and version resources");
    let source = Path::new("vendor/windows/platform-tools");
    if !source.join("adb.exe").is_file() {
        println!(
            "cargo:warning=Bundled ADB missing; run scripts/package-windows.ps1 to download and package it."
        );
        return;
    }
    // OUT_DIR is <target>/<profile>/build/<package>/out, also for explicit targets.
    let out_dir = env::var_os("OUT_DIR").expect("OUT_DIR");
    let profile = Path::new(&out_dir)
        .ancestors()
        .nth(3)
        .expect("profile directory");
    copy_directory(source, &profile.join("platform-tools")).expect("copy bundled platform-tools");
    let scrcpy = Path::new("vendor/windows/scrcpy");
    if scrcpy.join("scrcpy.exe").is_file() {
        copy_directory(scrcpy, &profile.join("scrcpy")).expect("copy bundled scrcpy");
    }
}
