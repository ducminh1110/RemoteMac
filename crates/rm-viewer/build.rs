//! Gives Taro.exe a version resource (Properties > Details, and Windows' crash records, read it):
//! an .rc written from the crate's version, compiled by the target's resource compiler (rc.exe
//! for MSVC, windres for GNU) and linked into the viewer binary only. Nothing on other targets.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let v = |k: &str| std::env::var(k).ok().and_then(|s| s.parse::<u16>().ok()).unwrap_or(0);
    let (major, minor, patch) = (v("CARGO_PKG_VERSION_MAJOR"), v("CARGO_PKG_VERSION_MINOR"), v("CARGO_PKG_VERSION_PATCH"));
    let dotted = format!("{major}.{minor}.{patch}.0");
    // numbers rather than winver.h's names, so neither compiler needs the SDK's headers:
    // VS_FFI_FILEFLAGSMASK 0x3F, VOS_NT_WINDOWS32 0x40004, VFT_APP 1, 0x0409 04B0 US English, Unicode
    let rc = format!(
        r#"1 VERSIONINFO
FILEVERSION {major},{minor},{patch},0
PRODUCTVERSION {major},{minor},{patch},0
FILEFLAGSMASK 0x3F
FILEFLAGS 0x0
FILEOS 0x40004
FILETYPE 0x1
FILESUBTYPE 0x0
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904B0"
        BEGIN
            VALUE "FileDescription", "Taro"
            VALUE "FileVersion", "{dotted}"
            VALUE "InternalName", "Taro"
            VALUE "LegalCopyright", "GPL-3.0"
            VALUE "OriginalFilename", "Taro.exe"
            VALUE "ProductName", "Taro"
            VALUE "ProductVersion", "{dotted}"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x0409, 0x04B0
    END
END
"#
    );
    let path = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("taro.rc");
    std::fs::write(&path, rc).unwrap();
    // required: a release without its version would quietly bring 0.0.0.0 back
    embed_resource::compile_for(&path, ["remote-mac-viewer"], embed_resource::NONE).manifest_required().unwrap();
}
