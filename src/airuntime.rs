//! AI Runtime 接入:模型推理集中服务(localhost HTTP)。
//!
//! 流程:需要模型(去背/OCR)时 → 若开关开启 → 尝试 HTTP 调用 WanAI Runtime;
//! 服务未运行则自动拉起(独立进程,等待就绪);未安装则后台自动下载安装
//! (设置面板可手动触发);任何失败返回 None,**静默回退本地推理**。
//!
//! 配置:`%APPDATA%\pointtofix\ai_runtime.json`
//! ```json
//! { "enabled": true, "addr": "127.0.0.1:17800", "exe": "" }
//! ```
//! 零新依赖:配置解析手写(文件由我们自己生成,格式可控);HTTP 走 localhost TcpStream(无 TLS);
//! 服务拉起走 std::process + CREATE_NO_WINDOW。

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::capture::Shot;

/// 最长等待服务就绪(拉起后服务监听很快;模型下载在首次推理时进行)
const STARTUP_WAIT: Duration = Duration::from_secs(30);
/// 单次推理超时(本地大模型 CPU 推理 + 大图传输)
const CALL_TIMEOUT: Duration = Duration::from_secs(180);

/// "找不到 exe → 触发一次后台下载"的防重标志(每进程只 kick 一次)
static AUTO_KICKED: AtomicBool = AtomicBool::new(false);

/// 极简 JSON 字符串值提取:取 "key" 后第一个带引号的值
fn json_str(src: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\"");
    let i = src.find(&pat)?;
    let rest = &src[i + pat.len()..];
    let a = rest.find('"')?;
    let b = rest[a + 1..].find('"')?;
    Some(rest[a + 1..a + 1 + b].to_string())
}

/// 极简 JSON 布尔值提取
fn json_bool(src: &str, key: &str) -> Option<bool> {
    let s = match json_str(src, key) {
        Some(s) => s,
        None => {
            let pat = format!("\"{key}\"");
            let i = src.find(&pat)?;
            let rest = &src[i + pat.len()..];
            rest.trim_start().trim_start_matches(':').trim_start().to_string()
        }
    };
    match s.as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn cfg_path() -> Option<PathBuf> {
    Some(std::env::var("APPDATA").ok()?.parse::<PathBuf>().ok()?.join("pointtofix").join("ai_runtime.json"))
}

fn read_cfg_raw() -> String {
    cfg_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default()
}

fn enabled() -> bool {
    let raw = read_cfg_raw();
    if raw.is_empty() {
        return true; // 文件缺失 = 默认启用,失败会静默回退,无副作用
    }
    json_bool(&raw, "enabled").unwrap_or(true)
}

fn addr() -> String {
    let raw = read_cfg_raw();
    if raw.is_empty() {
        return "127.0.0.1:17800".into();
    }
    json_str(&raw, "addr").unwrap_or_else(|| "127.0.0.1:17800".into())
}

fn cfg_exe() -> String {
    json_str(&read_cfg_raw(), "exe").unwrap_or_default()
}

/// 查找 ai-runtime.exe(env > 配置 > exe 旁 > 本程序下载安装的副本 > 开发目录)
fn find_exe(cfg_exe: &str) -> Option<PathBuf> {
    if !cfg_exe.is_empty() {
        let p = PathBuf::from(cfg_exe);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Ok(p) = std::env::var("WANAI_RUNTIME_EXE") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let p = parent.join("ai-runtime.exe");
            if p.is_file() {
                return Some(p);
            }
        }
    }
    // 本程序下载安装的副本(排在开发目录探针之前:下载完成立即生效)
    installed_exe()
        // 开发机便利:兄弟仓库的构建产物
        .or_else(|| {
            for probe in [
                "D:\\claw.forvia\\rs-aiprivaderCraft\\target\\release\\ai-runtime.exe",
                "D:\\claw.forvia\\rs-aiprivaderCraft\\target\\debug\\ai-runtime.exe",
            ] {
                let p = PathBuf::from(probe);
                if p.is_file() {
                    return Some(p);
                }
            }
            None
        })
}

/// 下载队列安装的副本(%APPDATA%\pointtofix\runtime\ai-runtime\)
fn installed_exe() -> Option<PathBuf> {
    let p = crate::download::runtime_dir().join("ai-runtime").join("ai-runtime.exe");
    p.is_file().then_some(p)
}

/// 统一推理服务是否已安装(UI「已就绪」标注用)
pub fn installed() -> bool {
    find_exe(&cfg_exe()).is_some()
}

fn healthy(addr: &str) -> bool {
    http(addr, "GET", "/health", None, Duration::from_secs(5))
        .map(|(status, _)| status == 200)
        .unwrap_or(false)
}

/// 服务未运行时拉起(独立进程,无窗口),等 /health 就绪
fn ensure_running(addr: &str) -> bool {
    if healthy(addr) {
        return true;
    }
    let Some(exe) = find_exe(&cfg_exe()) else {
        // 没装:后台下载一次 WanAI Runtime(约 16 MB),本次仍回退本地推理;
        // 下载完成后下一次调用即可拉起服务。UI 也可在设置里手动触发。
        eprintln!("[ai-runtime] 未找到 ai-runtime.exe,本次回退本地推理");
        if !AUTO_KICKED.swap(true, Ordering::Relaxed) {
            eprintln!("[ai-runtime] 后台下载 WanAI Runtime(Gitee 直连,失败换 GitHub)…");
            let _ = crate::download::start(
                crate::download::ModelKind::AiRuntime,
                crate::download::Source::ModelScope, // AiRuntime 忽略来源,占位
            );
        }
        return false;
    };
    // config.yaml 与 exe 同目录(部署形态);config 里 model_dir/cache_dir 是相对
    // 路径,以 exe 所在目录为工作目录才能解析到 models/onnxruntime.dll
    let mut cmd = std::process::Command::new(&exe);
    if let Some(parent) = exe.parent() {
        cmd.current_dir(parent);
    }
    let cfg = exe.parent().map(|d| d.join("config.yaml"));
    if let Some(cfg) = cfg.filter(|c| c.is_file()) {
        cmd.arg("--config").arg(cfg);
    }
    // CREATE_NO_WINDOW:后台拉起,不弹控制台;Stdio 隔离:
    // 否则子进程继承宿主 stdout 句柄,脚本管道会一直等它关闭
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    creation_flags_cmd(&mut cmd, CREATE_NO_WINDOW);
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    match cmd.spawn() {
        Ok(_) => eprintln!("[ai-runtime] 已拉起 {}", exe.display()),
        Err(e) => {
            eprintln!("[ai-runtime] 拉起失败({e}),回退本地推理");
            return false;
        }
    }
    let t0 = Instant::now();
    while t0.elapsed() < STARTUP_WAIT {
        std::thread::sleep(Duration::from_millis(400));
        if healthy(addr) {
            return true;
        }
    }
    eprintln!("[ai-runtime] 等待服务就绪超时,回退本地推理");
    false
}

/// 最小 HTTP/1.1 客户端(仅 localhost,无 chunked:runtime 响应都带 Content-Length)
fn http(
    addr: &str,
    method: &str,
    path: &str,
    body: Option<(&str, Vec<u8>)>,
    timeout: Duration,
) -> Option<(u16, Vec<u8>)> {
    let (host, port) = match addr.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse::<u16>().unwrap_or(17800)),
        None => (addr.to_string(), 17800),
    };
    let mut stream = TcpStream::connect((host.as_str(), port)).ok()?;
    stream.set_read_timeout(Some(timeout)).ok()?;
    stream.set_write_timeout(Some(timeout)).ok()?;

    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
    match body {
        Some((ct, bytes)) => {
            req.push_str(&format!(
                "Content-Type: {ct}\r\nContent-Length: {}\r\n\r\n",
                bytes.len()
            ));
            stream.write_all(req.as_bytes()).ok()?;
            stream.write_all(&bytes).ok()?;
        }
        None => {
            req.push_str("\r\n");
            stream.write_all(req.as_bytes()).ok()?;
        }
    }

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).ok()?;
    let sep = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = String::from_utf8_lossy(&raw[..sep]).to_string();
    let mut body = raw[sep + 4..].to_vec();
    let status: u16 = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())?;
    if let Some(cl) = head.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        (k.eq_ignore_ascii_case("content-length")).then(|| v.trim().parse::<usize>().ok())?
    }) {
        body.truncate(cl);
    }
    Some((status, body))
}

/// 抠图:Shot PNG → /remove-bg → RGBA PNG → alpha 作 mask 白底合成(与本地输出语义一致)。
/// 返回 None = 未启用/服务不可用/失败(调用方回退本地推理)。
pub fn try_remove_bg(shot: &Shot) -> Option<Vec<u8>> {
    if !enabled() {
        return None;
    }
    let addr = addr();
    if !ensure_running(&addr) {
        return None;
    }
    let t0 = Instant::now();
    let png = encode_png(shot)?;
    let (status, body) = http(
        &addr,
        "POST",
        "/remove-bg",
        Some(("application/octet-stream", png)),
        CALL_TIMEOUT,
    )?;
    if status != 200 {
        eprintln!("[ai-runtime] remove-bg HTTP {status},回退本地推理");
        return None;
    }
    let rgba = image::load_from_memory(&body).ok()?.to_rgba8();
    let (w, h) = (rgba.width() as i32, rgba.height() as i32);
    if w != shot.width || h != shot.height {
        eprintln!(
            "[ai-runtime] remove-bg 尺寸不符 {w}x{h} != {}x{},回退本地推理",
            shot.width, shot.height
        );
        return None;
    }
    // alpha 通道即前景 mask → 复用本地白底合成(输出语义与本地 ISNet 一致)
    let mask: Vec<u8> = rgba.pixels().map(|p| p[3]).collect();
    let out = crate::isnet::composite_on_white(shot, &mask);
    eprintln!("[ai-runtime] remove-bg 完成({}ms)", t0.elapsed().as_millis());
    Some(out)
}

/// OCR:Shot PNG → /ocr → lines JSON(与 ocr::lines_from_json 契约一致)。
/// 返回 Some(json 文本) = 成功;调用方用 lines_from_json 解析。
pub fn try_ocr_lines(shot: &Shot) -> Option<String> {
    if !enabled() {
        return None;
    }
    let addr = addr();
    if !ensure_running(&addr) {
        return None;
    }
    let png = encode_png(shot)?;
    let (status, body) = http(
        &addr,
        "POST",
        "/ocr",
        Some(("application/octet-stream", png)),
        CALL_TIMEOUT,
    )?;
    if status != 200 {
        eprintln!("[ai-runtime] ocr HTTP {status},回退本地推理");
        return None;
    }
    let text = String::from_utf8_lossy(&body).to_string();
    // runtime 响应是 {"lines":[...]};提取顶层数组交给 lines_from_json(契约:no/text/box)
    let a = text.find("\"lines\"")?;
    let arr_start = text[a..].find('[')? + a;
    // 从 arr_start 起找匹配的右括号(lines 数组是响应最后一个字段,取最后 ']' 即可)
    let arr_end = text.rfind(']')?;
    if arr_end <= arr_start {
        return None;
    }
    Some(text[arr_start..=arr_end].to_string())
}

fn encode_png(shot: &Shot) -> Option<Vec<u8>> {
    let img = image::RgbaImage::from_raw(shot.width as u32, shot.height as u32, shot.rgba.clone())?;
    let mut buf = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut buf, image::ImageFormat::Png)
        .ok()?;
    Some(buf.into_inner())
}

#[cfg(windows)]
fn creation_flags_cmd(cmd: &mut std::process::Command, flags: u32) -> &mut std::process::Command {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(flags)
}
#[cfg(not(windows))]
fn creation_flags_cmd(cmd: &mut std::process::Command, _flags: u32) -> &mut std::process::Command {
    cmd
}
