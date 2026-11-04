//! 无头 CLI:让脚本 / AI 代理能独立完成"截图 → OCR → 入库"闭环。
//!
//! ```text
//! pointtofix --capture x,y,w,h [--out path] [--ocr] [--json]
//! ```
//! - 坐标为虚拟桌面物理像素,语义与 GUI 框选一致(允许负值,越界自动裁);
//! - 无 `--out`:落到画廊目录(%APPDATA%\pointtofix\gallery)并写 SQLite,
//!   `--json` 输出回执(id / 文件路径 / OCR 行)供调用方解析;
//! - 有 `--out`:只写指定文件(PNG/WebP 按扩展名),不落库;
//! - `--ocr` 需要 det/rec/词典齐全,缺失时明确报错、退出码非 0;
//! - 全程无 GUI、无弹窗。退出码:0 成功,1 失败,2 参数错误。
//!
//! release 构建是 GUI 子系统:交互终端里没有控制台可写,先 AttachConsole
//! 挂到父进程的控制台;被程序化调用(管道重定向)时 stdout 本来就通。

use std::path::PathBuf;
use std::time::{Duration, Instant};

use sha2::Digest;

use crate::capture::{self, Shot};
use crate::gallery;
use crate::i18n::Lang;
use crate::ocr::{self, OcrLine};

pub struct CliArgs {
    pub region: (i32, i32, i32, i32),
    pub out: Option<PathBuf>,
    pub ocr: bool,
    pub json: bool,
}

pub enum CliAction {
    /// 不带 --capture:正常启动 GUI
    Gui,
    Run(CliArgs),
    Bad(String),
}

pub fn parse(args: impl Iterator<Item = String>) -> CliAction {
    let args: Vec<String> = args.collect();
    let mut region: Option<(i32, i32, i32, i32)> = None;
    let mut out: Option<PathBuf> = None;
    let mut ocr = false;
    let mut json = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--capture" => {
                i += 1;
                let Some(spec) = args.get(i) else {
                    return CliAction::Bad("--capture 需要 x,y,w,h 参数".into());
                };
                let vals: Vec<i32> = spec
                    .split(',')
                    .filter_map(|s| s.trim().parse().ok())
                    .collect();
                // x/y 允许 0 与负数(副屏在主屏左侧/上方);宽高必须为正
                if vals.len() != 4 || vals[2] <= 0 || vals[3] <= 0 {
                    return CliAction::Bad(format!(
                        "--capture 坐标无效: {spec}(应为 x,y,w,h;宽高必须为正,x/y 可为 0 或负数)"
                    ));
                }
                region = Some((vals[0], vals[1], vals[2], vals[3]));
            }
            "--out" => {
                i += 1;
                match args.get(i) {
                    Some(p) => out = Some(PathBuf::from(p)),
                    None => return CliAction::Bad("--out 需要一个文件路径".into()),
                }
            }
            "--ocr" => ocr = true,
            "--json" => json = true,
            other => {
                return CliAction::Bad(format!(
                    "未知参数 {other}(支持 --capture x,y,w,h / --out path / --ocr / --json)"
                ))
            }
        }
        i += 1;
    }
    match region {
        None => CliAction::Gui,
        Some(region) => CliAction::Run(CliArgs { region, out, ocr, json }),
    }
}

/// 无头执行:截图 →(可选 OCR)→ 落盘/入库 → 输出回执。返回进程退出码。
pub fn run(args: CliArgs) -> i32 {
    attach_console();
    let (x, y, w, h) = args.region;
    let Ok(shot) = capture::capture_region_px(x, y, w, h, Lang::En) else {
        return fail("截屏失败(区域无效或屏幕 DC 不可用)");
    };

    // OCR(阻塞等待:调用方就是要拿结果)
    let mut lines: Vec<OcrLine> = Vec::new();
    if args.ocr {
        match wait_ocr(&shot) {
            Ok(l) => lines = l,
            Err(ocr::OcrError::ModelMissing) => {
                return fail("未找到 OCR 模型(det/rec/词典);用 GUI 设置页下载或跑 scripts/fetch_ppocr.py");
            }
            Err(ocr::OcrError::Error(e)) => return fail(&format!("OCR 失败: {e}")),
        }
    }

    match &args.out {
        Some(out) => {
            // 只写文件,不落库(临时用途)
            let ext = out
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase())
                .unwrap_or_else(|| "png".into());
            let bytes = match ext.as_str() {
                "webp" => encode_webp(&shot, 90),
                _ => encode_png(&shot),
            };
            let bytes = match bytes {
                Ok(b) => b,
                Err(e) => return fail(&format!("编码失败: {e}")),
            };
            if let Err(e) = std::fs::write(out, &bytes) {
                return fail(&format!("写文件失败: {e}"));
            }
            report(args.json, None, out, &lines);
        }
        None => {
            // 落画廊 + SQLite(与 GUI 的保存链路同库)
            let bytes = match encode_png(&shot) {
                Ok(b) => b,
                Err(e) => return fail(&format!("编码失败: {e}")),
            };
            let dir = gallery::gallery_dir();
            let _ = std::fs::create_dir_all(&dir);
            let path = dir.join(gallery::timestamp_file_name("png", 0));
            if let Err(e) = std::fs::write(&path, &bytes) {
                return fail(&format!("写文件失败: {e}"));
            }
            // 缩略图
            let thumb = gallery::thumbnail_png(&shot, 320).ok().and_then(|tb| {
                let tdir = dir.join("thumbs");
                let _ = std::fs::create_dir_all(&tdir);
                let stem = path.file_stem()?.to_string_lossy();
                let tpath = tdir.join(format!("{stem}.png"));
                std::fs::write(&tpath, &tb).ok().map(|_| tpath)
            });
            let app_title = capture::foreground_app_title();
            let hash = sha2::Sha256::digest(&bytes);
            let hash_hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
            let lines_json = ocr::lines_to_json(&lines);
            let status = if args.ocr { "done" } else { "off" };
            let ocr_text = lines
                .iter()
                .map(|l| l.text.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            let row = gallery::NewShot {
                created_at: now_ms(),
                file: &path,
                thumb: thumb.as_deref(),
                format: "png",
                width: shot.width,
                height: shot.height,
                file_size: bytes.len() as i64,
                content_hash: &hash_hex,
                app_name: app_title.as_deref(),
                ocr_text: &ocr_text,
                ocr_lines_json: &lines_json,
                ocr_status: status,
            };
            match gallery::insert(row) {
                Ok(id) => report(args.json, Some(id), &path, &lines),
                Err(e) => return fail(&format!("入库失败(文件已保存): {e}")),
            }
        }
    }
    0
}

/// 阻塞等 OCR 结果(CLI 没有帧循环,直接轮询;2 分钟超时)
fn wait_ocr(shot: &Shot) -> Result<Vec<OcrLine>, ocr::OcrError> {
    let Some(id) = ocr::submit(shot.clone()) else {
        return Err(ocr::OcrError::Error("无法启动 OCR 线程".into()));
    };
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if let Some(done) = ocr::poll() {
            if done.id == id {
                return done.result;
            }
            // 其它过期结果:CLI 进程里不该出现,忽略继续等
        }
        if Instant::now() > deadline {
            return Err(ocr::OcrError::Error("OCR 超时(120s)".into()));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn report(json: bool, id: Option<i64>, path: &PathBuf, lines: &[OcrLine]) {
    if !json {
        println!("ok id={} file={}", id.unwrap_or(0), path.display());
        for l in lines {
            println!("ocr[{}] {}", l.no, l.text);
        }
        return;
    }
    let mut s = String::from("{");
    if let Some(id) = id {
        s.push_str(&format!("\"id\":{id},"));
    }
    s.push_str(&format!("\"file\":\"{}\"", ocr::json_escape(&path.display().to_string())));
    s.push_str(",\"ocr\":[");
    for (i, l) in lines.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&format!("{{\"no\":{},\"text\":\"{}\"}}", l.no, ocr::json_escape(&l.text)));
    }
    s.push_str("]}");
    println!("{s}");
}

fn fail(msg: &str) -> i32 {
    eprintln!("error: {msg}");
    1
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn encode_png(shot: &Shot) -> Result<Vec<u8>, String> {
    use image::ImageEncoder;
    let mut buf = Vec::new();
    image::codecs::png::PngEncoder::new(&mut buf)
        .write_image(
            &shot.rgba,
            shot.width as u32,
            shot.height as u32,
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|e| e.to_string())?;
    Ok(buf)
}

fn encode_webp(shot: &Shot, quality: u8) -> Result<Vec<u8>, String> {
    let mem = webp::Encoder::from_rgba(&shot.rgba, shot.width as u32, shot.height as u32)
        .encode(quality as f32);
    Ok(mem.to_vec())
}

/// GUI 子系统下挂到父进程控制台,交互终端里也能看到 stdout/stderr
fn attach_console() {
    use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}
