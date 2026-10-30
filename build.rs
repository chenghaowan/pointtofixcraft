use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let icon = manifest.join("assets").join("app.ico");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let rc = out.join("pointtofix.rc");
    // 用绝对路径生成 .rc,避免 rc.exe 对相对路径搜索目录的歧义
    let icon_path = icon.to_string_lossy().replace('\\', "/");
    std::fs::write(&rc, format!("1 ICON \"{icon_path}\"\n")).unwrap();

    embed_resource::compile(&rc, embed_resource::NONE)
        .manifest_optional()
        .unwrap();

    println!("cargo:rerun-if-changed=assets/app.ico");
}
