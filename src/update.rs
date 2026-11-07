//! 应用内升级:检查新版本 → 下载发布 zip → 解包出 exe → 改名替换重启。
//!
//! 版本源:Gitee Releases API 优先(国内直连,不需要 FastGithub),
//! 失败再试 GitHub Releases API——与模型下载的双源策略一致。
//! JSON 不引依赖,手写引号值提取(同 airuntime.rs 的做法)。
//!
//! 自替换用 Windows 的「运行中 exe 允许改名」特性:当前 exe 改名成
//! `*.exe.old` → 把新 exe 复制回原路径 → 分离启动新进程 → UI 发 Close 结束
//! 旧进程。新进程启动时后台清理残留的 `.exe.old`(旧实例退干净才能删掉)。
//! 中间任何一步失败都回滚改名,应用保持可用。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use crate::download::{self, Phase, Progress};
use crate::zipmin;

/// GitHub Releases API(WinHTTP 走系统代理,FastGithub 类代理可达)
const GITHUB_API: &str = "https://api.github.com/repos/chenghaowan/pointtofixcraft/releases/latest";
/// Gitee 的 Releases API 被 WAF 拦(HTTP 403,任何 UA 都不放行,实测 2026-11-07),
/// 但 HTML 版发布页可达——兜底源解析 HTML 拿最新 tag,下载直链按固定规则拼出来
const GITEE_RELEASES_PAGE: &str = "https://gitee.com/chenghao_wan/pointtofixcraft/releases";
const GITEE_DOWNLOAD_BASE: &str = "https://gitee.com/chenghao_wan/pointtofixcraft/releases/download";
/// 便携包附件名(两端发布流程保持一致)
const ZIP_ASSET: &str = "pointtofix-craft-portable.zip";

/// 检查到的新版本信息
#[derive(Clone, Debug, PartialEq)]
pub struct UpdateInfo {
    /// 远端 tag(形如 v0.3.2)
    pub version: String,
    /// 发布附件里的便携包 zip 直链
    pub zip_url: String,
}

/// 设置面板「关于」区的升级状态(UI 持有;下载进度另从 snapshot() 轮询)
#[derive(Clone, Debug, PartialEq)]
pub enum UiState {
    /// 还没查过 / 上次检查已取消
    Idle,
    Checking,
    /// 远端版本不比当前新
    UpToDate,
    /// 发现新版本
    Available(UpdateInfo),
    /// 升级包已下载解包完毕,可一键升级重启
    Ready(UpdateInfo),
    Failed(String),
}

/// 升级包落地目录(%APPDATA%\pointtofix\update)
pub fn update_dir() -> PathBuf {
    let base = std::env::var("APPDATA")
        .map(|p| PathBuf::from(p).join("pointtofix"))
        .unwrap_or_else(|_| PathBuf::from("."));
    base.join("update")
}

/// 解包好的新版本 exe(update_dir()/pointtofix.exe;Ready 之后才存在)
fn staged_exe() -> PathBuf {
    update_dir().join("pointtofix.exe")
}

// ---------- 极简 JSON 提取(同 airuntime.rs 的手写风格) ----------

/// 取 "key" 后第一个带引号的值
fn json_str(src: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\"");
    let i = src.find(&pat)?;
    let rest = &src[i + pat.len()..];
    let a = rest.find('"')?;
    let b = rest[a + 1..].find('"')?;
    Some(rest[a + 1..a + 1 + b].to_string())
}

/// 取 "key" 的全部带引号值(release 里有多个附件,要遍历 browser_download_url)
fn json_all_strs(src: &str, key: &str) -> Vec<String> {
    let pat = format!("\"{key}\"");
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(i) = src[at..].find(&pat) {
        let start = at + i + pat.len();
        let rest = &src[start..];
        let Some(a) = rest.find('"') else { break };
        let Some(b) = rest[a + 1..].find('"') else { break };
        out.push(rest[a + 1..a + 1 + b].to_string());
        at = start + a + 1 + b + 1;
    }
    out
}

// ---------- 版本比较 ----------

/// 远端版本是否比当前新:strip 前导 v,按 '.' 分段数值比较,缺段按 0
fn is_newer(current: &str, remote: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> {
        v.trim()
            .trim_start_matches('v')
            .split('.')
            .map(|p| p.trim().parse().unwrap_or(0))
            .collect()
    };
    let (a, b) = (parts(current), parts(remote));
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        if y != x {
            return y > x;
        }
    }
    false
}

// ---------- 版本检查 ----------

/// 解析 release API 响应:tag_name + 优先 portable 的 zip 附件直链
fn parse_release(body: &str) -> Result<UpdateInfo, String> {
    let version = json_str(body, "tag_name").ok_or("响应缺 tag_name")?;
    let urls = json_all_strs(body, "browser_download_url");
    let zip_url = urls
        .iter()
        .find(|u| u.contains("portable") && u.ends_with(".zip"))
        .or_else(|| urls.iter().find(|u| u.ends_with(".zip")))
        .ok_or("release 没有 zip 附件")?
        .clone();
    Ok(UpdateInfo { version, zip_url })
}

/// 查一个源的 latest release;解析失败返回 Err(由 check() 换下一个源)
fn check_one(api: &str) -> Result<Option<UpdateInfo>, String> {
    let body = download::http_get_text(api)?;
    let info = parse_release(&body)?;
    if is_newer(env!("CARGO_PKG_VERSION"), &info.version) {
        Ok(Some(info))
    } else {
        Ok(None)
    }
}

/// 解析 Gitee 发布页 HTML:最新 release 排最前,取第一个 `releases/tag/v*` 的版本号
fn parse_gitee_html(html: &str) -> Result<UpdateInfo, String> {
    const MARK: &str = "releases/tag/";
    let i = html.find(MARK).ok_or("HTML 没有 releases/tag 链接")?;
    let rest = html[i + MARK.len()..].trim_start_matches('v');
    let v: String = rest.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    if v.is_empty() {
        return Err("HTML tag 不是版本号".into());
    }
    let version = format!("v{v}");
    let zip_url = format!("{GITEE_DOWNLOAD_BASE}/{version}/{ZIP_ASSET}");
    Ok(UpdateInfo { version, zip_url })
}

/// Gitee HTML 兜底:发布页取版本,比较后返回是否更新
fn check_gitee_html() -> Result<Option<UpdateInfo>, String> {
    let html = download::http_get_text(GITEE_RELEASES_PAGE)?;
    let info = parse_gitee_html(&html)?;
    if is_newer(env!("CARGO_PKG_VERSION"), &info.version) {
        Ok(Some(info))
    } else {
        Ok(None)
    }
}

/// 依次查各源:GitHub API(走系统代理)→ Gitee HTML(国内直连)。
/// 全失败才报错;任一源成功即返回(有新版 Some(info),无新版 None)。
pub fn check() -> Result<Option<UpdateInfo>, String> {
    let mut errs: Vec<String> = Vec::new();
    match check_one(GITHUB_API) {
        Ok(found) => return Ok(found),
        Err(e) => errs.push(format!("GitHub: {e}")),
    }
    match check_gitee_html() {
        Ok(found) => return Ok(found),
        Err(e) => errs.push(format!("Gitee: {e}")),
    }
    Err(errs.join("; "))
}

// ---------- 检查任务(独立线程,结果放槽里由 UI 轮询) ----------

/// Some(result) = 检查完成;None = 检查中
static CHECK: OnceLock<Mutex<Option<Result<Option<UpdateInfo>, String>>>> = OnceLock::new();
/// 检查线程防重(UI 侧也有状态机,这里兜底)
static CHECK_BUSY: AtomicBool = AtomicBool::new(false);

fn check_slot() -> &'static Mutex<Option<Result<Option<UpdateInfo>, String>>> {
    CHECK.get_or_init(|| Mutex::new(None))
}

/// 发起一次版本检查(检查中重复调用忽略)
pub fn start_check() {
    if CHECK_BUSY.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::Builder::new()
        .name("pointtofix-update-check".into())
        .spawn(|| {
            let result = check();
            *check_slot().lock().unwrap() = Some(result);
            CHECK_BUSY.store(false, Ordering::SeqCst);
        })
        .ok();
}

/// 取走检查结果:None = 还在查
pub fn poll_check() -> Option<Result<Option<UpdateInfo>, String>> {
    check_slot().lock().unwrap().take()
}

// ---------- 下载任务(独立槽,不经模型下载队列) ----------

struct JobState {
    progress: Progress,
    /// Done 相位对应的版本:防止初始/上次残留的 Done 被误认成「这次下载完成」
    done_for: Option<String>,
    cancel: std::sync::Arc<AtomicBool>,
}

static JOB: OnceLock<Mutex<JobState>> = OnceLock::new();

fn job_slot() -> &'static Mutex<JobState> {
    JOB.get_or_init(|| {
        Mutex::new(JobState {
            progress: Progress { phase: Phase::Done, received: 0, total: 0, error: None },
            done_for: None,
            cancel: std::sync::Arc::new(AtomicBool::new(false)),
        })
    })
}

/// Done 相位归属的版本号(未完成过下载时为 None);UI 用它区分「刚下完」和「历史残留」
pub fn done_version() -> Option<String> {
    let st = job_slot().lock().unwrap();
    if st.progress.phase == Phase::Done {
        st.done_for.clone()
    } else {
        None
    }
}

/// 当前下载进度(总是返回;UI 只在 Available 状态读它,按 phase 分流)
pub fn snapshot() -> Progress {
    job_slot().lock().unwrap().progress.clone()
}

pub fn cancel_download() {
    let st = job_slot().lock().unwrap();
    st.cancel.store(true, Ordering::Relaxed);
}

/// 开始下载并解包升级包(重复调用忽略);完成后 snapshot() 变 Done
pub fn start_download(info: UpdateInfo) {
    let cancel = {
        let mut st = job_slot().lock().unwrap();
        if !matches!(st.progress.phase, Phase::Done | Phase::Failed | Phase::Cancelled) {
            return; // 正在下载
        }
        st.progress = Progress { phase: Phase::Connecting, received: 0, total: 0, error: None };
        st.cancel.store(false, Ordering::Relaxed);
        st.cancel.clone()
    };
    std::thread::Builder::new()
        .name("pointtofix-update-io".into())
        .spawn(move || run_download(info, cancel))
        .ok();
}

fn run_download(info: UpdateInfo, cancel: std::sync::Arc<AtomicBool>) {
    let set = |phase: Phase, received: u64, total: u64, error: Option<String>| {
        job_slot().lock().unwrap().progress =
            Progress { phase, received, total, error };
    };
    let zip_path = update_dir().join("pointtofix-craft-portable.zip");
    let result =
        download::http_download_file(&info.zip_url, &zip_path, &cancel, &|r, t| {
            set(Phase::Downloading, r, t, None)
        });
    match result {
        Err(e) if e == "__cancelled__" => set(Phase::Cancelled, 0, 0, None),
        Err(e) => set(Phase::Failed, 0, 0, Some(e)),
        Ok(()) => match unpack(&zip_path) {
            Ok(()) => {
                job_slot().lock().unwrap().done_for = Some(info.version.clone());
                set(Phase::Done, 0, 0, None);
            }
            Err(e) => set(Phase::Failed, 0, 0, Some(e)),
        },
    }
}

/// 从发布 zip 解包出 pointtofix.exe 到 update_dir()。
/// zip 顶层可能带一层目录(Compress-Archive 的习惯),两种布局都找一遍。
fn unpack(zip_path: &Path) -> Result<(), String> {
    let dir = update_dir();
    let unpack = dir.join("unpack");
    let _ = std::fs::remove_dir_all(&unpack);
    zipmin::extract(zip_path, &unpack, 0)?;
    let src = find_exe(&unpack).ok_or("zip 里没找到 pointtofix.exe")?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("建目录失败: {e}"))?;
    std::fs::copy(&src, staged_exe()).map_err(|e| format!("复制新版本失败: {e}"))?;
    let _ = std::fs::remove_dir_all(&unpack);
    let _ = std::fs::remove_file(zip_path);
    Ok(())
}

/// 在解包目录(及其一层子目录)里找 pointtofix.exe
fn find_exe(dir: &Path) -> Option<PathBuf> {
    let direct = dir.join("pointtofix.exe");
    if direct.is_file() {
        return Some(direct);
    }
    for entry in std::fs::read_dir(dir).ok()? {
        let p = entry.ok()?.path();
        let candidate = p.join("pointtofix.exe");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

// ---------- 应用升级(改名替换 + 重启) ----------

/// 用已就绪的升级包替换当前 exe 并启动新版本;成功后调用方发 Close 退出。
/// 失败会回滚(旧 exe 改回原名),应用继续可用。
pub fn apply_and_restart() -> Result<(), String> {
    let new_exe = staged_exe();
    if !new_exe.is_file() {
        return Err("升级包还没就绪".into());
    }
    let cur = std::env::current_exe().map_err(|e| format!("定位当前程序失败: {e}"))?;
    let bak = cur.with_extension("exe.old");
    if bak.exists() {
        // 上次升级的残留(理论上启动清理已删);删不掉就放弃,提示手动处理
        std::fs::remove_file(&bak).map_err(|e| format!("旧备份无法清理(重启后再试): {e}"))?;
    }
    // 运行中的 exe 不能被删除/覆盖写,但允许改名——Windows 自更新的标准做法
    std::fs::rename(&cur, &bak).map_err(|e| format!("无法改名旧程序(可能被杀毒软件占用): {e}"))?;
    if let Err(e) = std::fs::copy(&new_exe, &cur) {
        let _ = std::fs::rename(&bak, &cur); // 回滚:旧程序恢复原名
        return Err(format!("写入新版本失败: {e}"));
    }
    // 分离启动新版本;当前进程由 UI 层随后 Close 结束。
    // CREATE_NO_WINDOW:debug 版是控制台子系统,避免弹黑框。
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        std::process::Command::new(&cur)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|e| {
                // 新进程没起来:回滚,旧程序还挂在 *.exe.old
                let _ = std::fs::rename(&cur, &new_exe);
                let _ = std::fs::rename(&bak, &cur);
                format!("启动新版本失败: {e}")
            })?;
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new(&cur)
            .spawn()
            .map_err(|e| format!("启动新版本失败: {e}"))?;
    }
    Ok(())
}

/// 启动清理(新进程开头调用):等旧实例退干净后删掉 *.exe.old,
/// 清掉升级目录里没下完的 .part。放在后台线程,不拖慢启动。
pub fn cleanup_startup() {
    std::thread::Builder::new()
        .name("pointtofix-update-clean".into())
        .spawn(|| {
            if let Ok(cur) = std::env::current_exe() {
                let bak = cur.with_extension("exe.old");
                for _ in 0..3 {
                    if !bak.exists() {
                        break;
                    }
                    if std::fs::remove_file(&bak).is_ok() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(300));
                }
            }
            if let Ok(entries) = std::fs::read_dir(update_dir()) {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    if name.to_string_lossy().ends_with(".part") {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
        })
        .ok();
}

// ---------- 测试 ----------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_str_finds_first_value() {
        assert_eq!(json_str(r#"{"tag_name": "v0.3.2"}"#, "tag_name"), Some("v0.3.2".into()));
        assert_eq!(json_str("no key here", "tag_name"), None);
    }

    #[test]
    fn json_all_strs_lists_every_value() {
        let body = r#"{"assets":[
            {"name":"a.zip","browser_download_url":"https://x/a.zip"},
            {"name":"b.exe","browser_download_url":"https://x/b.exe"},
            {"name":"pointtofix-craft-portable.zip","browser_download_url":"https://x/pointtofix-craft-portable.zip"}]}"#;
        let urls = json_all_strs(body, "browser_download_url");
        assert_eq!(urls.len(), 3);
        assert!(urls.iter().any(|u| u.ends_with("/b.exe")));
    }

    #[test]
    fn parse_release_prefers_portable_zip() {
        let body = r#"{"tag_name":"v0.4.0","assets":[
            {"browser_download_url":"https://x/src.tar.gz"},
            {"browser_download_url":"https://x/pointtofix-craft-portable.zip"}]}"#;
        let info = parse_release(body).unwrap();
        assert_eq!(info.version, "v0.4.0");
        assert!(info.zip_url.ends_with("pointtofix-craft-portable.zip"));
    }

    #[test]
    fn parse_release_without_zip_fails() {
        let body = r#"{"tag_name":"v0.4.0","assets":[{"browser_download_url":"https://x/src.tar.gz"}]}"#;
        assert!(parse_release(body).is_err());
    }

    #[test]
    fn parse_gitee_html_takes_latest_tag() {
        // 发布页最新 release 排最前;取第一个 tag,zip 直链按固定规则拼
        let html = r#"<a href="/chenghao_wan/pointtofixcraft/releases/tag/v0.3.1">v0.3.1</a>
            <a href="/chenghao_wan/pointtofixcraft/releases/tag/v0.3.0">v0.3.0</a>"#;
        let info = parse_gitee_html(html).unwrap();
        assert_eq!(info.version, "v0.3.1");
        assert!(info.zip_url.ends_with("/releases/download/v0.3.1/pointtofix-craft-portable.zip"));
        assert!(!is_newer("0.3.1", &info.version));
        assert!(is_newer("0.3.1", "v0.3.2"));
    }

    #[test]
    fn parse_gitee_html_without_tag_fails() {
        assert!(parse_gitee_html("<html>nothing here</html>").is_err());
    }

    #[test]
    fn version_compare_numeric() {
        assert!(is_newer("0.3.1", "v0.3.2"));
        assert!(is_newer("0.3.1", "v0.4"));
        assert!(is_newer("0.9", "v0.10"));
        assert!(!is_newer("0.3.1", "0.3.1"));
        assert!(!is_newer("0.3.2", "v0.3.1"));
        assert!(!is_newer("0.3.1", "v0.3"));
        // 坏版本号按 0 处理,不至于 panic
        assert!(!is_newer("0.3.1", "v0.3.x"));
    }

    #[test]
    fn apply_file_mechanics_rename_copy_rollback() {
        // apply_and_restart 的文件机制单测:改名 → 复制 → 失败回滚
        let dir = std::env::temp_dir().join(format!("ptf-update-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cur = dir.join("app.exe");
        let new = dir.join("new.exe");
        std::fs::write(&cur, b"old").unwrap();
        std::fs::write(&new, b"new").unwrap();
        let bak = cur.with_extension("exe.old");
        // 成功路径
        std::fs::rename(&cur, &bak).unwrap();
        std::fs::copy(&new, &cur).unwrap();
        assert_eq!(std::fs::read(&cur).unwrap(), b"new");
        assert_eq!(std::fs::read(&bak).unwrap(), b"old");
        // 回滚路径:复制失败(源不存在)→ 改回原名
        std::fs::remove_file(&cur).unwrap();
        std::fs::rename(&bak, &cur).unwrap();
        assert_eq!(std::fs::read(&cur).unwrap(), b"old");
        assert!(!bak.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 在线验证真实 WinHTTP 链路(curl 不走系统代理,行为不代表 WinHTTP)。
    /// cargo test -- --ignored 手动跑;网络不可达时失败属正常。
    #[test]
    #[ignore]
    fn live_github_api_reachable_via_winhttp() {
        let r = check_one(GITHUB_API);
        println!("github check_one: {r:?}");
        assert!(r.is_ok(), "GitHub API 经 WinHTTP 失败: {r:?}");
    }

    #[test]
    #[ignore]
    fn live_gitee_html_reachable_via_winhttp() {
        let r = check_gitee_html();
        println!("gitee html check: {r:?}");
        assert!(r.is_ok(), "Gitee 发布页经 WinHTTP 失败: {r:?}");
    }

    /// 验证 Gitee 附件直链能否经 WinHTTP 下载(升级真正要走的路),
    /// 下载后顺带用真实发布 zip 验证解包与 pointtofix.exe 定位
    #[test]
    #[ignore]
    fn live_gitee_zip_downloadable_via_winhttp() {
        let url = format!("{GITEE_DOWNLOAD_BASE}/v0.3.1/{ZIP_ASSET}");
        let dest = std::env::temp_dir().join("ptf-live-zip-test.zip");
        let _ = std::fs::remove_file(&dest);
        let r = download::http_download_file(&url, &dest, &AtomicBool::new(false), &|_, _| {});
        let size = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
        println!("gitee zip download: {r:?}, {size} bytes");
        assert!(r.is_ok(), "Gitee zip 直链下载失败: {r:?}");
        assert!(size > 1_000_000, "zip 尺寸不对: {size}");
        // 真实发布 zip 解包到临时目录,确认能定位到 pointtofix.exe
        let out = std::env::temp_dir().join("ptf-live-unpack-test");
        let _ = std::fs::remove_dir_all(&out);
        zipmin::extract(&dest, &out, 0).expect("真实 zip 解包失败");
        let exe = find_exe(&out).expect("发布 zip 里没找到 pointtofix.exe");
        println!("unpacked exe: {}", exe.display());
        let _ = std::fs::remove_dir_all(&out);
        let _ = std::fs::remove_file(&dest);
    }
}
