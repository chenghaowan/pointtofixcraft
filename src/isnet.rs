//! ISNet(DIS general-use)ONNX 去背:走 ONNX Runtime,CPU 上比纯 Rust 推理快一个数量级
//! (本机 1024×1024:ORT 约 1.6 s,纯 Rust 解释器约 20 s)。
//!
//! `onnxruntime.dll` 与模型文件都不入库,运行时按下面顺序查找:
//!
//! ```text
//! 1. POINTTOFIX_ORT_DLL / POINTTOFIX_ISNET_MODEL 环境变量
//! 2. exe 同目录、exe 目录\models、工作目录、工作目录\models、工作目录\assets
//! 3. %APPDATA%\pointtofix\models、%USERPROFILE%\.u2net
//! ```
//!
//! 模型按这个优先级挑第一个存在的(想用哪版就把其它同名版本挪走):
//! `isnet-general-uxe.onnx`(用户命名)→ `isnet-general-use-fp16.onnx`(88 MB,画质≈fp32、体积一半)
//! → `isnet-general-use.onnx`(176 MB)→ int8 量化版(44 MB,省体积但更慢)→ 任意 `isnet*.onnx`。
//!
//! 预处理沿用 rembg:RGB 缩放成模型输入尺寸(ISNet 为 1024×1024),像素 `/255 - 0.5`,
//! NCHW float32。后处理 **不做 alpha matting(用户要求 matting off)**:模型前景遮罩做
//! min-max 归一化、缩放到选区尺寸,再用一条窄 soft-threshold 收紧边缘,最后把背景混成白色。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use image::imageops::FilterType;
use image::{GrayImage, RgbImage};
use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;
use ort::value::Tensor;

use crate::capture::Shot;

/// 按优先级排列的模型文件名(先匹配到的先用)
const MODEL_NAMES: [&str; 5] = [
    "isnet-general-uxe.onnx",
    "isnet-general-use-fp16.onnx",
    "isnet-general-use.onnx",
    "isnet-general-use-int8.onnx",
    "isnet-general-use-q8.onnx",
];
const DLL_NAME: &str = "onnxruntime.dll";
/// 模型输入不是静态时的兜底尺寸(ISNet general-use 的标准输入)
const FALLBACK_INPUT: usize = 1024;
/// soft-threshold 的半宽:0.5±此值之间平滑过渡。不是 matting,只收紧模型遮罩边缘。
const MASK_SOFTNESS: f32 = 0.12;

/// onnxruntime 的 Environment 进程内只能建一次(init_from 也只能调一次)
static ENVIRONMENT: OnceLock<Result<ort::environment::Environment, String>> = OnceLock::new();

fn environment() -> Result<&'static ort::environment::Environment, String> {
    ENVIRONMENT
        .get_or_init(|| {
            let dll = find_dll().ok_or_else(|| {
                format!("未找到 {DLL_NAME}(放到程序目录或 models 子目录,或用 POINTTOFIX_ORT_DLL 指定)")
            })?;
            // init_from 必须早于任何其它 ort API;失败时缓存错误,避免反复尝试
            let builder = ort::init_from(&dll)
                .map_err(|e| format!("加载 {} 失败: {e}", dll.display()))?;
            builder.build().map_err(|e| format!("初始化 ONNX Runtime 失败: {e}"))
        })
        .as_ref()
        .map_err(|e| e.clone())
}

/// 已加载的 ISNet 会话;创建一次后可反复推理
pub struct Isnet {
    session: Session,
    width: usize,
    height: usize,
    path: PathBuf,
    load_time: Duration,
}

impl Isnet {
    pub fn load(path: &Path) -> Result<Isnet, String> {
        let started = Instant::now();
        let env = environment()?;
        let threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let mut session = Session::builder(env)
            .map_err(|e| format!("创建会话失败: {e}"))?
            // 图优化开到最大:Conv+BN 折叠对 ISNet 提升明显
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| format!("设置优化级别失败: {e}"))?
            .with_intra_threads(threads)
            .map_err(|e| format!("设置线程数失败: {e}"))?
            .commit_from_file(path)
            .map_err(|e| format!("加载模型 {} 失败: {e}", path.display()))?;

        // 输入尺寸:静态就按模型写的来,动态就按 ISNet 标准 1024
        let (width, height) = session
            .inputs()
            .first()
            .and_then(|i| i.dtype().tensor_shape())
            .and_then(|shape| match shape.as_ref() {
                [_, _, h, w] if *h > 0 && *w > 0 => Some((*w as usize, *h as usize)),
                _ => None,
            })
            .unwrap_or((FALLBACK_INPUT, FALLBACK_INPUT));

        // 预热一次:ORT 首次运行会把内存分配/算子初始化做掉,否则第一张图明显更慢
        let warm = vec![0f32; 3 * height * width];
        let tensor = Tensor::from_array(([1usize, 3, height, width], warm))
            .map_err(|e| format!("构造张量失败: {e}"))?;
        let _ = session.run(ort::inputs![tensor]);

        Ok(Isnet {
            session,
            width,
            height,
            path: path.to_path_buf(),
            load_time: started.elapsed(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load_time(&self) -> Duration {
        self.load_time
    }

    pub fn input_size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// 返回与输入同尺寸的 8-bit 前景遮罩(255=前景,0=背景)
    pub fn foreground_mask(&mut self, shot: &Shot) -> Result<Vec<u8>, String> {
        let (w, h) = (shot.width.max(1) as usize, shot.height.max(1) as usize);
        if shot.rgba.len() < w * h * 4 {
            return Err("图像数据长度不足".to_string());
        }

        // RGBA -> RGB,再缩放到模型输入尺寸(rembg 用 LANCZOS 重采样)
        let mut rgb = Vec::with_capacity(w * h * 3);
        for px in shot.rgba[..w * h * 4].chunks_exact(4) {
            rgb.extend_from_slice(&px[..3]);
        }
        let src = RgbImage::from_raw(w as u32, h as u32, rgb)
            .ok_or_else(|| "无法构建 RGB 图像".to_string())?;
        let resized = image::imageops::resize(
            &src,
            self.width as u32,
            self.height as u32,
            FilterType::Lanczos3,
        );

        // mean=0.5, std=1(rembg 的 isnet-general-use 预处理),NCHW
        let n = self.width * self.height;
        let px = resized.as_raw();
        let mut data = vec![0f32; n * 3];
        for i in 0..n {
            for c in 0..3 {
                data[c * n + i] = px[i * 3 + c] as f32 / 255.0 - 0.5;
            }
        }
        let tensor = Tensor::from_array(([1usize, 3, self.height, self.width], data))
            .map_err(|e| format!("构造输入张量失败: {e}"))?;

        let outputs = self
            .session
            .run(ort::inputs![tensor])
            .map_err(|e| format!("模型推理失败: {e}"))?;
        // rembg 取第一个输出作为主遮罩
        let (shape, values) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("读取模型输出失败: {e}"))?;

        // 输出形状一般是 [1,1,H,W];取最后两维当作遮罩尺寸
        let dims: Vec<usize> = shape.iter().map(|d| (*d).max(0) as usize).collect();
        let (mh, mw) = match dims.as_slice() {
            [_, _, mh, mw] => (*mh, *mw),
            [_, mh, mw] => (*mh, *mw),
            [mh, mw] => (*mh, *mw),
            _ => return Err(format!("无法识别的模型输出形状: {dims:?}")),
        };
        if mh == 0 || mw == 0 {
            return Err("模型输出尺寸为 0".to_string());
        }
        let count = mh * mw;
        if values.len() < count {
            return Err("模型输出数据不足".to_string());
        }

        // rembg 的 min-max 归一化;整张遮罩几乎常数时直接判定全前景/全背景,
        // 避免把背景噪点放大成花斑。
        let mut lo = f32::INFINITY;
        let mut hi = f32::NEG_INFINITY;
        for &v in values.iter().take(count) {
            if v.is_finite() {
                lo = lo.min(v);
                hi = hi.max(v);
            }
        }
        if !lo.is_finite() || hi <= lo + 1e-6 {
            let fg = lo.is_finite() && lo > 0.5;
            return Ok(vec![if fg { 255 } else { 0 }; w * h]);
        }

        let inv = 1.0 / (hi - lo);
        let mut mask = Vec::with_capacity(count);
        for &v in values.iter().take(count) {
            let t = ((v - lo) * inv).clamp(0.0, 1.0);
            // 软阈值:0.5±MASK_SOFTNESS 之间平滑过渡,边缘仍是抗锯齿的;
            // 这不是 alpha matting(没有 trimap / 前景背景色估计)。
            let s = ((t - (0.5 - MASK_SOFTNESS)) / (2.0 * MASK_SOFTNESS)).clamp(0.0, 1.0);
            let eased = s * s * (3.0 - 2.0 * s);
            mask.push((eased * 255.0).round() as u8);
        }

        let mask_img = GrayImage::from_raw(mw as u32, mh as u32, mask)
            .ok_or_else(|| "无法构建遮罩图像".to_string())?;
        let resized_mask =
            image::imageops::resize(&mask_img, w as u32, h as u32, FilterType::Lanczos3);
        Ok(resized_mask.into_raw())
    }

    /// 去背结果:背景混成白色(与键控去背一致),便于继续在白底上标注/导出
    pub fn whiten(&mut self, shot: &Shot) -> Result<Vec<u8>, String> {
        let mask = self.foreground_mask(shot)?;
        Ok(composite_on_white(shot, &mask))
    }
}

/// 按 8-bit 前景遮罩把背景混成白色(mask=255 保留原色,mask=0 变纯白)。
/// 独立成纯函数,便于单元测试。
pub fn composite_on_white(shot: &Shot, mask: &[u8]) -> Vec<u8> {
    let w = shot.width.max(1) as usize;
    let h = shot.height.max(1) as usize;
    let mut out = shot.rgba.clone();
    if out.len() < w * h * 4 || mask.len() < w * h {
        return out;
    }
    for i in 0..w * h {
        let a = mask[i] as u32;
        let inv = 255 - a;
        let b = i * 4;
        for c in 0..3 {
            out[b + c] = ((out[b + c] as u32 * a + 255 * inv) / 255) as u8;
        }
        out[b + 3] = 255;
    }
    out
}

/// 查找目录列表:exe 目录、工作目录,以及各自的 models / assets 子目录,最后是用户目录
fn search_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            dirs.push(parent.to_path_buf());
            dirs.push(parent.join("models"));
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        dirs.push(cwd.clone());
        dirs.push(cwd.join("models"));
        dirs.push(cwd.join("assets"));
    }
    if let Ok(appdata) = std::env::var("APPDATA") {
        dirs.push(PathBuf::from(appdata).join("pointtofix").join("models"));
    }
    if let Ok(home) = std::env::var("USERPROFILE") {
        dirs.push(PathBuf::from(home).join(".u2net"));
    }
    dirs
}

fn find_in_dirs(name: &str) -> Option<PathBuf> {
    for dir in search_dirs() {
        let p = dir.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// 在常见位置查找 onnxruntime.dll
pub fn find_dll() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("POINTTOFIX_ORT_DLL") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    find_in_dirs(DLL_NAME)
}

/// 在常见位置查找 ISNet 模型;按 [`MODEL_NAMES`] 的优先级挑第一个
pub fn find_model() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("POINTTOFIX_ISNET_MODEL") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    for name in MODEL_NAMES {
        if let Some(p) = find_in_dirs(name) {
            return Some(p);
        }
    }
    // 用户可能改名(带版本号等):再按 isnet*.onnx 扫描一遍
    for dir in search_dirs() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut hits: Vec<PathBuf> = rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| {
                            let n = n.to_ascii_lowercase();
                            n.starts_with("isnet") && n.ends_with(".onnx")
                        })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::Shot;

    fn solid_shot(w: i32, h: i32, rgb: (u8, u8, u8)) -> Shot {
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..w * h {
            rgba.extend_from_slice(&[rgb.0, rgb.1, rgb.2, 255]);
        }
        Shot {
            width: w,
            height: h,
            rgba,
        }
    }

    #[test]
    fn mask_composite_white_and_original() {
        let shot = solid_shot(2, 1, (10, 20, 30));
        let out = composite_on_white(&shot, &[0, 255]);
        assert_eq!(&out[0..4], &[255, 255, 255, 255]);
        assert_eq!(&out[4..8], &[10, 20, 30, 255]);
    }

    #[test]
    fn mask_composite_half_blend() {
        let shot = solid_shot(1, 1, (100, 100, 100));
        let out = composite_on_white(&shot, &[128]);
        // (100*128 + 255*127) / 255 = 177
        assert_eq!(&out[0..3], &[177, 177, 177]);
        assert_eq!(out[3], 255);
    }

    #[test]
    fn model_priority_prefers_user_name() {
        // 优先级表本身要保证「用户命名的文件」排在官方名之前
        assert_eq!(MODEL_NAMES[0], "isnet-general-uxe.onnx");
        assert_eq!(MODEL_NAMES[1], "isnet-general-use-fp16.onnx");
        assert_eq!(MODEL_NAMES[2], "isnet-general-use.onnx");
    }

    /// 本地验证用:设置 POINTTOFIX_TEST_MODEL 指向 ISNet ONNX 后运行。
    /// 不设置时直接跳过,CI / 无模型机器不会失败。
    #[test]
    fn loads_and_runs_model_when_configured() {
        let Ok(path) = std::env::var("POINTTOFIX_TEST_MODEL") else {
            return;
        };
        let mut model = Isnet::load(Path::new(&path)).expect("load model");
        eprintln!(
            "model {} input={:?} load={:?}",
            model.path().display(),
            model.input_size(),
            model.load_time()
        );
        // 背景统一、中央放一块深色方形,模型至少应能输出一张合法遮罩
        let (w, h) = (160i32, 120i32);
        let mut shot = solid_shot(w, h, (230, 235, 240));
        for y in 35..85 {
            for x in 55..105 {
                let i = ((y * w + x) * 4) as usize;
                shot.rgba[i] = 30;
                shot.rgba[i + 1] = 40;
                shot.rgba[i + 2] = 50;
            }
        }
        let started = Instant::now();
        let mask = model.foreground_mask(&shot).expect("run model");
        eprintln!("inference took {:?}", started.elapsed());
        assert_eq!(mask.len(), (w * h) as usize);
        let center = mask[(60 * w + 80) as usize];
        let corner = mask[(5 * w + 5) as usize];
        eprintln!("mask center={center} corner={corner}");
        let out = model.whiten(&shot).expect("whiten");
        assert_eq!(out.len(), shot.rgba.len());
    }
}
