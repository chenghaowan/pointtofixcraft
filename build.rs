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

    // 把仓库 models\ 里的推理库复制到 exe 旁边:源码构建后直接 cargo run /
    // 双击 target\release\pointtofix.exe 就能用 AI 去背和 OCR,不用手动搬 DLL。
    // (便携包由 build-portable.bat 打包 models\,这条只覆盖开发运行的场景)
    // OUT_DIR = target\<profile>\build\<pkg>-<hash>\out,上跳三级就是 target\<profile>。
    if let Some(profile_dir) = out.ancestors().nth(3) {
        for dll in ["onnxruntime.dll", "onnxruntime_providers_shared.dll"] {
            let src = manifest.join("models").join(dll);
            if src.is_file() {
                let dst = profile_dir.join(dll);
                // 内容一致就跳过,避免每次构建都重写 19MB
                let same = std::fs::metadata(&dst)
                    .and_then(|a| std::fs::metadata(&src).map(|b| a.len() == b.len()))
                    .unwrap_or(false);
                if !same {
                    let _ = std::fs::copy(&src, &dst);
                }
            }
        }
    }

    // 仓库里有 models\onnxruntime.dll 就把它嵌进二进制(单文件便携:分发物只剩一个 exe);
    // 没有(全新 clone)就生成 None,运行期退回"按目录查找 + 提示脚本获取"。
    // 用户的 POINTTOFIX_ORT_DLL / exe 旁的 DLL 优先级更高,嵌入版只是兜底。
    let dll = manifest.join("models").join("onnxruntime.dll");
    let gen = out.join("embedded_dll.rs");
    if dll.is_file() {
        let p = dll.to_string_lossy().replace('\\', "/");
        std::fs::write(
            &gen,
            format!("pub static ORT_DLL_EMBEDDED: Option<&[u8]> = Some(include_bytes!(\"{p}\"));\n"),
        )
        .unwrap();
    } else {
        std::fs::write(&gen, "pub static ORT_DLL_EMBEDDED: Option<&[u8]> = None;\n").unwrap();
    }

    println!("cargo:rerun-if-changed=assets/app.ico");
    println!("cargo:rerun-if-changed=models/onnxruntime.dll");
}
