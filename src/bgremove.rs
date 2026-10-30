//! 选区去背:键控(keying)+ ISNet ONNX 模型的后台任务调度。
//!
//! 去背方式由 [`BgMethod`] 选择:键控、ISNet、或自动(有模型用 ISNet,失败回退键控)。
//! ISNet 首次要解析/优化 ~176 MB 模型,1024×1024 CPU 推理也慢,所以统一放到
//! 后台线程([`submit`] / [`poll`]),UI 只负责抓图、显示进度和替换选区画面。
//!
//! 下面是键控算法本身:
//!
//! 1. **取键**:边框直方图里出现最多的颜色作为背景键;再从边框种子做一次泛洪,
//!    把渐变背景的整段色域都收进"背景色板"(跨出选框的标注颜色差得远,不会被收进来);
//! 2. **距离场**:把色板量化成 6 bit/通道,在颜色空间里铺 Chebyshev 距离场,
//!    每个色格记住最近的那个背景色格;
//! 3. **键控**:全图逐像素比较"实际颜色 ↔ 最近背景色",≤ 容差直接涂白,
//!    容差外一圈按比例向白过渡(软键边,消掉抗锯齿彩边)。
//!
//! 与逐像素连通泛洪的关键区别:第 3 步只看键值、不看连通性。被细线切开的背景块、
//! 被物体围住的镂空背景、以及框内其它位置的同色背景都会一起去掉。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::capture::Shot;
use crate::isnet;

/// 泛洪判定相邻像素同色系的容差(每通道)。只用于生长背景色板,不直接决定去留。
/// 调小一点(8):避免从背景"渗"进颜色接近的物体侧面(如 CAD 的灰色侧壁)。
const FLOOD_TOLERANCE: i32 = 8;
/// 背景键种子容差:只让背景聚类附近的颜色参与泛洪;太大会把物体边缘色也当成背景
const ANCHOR_SEED_TOLERANCE: i32 = 14;
/// 未与边框背景连通的区域:按"连通块平均色距"判定。
/// 真正的封闭背景块(表格格/孔底)平均色距≈0~2,整块去掉;
/// 孔壁高光/零件侧面虽有部分像素和背景同色,但平均色距更大,整块保留。
const ENCLOSED_AVG_TOLERANCE: f32 = 3.0;
/// 键控容差:与最近背景色的每通道差 ≤ 此值 → 完全按背景抹白
const KEY_TOLERANCE: i32 = 14;
/// 软键边宽度:容差外这一圈按比例向白混合,消除物体边缘的抗锯齿彩边
const KEY_FEATHER: i32 = 10;

/// 颜色量化:6 bit/通道(4 单位一格)
const QBITS: u32 = 6;
const QSHIFT: u32 = 8 - QBITS;
const QSIZE: i32 = 1 << QBITS;
const QBIN: i32 = 1 << QSHIFT;
const QN: usize = (QSIZE * QSIZE * QSIZE) as usize;
/// 距离场铺开的格数:足够覆盖「容差 + 软键边」再留一格余量
const MAX_BINS: i32 = 8;

fn qbin(r: u8, g: u8, b: u8) -> u32 {
    ((r as u32 >> QSHIFT) << (2 * QBITS))
        | ((g as u32 >> QSHIFT) << QBITS)
        | (b as u32 >> QSHIFT)
}

/// 色格中心色:用于精确到像素的颜色距离(只有"最近色格"被量化,距离本身不量化)
fn bin_center(k: u32) -> (i32, i32, i32) {
    let mask = (QSIZE - 1) as u32;
    let half = QBIN / 2;
    (
        (((k >> (2 * QBITS)) & mask) as i32) * QBIN + half,
        (((k >> QBITS) & mask) as i32) * QBIN + half,
        ((k & mask) as i32) * QBIN + half,
    )
}

fn cheb(a: (i32, i32, i32), b: (i32, i32, i32)) -> i32 {
    (a.0 - b.0).abs().max((a.1 - b.1).abs()).max((a.2 - b.2).abs())
}

/// 选区去背方式(设置面板可切,持久化到 ui.txt)
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum BgMethod {
    /// 有 ISNet 模型就用 AI,否则自动回退键控
    #[default]
    Auto,
    /// 纯颜色键控:快、无需模型,适合纯色/渐变背景
    Keying,
    /// 只用 ISNet 模型;模型缺失/失败时报错,不静默回退
    Isnet,
}

impl BgMethod {
    pub fn code(self) -> &'static str {
        match self {
            BgMethod::Auto => "auto",
            BgMethod::Keying => "keying",
            BgMethod::Isnet => "isnet",
        }
    }

    pub fn from_code(code: &str) -> Option<BgMethod> {
        match code.trim() {
            "auto" => Some(BgMethod::Auto),
            "keying" => Some(BgMethod::Keying),
            "isnet" => Some(BgMethod::Isnet),
            _ => None,
        }
    }
}

/// 实际完成去背的引擎
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BgEngine {
    Keying,
    Isnet,
}

/// 后台去背遇到的问题
#[derive(Clone, Debug)]
pub enum BgIssue {
    /// 没找到任何 ISNet 模型文件
    ModelMissing,
    /// 模型加载/推理失败(消息为技术细节)
    Error(String),
}

/// 后台去背任务的结果
pub struct BgDone {
    pub id: u64,
    pub width: i32,
    pub height: i32,
    /// None = 失败(显式 ISNet 模式);Some = 处理后的 RGBA
    pub rgba: Option<Vec<u8>>,
    pub engine: BgEngine,
    /// Some = 请求的是 AI,但自动回退到了键控
    pub issue: Option<BgIssue>,
    /// 后台处理总耗时(含首次加载/优化模型)
    pub elapsed: Duration,
}

struct Job {
    id: u64,
    shot: Shot,
    method: BgMethod,
    /// 键控结果是否把浅色前景反相压暗(白底可读性)
    dark_light: bool,
}

struct Worker {
    tx: Sender<Job>,
    rx: Mutex<Receiver<BgDone>>,
}

static WORKER: OnceLock<Worker> = OnceLock::new();
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn worker() -> &'static Worker {
    WORKER.get_or_init(|| {
        let (job_tx, job_rx) = channel::<Job>();
        let (done_tx, done_rx) = channel::<BgDone>();
        std::thread::Builder::new()
            .name("pointtofix-isnet".into())
            // ONNX Runtime 首次跑大模型需要较深的调用栈/临时内存,给足栈空间
            .stack_size(32 * 1024 * 1024)
            .spawn(move || worker_loop(job_rx, done_tx))
            .expect("无法启动去背线程");
        Worker {
            tx: job_tx,
            rx: Mutex::new(done_rx),
        }
    })
}

/// 提交一次后台去背(不阻塞 UI);返回任务 id,用于丢弃过期结果
pub fn submit(shot: Shot, method: BgMethod, dark_light: bool) -> Option<u64> {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    worker()
        .tx
        .send(Job {
            id,
            shot,
            method,
            dark_light,
        })
        .ok()?;
    Some(id)
}

/// 非阻塞取回已完成的任务(没有完成的任务时返回 None)
pub fn poll() -> Option<BgDone> {
    worker().rx.lock().ok()?.try_recv().ok()
}

struct ModelSlot {
    path: Option<PathBuf>,
    model: Option<isnet::Isnet>,
    error: Option<String>,
}

impl ModelSlot {
    /// 惰性加载并缓存模型;同一路径加载失败后不反复重试(大模型解析很慢)
    fn get(&mut self, path: &Path) -> Result<&mut isnet::Isnet, String> {
        if self.path.as_deref() != Some(path) {
            self.path = Some(path.to_path_buf());
            self.model = None;
            self.error = None;
        }
        if let Some(err) = &self.error {
            return Err(err.clone());
        }
        if self.model.is_none() {
            match isnet::Isnet::load(path) {
                Ok(model) => {
                    let (w, h) = model.input_size();
                    eprintln!(
                        "[isnet] 已加载 {} ({}×{}, 加载 {:?})",
                        model.path().display(),
                        w,
                        h,
                        model.load_time()
                    );
                    self.model = Some(model);
                }
                Err(e) => {
                    eprintln!("[isnet] 加载失败: {e}");
                    self.error = Some(e.clone());
                    return Err(e);
                }
            }
        }
        self.model
            .as_mut()
            .ok_or_else(|| "模型未加载".to_string())
    }
}

fn worker_loop(rx: Receiver<Job>, tx: Sender<BgDone>) {
    let mut slot = ModelSlot {
        path: None,
        model: None,
        error: None,
    };
    while let Ok(job) = rx.recv() {
        let started = Instant::now();
        let (width, height) = (job.shot.width, job.shot.height);
        let (rgba, engine, issue) = run_job(&job, &mut slot);
        let done = BgDone {
            id: job.id,
            width,
            height,
            rgba,
            engine,
            issue,
            elapsed: started.elapsed(),
        };
        if tx.send(done).is_err() {
            break; // UI 已退出
        }
    }
}

fn run_job(job: &Job, slot: &mut ModelSlot) -> (Option<Vec<u8>>, BgEngine, Option<BgIssue>) {
    if job.method == BgMethod::Keying {
        return (
            Some(keying_whiten(&job.shot, job.dark_light).rgba),
            BgEngine::Keying,
            None,
        );
    }

    let explicit = job.method == BgMethod::Isnet;
    // 自动模式先跑一次键控:纯色/渐变背景(截图、图表、CAD、表格)用键控更稳,
    // ISNet 是显著性分割,细线/网格线容易被当成背景抹掉。
    let mut keying = if explicit {
        None
    } else {
        Some(keying_whiten(&job.shot, job.dark_light))
    };
    if let Some(k) = keying.as_ref() {
        if std::env::var("POINTTOFIX_DEBACK_LOG").is_ok() {
            eprintln!(
                "[isnet] auto: keying bg={:.1}% (阈值 {:.0}%)",
                k.bg_fraction * 100.0,
                AUTO_KEYING_MIN_BG * 100.0
            );
        }
        if k.bg_fraction >= AUTO_KEYING_MIN_BG {
            let rgba = keying.take().expect("keying outcome").rgba;
            return (Some(rgba), BgEngine::Keying, None);
        }
    }

    let fallback_keying = |keying: &mut Option<KeyingOutcome>| {
        keying
            .take()
            .map(|k| k.rgba)
            .unwrap_or_else(|| keying_whiten(&job.shot, job.dark_light).rgba)
    };

    let Some(path) = isnet::find_model() else {
        return if explicit {
            (None, BgEngine::Isnet, Some(BgIssue::ModelMissing))
        } else {
            (
                Some(fallback_keying(&mut keying)),
                BgEngine::Keying,
                None,
            )
        };
    };

    // ONNX Runtime/模型异常时可能 panic:兜住,避免后台线程直接死掉
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        slot.get(&path).and_then(|model| model.whiten(&job.shot))
    }));
    let failure = match result {
        Ok(Ok(rgba)) => return (Some(rgba), BgEngine::Isnet, None),
        Ok(Err(e)) => e,
        Err(_) => {
            // 同一模型不再重试,避免每个任务都 panic 一次
            let e = "模型推理异常(panic)".to_string();
            slot.error = Some(e.clone());
            e
        }
    };
    if explicit {
        (None, BgEngine::Isnet, Some(BgIssue::Error(failure)))
    } else {
        (
            Some(fallback_keying(&mut keying)),
            BgEngine::Keying,
            Some(BgIssue::Error(failure)),
        )
    }
}

/// 自动模式:键控判为背景的像素占比 ≥ 此值时优先用键控。
/// 截图/图表/CAD/表格的键控很可靠(即使物体占了大半,背景仍有 15%+);
/// 只有键控几乎找不到背景(复杂照片)时才交给 ISNet。
const AUTO_KEYING_MIN_BG: f32 = 0.15;
/// 浅色前景反相:亮度高于此值(0-255)的键控前景会被压暗,避免白底上浅灰文字看不清
const DARKEN_LUMINANCE: f32 = 166.0;
/// 只有白底占主导时才反相;键控没去掉多少背景时保持原色更安全
const DARKEN_MIN_BG: f32 = 0.50;

/// 键控结果 + 决策依据
pub struct KeyingOutcome {
    pub rgba: Vec<u8>,
    /// 被键控完全判为背景的像素占全图比例
    pub bg_fraction: f32,
}

/// 返回一张把背景涂白的 RGBA 图(尺寸同输入)。不反相浅色前景(测试用)
#[cfg(test)]
pub fn whiten_background(shot: &Shot) -> Vec<u8> {
    keying_whiten(shot, false).rgba
}

/// 键控去背 + 统计背景占比(Auto 模式用来决定是否交给 ISNet)。
/// `darken_light`:白底占主导时,把亮度高的前景(深底上的浅灰文字)反相压暗。
pub fn keying_whiten(shot: &Shot, darken_light: bool) -> KeyingOutcome {
    let w = shot.width.max(1) as usize;
    let h = shot.height.max(1) as usize;
    if shot.rgba.len() < w * h * 4 {
        return KeyingOutcome {
            rgba: shot.rgba.clone(),
            bg_fraction: 0.0,
        };
    }
    let (palette, visited) = background_palette(shot, w, h);
    if palette.is_empty() {
        return KeyingOutcome {
            rgba: shot.rgba.clone(),
            bg_fraction: 0.0,
        };
    }
    let nearest = nearest_key_grid(&palette);
    let (rgba, bg_pixels) = key_out(shot, w, h, &nearest, &visited, darken_light);
    KeyingOutcome {
        rgba,
        bg_fraction: bg_pixels as f32 / (w * h) as f32,
    }
}

/// 亮度(0-255,Rec.601 权重)
fn luminance(p: &[u8]) -> f32 {
    0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32
}

/// 浅色前景反相:保留色相、把亮度翻到暗部。
/// 例:200,200,200(浅灰文字)→ 55,55,55;255,255,0(浅黄)→ 深橄榄色。
fn darken_light_pixel(p: &mut [u8]) {
    let l = luminance(p);
    if l < DARKEN_LUMINANCE {
        return;
    }
    let target = 255.0 - l;
    let scale = if l > 1.0 { target / l } else { 0.0 };
    for c in 0..3 {
        p[c] = (p[c] as f32 * scale).round().clamp(0.0, 255.0) as u8;
    }
}

/// 只压暗"小而孤立"的浅色前景组件(文字/细线)。
/// 大块浅色物体(浅色零件面、白色面板)保持原色;贴边的浅色组件(白边/贴边文字)
/// 也不动,避免把截图边框反相成黑框。
fn darken_small_light_components(out: &mut [u8], mask: &[bool], w: usize, h: usize) {
    // 组件面积上限:全图 2%(至少 512 像素)。超过就认为是大块物体,不反相。
    let max_area = (w * h / 50).max(512);
    let mut seen = vec![false; w * h];
    let mut stack: Vec<u32> = Vec::new();
    let mut pixels: Vec<u32> = Vec::new();
    for start in 0..w * h {
        if !mask[start] || seen[start] {
            continue;
        }
        seen[start] = true;
        stack.clear();
        stack.push(start as u32);
        pixels.clear();
        let mut touches_border = false;
        let mut too_big = false;
        while let Some(iu) = stack.pop() {
            let i = iu as usize;
            let x = i % w;
            let y = i / w;
            if x == 0 || y == 0 || x + 1 == w || y + 1 == h {
                touches_border = true;
            }
            if !too_big {
                pixels.push(iu);
                if pixels.len() > max_area {
                    too_big = true;
                    pixels.clear();
                }
            }
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    if dx == 0 && dy == 0 {
                        continue;
                    }
                    let nx = x as i32 + dx;
                    let ny = y as i32 + dy;
                    if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                        continue;
                    }
                    let ni = ny as usize * w + nx as usize;
                    if mask[ni] && !seen[ni] {
                        seen[ni] = true;
                        stack.push(ni as u32);
                    }
                }
            }
        }
        if !too_big && !touches_border {
            for &iu in &pixels {
                let b = iu as usize * 4;
                darken_light_pixel(&mut out[b..b + 3]);
            }
        }
    }
}

/// 从边框取背景键并泛洪出完整背景色板;`visited` 是从边框连通出去的背景区域,
/// 用来区分"背景本体"和"颜色接近背景、但被物体包住的侧面/孔壁"。
fn background_palette(shot: &Shot, w: usize, h: usize) -> (HashSet<u32>, Vec<bool>) {
    let src = &shot.rgba;

    // 选框四边像素(去重)。跳过最外 2px:截图/导出图经常带一圈白边或选框描边,
    // 直接拿最外圈会把"白边"当成背景键,真正的背景反而完全没去掉。
    let inset = if w > 8 && h > 8 { 2 } else { 0 };
    let (x0, x1) = (inset, w - 1 - inset);
    let (y0, y1) = (inset, h - 1 - inset);
    let mut border: Vec<usize> = Vec::with_capacity(2 * (w + h));
    for x in x0..=x1 {
        border.push(y0 * w + x);
        if y1 != y0 {
            border.push(y1 * w + x);
        }
    }
    for y in y0 + 1..y1 {
        border.push(y * w + x0);
        if x1 != x0 {
            border.push(y * w + x1);
        }
    }

    // 边框颜色聚类:6bit 直方图会被渐变背景拆成很多小格,反而让"占满边框的大块物体"
    // 胜出。这里用 4bit 粗直方图 + 3×3×3 邻域合并,再按邻域像素数取最多的颜色作为背景键。
    const ANCHOR_BITS: u32 = 4;
    const ANCHOR_SIZE: i32 = 1 << ANCHOR_BITS;
    const ANCHOR_SHIFT: u32 = 8 - ANCHOR_BITS;
    const ANCHOR_N: usize = (ANCHOR_SIZE * ANCHOR_SIZE * ANCHOR_SIZE) as usize;
    let abin = |r: u8, g: u8, b: u8| -> usize {
        (((r as usize) >> ANCHOR_SHIFT) << (2 * ANCHOR_BITS))
            | (((g as usize) >> ANCHOR_SHIFT) << ANCHOR_BITS)
            | ((b as usize) >> ANCHOR_SHIFT)
    };
    let mut coarse = vec![0u32; ANCHOR_N];
    for &i in &border {
        let b = i * 4;
        coarse[abin(src[b], src[b + 1], src[b + 2])] += 1;
    }

    let mut anchor_rgb = (0, 0, 0);
    let mut best = 0u64;
    for r in 0..ANCHOR_SIZE {
        for g in 0..ANCHOR_SIZE {
            for b in 0..ANCHOR_SIZE {
                let (mut sum, mut sr, mut sg, mut sb) = (0u64, 0u64, 0u64, 0u64);
                for dr in -1i32..=1 {
                    let rr = r + dr;
                    if !(0..ANCHOR_SIZE).contains(&rr) {
                        continue;
                    }
                    for dg in -1i32..=1 {
                        let gg = g + dg;
                        if !(0..ANCHOR_SIZE).contains(&gg) {
                            continue;
                        }
                        for db in -1i32..=1 {
                            let bb = b + db;
                            if !(0..ANCHOR_SIZE).contains(&bb) {
                                continue;
                            }
                            let idx = ((rr as usize) << (2 * ANCHOR_BITS))
                                | ((gg as usize) << ANCHOR_BITS)
                                | (bb as usize);
                            let c = coarse[idx] as u64;
                            if c == 0 {
                                continue;
                            }
                            sum += c;
                            sr += c * ((rr as u64) * 16 + 8);
                            sg += c * ((gg as u64) * 16 + 8);
                            sb += c * ((bb as u64) * 16 + 8);
                        }
                    }
                }
                if sum > best {
                    best = sum;
                    anchor_rgb = (
                        (sr / sum) as i32,
                        (sg / sum) as i32,
                        (sb / sum) as i32,
                    );
                }
            }
        }
    }
    if std::env::var("POINTTOFIX_DEBACK_LOG").is_ok() {
        let mut top: Vec<(usize, u32)> = coarse
            .iter()
            .enumerate()
            .filter(|(_, c)| **c > 0)
            .map(|(i, c)| (i, *c))
            .collect();
        top.sort_by(|a, b| b.1.cmp(&a.1));
        let top: Vec<_> = top
            .iter()
            .take(5)
            .map(|(i, c)| {
                let r = (i >> (2 * ANCHOR_BITS)) & (ANCHOR_SIZE as usize - 1);
                let g = (i >> ANCHOR_BITS) & (ANCHOR_SIZE as usize - 1);
                let b = i & (ANCHOR_SIZE as usize - 1);
                ((r * 16 + 8, g * 16 + 8, b * 16 + 8), *c)
            })
            .collect();
        eprintln!(
            "[keying] border={} anchor={:?} cluster_pixels={} top={:?}",
            border.len(),
            anchor_rgb,
            best,
            top
        );
    }

    // 种子:边框上颜色接近背景键的像素
    let mut visited = vec![false; w * h];
    let mut queue: Vec<usize> = Vec::with_capacity(w * h / 4);
    for &i in &border {
        let b = i * 4;
        let px = (src[b] as i32, src[b + 1] as i32, src[b + 2] as i32);
        if cheb(px, anchor_rgb) <= ANCHOR_SEED_TOLERANCE {
            visited[i] = true;
            queue.push(i);
        }
    }

    // 相邻色容差泛洪:让渐变背景一路连到底
    let mut head = 0usize;
    while head < queue.len() {
        let i = queue[head];
        head += 1;
        let x = i % w;
        let y = i / w;
        let b0 = i * 4;
        let cur = (
            src[b0] as i32,
            src[b0 + 1] as i32,
            src[b0 + 2] as i32,
        );

        let mut neighbors = [0usize; 4];
        let mut n = 0usize;
        if x > 0 {
            neighbors[n] = i - 1;
            n += 1;
        }
        if x + 1 < w {
            neighbors[n] = i + 1;
            n += 1;
        }
        if y > 0 {
            neighbors[n] = i - w;
            n += 1;
        }
        if y + 1 < h {
            neighbors[n] = i + w;
            n += 1;
        }

        for &ni in &neighbors[..n] {
            if visited[ni] {
                continue;
            }
            let b2 = ni * 4;
            let npx = (
                src[b2] as i32,
                src[b2 + 1] as i32,
                src[b2 + 2] as i32,
            );
            if cheb(npx, cur) <= FLOOD_TOLERANCE {
                visited[ni] = true;
                queue.push(ni);
            }
        }
    }

    // 只取"内部"背景像素上色:物体边缘的抗锯齿过渡色也算被泛洪到过,
    // 但它们离物体太近,收进色板会把物体边缘一起键掉。
    let mut palette: HashSet<u32> = HashSet::new();
    let mut interior = 0usize;
    for i in 0..w * h {
        if !visited[i] {
            continue;
        }
        let x = i % w;
        let y = i / w;
        let inside = (x == 0 || visited[i - 1])
            && (x + 1 == w || visited[i + 1])
            && (y == 0 || visited[i - w])
            && (y + 1 == h || visited[i + w]);
        if inside {
            interior += 1;
            let b = i * 4;
            palette.insert(qbin(src[b], src[b + 1], src[b + 2]));
        }
    }
    // 背景只有很细的一圈(内部像素太少)时退回全部背景像素
    if interior * 4 < w * h {
        for (i, &is_bg) in visited.iter().enumerate() {
            if is_bg {
                let b = i * 4;
                palette.insert(qbin(src[b], src[b + 1], src[b + 2]));
            }
        }
    }
    if std::env::var("POINTTOFIX_DEBACK_LOG").is_ok() {
        let visited_n = visited.iter().filter(|v| **v).count();
        eprintln!(
            "[keying] seeds={} visited={} interior={} palette={}",
            queue.len(),
            visited_n,
            interior,
            palette.len()
        );
    }
    (palette, visited)
}

/// 颜色空间 Chebyshev 距离场:每个色格记录最近背景色格的编号
fn nearest_key_grid(palette: &HashSet<u32>) -> Vec<u32> {
    let mut nearest = vec![u32::MAX; QN];
    let mut queue: Vec<u32> = Vec::with_capacity(palette.len() * 8);
    for &k in palette {
        if nearest[k as usize] == u32::MAX {
            nearest[k as usize] = k;
            queue.push(k);
        }
    }

    let mask = (QSIZE - 1) as u32;
    let mut head = 0usize;
    let mut layer_end = queue.len();
    let mut depth = 0i32;
    while head < queue.len() && depth < MAX_BINS {
        while head < layer_end {
            let k = queue[head];
            head += 1;
            let key_bin = nearest[k as usize];
            let r = (k >> (2 * QBITS)) & mask;
            let g = (k >> QBITS) & mask;
            let b = k & mask;
            for dr in -1i32..=1 {
                let rr = r as i32 + dr;
                if !(0..QSIZE).contains(&rr) {
                    continue;
                }
                for dg in -1i32..=1 {
                    let gg = g as i32 + dg;
                    if !(0..QSIZE).contains(&gg) {
                        continue;
                    }
                    for db in -1i32..=1 {
                        let bb = b as i32 + db;
                        if !(0..QSIZE).contains(&bb) {
                            continue;
                        }
                        let n = ((rr as u32) << (2 * QBITS))
                            | ((gg as u32) << QBITS)
                            | (bb as u32);
                        if nearest[n as usize] == u32::MAX {
                            nearest[n as usize] = key_bin;
                            queue.push(n);
                        }
                    }
                }
            }
        }
        layer_end = queue.len();
        depth += 1;
    }
    nearest
}

/// 按键控判定整幅图:全白 / 软键边混合 / 原样保留;返回 (结果, 完全判为背景的像素数)
fn key_out(
    shot: &Shot,
    w: usize,
    h: usize,
    nearest: &[u32],
    visited: &[bool],
    darken_light: bool,
) -> (Vec<u8>, usize) {
    let mut out = shot.rgba.clone();
    let full = KEY_TOLERANCE + KEY_FEATHER;
    let mut bg_pixels = 0usize;
    // 完全保留的浅色前景(通常是深色背景上的浅灰文字),等确认白底占主导后再压暗
    let mut light_mask = vec![false; w * h];
    // 未与边框背景连通、但颜色落在背景容差内的像素:稍后按连通块整体判定
    let mut candidate = vec![false; w * h];
    let mut dist = vec![0u8; w * h];

    for i in 0..w * h {
        let b = i * 4;
        let px = (out[b] as i32, out[b + 1] as i32, out[b + 2] as i32);
        let key = nearest[qbin(out[b], out[b + 1], out[b + 2]) as usize];
        if key == u32::MAX {
            // 离背景色板太远(超出距离场):一定是前景。深底上的浅色文字通常落在这里
            if darken_light && luminance(&out[b..b + 3]) >= DARKEN_LUMINANCE {
                light_mask[i] = true;
            }
            continue;
        }
        let d = cheb(px, bin_center(key));
        if d > full {
            if darken_light && luminance(&out[b..b + 3]) >= DARKEN_LUMINANCE {
                light_mask[i] = true;
            }
            continue;
        }
        if !visited[i] {
            // 先记录下来,等整个连通块的平均色距算完再决定
            candidate[i] = true;
            dist[i] = d.min(255) as u8;
            continue;
        }
        if d <= KEY_TOLERANCE {
            out[b] = 255;
            out[b + 1] = 255;
            out[b + 2] = 255;
            out[b + 3] = 255;
            bg_pixels += 1;
        } else {
            // 软键边:越靠近容差越接近纯白,到 full 处完全保留原色
            let n = (full - d) as u32;
            let f = KEY_FEATHER as u32;
            for c in 0..3 {
                let v = out[b + c] as u32;
                out[b + c] = ((v * (f - n) + 255 * n) / f) as u8;
            }
            out[b + 3] = 255;
        }
    }

    // 按连通块判定未连通区域:平均色距很小的整块是封闭背景(表格格/孔底),
    // 孔壁高光、零件侧面这类"部分像素接近背景"的块整体保留。
    {
        let mut seen = vec![false; w * h];
        let mut stack: Vec<u32> = Vec::new();
        let mut pixels: Vec<u32> = Vec::new();
        for start in 0..w * h {
            if !candidate[start] || seen[start] {
                continue;
            }
            seen[start] = true;
            stack.clear();
            stack.push(start as u32);
            pixels.clear();
            let mut sum = 0u32;
            while let Some(iu) = stack.pop() {
                let i = iu as usize;
                pixels.push(iu);
                sum += dist[i] as u32;
                let x = i % w;
                let y = i / w;
                for dy in -1i32..=1 {
                    for dx in -1i32..=1 {
                        if dx == 0 && dy == 0 {
                            continue;
                        }
                        let nx = x as i32 + dx;
                        let ny = y as i32 + dy;
                        if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                            continue;
                        }
                        let ni = ny as usize * w + nx as usize;
                        if candidate[ni] && !seen[ni] {
                            seen[ni] = true;
                            stack.push(ni as u32);
                        }
                    }
                }
            }
            let avg = sum as f32 / pixels.len().max(1) as f32;
            if avg <= ENCLOSED_AVG_TOLERANCE {
                for &iu in &pixels {
                    let b = iu as usize * 4;
                    out[b] = 255;
                    out[b + 1] = 255;
                    out[b + 2] = 255;
                    out[b + 3] = 255;
                    bg_pixels += 1;
                }
            } else if darken_light {
                // 保留的块里如果有浅色像素,照常参与"小文字转黑"
                for &iu in &pixels {
                    let b = iu as usize * 4;
                    if luminance(&out[b..b + 3]) >= DARKEN_LUMINANCE {
                        light_mask[iu as usize] = true;
                    }
                }
            }
        }
    }

    let bg_fraction = bg_pixels as f32 / (w * h).max(1) as f32;
    if darken_light && bg_fraction >= DARKEN_MIN_BG {
        darken_small_light_components(&mut out, &light_mask, w, h);
    }
    (out, bg_pixels)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::Shot;

    fn shot_of(w: i32, h: i32, rgba: Vec<u8>) -> Shot {
        Shot { width: w, height: h, rgba }
    }

    #[test]
    fn bg_method_codes_roundtrip() {
        for m in [BgMethod::Auto, BgMethod::Keying, BgMethod::Isnet] {
            assert_eq!(BgMethod::from_code(m.code()), Some(m));
        }
        assert_eq!(BgMethod::from_code("nope"), None);
        assert_eq!(BgMethod::default(), BgMethod::Auto);
    }

    /// 白底细线(表格/图表/CAD):键控必须保留线条,Auto 也应判定"背景占比高 → 用键控"
    #[test]
    fn keying_keeps_thin_lines_and_reports_high_background_fraction() {
        let (w, h) = (200i32, 160i32);
        let mut rgba = vec![255u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let line = x % 20 == 0 || y % 20 == 0;
                if line {
                    rgba[i] = 0;
                    rgba[i + 1] = 0;
                    rgba[i + 2] = 0;
                }
            }
        }
        let shot = shot_of(w, h, rgba);
        let out = keying_whiten(&shot, false);
        assert!(
            out.bg_fraction > 0.85,
            "background fraction: {}",
            out.bg_fraction
        );
        let px = |x: i32, y: i32| px_of(&out.rgba, w, x, y);
        // 背景变白,1px 黑线保留
        assert_eq!(px(5, 5), (255, 255, 255));
        assert_eq!(px(20, 7), (0, 0, 0));
        assert_eq!(px(7, 40), (0, 0, 0));
    }

    /// 自动模式的判定阈值:键控能找到背景(≥15%)就走键控,复杂照片才交给 ISNet
    #[test]
    fn auto_uses_keying_when_background_is_clean() {
        assert!(AUTO_KEYING_MIN_BG <= 0.30, "阈值过松");
        assert!(AUTO_KEYING_MIN_BG >= 0.05, "阈值过严");
    }

    /// 深色背景 + 浅灰文字:去背后文字应被反相压暗,否则白底上看不见
    #[test]
    fn keying_darkens_light_text_on_dark_background() {
        let (w, h) = (200i32, 160i32);
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                // 深色背景(模仿暗色主题编辑器),中间一小块浅灰"文字"(小连通组件)
                let text = (100..106).contains(&x) && (63..69).contains(&y);
                let (r, g, b) = if text {
                    (200, 200, 200)
                } else {
                    (30, 33, 40)
                };
                rgba.extend_from_slice(&[r, g, b, 255]);
            }
        }
        let shot = shot_of(w, h, rgba);

        let plain = keying_whiten(&shot, false);
        let dark = keying_whiten(&shot, true);
        assert!(dark.bg_fraction > 0.5, "背景占比: {}", dark.bg_fraction);

        assert_eq!(px_of(&plain.rgba, w, 102, 65), (200, 200, 200));
        let p = px_of(&dark.rgba, w, 102, 65);
        assert!(p.0 < 80 && p.1 < 80 && p.2 < 80, "文字未压暗: {p:?}");
        // 背景仍然是纯白
        assert_eq!(px_of(&dark.rgba, w, 5, 5), (255, 255, 255));
    }

    /// 后台任务队列:提交 → 取回结果(键控路径,不依赖模型)
    #[test]
    fn background_worker_keying_roundtrip() {
        use std::time::{Duration, Instant};
        let shot = make_gradient_scene();
        let (w, h) = (shot.width, shot.height);
        let id = submit(shot, BgMethod::Keying, true).expect("submit job");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(done) = poll() {
                if done.id == id {
                    assert_eq!((done.width, done.height), (w, h));
                    assert_eq!(done.engine, BgEngine::Keying);
                    assert!(done.issue.is_none());
                    let rgba = done.rgba.expect("keying result");
                    assert_eq!(rgba.len(), (w * h * 4) as usize);
                    return;
                }
            }
            assert!(Instant::now() < deadline, "background worker timed out");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn px_of(out: &[u8], w: i32, x: i32, y: i32) -> (u8, u8, u8) {
        let i = ((y * w + x) * 4) as usize;
        (out[i], out[i + 1], out[i + 2])
    }

    /// 合成场景:垂直渐变的蓝紫背景 + 中间一个黑描边、粉色填充的物体
    fn make_gradient_scene() -> Shot {
        let (w, h) = (100i32, 80i32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let t = y as f32 / h as f32;
                rgba[i] = (60.0 + 120.0 * t) as u8;
                rgba[i + 1] = (55.0 + 100.0 * t) as u8;
                rgba[i + 2] = (140.0 + 80.0 * t) as u8;
                rgba[i + 3] = 255;
            }
        }
        for y in 25..55 {
            for x in 35..65 {
                let i = ((y * w + x) * 4) as usize;
                let edge = !(x > 37 && x < 62 && y > 27 && y < 52);
                if edge {
                    rgba[i] = 0;
                    rgba[i + 1] = 0;
                    rgba[i + 2] = 0;
                } else {
                    rgba[i] = 230;
                    rgba[i + 1] = 150;
                    rgba[i + 2] = 180;
                }
                rgba[i + 3] = 255;
            }
        }
        shot_of(w, h, rgba)
    }

    #[test]
    fn removes_gradient_background_and_keeps_object() {
        let shot = make_gradient_scene();
        let out = whiten_background(&shot);
        let px = |x: i32, y: i32| px_of(&out, shot.width, x, y);
        // 渐变背景各处都应变纯白
        assert_eq!(px(2, 2), (255, 255, 255));
        assert_eq!(px(10, 70), (255, 255, 255));
        assert_eq!(px(80, 10), (255, 255, 255));
        assert_eq!(px(80, 70), (255, 255, 255));
        // 物体原样保留:黑描边 / 粉芯
        assert_eq!(px(36, 40), (0, 0, 0));
        assert_eq!(px(50, 40), (230, 150, 180));
    }

    /// 环状物体:键控不看连通性,被物体围住的"镂空处背景"一样涂白
    #[test]
    fn removes_interior_hole_background() {
        let (w, h) = (100i32, 100i32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let t = y as f32 / h as f32;
                rgba[i] = (60.0 + 100.0 * t) as u8;
                rgba[i + 1] = (55.0 + 90.0 * t) as u8;
                rgba[i + 2] = (140.0 + 70.0 * t) as u8;
                rgba[i + 3] = 255;
            }
        }
        // 环形:外框 30..70,内孔(纯背景)40..60,环体粉色、内外黑描边
        for y in 30..70 {
            for x in 30..70 {
                let i = ((y * w + x) * 4) as usize;
                let hole = x >= 40 && x < 60 && y >= 40 && y < 60;
                if hole {
                    continue;
                }
                let edge = x < 32
                    || x >= 68
                    || y < 32
                    || y >= 68
                    || (x >= 38 && x < 40)
                    || (x >= 60 && x < 62)
                    || (y >= 38 && y < 40)
                    || (y >= 60 && y < 62);
                if edge {
                    rgba[i] = 0;
                    rgba[i + 1] = 0;
                    rgba[i + 2] = 0;
                } else {
                    rgba[i] = 230;
                    rgba[i + 1] = 150;
                    rgba[i + 2] = 180;
                }
                rgba[i + 3] = 255;
            }
        }
        let shot = shot_of(w, h, rgba);
        let out = whiten_background(&shot);
        let px = |x: i32, y: i32| px_of(&out, shot.width, x, y);
        // 四周背景变白
        assert_eq!(px(2, 2), (255, 255, 255));
        // 孔内背景也被涂白
        assert_eq!(px(50, 50), (255, 255, 255));
        assert_eq!(px(44, 44), (255, 255, 255));
        // 环体与黑描边保留
        assert_eq!(px(35, 50), (230, 150, 180));
        assert_eq!(px(31, 50), (0, 0, 0));
        assert_eq!(px(39, 50), (0, 0, 0));
    }

    /// 标注(红色箭杆)跨出选框边线时不能被当成背景涂白
    #[test]
    fn keeps_annotation_crossing_border() {
        let (w, h) = (100i32, 80i32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let t = y as f32 / h as f32;
                rgba[i] = (60.0 + 120.0 * t) as u8;
                rgba[i + 1] = (55.0 + 100.0 * t) as u8;
                rgba[i + 2] = (140.0 + 80.0 * t) as u8;
                rgba[i + 3] = 255;
            }
        }
        // 7px 粗红线,从框内 (20,10) 斜到触碰下边框 (80,79)
        for t in 0..=400 {
            let f = t as f32 / 400.0;
            let x = (20.0 + 60.0 * f).round() as i32;
            let y = (10.0 + 69.0 * f).round() as i32;
            if !(0..w).contains(&x) || !(0..h).contains(&y) {
                continue;
            }
            for dx in -3i32..=3 {
                let xx = (x + dx).clamp(0, w - 1) as usize;
                let i = (y as usize * w as usize + xx) * 4;
                rgba[i] = 229;
                rgba[i + 1] = 57;
                rgba[i + 2] = 53;
            }
        }
        let shot = shot_of(w, h, rgba);
        let out = whiten_background(&shot);
        let px = |x: i32, y: i32| px_of(&out, shot.width, x, y);
        // 背景变白
        assert_eq!(px(2, 2), (255, 255, 255));
        // 红线整体保留(中段 + 靠近边框处)
        assert_eq!(px(50, 45), (229, 57, 53));
        assert_eq!(px(79, 78), (229, 57, 53));
    }

    /// 键控不需要背景与边框连通:被 1px 黑线切成小格的背景也应整片变白
    #[test]
    fn keys_background_cut_off_by_lines() {
        let (w, h) = (60i32, 60i32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let grid = x % 10 == 0 || y % 10 == 0;
                let c: (u8, u8, u8) = if grid { (0, 0, 0) } else { (40, 60, 120) };
                rgba[i] = c.0;
                rgba[i + 1] = c.1;
                rgba[i + 2] = c.2;
                rgba[i + 3] = 255;
            }
        }
        let shot = shot_of(w, h, rgba);
        let out = whiten_background(&shot);
        let px = |x: i32, y: i32| px_of(&out, shot.width, x, y);
        // 格子中心(有的格子并不与选框边直接相连)全部变白
        assert_eq!(px(25, 25), (255, 255, 255));
        assert_eq!(px(35, 45), (255, 255, 255));
        // 黑线保留
        assert_eq!(px(30, 25), (0, 0, 0));
    }

    /// 接近真实用法的整幅场景:深蓝渐变视口 + 抗锯齿灰零件 + 红色标注
    #[test]
    fn realistic_viewport_scene() {
        use std::time::Instant;
        let (w, h) = (1200i32, 800i32);
        let bg_at = |y: i32| {
            let t = y as f32 / h as f32;
            (
                (48.0 + 22.0 * t) as u8,
                (48.0 + 27.0 * t) as u8,
                (95.0 + 45.0 * t) as u8,
            )
        };
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        let (cx, cy, hw, hh, r) = (600.0f32, 400.0f32, 200.0f32, 150.0f32, 30.0f32);
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let (br, bg, bb) = bg_at(y);
                // 圆角矩形的有符号距离:sd<0 在零件内
                let dx = ((x as f32 - cx).abs() - (hw - r)).max(0.0);
                let dy = ((y as f32 - cy).abs() - (hh - r)).max(0.0);
                let sd = (dx * dx + dy * dy).sqrt() - r;
                // 黑色描边 3px + 1px 线性抗锯齿;零件内部平灰
                let (cr, cg, cb) = if sd <= -3.0 {
                    (150, 150, 155)
                } else if sd <= 0.0 {
                    (0, 0, 0)
                } else {
                    (br, bg, bb)
                };
                let cov = (0.5 - sd).clamp(0.0, 1.0);
                rgba[i] = (br as f32 * (1.0 - cov) + cr as f32 * cov) as u8;
                rgba[i + 1] = (bg as f32 * (1.0 - cov) + cg as f32 * cov) as u8;
                rgba[i + 2] = (bb as f32 * (1.0 - cov) + cb as f32 * cov) as u8;
                rgba[i + 3] = 255;
            }
        }
        // 一条红色标注线,从零件上穿过
        for x in 350..850 {
            for dy in -2i32..=2 {
                let y = 700 + dy;
                if !(0..h).contains(&y) {
                    continue;
                }
                let i = ((y * w + x) * 4) as usize;
                rgba[i] = 229;
                rgba[i + 1] = 57;
                rgba[i + 2] = 53;
            }
        }
        let shot = shot_of(w, h, rgba);
        let t0 = Instant::now();
        let out = whiten_background(&shot);
        eprintln!("keying 1200x800 took {:?}", t0.elapsed());
        let px = |x: i32, y: i32| px_of(&out, shot.width, x, y);
        assert_eq!(px(10, 10), (255, 255, 255));
        assert_eq!(px(1190, 790), (255, 255, 255));
        // 零件主体与描边保留
        assert_eq!(px(600, 400), (150, 150, 155));
        assert_eq!(px(600, 252), (0, 0, 0));
        // 红色标注保留
        assert_eq!(px(500, 700), (229, 57, 53));
    }

    /// 没和边框背景连通、颜色又只是接近背景的竖条(物体侧面/抗锯齿边缘)必须保留;
    /// 只有颜色和背景几乎一致的封闭色块才会被去掉。
    #[test]
    fn unconnected_near_background_colors_are_preserved() {
        let (w, h) = (40i32, 40i32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                // 背景 100,100,100;一条 122(近背景色)和一条 160(明显不同)
                let r = match x {
                    20..=22 => 122,
                    26..=28 => 160,
                    _ => 100,
                };
                rgba[i] = r;
                rgba[i + 1] = 100;
                rgba[i + 2] = 100;
                rgba[i + 3] = 255;
            }
        }
        let shot = shot_of(w, h, rgba);
        let out = whiten_background(&shot);
        // 两条都不是背景连通区域,原样保留(不再被误当软键边提亮)
        assert_eq!(px_of(&out, shot.width, 21, 20), (122, 100, 100));
        assert_eq!(px_of(&out, shot.width, 27, 20), (160, 100, 100));
        assert_eq!(px_of(&out, shot.width, 5, 20), (255, 255, 255));
    }

    /// 被黑色描边围住、颜色和背景完全一致的封闭格子仍应去掉;
    /// 围住但颜色比背景亮 12 的"侧壁"应保留(模拟 CAD 零件侧面)。
    #[test]
    fn enclosed_background_cell_and_near_background_wall() {
        let (w, h) = (100i32, 100i32);
        let mut rgba = vec![100u8; (w * h * 4) as usize];
        for px in rgba.chunks_exact_mut(4) {
            px[3] = 255;
        }
        // 封闭格子 A:颜色 = 背景(10..20),1px 黑框
        // 侧壁 B:颜色 = 112(比背景亮 12,30..40),1px 黑框
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let in_box = |x0: i32, x1: i32, y0: i32, y1: i32| {
                    x >= x0 - 1 && x <= x1 + 1 && y >= y0 - 1 && y <= y1 + 1
                };
                if in_box(10, 20, 10, 20) {
                    let c = if x >= 10 && x <= 20 && y >= 10 && y <= 20 {
                        100
                    } else {
                        0
                    };
                    rgba[i] = c;
                    rgba[i + 1] = c;
                    rgba[i + 2] = c;
                } else if in_box(30, 40, 30, 40) {
                    let c = if x >= 30 && x <= 40 && y >= 30 && y <= 40 {
                        // 块内嵌一条 2px 宽、颜色完全等于背景的细高光条纹:
                        // 它没有 5×5 内核,必须保留,不能被撕成白线。
                        if (34..=35).contains(&x) {
                            100
                        } else {
                            112
                        }
                    } else {
                        0
                    };
                    rgba[i] = c;
                    rgba[i + 1] = c;
                    rgba[i + 2] = c;
                }
            }
        }
        let shot = shot_of(w, h, rgba);
        let out = whiten_background(&shot);
        // 背景变白
        assert_eq!(px_of(&out, w, 5, 5), (255, 255, 255));
        // 封闭格子(和背景完全一致)去掉
        assert_eq!(px_of(&out, w, 15, 15), (255, 255, 255));
        // 近背景色侧壁(差 12)保留
        assert_eq!(px_of(&out, w, 38, 35), (112, 112, 112));
        // 2px 宽的背景色高光条纹没有 5×5 内核,保留
        assert_eq!(px_of(&out, w, 34, 35), (100, 100, 100));
    }
}
