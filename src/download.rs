//! 模型在线下载:WinHTTP(系统 TLS、自动跟随重定向、走系统代理)+ 单线程顺序队列。
//!
//! 选 WinHTTP 而非 ureq/reqwest:零新增第三方依赖(不引入 rustls/ring 的体积),
//! 走系统代理设置(经 FastGithub 类代理访问 HuggingFace 正需要),与仓库
//! 手写 Win32 的风格一致。resolve 直链都会 302 到 CDN,WinHTTP 自动跟随。
//!
//! 每个文件下到 `%APPDATA%\pointtofix\models\` 的 `.part` 临时名,
//! 完成后校验 Content-Length 再改名——半截文件不会被 `isnet*.onnx` /
//! `ppocr*.onnx` 的发现逻辑误认。断点续传不做(最大 176 MB 单文件直下,失败重下)。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};

use windows::core::{HSTRING, PCWSTR};

/// 可下载的模型文件
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum ModelKind {
    IsnetFp16,
    IsnetFp32,
    OcrDet,
    OcrRec,
    OcrDict,
    /// WanAI Runtime(rs-aiprivaderCraft)便携包 zip:去背/OCR 的统一推理服务,
    /// 下载后自动解包到 runtime_dir()/ai-runtime(见 install_runtime)
    AiRuntime,
}

/// 下载源:魔搭国内直连(默认);HuggingFace 走系统代理
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    ModelScope,
    HuggingFace,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    /// 排队中(前一个文件还没下完)
    Queued,
    Connecting,
    Downloading,
    Done,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug)]
pub struct Progress {
    pub phase: Phase,
    pub received: u64,
    pub total: u64,
    pub error: Option<String>,
}

impl ModelKind {
    /// 落地文件名:必须与各发现函数的优先级表对得上
    pub fn file_name(self) -> &'static str {
        match self {
            ModelKind::IsnetFp16 => "isnet-general-use-fp16.onnx",
            ModelKind::IsnetFp32 => "isnet-general-use.onnx",
            ModelKind::OcrDet => "ppocrv5_mobile_det.onnx",
            ModelKind::OcrRec => "ppocrv5_mobile_rec.onnx",
            ModelKind::OcrDict => "ppocrv5_dict.txt",
            ModelKind::AiRuntime => "ai-runtime.zip",
        }
    }
}

/// 下载 URL。ModelScope 各条已 HEAD 校验(200);HuggingFace 本机不可达,
/// 路径按仓库标准 resolve 形式给出,失败时 UI 显示 HTTP 错误、可切回魔搭。
/// 词典的 jsdelivr 直链国内外都可达(PaddleOCR 官方仓库镜像),两种源共用。
pub fn download_url(kind: ModelKind, source: Source) -> &'static str {
    // WanAI Runtime 两端 Release 都有公开便携包;忽略 Source 选择,固定 Gitee 直连,
    // 失败由 worker 自动换 GitHub(fallback_url)
    if kind == ModelKind::AiRuntime {
        return AI_RUNTIME_URL_GITEE;
    }
    if kind == ModelKind::OcrDict {
        return "https://cdn.jsdelivr.net/gh/PaddlePaddle/PaddleOCR@main/ppocr/utils/dict/ppocrv5_dict.txt";
    }
    match (kind, source) {
        (ModelKind::IsnetFp16, Source::ModelScope) => {
            "https://www.modelscope.cn/models/onnx-community/ISNet-ONNX/resolve/master/onnx/model_fp16.onnx"
        }
        (ModelKind::IsnetFp32, Source::ModelScope) => {
            "https://www.modelscope.cn/models/onnx-community/ISNet-ONNX/resolve/master/onnx/model.onnx"
        }
        (ModelKind::OcrDet, Source::ModelScope) => {
            "https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/master/onnx/PP-OCRv5/det/ch_PP-OCRv5_det_mobile.onnx"
        }
        (ModelKind::OcrRec, Source::ModelScope) => {
            "https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/master/onnx/PP-OCRv5/rec/ch_PP-OCRv5_rec_mobile.onnx"
        }
        (ModelKind::IsnetFp16, Source::HuggingFace) => {
            "https://huggingface.co/tiper-penta/isnet-general-use-fp16/resolve/main/isnet-general-use-fp16.onnx"
        }
        (ModelKind::IsnetFp32, Source::HuggingFace) => {
            "https://huggingface.co/x-Liola-x/isnet-general-use-onnx/resolve/main/isnet-general-use.onnx"
        }
        (ModelKind::OcrDet, Source::HuggingFace) => {
            "https://huggingface.co/ilaylow/PP_OCRv5_mobile_onnx/resolve/main/PP-OCRv5_mobile_det.onnx"
        }
        (ModelKind::OcrRec, Source::HuggingFace) => {
            "https://huggingface.co/ilaylow/PP_OCRv5_mobile_onnx/resolve/main/PP-OCRv5_mobile_rec.onnx"
        }
        (ModelKind::OcrDict, _) => unreachable!(),
        (ModelKind::AiRuntime, _) => unreachable!(),
    }
}

/// WanAI Runtime 便携包直链:两端 Release 各一份(Gitee 国内直连,GitHub 走系统代理)
const AI_RUNTIME_URL_GITEE: &str = "https://gitee.com/chenghao_wan/rs-aiprivader-craft/releases/download/v0.1.0/wanai-runtime-v0.1.0-windows-x64.zip";
const AI_RUNTIME_URL_GITHUB: &str = "https://github.com/chenghaowan/rs-aiprivader-craft/releases/download/v0.1.0/wanai-runtime-v0.1.0-windows-x64.zip";

/// 主源失败时的镜像;目前只有 AiRuntime 需要(Gitee → GitHub)
fn fallback_url(kind: ModelKind) -> Option<&'static str> {
    (kind == ModelKind::AiRuntime).then_some(AI_RUNTIME_URL_GITHUB)
}

/// 模型落地目录(%APPDATA%\pointtofix\models;已在 isnet 的搜索白名单里)
pub fn models_dir() -> PathBuf {
    let base = std::env::var("APPDATA")
        .map(|p| PathBuf::from(p).join("pointtofix"))
        .unwrap_or_else(|_| std::path::PathBuf::from("."));
    base.join("models")
}

/// runtime 目录(%APPDATA%\pointtofix\runtime):isnet 的内嵌 DLL 解包用其根,
/// WanAI Runtime 整套装在 ai-runtime 子目录(互不覆盖)
pub fn runtime_dir() -> PathBuf {
    let base = std::env::var("APPDATA")
        .map(|p| PathBuf::from(p).join("pointtofix"))
        .unwrap_or_else(|_| std::path::PathBuf::from("."));
    base.join("runtime")
}

struct Job {
    kind: ModelKind,
    url: String,
    dest: PathBuf,
}

struct QueueState {
    queue: Vec<Job>,
    /// 当前正在下载的任务的取消标志
    running: Option<(ModelKind, Arc<AtomicBool>)>,
    status: HashMap<ModelKind, Progress>,
}

impl QueueState {
    fn pop_front(&mut self) -> Option<Job> {
        if self.queue.is_empty() {
            None
        } else {
            Some(self.queue.remove(0))
        }
    }

    fn set(&mut self, kind: ModelKind, phase: Phase, received: u64, total: u64, error: Option<String>) {
        self.status.insert(kind, Progress { phase, received, total, error });
    }

    fn busy_kind(&self, kind: ModelKind) -> bool {
        self.queue.iter().any(|j| j.kind == kind)
            || matches!(&self.running, Some((k, _)) if *k == kind)
            || matches!(
                self.status.get(&kind).map(|p| p.phase),
                Some(Phase::Downloading | Phase::Connecting | Phase::Queued)
            )
    }
}

enum Cmd {
    Enqueue { kind: ModelKind, source: Source },
    Cancel { kind: ModelKind },
}

static STATE: OnceLock<Mutex<QueueState>> = OnceLock::new();
static WORKER: OnceLock<Sender<Cmd>> = OnceLock::new();

fn state() -> &'static Mutex<QueueState> {
    STATE.get_or_init(|| {
        Mutex::new(QueueState {
            queue: Vec::new(),
            running: None,
            status: HashMap::new(),
        })
    })
}

fn worker() -> &'static Sender<Cmd> {
    WORKER.get_or_init(|| {
        let (tx, rx) = channel::<Cmd>();
        std::thread::Builder::new()
            .name("pointtofix-download".into())
            .spawn(move || worker_loop(rx))
            .expect("无法启动下载线程");
        tx
    })
}

fn worker_loop(rx: Receiver<Cmd>) {
    loop {
        match rx.recv() {
            Ok(Cmd::Enqueue { kind, source }) => {
                let url = download_url(kind, source).to_string();
                let dest = match kind {
                    // AiRuntime 的 zip 落 runtime 目录(模型仍落 models)
                    ModelKind::AiRuntime => runtime_dir().join(kind.file_name()),
                    _ => models_dir().join(kind.file_name()),
                };
                {
                    let mut st = state().lock().unwrap();
                    if st.busy_kind(kind) {
                        continue;
                    }
                    st.set(kind, Phase::Queued, 0, 0, None);
                    st.queue.push(Job { kind, url, dest });
                }
                pump_queue();
            }
            Ok(Cmd::Cancel { kind }) => {
                let mut st = state().lock().unwrap();
                // 还在排队:直接摘掉
                if let Some(pos) = st.queue.iter().position(|j| j.kind == kind) {
                    st.queue.remove(pos);
                    st.set(kind, Phase::Cancelled, 0, 0, None);
                } else if let Some((k, flag)) = &st.running {
                    if *k == kind {
                        // 正在下载:置标志,下载循环会在下个分块前退出
                        flag.store(true, Ordering::Relaxed);
                    }
                }
            }
            Err(_) => break, // 发送端已释放(UI 退出)
        }
    }
}

/// 队列非空且空闲时启动下一个下载(阻塞在下载线程内执行)
fn pump_queue() {
    let job = {
        let mut st = state().lock().unwrap();
        if st.running.is_some() {
            return;
        }
        st.pop_front()
    };
    let Some(job) = job else { return };
    let flag = Arc::new(AtomicBool::new(false));
    {
        let mut st = state().lock().unwrap();
        st.running = Some((job.kind, flag.clone()));
        st.set(job.kind, Phase::Connecting, 0, 0, None);
    }
    let kind = job.kind;
    std::thread::Builder::new()
        .name("pointtofix-download-io".into())
        .spawn(move || {
            let mut result = http_download(kind, &job.url, &job.dest, &flag);
            // 主源失败换镜像再试一次(目前只有 AiRuntime:Gitee → GitHub)
            if result.is_err() {
                if let Some(fb) = fallback_url(kind).filter(|fb| *fb != job.url) {
                    eprintln!("[download] 主源失败,重试镜像: {fb}");
                    result = http_download(kind, fb, &job.dest, &flag);
                }
            }
            // AiRuntime:zip 下载成功后立即解包安装,失败按下载失败上报
            if result.is_ok() && kind == ModelKind::AiRuntime {
                result = install_runtime(&job.dest);
            }
            let mut st = state().lock().unwrap();
            st.running = None;
            match result {
                Ok(()) => st.set(kind, Phase::Done, 0, 0, None),
                Err(e) if e == "__cancelled__" => st.set(kind, Phase::Cancelled, 0, 0, None),
                Err(e) => {
                    let _ = std::fs::remove_file(job.dest.with_extension("part"));
                    st.set(kind, Phase::Failed, 0, 0, Some(e))
                }
            }
            drop(st);
            pump_queue();
        })
        .ok();
}

/// 解包安装 WanAI Runtime:zip → runtime_dir()/ai-runtime/(strip 顶层版本目录)。
/// 覆盖安装时先清空旧目录;成功后删 zip 省空间。
fn install_runtime(zip: &Path) -> Result<(), String> {
    let dest = runtime_dir().join("ai-runtime");
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::create_dir_all(&dest).map_err(|e| format!("创建 runtime 目录失败: {e}"))?;
    crate::zipmin::extract(zip, &dest, 1)?;
    if !dest.join("ai-runtime.exe").is_file() {
        return Err("解包完成但未找到 ai-runtime.exe(包内容不符?)".into());
    }
    let _ = std::fs::remove_file(zip);
    eprintln!("[download] WanAI Runtime 已安装到 {}", dest.display());
    Ok(())
}

/// 排一个下载任务;已在队列/下载中时返回 false
pub fn start(kind: ModelKind, source: Source) -> bool {
    {
        let st = state().lock().unwrap();
        if st.busy_kind(kind) {
            return false;
        }
    }
    let _ = worker().send(Cmd::Enqueue { kind, source });
    true
}

pub fn cancel(kind: ModelKind) {
    let _ = worker().send(Cmd::Cancel { kind });
}

/// 当前各模型的下载状态快照(UI 每帧轮询)
pub fn snapshot() -> Vec<(ModelKind, Progress)> {
    let st = state().lock().unwrap();
    let mut out: Vec<(ModelKind, Progress)> =
        st.status.iter().map(|(k, p)| (*k, p.clone())).collect();
    out.sort_by_key(|(k, _)| *k);
    out
}

pub fn is_busy() -> bool {
    let st = state().lock().unwrap();
    st.running.is_some() || !st.queue.is_empty()
}

// ---------- WinHTTP 下载 ----------

type Handle = *mut std::ffi::c_void;

/// 拆 URL 为 (https?, host, path)
fn split_url(url: &str) -> Option<(bool, String, String)> {
    let (secure, rest) = url
        .strip_prefix("https://")
        .map(|r| (true, r))
        .or_else(|| url.strip_prefix("http://").map(|r| (false, r)))?;
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    Some((secure, host.to_string(), path.to_string()))
}

/// 句柄创建类 API 失败返回 null:取 GetLastError 包装成消息
fn last_err(what: &str) -> String {
    format!("{what}: {}", windows::core::Error::from_thread())
}

fn http_download(
    kind: ModelKind,
    url: &str,
    dest: &Path,
    cancel: &AtomicBool,
) -> Result<(), String> {
    use windows::Win32::Networking::WinHttp::*;
    let Some((secure, host, path)) = split_url(url) else {
        return Err(format!("无法解析 URL: {url}"));
    };
    if let Some(dir) = dest.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    unsafe {
        // User-Agent 必须带上:魔搭的 LFS CDN(cdn-lfs-cn-1.modelscope.cn)对
        // 空 UA 直接 403(实测 2026-11-05);WinHttpOpen 的 agent 就是 UA 头。
        let agent = HSTRING::from("Mozilla/5.0 (Windows NT 10.0; Win64; x64) PointToFix/0.3");
        let session = WinHttpOpen(
            PCWSTR(agent.as_ptr()),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        );
        if session.is_null() {
            return Err(last_err("WinHttpOpen"));
        }
        let result = open_connect(session, kind, secure, &host, &path, dest, cancel);
        let _ = WinHttpCloseHandle(session);
        result
    }
}

fn open_connect(
    session: Handle,
    kind: ModelKind,
    secure: bool,
    host: &str,
    path: &str,
    dest: &Path,
    cancel: &AtomicBool,
) -> Result<(), String> {
    use windows::Win32::Networking::WinHttp::*;
    unsafe {
        let hconnect = WinHttpConnect(
            session,
            PCWSTR(HSTRING::from(host).as_ptr()),
            if secure { 443u16 } else { 80u16 },
            0,
        );
        if hconnect.is_null() {
            return Err(last_err("连接服务器失败(WinHttpConnect)"));
        }
        let result = send_request(hconnect, kind, secure, path, dest, cancel);
        let _ = WinHttpCloseHandle(hconnect);
        result
    }
}

fn send_request(
    hconnect: Handle,
    kind: ModelKind,
    secure: bool,
    path: &str,
    dest: &Path,
    cancel: &AtomicBool,
) -> Result<(), String> {
    use windows::Win32::Networking::WinHttp::*;
    unsafe {
        let flags = if secure { WINHTTP_FLAG_SECURE } else { WINHTTP_OPEN_REQUEST_FLAGS(0) };
        let hrequest = WinHttpOpenRequest(
            hconnect,
            PCWSTR(HSTRING::from("GET").as_ptr()),
            PCWSTR(HSTRING::from(path).as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            flags,
        );
        if hrequest.is_null() {
            return Err(last_err("WinHttpOpenRequest"));
        }
        let result = read_response(hrequest, kind, dest, cancel);
        let _ = WinHttpCloseHandle(hrequest);
        result
    }
}

fn read_response(
    hrequest: Handle,
    kind: ModelKind,
    dest: &Path,
    cancel: &AtomicBool,
) -> Result<(), String> {
    use windows::Win32::Networking::WinHttp::*;
    unsafe {
        // resolve 直链会 302 到 CDN:允许跟随
        let policy = WINHTTP_OPTION_REDIRECT_POLICY_ALWAYS;
        let _ = WinHttpSetOption(
            Some(hrequest),
            WINHTTP_OPTION_REDIRECT_POLICY,
            Some(std::slice::from_raw_parts(
                &policy as *const u32 as *const u8,
                std::mem::size_of::<u32>(),
            )),
        );
        WinHttpSendRequest(hrequest, None, None, 0, 0, 0)
            .map_err(|e| format!("连接失败(检查网络 / 系统代理): {e}"))?;
        WinHttpReceiveResponse(hrequest, std::ptr::null_mut())
            .map_err(|e| format!("接收响应失败: {e}"))?;

        let status = query_u32(hrequest, WINHTTP_QUERY_STATUS_CODE)?;
        if status != 200 {
            return Err(format!("HTTP {status}(检查网络 / 代理,或换一个下载源)"));
        }
        let total = query_u32(hrequest, WINHTTP_QUERY_CONTENT_LENGTH).unwrap_or(0) as u64;

        let part = dest.with_extension("part");
        let mut file = std::fs::File::create(&part).map_err(|e| format!("创建临时文件失败: {e}"))?;
        let mut received: u64 = 0;
        let mut buf = [0u8; 64 * 1024];
        loop {
            if cancel.load(Ordering::Relaxed) {
                drop(file);
                let _ = std::fs::remove_file(&part);
                return Err("__cancelled__".to_string());
            }
            let mut avail: u32 = 0;
            WinHttpQueryDataAvailable(hrequest, &mut avail)
                .map_err(|e| format!("查询数据失败: {e}"))?;
            if avail == 0 {
                break;
            }
            let want = avail.min(buf.len() as u32);
            let mut read: u32 = 0;
            WinHttpReadData(
                hrequest,
                buf.as_mut_ptr() as *mut std::ffi::c_void,
                want,
                &mut read,
            )
            .map_err(|e| format!("读取数据失败: {e}"))?;
            if read == 0 {
                break;
            }
            use std::io::Write;
            file.write_all(&buf[..read as usize]).map_err(|e| format!("写文件失败: {e}"))?;
            received += read as u64;
            // 进度按 256KB 节流,避免高频加锁
            if received / (256 * 1024) != (received - read as u64) / (256 * 1024) {
                let mut st = state().lock().unwrap();
                st.set(kind, Phase::Downloading, received, total, None);
            }
        }
        drop(file);
        if total > 0 && received != total {
            let _ = std::fs::remove_file(&part);
            return Err(format!("下载不完整:{received} / {total} 字节"));
        }
        std::fs::rename(&part, dest).map_err(|e| format!("落盘失败: {e}"))?;
        Ok(())
    }
}

/// 只取响应头(GET + status + Content-Length,不读 body):测试/诊断用
pub fn probe_url(url: &str) -> Result<(u32, u64), String> {
    use windows::Win32::Networking::WinHttp::*;
    let Some((secure, host, path)) = split_url(url) else {
        return Err(format!("无法解析 URL: {url}"));
    };
    let agent = HSTRING::from("Mozilla/5.0 (Windows NT 10.0; Win64; x64) PointToFix/0.3");
    unsafe {
        let session = WinHttpOpen(
            PCWSTR(agent.as_ptr()),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        );
        if session.is_null() {
            return Err(last_err("WinHttpOpen"));
        }
        let result = (|| -> Result<(u32, u64), String> {
            let hconnect = WinHttpConnect(
                session,
                PCWSTR(HSTRING::from(&host).as_ptr()),
                if secure { 443u16 } else { 80u16 },
                0,
            );
            if hconnect.is_null() {
                return Err(last_err("WinHttpConnect"));
            }
            let flags = if secure { WINHTTP_FLAG_SECURE } else { WINHTTP_OPEN_REQUEST_FLAGS(0) };
            let hrequest = WinHttpOpenRequest(
                hconnect,
                PCWSTR(HSTRING::from("GET").as_ptr()),
                PCWSTR(HSTRING::from(&path).as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                flags,
            );
            if hrequest.is_null() {
                let r = last_err("WinHttpOpenRequest");
                let _ = WinHttpCloseHandle(hconnect);
                return Err(r);
            }
            let policy = WINHTTP_OPTION_REDIRECT_POLICY_ALWAYS;
            let _ = WinHttpSetOption(
                Some(hrequest),
                WINHTTP_OPTION_REDIRECT_POLICY,
                Some(std::slice::from_raw_parts(
                    &policy as *const u32 as *const u8,
                    std::mem::size_of::<u32>(),
                )),
            );
            let out = (|| -> Result<(u32, u64), String> {
                WinHttpSendRequest(hrequest, None, None, 0, 0, 0)
                    .map_err(|e| format!("连接失败: {e}"))?;
                WinHttpReceiveResponse(hrequest, std::ptr::null_mut())
                    .map_err(|e| format!("接收响应失败: {e}"))?;
                let status = query_u32(hrequest, WINHTTP_QUERY_STATUS_CODE)?;
                let len = query_u32(hrequest, WINHTTP_QUERY_CONTENT_LENGTH).unwrap_or(0) as u64;
                Ok((status, len))
            })();
            let _ = WinHttpCloseHandle(hrequest);
            let _ = WinHttpCloseHandle(hconnect);
            out
        })();
        let _ = WinHttpCloseHandle(session);
        result
    }
}

fn query_u32(request: Handle, flag: u32) -> Result<u32, String> {
    use windows::Win32::Networking::WinHttp::*;
    let mut value: u32 = 0;
    let mut len = std::mem::size_of::<u32>() as u32;
    let mut index: u32 = 0;
    unsafe {
        WinHttpQueryHeaders(
            request,
            flag | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut value as *mut u32 as *mut std::ffi::c_void),
            &mut len,
            &mut index,
        )
        .map_err(|e| format!("查询响应头失败: {e}"))?;
    }
    Ok(value)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_url_basic() {
        assert_eq!(
            split_url("https://www.modelscope.cn/models/a/b/resolve/master/c.onnx"),
            Some((true, "www.modelscope.cn".into(), "/models/a/b/resolve/master/c.onnx".into()))
        );
        assert_eq!(
            split_url("http://x.cn"),
            Some((false, "x.cn".into(), "/".into()))
        );
        assert_eq!(split_url("ftp://x"), None);
    }

    /// 真机网络自检:POINTTOFIX_TEST_NET=1 时验证魔搭直链可达且 UA 修复生效。
    /// det(5MB)走完整下载路径;fp16(88MB)只探测响应头。
    #[test]
    fn modelscope_downloads_work_with_ua() {
        if std::env::var("POINTTOFIX_TEST_NET").is_err() {
            return;
        }
        let (status, len) = probe_url(download_url(
            ModelKind::IsnetFp16,
            Source::ModelScope,
        ))
        .expect("探测 ISNet fp16 失败");
        assert_eq!(status, 200, "ISNet fp16 应 200");
        assert!(len > 80_000_000, "fp16 应约 88MB,实际 {len}");

        let dir = std::env::temp_dir().join("ptf-net-test");
        let _ = std::fs::create_dir_all(&dir);
        let dest = dir.join(ModelKind::OcrDet.file_name());
        let _ = std::fs::remove_file(&dest);
        http_download(ModelKind::OcrDet, download_url(ModelKind::OcrDet, Source::ModelScope), &dest, &std::sync::atomic::AtomicBool::new(false))
            .expect("det 下载失败");
        let size = std::fs::metadata(&dest).expect("det 文件不存在").len();
        assert!(size > 4_000_000, "det 应约 4.8MB,实际 {size}");
        let _ = std::fs::remove_file(&dest);
    }

    #[test]
    fn file_names_match_discovery() {
        // 落地名必须能被 isnet / ocr 的发现逻辑按优先级捡到
        assert!(ModelKind::IsnetFp16.file_name().starts_with("isnet"));
        assert!(ModelKind::OcrDet.file_name().starts_with("ppocr"));
        assert!(ModelKind::OcrDict.file_name().starts_with("ppocr"));
    }

    /// 真机端到端:POINTTOFIX_TEST_NET=1 时走完整链路 ——
    /// Gitee 直链探测 → 下载队列拿 WanAI Runtime 便携包 → 解包安装到
    /// runtime_dir()/ai-runtime → 拉起安装副本 → /ocr 与 /remove-bg 真实推理
    /// (服务首次调用会自动下载它自己的 PP-OCR/ISNet 模型,约 110 MB)。
    /// 结束时恢复配置并杀掉拉起的服务。
    #[test]
    fn airuntime_e2e_install_and_serve() {
        use crate::capture::Shot;
        use std::time::Instant;
        if std::env::var("POINTTOFIX_TEST_NET").is_err() {
            return;
        }

        // 1) 直链探测(下载层 WinHTTP 会自动跟随 302,这里只验证可达与体积)
        let (status, len) = probe_url(AI_RUNTIME_URL_GITEE).expect("探测 Gitee 直链失败");
        assert_eq!(status, 200, "Gitee Release 直链应 200");
        assert!(len > 10_000_000, "便携包应约 16MB,实际 {len}");

        // 2) 走真实下载队列(含失败换 GitHub、下载后解包安装)
        assert!(start(ModelKind::AiRuntime, Source::ModelScope), "入队失败");
        let t0 = Instant::now();
        loop {
            let phase = snapshot()
                .into_iter()
                .find(|(k, _)| *k == ModelKind::AiRuntime)
                .map(|(_, p)| p.phase);
            if matches!(phase, Some(Phase::Done)) {
                break;
            }
            if matches!(phase, Some(Phase::Failed)) || t0.elapsed() > std::time::Duration::from_secs(300) {
                panic!("AiRuntime 下载/安装未完成(阶段 {phase:?},耗时 {:?})", t0.elapsed());
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        let install = runtime_dir().join("ai-runtime");
        assert!(install.join("ai-runtime.exe").is_file(), "解包后应有 ai-runtime.exe");
        assert!(install.join("config.yaml").is_file());
        assert!(install.join("models").join("onnxruntime.dll").is_file());
        assert!(!runtime_dir().join("ai-runtime.zip").is_file(), "安装成功后 zip 应删除");

        // 3) 端到端:配置指向默认端口 → 拉起「安装副本」(而非开发目录产物)→ 推理
        let _guard = E2eGuard::install(cfg_for_test());
        std::process::Command::new("taskkill")
            .args(["/F", "/IM", "ai-runtime.exe"])
            .output()
            .ok();
        std::thread::sleep(std::time::Duration::from_millis(500));

        let shot = Shot { width: 96, height: 48, rgba: vec![255; 96 * 48 * 4] };
        // OCR:白图 → 服务首调自动下载它的 PP-OCR 模型 → 返回 lines JSON(空行也算通;
        // try_ocr_lines 契约:返回剥离 "lines" 键后的顶层数组文本)
        let ocr = crate::airuntime::try_ocr_lines(&shot);
        let ocr = ocr.expect("OCR 应走通服务(失败会回退本地返回 None)");
        assert!(ocr.trim_start().starts_with('['), "OCR 应返回顶层数组,实际: {ocr}");
        // 去背:alpha mask 合成白底,输出尺寸与输入一致
        let out = crate::airuntime::try_remove_bg(&shot);
        assert!(out.is_some(), "remove-bg 应走通服务(失败会回退本地返回 None)");
        assert_eq!(out.unwrap().len(), shot.rgba.len(), "remove-bg 输出应与输入同尺寸");
    }

    /// 测试期写真实配置文件的快照(原内容存在则记下,结束时恢复)
    fn cfg_for_test() -> (std::path::PathBuf, Option<Vec<u8>>) {
        let dir = std::env::var("APPDATA")
            .map(|p| std::path::PathBuf::from(p).join("pointtofix"))
            .unwrap_or_else(|_| std::path::PathBuf::from("."));
        let _ = std::fs::create_dir_all(&dir);
        let cfg = dir.join("ai_runtime.json");
        let saved = std::fs::read(&cfg).ok();
        std::fs::write(&cfg, r#"{"enabled":true,"addr":"127.0.0.1:17800","exe":""}"#).ok();
        (cfg, saved)
    }

    /// 测试结束(含 panic 展开)恢复 ai_runtime.json 并杀掉拉起的服务
    struct E2eGuard {
        cfg: std::path::PathBuf,
        saved: Option<Vec<u8>>,
    }
    impl E2eGuard {
        fn install((cfg, saved): (std::path::PathBuf, Option<Vec<u8>>)) -> Self {
            Self { cfg, saved }
        }
    }
    impl Drop for E2eGuard {
        fn drop(&mut self) {
            match &self.saved {
                Some(bytes) => {
                    let _ = std::fs::write(&self.cfg, bytes);
                }
                None => {
                    let _ = std::fs::remove_file(&self.cfg);
                }
            }
            std::process::Command::new("taskkill")
                .args(["/F", "/IM", "ai-runtime.exe"])
                .output()
                .ok();
        }
    }
}
