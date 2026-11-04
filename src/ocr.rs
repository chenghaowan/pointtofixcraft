//! OCR:PP-OCRv5 Mobile(ONNX)文字识别 + 阅读顺序编号。
//!
//! 流水线(单个后台任务内完成):
//! 1. **det**:截图等比缩放(最长边 ≤960、对齐 32)→ DBNet 概率图 →
//!    阈值 0.3 二值化 → 连通域 → 轴对齐包围盒 → 矩形精确外扩(unclip)。
//!    截图文本都是水平矩形,不做旋转框,也无需 geo 依赖;
//! 2. **阅读顺序编号**:按行聚类(行内按 x 排序,行间按 y),顺序赋 1..n;
//! 3. **rec**:每个框按原比例裁回原图 → 高度归一到 48 → CTC 贪心解码 +
//!    ppocrv5 词典。PP-OCRv5 的 rec 一个模型同时覆盖简中/繁中/英/日。
//!
//! 架构完全镜像 isnet/bgremove:复用进程内唯一的 ONNX Runtime Environment
//! (同一个 onnxruntime.dll),独立 worker 线程 `pointtofix-ocr`,
//! submit/poll + u64 任务 id(UI 丢弃过期结果)。
//!
//! 模型文件(可用设置页下载,或 scripts/fetch_ppocr.py)按此优先级查找:
//! `ppocrv5_mobile_det.onnx` → 其它 `ppocr*det*.onnx`;rec/词典同理。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use image::imageops::FilterType;
use image::RgbImage;
use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;
use ort::value::Tensor;

use crate::capture::Shot;
use crate::isnet;

/// det 最长边上限(limit_type=max:只缩小不放大,小图保持原尺寸)
const DET_MAX_SIDE: i32 = 960;
/// det 概率图二值化阈值(PaddleOCR DB 默认 0.3)
const DET_THRESH: f32 = 0.3;
/// DB unclip 外扩比例(area*ratio/perimeter = 外扩距离)
const DET_UNCLIP_RATIO: f32 = 1.6;
/// rec 输入高度(PP-OCR 全系固定 48)
const REC_H: usize = 48;
/// rec 动态宽度上限(超宽裁切会丢字,压扁保内容)
const REC_MAX_W: usize = 512;
/// rec 无效框的最小尺寸(原始像素)
const MIN_BOX: i32 = 6;
/// 最坏情况保护:单张截图最多识别的文本框数
const MAX_BOXES: usize = 300;

/// 一行识别结果:no = 阅读顺序编号(1 起),rect = 选区内的原始像素矩形 [x,y,w,h]
/// (JSON 序列化时键名仍是 "box",对外契约不变;`box` 在 Rust 里是保留字)
#[derive(Clone, Debug, PartialEq)]
pub struct OcrLine {
    pub no: u32,
    pub text: String,
    pub rect: [i32; 4],
    pub score: f32,
}

#[derive(Clone, Debug)]
pub enum OcrError {
    /// det/rec/词典任一缺失
    ModelMissing,
    Error(String),
}

pub struct OcrDone {
    pub id: u64,
    /// 线程内总耗时(含首次加载模型)
    pub elapsed: Duration,
    pub result: Result<Vec<OcrLine>, OcrError>,
}

struct Job {
    id: u64,
    shot: Shot,
}

struct Worker {
    tx: Sender<Job>,
    rx: Mutex<Receiver<OcrDone>>,
}

static WORKER: OnceLock<Worker> = OnceLock::new();
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn worker() -> &'static Worker {
    WORKER.get_or_init(|| {
        let (job_tx, job_rx) = channel::<Job>();
        let (done_tx, done_rx) = channel::<OcrDone>();
        std::thread::Builder::new()
            .name("pointtofix-ocr".into())
            // ORT 首次跑模型需要较深的调用栈,与去背线程同款配置
            .stack_size(32 * 1024 * 1024)
            .spawn(move || worker_loop(job_rx, done_tx))
            .expect("无法启动 OCR 线程");
        Worker { tx: job_tx, rx: Mutex::new(done_rx) }
    })
}

/// 提交一次后台识别;返回任务 id(UI 用它丢弃过期结果)
pub fn submit(shot: Shot) -> Option<u64> {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    worker().tx.send(Job { id, shot }).ok()?;
    Some(id)
}

/// 非阻塞取回已完成的任务
pub fn poll() -> Option<OcrDone> {
    worker().rx.lock().ok()?.try_recv().ok()
}

// ---------- 模型发现(复用 isnet 的搜索目录) ----------

const DET_NAMES: [&str; 3] =
    ["ppocrv5_mobile_det.onnx", "ch_PP-OCRv5_det_mobile.onnx", "PP-OCRv5_mobile_det.onnx"];
const REC_NAMES: [&str; 3] =
    ["ppocrv5_mobile_rec.onnx", "ch_PP-OCRv5_rec_mobile.onnx", "PP-OCRv5_mobile_rec.onnx"];
const DICT_NAMES: [&str; 2] = ["ppocrv5_dict.txt", "ppocr_dict.txt"];

pub fn find_det() -> Option<PathBuf> {
    for name in DET_NAMES {
        if let Some(p) = isnet::find_in_dirs(name) {
            return Some(p);
        }
    }
    scan(|n| n.starts_with("ppocr") && n.contains("det") && n.ends_with(".onnx"))
}

pub fn find_rec() -> Option<PathBuf> {
    for name in REC_NAMES {
        if let Some(p) = isnet::find_in_dirs(name) {
            return Some(p);
        }
    }
    scan(|n| n.starts_with("ppocr") && n.contains("rec") && n.ends_with(".onnx"))
}

pub fn find_dict() -> Option<PathBuf> {
    for name in DICT_NAMES {
        if let Some(p) = isnet::find_in_dirs(name) {
            return Some(p);
        }
    }
    scan(|n| n.starts_with("ppocr") && n.ends_with(".txt"))
}

/// 在所有搜索目录里按名字过滤(改过名的模型也能捡到)
fn scan(pred: impl Fn(&str) -> bool) -> Option<PathBuf> {
    for dir in isnet::search_dirs() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        let mut hits: Vec<PathBuf> = rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| pred(&n.to_ascii_lowercase()))
                        .unwrap_or(false)
            })
            .collect();
        hits.sort();
        if let Some(p) = hits.into_iter().next() {
            return Some(p);
        }
    }
    None
}

// ---------- 会话管理 ----------

/// det/rec 会话对 + 输入形状缓存;同路径只加载一次
struct OcrSessions {
    det: Session,
    rec: Session,
    /// 0 = 动态宽度(按内容算),>0 = 固定宽度(按官方 pad/截断)
    rec_w: usize,
}

fn load_session(path: &Path, what: &str) -> Result<Session, String> {
    let env = isnet::environment()?;
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    Session::builder(env)
        .map_err(|e| format!("创建 {what} 会话失败: {e}"))?
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .map_err(|e| format!("设置优化级别失败: {e}"))?
        .with_intra_threads(threads)
        .map_err(|e| format!("设置线程数失败: {e}"))?
        .commit_from_file(path)
        .map_err(|e| format!("加载模型 {} 失败: {e}", path.display()))
}

/// 静态输入形状 [n,3,h,w];动态维(负值)或离谱值返回 0,按动态处理。
/// 不设上限的话,-1 转 usize 会变成天文数字,后面按它分配张量直接 capacity overflow。
fn static_dims(session: &Session) -> (usize, usize) {
    const SANE_MAX: i64 = 8192;
    session
        .inputs()
        .first()
        .and_then(|i| i.dtype().tensor_shape())
        .and_then(|shape| match shape.as_ref() {
            [_, _, h, w] if (0..=SANE_MAX).contains(h) && (0..=SANE_MAX).contains(w) => {
                Some((*h as usize, *w as usize))
            }
            _ => None,
        })
        .unwrap_or((0, 0))
}

impl OcrSessions {
    fn load(det_path: &Path, rec_path: &Path) -> Result<OcrSessions, String> {
        let mut det = load_session(det_path, "OCR det")?;
        let mut rec = load_session(rec_path, "OCR rec")?;
        let (rec_h, rec_w) = static_dims(&rec);
        if rec_h != 0 && rec_h != REC_H {
            return Err(format!("rec 模型输入高度 {rec_h} 与预期 {REC_H} 不符"));
        }
        // 预热:ORT 首跑做内存分配/算子初始化,否则第一张明显慢
        // det 预热:动态形状就用一个小的固定尺寸
        let warm_det = Tensor::from_array(([1usize, 3, 64, 64], vec![0f32; 3 * 64 * 64]))
            .map_err(|e| format!("构造张量失败: {e}"))?;
        let _ = det.run(ort::inputs![warm_det]);
        let rw = if rec_w > 0 { rec_w } else { 64 };
        let warm_rec = Tensor::from_array(([1usize, 3, REC_H, rw], vec![0f32; 3 * REC_H * rw]))
            .map_err(|e| format!("构造张量失败: {e}"))?;
        let _ = rec.run(ort::inputs![warm_rec]);
        Ok(OcrSessions { det, rec, rec_w })
    }
}

fn worker_loop(rx: Receiver<Job>, tx: Sender<OcrDone>) {
    let mut cached: Option<(PathBuf, PathBuf, Vec<String>, OcrSessions)> = None;
    while let Ok(job) = rx.recv() {
        let started = Instant::now();
        let result = run_job(&job, &mut cached);
        let done = OcrDone { id: job.id, elapsed: started.elapsed(), result };
        if tx.send(done).is_err() {
            break; // UI 已退出
        }
    }
}

fn run_job(
    job: &Job,
    cached: &mut Option<(PathBuf, PathBuf, Vec<String>, OcrSessions)>,
) -> Result<Vec<OcrLine>, OcrError> {
    let (Some(det_path), Some(rec_path), Some(dict_path)) =
        (find_det(), find_rec(), find_dict())
    else {
        return Err(OcrError::ModelMissing);
    };
    // 词典小,直接读;内容每次任务重读(下载完成后立刻生效)
    let dict = std::fs::read(&dict_path)
        .ok()
        .map(|b| decode_dict(&b))
        .filter(|d| !d.is_empty())
        .ok_or(OcrError::ModelMissing)?;

    // 模型路径变了(下载完成/换文件)才重建会话
    let need_reload = !matches!(cached, Some((d, r, _, _)) if *d == det_path && *r == rec_path);
    if need_reload {
        let sessions = OcrSessions::load(&det_path, &rec_path).map_err(OcrError::Error)?;
        *cached = Some((det_path.clone(), rec_path.clone(), dict.clone(), sessions));
    }
    let (_, _, cached_dict, sessions) = cached.as_mut().unwrap();
    // 词典条数与缓存的会话无关,永远用最新的
    *cached_dict = dict;

    // ONNX 异常可能 panic:兜住,别让后台线程死掉
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        recognize(&job.shot, sessions, cached_dict)
    }))
    .unwrap_or_else(|_| Err(OcrError::Error("OCR 推理异常(panic)".to_string())))
}

/// UTF-8 / UTF-8-BOM 词典:一行一个字符(按行,极少数多字节字符也按整行算)
fn decode_dict(bytes: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(bytes);
    text.trim_start_matches('\u{feff}')
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.trim_end_matches('\r').to_string())
        .collect()
}

/// 完整识别:det → 阅读顺序编号 → rec
fn recognize(shot: &Shot, sessions: &mut OcrSessions, dict: &[String]) -> Result<Vec<OcrLine>, OcrError> {
    let boxes = detect(shot, sessions).map_err(OcrError::Error)?;
    let ordered = reading_order(boxes);
    let mut lines = Vec::with_capacity(ordered.len().min(MAX_BOXES));
    for (i, b) in ordered.iter().take(MAX_BOXES).enumerate() {
        let (text, score) = recognize_line(shot, *b, sessions, dict).map_err(OcrError::Error)?;
        if text.is_empty() {
            continue;
        }
        lines.push(OcrLine { no: (i + 1) as u32, text, rect: *b, score });
    }
    Ok(lines)
}

// ---------- det ----------

/// DBNet 检测:返回原图坐标系下的文本框 [x,y,w,h]
fn detect(shot: &Shot, sessions: &mut OcrSessions) -> Result<Vec<[i32; 4]>, String> {
    let (w, h) = (shot.width.max(1), shot.height.max(1));
    // 缩放:最长边 ≤960,只缩小;尺寸对齐 32
    let scale = if w.max(h) > DET_MAX_SIDE {
        DET_MAX_SIDE as f32 / w.max(h) as f32
    } else {
        1.0
    };
    let align32 = |v: f32| {
        let x = (v.round() as i32).max(32);
        ((x + 31) / 32) * 32
    };
    let nw = align32(w as f32 * scale) as usize;
    let nh = align32(h as f32 * scale) as usize;

    // RGBA → RGB → resize → 归一化(ImageNet mean/std)→ CHW
    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    for px in shot.rgba[..(w * h * 4) as usize].chunks_exact(4) {
        rgb.extend_from_slice(&px[..3]);
    }
    let src = RgbImage::from_raw(w as u32, h as u32, rgb).ok_or("无法构建 RGB 图像")?;
    let resized = if (nw as u32, nh as u32) != (w as u32, h as u32) {
        image::imageops::resize(&src, nw as u32, nh as u32, FilterType::Lanczos3)
    } else {
        src
    };
    const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
    const STD: [f32; 3] = [0.229, 0.224, 0.225];
    let n = nw * nh;
    let mut data = vec![0f32; n * 3];
    for (i, px) in resized.as_raw().chunks_exact(3).enumerate() {
        for c in 0..3 {
            data[c * n + i] = (px[c] as f32 / 255.0 - MEAN[c]) / STD[c];
        }
    }
    let tensor = Tensor::from_array(([1usize, 3, nh, nw], data)).map_err(|e| format!("构造张量失败: {e}"))?;
    let outputs = sessions
        .det
        .run(ort::inputs![tensor])
        .map_err(|e| format!("det 推理失败: {e}"))?;
    let (shape, values) = outputs[0]
        .try_extract_tensor::<f32>()
        .map_err(|e| format!("读取 det 输出失败: {e}"))?;
    let dims: Vec<usize> = shape.iter().map(|d| (*d).max(0) as usize).collect();
    let (ph, pw) = match dims.as_slice() {
        [_, _, ph, pw] => (*ph, *pw),
        [_, ph, pw] => (*ph, *pw),
        [ph, pw] => (*ph, *pw),
        _ => return Err(format!("无法识别 det 输出形状: {dims:?}")),
    };
    if ph == 0 || pw == 0 || values.len() < ph * pw {
        return Err("det 输出数据不足".to_string());
    }

    // 阈值 → 连通域 → 包围盒 → 外扩 → 映射回原图
    let mut boxes = Vec::new();
    let mut visited = vec![false; ph * pw];
    let mut stack: Vec<u32> = Vec::new();
    for idx in 0..(ph * pw) {
        if visited[idx] || values[idx] <= DET_THRESH {
            continue;
        }
        let (mut minx, mut miny) = (u32::MAX, u32::MAX);
        let (mut maxx, mut maxy) = (0u32, 0u32);
        stack.clear();
        stack.push(idx as u32);
        visited[idx] = true;
        while let Some(cur) = stack.pop() {
            let (cx, cy) = (cur % pw as u32, cur / pw as u32);
            minx = minx.min(cx);
            maxx = maxx.max(cx);
            miny = miny.min(cy);
            maxy = maxy.max(cy);
            // 4 邻域(对角连通会把相邻行粘成一坨)
            for (dx, dy) in [(1i32, 0i32), (-1, 0), (0, 1), (0, -1)] {
                let (nx, ny) = (cx as i32 + dx, cy as i32 + dy);
                if nx < 0 || ny < 0 || nx >= pw as i32 || ny >= ph as i32 {
                    continue;
                }
                let nidx = ny as usize * pw + nx as usize;
                if !visited[nidx] && values[nidx] > DET_THRESH {
                    visited[nidx] = true;
                    stack.push(nidx as u32);
                }
            }
        }
        let (bw, bh) = ((maxx - minx + 1) as i32, (maxy - miny + 1) as i32);
        if bw < 2 || bh < 2 {
            continue;
        }
        // DB unclip:矩形外扩距离 = area * ratio / perimeter
        let perimeter = ((bw + bh) * 2) as f32;
        let offset = (bw * bh) as f32 * DET_UNCLIP_RATIO / perimeter;
        let x0 = ((minx as f32 - offset) / scale).round().max(0.0) as i32;
        let y0 = ((miny as f32 - offset) / scale).round().max(0.0) as i32;
        let x1 = (((maxx + 1) as f32 + offset) / scale).round().min(w as f32) as i32;
        let y1 = (((maxy + 1) as f32 + offset) / scale).round().min(h as f32) as i32;
        if x1 - x0 >= MIN_BOX && y1 - y0 >= MIN_BOX {
            boxes.push([x0, y0, x1 - x0, y1 - y0]);
        }
        if boxes.len() >= MAX_BOXES * 2 {
            break;
        }
    }
    Ok(boxes)
}

/// 阅读顺序:行聚类(y 重叠算同行),行内按 x,行间按 y
fn reading_order(mut boxes: Vec<[i32; 4]>) -> Vec<[i32; 4]> {
    boxes.sort_by_key(|b| (b[1], b[0]));
    let mut rows: Vec<Vec<[i32; 4]>> = Vec::new();
    for b in boxes {
        let top = b[1];
        match rows.last_mut() {
            // 与当前行的纵向范围有实质重叠(≥框高一半)算同行
            Some(row) if top < row.iter().map(|r| r[1] + r[3]).max().unwrap_or(0)
                - (b[3] / 2).max(1) =>
            {
                row.push(b)
            }
            _ => rows.push(vec![b]),
        }
    }
    rows.into_iter().flatten().collect()
}

// ---------- rec ----------

/// 识别一个文本框:裁原图 → 高度 48 → CTC 贪心解码
fn recognize_line(
    shot: &Shot,
    b: [i32; 4],
    sessions: &mut OcrSessions,
    dict: &[String],
) -> Result<(String, f32), String> {
    let (w, h) = (shot.width, shot.height);
    // 裁切(带 2px 余量,clamped)
    let x0 = b[0].saturating_sub(2).max(0);
    let y0 = b[1].saturating_sub(2).max(0);
    let x1 = (b[0] + b[2] + 2).min(w);
    let y1 = (b[1] + b[3] + 2).min(h);
    let (cw, ch) = ((x1 - x0).max(1) as usize, (y1 - y0).max(1) as usize);
    if cw < 4 || ch < 4 {
        return Ok((String::new(), 0.0));
    }
    let crop = crop_rgb(shot, x0, y0, cw as i32, ch as i32);

    // 高度归一到 48;宽度按比例,动态模型取 min(缩放宽, 512),
    // 固定宽模型按官方语义:超宽压到固定宽,不足右侧补零
    let rw_scaled = ((cw as f32) * (REC_H as f32 / ch as f32)).round().max(8.0) as usize;
    let (rw, draw_w) = if sessions.rec_w > 0 {
        (sessions.rec_w, rw_scaled.min(sessions.rec_w))
    } else {
        let w = rw_scaled.min(REC_MAX_W);
        (w, w)
    };

    let resized = image::imageops::resize(&crop, draw_w as u32, REC_H as u32, FilterType::Triangle);
    let n = rw * REC_H;
    let mut data = vec![0f32; n * 3];
    let px = resized.as_raw();
    // resize 输出宽 = draw_w,不足 rw(padded 场景)的部分保持补零
    for y in 0..REC_H {
        for x in 0..draw_w {
            let s = (y * draw_w + x) * 3;
            for c in 0..3 {
                data[c * n + y * rw + x] = px[s + c] as f32 / 127.5 - 1.0;
            }
        }
    }
    let tensor =
        Tensor::from_array(([1usize, 3, REC_H, rw], data)).map_err(|e| format!("构造张量失败: {e}"))?;
    let outputs = sessions
        .rec
        .run(ort::inputs![tensor])
        .map_err(|e| format!("rec 推理失败: {e}"))?;
    let (shape, values) = outputs[0]
        .try_extract_tensor::<f32>()
        .map_err(|e| format!("读取 rec 输出失败: {e}"))?;
    let dims: Vec<usize> = shape.iter().map(|d| (*d).max(0) as usize).collect();
    let (steps, classes) = match dims.as_slice() {
        // [1, T, C](有的导出带 batch,有的把 T 放第 1 维)
        [_, t, c] => (*t, *c),
        [t, c] => (*t, *c),
        _ => return Err(format!("无法识别 rec 输出形状: {dims:?}")),
    };
    if steps == 0 || classes == 0 || values.len() < steps * classes {
        return Err("rec 输出数据不足".to_string());
    }
    // 类别布局:0 = CTC blank;1..=len(dict) = 词典字符;最后一位 = 空格(可选)
    let space_idx = if classes == dict.len() + 2 { dict.len() + 1 } else { usize::MAX };
    if classes != dict.len() + 1 && classes != dict.len() + 2 {
        return Err(format!(
            "rec 类别数 {classes} 与词典 {} 条不匹配",
            dict.len()
        ));
    }
    let mut text = String::new();
    let mut score_sum = 0f32;
    let mut score_n = 0u32;
    let mut prev = usize::MAX;
    for t in 0..steps {
        let row = &values[t * classes..(t + 1) * classes];
        let (mut best, mut best_p) = (0usize, f32::NEG_INFINITY);
        for (i, &v) in row.iter().enumerate() {
            if v > best_p {
                best_p = v;
                best = i;
            }
        }
        if best != 0 && best != prev {
            if best == space_idx {
                text.push(' ');
            } else if let Some(ch) = dict.get(best - 1) {
                text.push_str(ch);
            }
            score_sum += best_p.max(0.0);
            score_n += 1;
        }
        prev = best;
    }
    let score = if score_n > 0 { score_sum / score_n as f32 } else { 0.0 };
    Ok((text.trim().to_string(), score))
}

/// 从 RGBA 截图裁一块 RGB 区域
fn crop_rgb(shot: &Shot, x: i32, y: i32, w: i32, h: i32) -> RgbImage {
    let mut out = Vec::with_capacity((w * h * 3) as usize);
    let (sw, sh) = (shot.width, shot.height);
    for yy in y..(y + h).min(sh) {
        for xx in x..(x + w).min(sw) {
            let i = ((yy * sw + xx) * 4) as usize;
            if i + 3 < shot.rgba.len() {
                out.extend_from_slice(&shot.rgba[i..i + 3]);
            } else {
                out.extend_from_slice(&[0, 0, 0]);
            }
        }
    }
    RgbImage::from_raw(w as u32, h as u32, out)
        .unwrap_or_else(|| RgbImage::new(w as u32, h as u32))
}

// ---------- 序列化(给 SQLite 的 ocr_lines JSON 列,外部 RAG 软件直接可读) ----------

pub fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// ocr_lines → JSON 数组字符串([{"no":1,"text":"..","box":[x,y,w,h]},…])
pub fn lines_to_json(lines: &[OcrLine]) -> String {
    let mut out = String::from("[");
    for (i, l) in lines.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"no\":{},\"text\":\"{}\",\"box\":[{},{},{},{}]}}",
            l.no,
            json_escape(&l.text),
            l.rect[0],
            l.rect[1],
            l.rect[2],
            l.rect[3]
        ));
    }
    out.push(']');
    out
}

/// 解析 [`lines_to_json`] 生成的 JSON(固定结构的小解析器,不引 serde)
pub fn lines_from_json(s: &str) -> Vec<OcrLine> {
    let b = s.as_bytes();
    let mut i = 0usize;
    let mut out = Vec::new();
    let skip_ws = |i: &mut usize| {
        while *i < b.len() && (b[*i] as char).is_ascii_whitespace() {
            *i += 1;
        }
    };
    let expect = |i: &mut usize, c: u8| -> bool {
        skip_ws(i);
        if *i < b.len() && b[*i] == c {
            *i += 1;
            true
        } else {
            false
        }
    };
    let parse_string = |i: &mut usize| -> Option<String> {
        skip_ws(i);
        if !expect(i, b'"') {
            return None;
        }
        let mut s = String::new();
        while *i < b.len() {
            match b[*i] {
                b'"' => {
                    *i += 1;
                    return Some(s);
                }
                b'\\' => {
                    *i += 1;
                    match b.get(*i) {
                        Some(b'"') => s.push('"'),
                        Some(b'\\') => s.push('\\'),
                        Some(b'n') => s.push('\n'),
                        Some(b'r') => s.push('\r'),
                        Some(b't') => s.push('\t'),
                        Some(b'u') => {
                            let end = (*i + 5).min(b.len());
                            let code = u32::from_str_radix(
                                std::str::from_utf8(&b[*i + 1..end]).ok()?,
                                16,
                            )
                            .ok()?;
                            s.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                            *i += 4;
                        }
                        _ => return None,
                    }
                    *i += 1;
                }
                _ => {
                    // 按 UTF-8 取一个字符
                    let rest = std::str::from_utf8(&b[*i..]).ok()?;
                    let c = rest.chars().next()?;
                    s.push(c);
                    *i += c.len_utf8();
                }
            }
        }
        None
    };
    let parse_int = |i: &mut usize| -> Option<i64> {
        skip_ws(i);
        let start = *i;
        if *i < b.len() && (b[*i] == b'-' || b[*i] == b'+') {
            *i += 1;
        }
        while *i < b.len() && b[*i].is_ascii_digit() {
            *i += 1;
        }
        std::str::from_utf8(&b[start..*i]).ok()?.parse().ok()
    };
    if !expect(&mut i, b'[') {
        return out;
    }
    loop {
        if expect(&mut i, b']') {
            break;
        }
        if !expect(&mut i, b'{') {
            break;
        }
        let mut no = 0u32;
        let mut text = String::new();
        let mut bx = [0i32; 4];
        loop {
            if expect(&mut i, b'}') {
                break;
            }
            let key = parse_string(&mut i).unwrap_or_default();
            if !expect(&mut i, b':') {
                break;
            }
            match key.as_str() {
                "no" => no = parse_int(&mut i).unwrap_or(0) as u32,
                "text" => text = parse_string(&mut i).unwrap_or_default(),
                "box" => {
                    if expect(&mut i, b'[') {
                        for k in 0..4 {
                            bx[k] = parse_int(&mut i).unwrap_or(0) as i32;
                            if k < 3 {
                                expect(&mut i, b',');
                            }
                        }
                        expect(&mut i, b']');
                    }
                }
                _ => {
                    // 未知键:跳过字符串或数字值
                    let save = i;
                    if parse_string(&mut i).is_none() {
                        i = save;
                        parse_int(&mut i);
                    }
                }
            }
            if !expect(&mut i, b',') {
                expect(&mut i, b'}');
                break;
            }
        }
        if !text.is_empty() {
            out.push(OcrLine { no, text, rect: bx, score: 0.0 });
        }
        if !expect(&mut i, b',') {
            expect(&mut i, b']');
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_shot(w: i32, h: i32, rgb: (u8, u8, u8)) -> Shot {
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..w * h {
            rgba.extend_from_slice(&[rgb.0, rgb.1, rgb.2, 255]);
        }
        Shot { width: w, height: h, rgba }
    }

    #[test]
    fn reading_order_groups_rows() {
        // 两行:第一行 y=0(三个框),第二行 y=40(两个框);行内乱序给出
        let boxes = vec![
            [100, 0, 20, 10],
            [0, 40, 20, 10],
            [0, 0, 20, 10],
            [50, 0, 20, 10],
            [100, 40, 20, 10],
        ];
        let ordered = reading_order(boxes);
        let tops: Vec<i32> = ordered.iter().map(|b| b[1]).collect();
        assert_eq!(tops, vec![0, 0, 0, 40, 40]);
        let row0_x: Vec<i32> = ordered[..3].iter().map(|b| b[0]).collect();
        assert_eq!(row0_x, vec![0, 50, 100]);
    }

    #[test]
    fn json_roundtrip() {
        let lines = vec![
            OcrLine { no: 1, text: "登录\"账号\"".into(), rect: [3, 5, 100, 20], score: 0.9 },
            OcrLine { no: 2, text: "第二行\n带换行".into(), rect: [3, 40, 80, 20], score: 0.8 },
        ];
        let json = lines_to_json(&lines);
        assert!(json.contains("\"no\":1"));
        let parsed = lines_from_json(&json);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].text, "登录\"账号\"");
        assert_eq!(parsed[0].rect, [3, 5, 100, 20]);
        assert_eq!(parsed[1].no, 2);
        assert_eq!(parsed[1].text, "第二行\n带换行");
    }

    #[test]
    fn dict_decode_strips_bom_and_empty() {
        let dict = decode_dict("\u{feff}一\n二\n\n三\r\n".as_bytes());
        assert_eq!(dict, vec!["一", "二", "三"]);
    }

    /// 本地验证:设置 POINTTOFIX_TEST_OCR=1 且模型齐全后运行
    #[test]
    fn runs_pipeline_when_configured() {
        if std::env::var("POINTTOFIX_TEST_OCR").is_err() {
            return;
        }
        let mut rgba = vec![255u8; 200 * 60 * 4];
        // 画几条"文字样"黑条,至少能测出 det 出框、rec 不崩
        for y in (10..20).chain(30..40) {
            for x in (10..80).step_by(4) {
                let i = (y * 200 + x) * 4;
                for c in 0..3 {
                    rgba[i + c] = 20;
                }
            }
        }
        let shot = Shot { width: 200, height: 60, rgba };
        match submit(shot) {
            Some(id) => {
                while poll().map(|d| d.id != id).unwrap_or(true) {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
            None => eprintln!("OCR 线程未启动"),
        }
    }
}
