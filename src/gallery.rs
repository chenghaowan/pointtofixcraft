//! 画廊:已保存截图的存储层(SQLite + FTS5)+ 双栏浏览窗口。
//!
//! 存储层(`gallery.db`,位于 %APPDATA%\pointtofix\gallery.db):
//! - `shots` 主表 + `shots_fts` FTS5 外部内容表(trigger 自动同步,文本不落两份);
//! - tokenizer 用 SQLite 内建的 **trigram**:FTS5 分词器是注册进进程的,自定义
//!   分词器(如 jieba)会让任何外部程序(后期接入的 RAG 软件、python sqlite3)
//!   打开表时报 `unknown tokenizer`;trigram 对中/英/日混排做子串匹配,
//!   查询 <3 字符时回退 LIKE 全表扫(库只有几百行,毫秒级);
//! - WAL 模式:外部 RAG 软件可只读并发访问;
//! - 字段契约见 docs/db-schema.md:`id` 是外部接入的稳定外键,
//!   `file_path` 相对 gallery 根目录(整目录可搬家)。
//!
//! UI:顶部搜索栏;主画预览 + 元数据 + 带序号 OCR + 操作按钮;底部横向
//! 缩略图条(懒加载 + LRU 纹理缓存)。双击主画全屏查看;待机打开时
//! 原生窗口是临时放大的载体,顶部拖动条走 OS 级拖动(可拖出视口边界)。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use eframe::egui::{self, Align2, Color32, FontId, Id, Pos2, Rect, TextureHandle, TextureOptions, Vec2};
use rusqlite::Connection;

use crate::i18n::Strings;
use crate::ocr::{self, OcrLine};

const THUMB_W: f32 = 120.0;
const THUMB_H: f32 = 88.0;
/// 缩略图纹理 LRU 上限(每张约 120×88×4 ≈ 42 KB,60 张 ≈ 2.5 MB 显存)
const THUMB_CACHE: usize = 60;

// ---------- 路径 ----------

pub fn gallery_dir() -> PathBuf {
    std::env::var("APPDATA")
        .map(|p| PathBuf::from(p).join("pointtofix").join("gallery"))
        .unwrap_or_else(|_| std::path::PathBuf::from("gallery"))
}

/// DB 路径(与配置文件同目录)
pub fn db_path() -> PathBuf {
    std::env::var("APPDATA")
        .map(|p| PathBuf::from(p).join("pointtofix").join("gallery.db"))
        .unwrap_or_else(|_| std::path::PathBuf::from("gallery.db"))
}

/// DB 里的 file_path 存相对路径(用 / 分隔,跨工具可读);读取时拼回绝对路径
fn to_rel(abs: &Path) -> String {
    abs.strip_prefix(gallery_dir())
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| abs.to_string_lossy().replace('\\', "/"))
}

fn from_rel(rel: &str) -> PathBuf {
    gallery_dir().join(rel.replace('/', "\\"))
}

// ---------- 数据模型 ----------

#[derive(Clone, Debug)]
pub struct ShotRow {
    pub id: i64,
    /// unix 毫秒(显示时转本地时间,存储不做时区换算)
    pub created_at: i64,
    pub file: PathBuf,
    pub thumb: Option<PathBuf>,
    pub format: String,
    pub width: i32,
    pub height: i32,
    pub file_size: i64,
    pub app_name: Option<String>,
    pub ocr_text: String,
    pub ocr_lines: Vec<OcrLine>,
    pub ocr_status: String,
}

pub struct NewShot<'a> {
    pub created_at: i64,
    pub file: &'a Path,
    pub thumb: Option<&'a Path>,
    pub format: &'a str,
    pub width: i32,
    pub height: i32,
    pub file_size: i64,
    pub content_hash: &'a str,
    pub app_name: Option<&'a str>,
    pub ocr_text: &'a str,
    pub ocr_lines_json: &'a str,
    pub ocr_status: &'a str,
}

// ---------- SQLite ----------

static DB: OnceLock<Mutex<Option<Connection>>> = OnceLock::new();

fn db() -> &'static Mutex<Option<Connection>> {
    DB.get_or_init(|| Mutex::new(None))
}

fn open_db() -> Result<Connection, String> {
    if let Some(dir) = db_path().parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let conn = Connection::open(db_path()).map_err(|e| e.to_string())?;
    conn.execute_batch(
        // WAL:外部 RAG 软件只读连接可并发;user_version 是迁移锚点
        "PRAGMA journal_mode=WAL;
         PRAGMA user_version=1;
         CREATE TABLE IF NOT EXISTS shots (
           id           INTEGER PRIMARY KEY AUTOINCREMENT,
           created_at   INTEGER NOT NULL,
           file_path    TEXT NOT NULL,
           thumb_path   TEXT,
           format       TEXT NOT NULL,
           width        INTEGER NOT NULL,
           height       INTEGER NOT NULL,
           file_size    INTEGER NOT NULL,
           content_hash TEXT,
           app_name     TEXT,
           ocr_text     TEXT NOT NULL DEFAULT '',
           ocr_lines    TEXT,
           ocr_status   TEXT NOT NULL DEFAULT 'pending'
         );
         CREATE INDEX IF NOT EXISTS idx_shots_created ON shots(created_at);
         CREATE VIRTUAL TABLE IF NOT EXISTS shots_fts USING fts5(
           ocr_text, app_name,
           content='shots', content_rowid='id', tokenize='trigram'
         );
         CREATE TRIGGER IF NOT EXISTS shots_ai AFTER INSERT ON shots BEGIN
           INSERT INTO shots_fts(rowid, ocr_text, app_name)
             VALUES (new.id, new.ocr_text, new.app_name);
         END;
         CREATE TRIGGER IF NOT EXISTS shots_ad AFTER DELETE ON shots BEGIN
           INSERT INTO shots_fts(shots_fts, rowid, ocr_text, app_name)
             VALUES ('delete', old.id, old.ocr_text, old.app_name);
         END;
         CREATE TRIGGER IF NOT EXISTS shots_au AFTER UPDATE OF ocr_text, app_name ON shots BEGIN
           INSERT INTO shots_fts(shots_fts, rowid, ocr_text, app_name)
             VALUES ('delete', old.id, old.ocr_text, old.app_name);
           INSERT INTO shots_fts(rowid, ocr_text, app_name)
             VALUES (new.id, new.ocr_text, new.app_name);
         END;",
    )
    .map_err(|e| e.to_string())?;
    Ok(conn)
}

fn with_db<T>(f: impl FnOnce(&Connection) -> Result<T, rusqlite::Error>) -> Result<T, String> {
    let mut guard = db().lock().map_err(|_| "DB 锁中毒".to_string())?;
    if guard.is_none() {
        *guard = Some(open_db()?);
    }
    f(guard.as_ref().expect("DB 已打开")).map_err(|e| e.to_string())
}

fn row_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<ShotRow> {
    let file_rel: String = r.get(2)?;
    let thumb_rel: Option<String> = r.get(3)?;
    let ocr_json: Option<String> = r.get(11)?;
    Ok(ShotRow {
        id: r.get(0)?,
        created_at: r.get(1)?,
        file: from_rel(&file_rel),
        thumb: thumb_rel.as_deref().map(from_rel),
        format: r.get(4)?,
        width: r.get(5)?,
        height: r.get(6)?,
        file_size: r.get(7)?,
        app_name: r.get(8)?,
        ocr_text: r.get::<_, String>(10).unwrap_or_default(),
        ocr_lines: ocr_json
            .as_deref()
            .filter(|s| !s.is_empty() && *s != "[]")
            .map(ocr::lines_from_json)
            .unwrap_or_default(),
        ocr_status: r.get::<_, String>(12).unwrap_or_else(|_| "pending".into()),
    })
}

const SELECT_COLS: &str =
    "id, created_at, file_path, thumb_path, format, width, height, file_size, \
     app_name, content_hash, ocr_text, ocr_lines, ocr_status";

/// 按时间倒序列出;query 为空取全部,否则走 FTS(≥3 字符)或 LIKE 兜底(<3 字符)
pub fn list(query: &str) -> Result<Vec<ShotRow>, String> {
    let q = query.trim();
    with_db(|conn| {
        let mut stmt;
        if q.is_empty() {
            stmt = conn.prepare(&format!(
                "SELECT {SELECT_COLS} FROM shots ORDER BY created_at DESC LIMIT 500"
            ))?;
            let rows = stmt
                .query_map([], row_from)?
                .filter_map(|r| r.ok())
                .collect();
            return Ok(rows);
        }
        if q.chars().count() >= 3 {
            // FTS 短语查询:引号包住,内部引号转义;trigram 对子串命中
            let m = format!("\"{}\"", q.replace('"', "\"\""));
            stmt = conn.prepare(&format!(
                "SELECT {SELECT_COLS} FROM shots WHERE id IN \
                 (SELECT rowid FROM shots_fts WHERE shots_fts MATCH ?1) \
                 ORDER BY created_at DESC LIMIT 300"
            ))?;
            let rows = stmt.query_map([&m], row_from)?.filter_map(|r| r.ok()).collect();
            Ok(rows)
        } else {
            // trigram 需要 ≥3 字符;短查询回退 LIKE(库规模小,全表扫毫秒级)
            let esc = q
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_");
            let pat = format!("%{esc}%");
            stmt = conn.prepare(&format!(
                "SELECT {SELECT_COLS} FROM shots \
                 WHERE ocr_text LIKE ?1 ESCAPE '\\' OR app_name LIKE ?1 ESCAPE '\\' \
                 ORDER BY created_at DESC LIMIT 300"
            ))?;
            let rows = stmt.query_map([&pat], row_from)?.filter_map(|r| r.ok()).collect();
            Ok(rows)
        }
    })
}

/// 入库一条(文件先落盘,再写 DB;失败返回 Err)
pub fn insert(n: NewShot<'_>) -> Result<i64, String> {
    with_db(|conn| {
        conn.execute(
            "INSERT INTO shots(created_at, file_path, thumb_path, format, width, height, \
             file_size, content_hash, app_name, ocr_text, ocr_lines, ocr_status) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            rusqlite::params![
                n.created_at,
                to_rel(n.file),
                n.thumb.map(to_rel),
                n.format,
                n.width,
                n.height,
                n.file_size,
                n.content_hash,
                n.app_name,
                n.ocr_text,
                n.ocr_lines_json,
                n.ocr_status,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    })
}

/// OCR 完成晚于保存时回写文本(CLI 与外部脚本也可直接 UPDATE shots 表)
#[allow(dead_code)]
pub fn update_ocr(id: i64, ocr_text: &str, lines_json: &str, status: &str) -> Result<(), String> {
    with_db(|conn| {
        conn.execute(
            "UPDATE shots SET ocr_text=?2, ocr_lines=?3, ocr_status=?4 WHERE id=?1",
            rusqlite::params![id, ocr_text, lines_json, status],
        )?;
        Ok(())
    })
}

/// 删除 DB 行(文件/缩略图由调用方处理——文件走回收站)
pub fn delete_row(id: i64) -> Result<(), String> {
    with_db(|conn| {
        conn.execute("DELETE FROM shots WHERE id=?1", rusqlite::params![id])?;
        Ok(())
    })
}

// ---------- 杂项工具 ----------

/// 时间戳文件名:PointToFix_20261104_153012.png(本地时间,可读);
/// 同一秒内的第 seq 张追加 _2/_3(seq=0 不加后缀),再冲突才退回毫秒
pub fn timestamp_file_name(ext: &str, seq: u32) -> String {
    use windows::Win32::System::SystemInformation::GetLocalTime;
    let st = unsafe { GetLocalTime() };
    let base = format!(
        "PointToFix_{:04}{:02}{:02}_{:02}{:02}{:02}",
        st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond
    );
    if seq == 0 {
        format!("{base}.{ext}")
    } else {
        format!("{base}_{seq}.{ext}")
    }
}

/// 目录里下一个不冲突的时间戳文件名(同秒冲突追加 _2.._9,之后退回毫秒名)
pub fn unique_file_name(dir: &Path, ext: &str) -> PathBuf {
    for seq in 0..9u32 {
        let p = dir.join(timestamp_file_name(ext, seq));
        if !p.exists() {
            return p;
        }
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    dir.join(format!("pointtofix_{ts}.{ext}"))
}

/// 生成缩略图 PNG(宽缩到 target_w,保持比例;仅 10~30 KB,开画廊零解码等待)
pub fn thumbnail_png(shot: &crate::capture::Shot, target_w: u32) -> Result<Vec<u8>, String> {
    use image::ImageEncoder;
    let (w, h) = (shot.width as u32, shot.height as u32);
    let src = image::RgbaImage::from_raw(w, h, shot.rgba.clone()).ok_or("图像数据不完整")?;
    let tw = target_w.min(w).max(1);
    let th = ((h as f64 * tw as f64 / w as f64).round() as u32).max(1);
    let small = image::imageops::resize(&src, tw, th, image::imageops::FilterType::Lanczos3);
    let mut buf = Vec::new();
    image::codecs::png::PngEncoder::new(&mut buf)
        .write_image(small.as_raw(), tw, th, image::ExtendedColorType::Rgba8)
        .map_err(|e| e.to_string())?;
    Ok(buf)
}

/// unix 毫秒 → 本地时间 "2026-11-04 15:30:12"(Win32 时区转换,不引 chrono)
pub fn format_local_ms(ms: i64) -> String {
    use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
    // 100ns 间隔;1601-01-01 与 1970-01-01 相差 116444736000000000
    const EPOCH_OFFSET: i64 = 116444_736_000_000_000;
    let ticks = (ms.saturating_mul(10_000)).saturating_add(EPOCH_OFFSET);
    let ft = FILETIME {
        dwLowDateTime: (ticks as u64 & 0xFFFF_FFFF) as u32,
        dwHighDateTime: ((ticks as u64 >> 32) & 0xFFFF_FFFF) as u32,
    };
    unsafe {
        let mut utc = SYSTEMTIME::default();
        if FileTimeToSystemTime(&ft, &mut utc).is_err() {
            return format!("{ms}");
        }
        let mut local = SYSTEMTIME::default();
        let _ = SystemTimeToTzSpecificLocalTime(None, &utc, &mut local);
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            local.wYear, local.wMonth, local.wDay, local.wHour, local.wMinute, local.wSecond
        )
    }
}

/// 序号 → ①②③(20 以内用带圈数字,超出退回 "n.")
pub fn circled(n: u32) -> String {
    const CIRCLED: [char; 20] = [
        '①', '②', '③', '④', '⑤', '⑥', '⑦', '⑧', '⑨', '⑩',
        '⑪', '⑫', '⑬', '⑭', '⑮', '⑯', '⑰', '⑱', '⑲', '⑳',
    ];
    match (n as usize).checked_sub(1).and_then(|i| CIRCLED.get(i)) {
        Some(c) => c.to_string(),
        None => format!("{n}."),
    }
}

// ---------- 画廊窗口 ----------

/// 帧循环里调用;返回要显示的 toast(成功/失败)
pub struct GalleryUi {
    items: Vec<ShotRow>,
    loaded: bool,
    load_error: Option<String>,
    search: String,
    selected: Option<i64>,
    thumbs: HashMap<i64, TextureHandle>,
    thumb_order: Vec<i64>,
    preview: Option<(i64, TextureHandle)>,
    /// show() 检测到 × 关闭后置位,由调用方 take_closed() 收口
    just_closed: bool,
    /// 主画被双击,请求进入全屏查看(调用方 take_want_viewer 后接管)
    want_viewer: bool,
}

impl Default for GalleryUi {
    fn default() -> Self {
        GalleryUi {
            items: Vec::new(),
            loaded: false,
            load_error: None,
            search: String::new(),
            selected: None,
            thumbs: HashMap::new(),
            thumb_order: Vec::new(),
            preview: None,
            just_closed: false,
            want_viewer: false,
        }
    }
}

impl GalleryUi {
    fn reload(&mut self) {
        match list(&self.search) {
            Ok(rows) => {
                self.items = rows;
                self.load_error = None;
            }
            Err(e) => {
                self.load_error = Some(e);
                self.items.clear();
            }
        }
        self.loaded = true;
        self.selected = self
            .selected
            .filter(|id| self.items.iter().any(|r| r.id == *id));
        if self.selected.is_none() {
            self.selected = self.items.first().map(|r| r.id);
        }
    }

    fn load_thumb(&mut self, ctx: &egui::Context, row: &ShotRow) {
        if self.thumbs.contains_key(&row.id) {
            return;
        }
        let Some(path) = row.thumb.as_ref().filter(|p| p.is_file()) else {
            return;
        };
        if let Ok(img) = image::open(path) {
            let rgba = img.to_rgba8();
            let size = rgba.dimensions();
            let ci = egui::ColorImage::from_rgba_unmultiplied(
                [size.0 as usize, size.1 as usize],
                rgba.as_raw(),
            );
            let tex = ctx.load_texture(format!("thumb-{}", row.id), ci, TextureOptions::LINEAR);
            self.thumbs.insert(row.id, tex);
            self.thumb_order.insert(0, row.id);
            if self.thumb_order.len() > THUMB_CACHE {
                let evict = self.thumb_order.pop();
                if let Some(id) = evict {
                    self.thumbs.remove(&id);
                }
            }
        }
    }

    fn load_preview(&mut self, ctx: &egui::Context, row: &ShotRow) {
        if matches!(&self.preview, Some((id, _)) if *id == row.id) {
            return;
        }
        self.preview = None;
        if !row.file.is_file() {
            return;
        }
        if let Ok(img) = image::open(&row.file) {
            let rgba = img.to_rgba8();
            let size = rgba.dimensions();
            let ci = egui::ColorImage::from_rgba_unmultiplied(
                [size.0 as usize, size.1 as usize],
                rgba.as_raw(),
            );
            let tex = ctx.load_texture(format!("preview-{}", row.id), ci, TextureOptions::LINEAR);
            self.preview = Some((row.id, tex));
        }
    }
}

impl GalleryUi {
    /// 已加载条数(自检日志用)
    pub fn items_len(&self) -> usize {
        self.items.len()
    }

    /// 标注模式:egui::Window 浮窗。视口本就全屏、原点 (0,0),浮窗定位一直正常。
    pub fn show(&mut self, ctx: &egui::Context, tr: &Strings) -> Option<Result<String, String>> {
        if !self.loaded {
            self.reload();
        }
        let mut open = true;
        let mut toast: Option<Result<String, String>> = None;
        let screen_center = ctx.input(|i| i.raw.screen_rect).map(|r| r.center()).unwrap_or(Pos2::ZERO);

        let win = egui::Window::new(tr.gallery_title)
            .id(Id::unique("gallery"))
            .open(&mut open)
            .resizable(true)
            .collapsible(false)
            .pivot(Align2::CENTER_CENTER)
            .default_pos(screen_center)
            .default_size(Vec2::new(820.0, 560.0))
            .min_size(Vec2::new(640.0, 540.0))
            .show(ctx, |ui| {
                toast = self.body(ctx, ui, tr);
            });
        if std::env::var("POINTTOFIX_GEOM_LOG").is_ok() {
            if let Some(r) = &win {
                let (sr, or, npp, mon) = ctx.input(|i| {
                    (
                        i.raw.screen_rect,
                        i.viewport().outer_rect,
                        i.viewport().native_pixels_per_point,
                        i.viewport().monitor_size,
                    )
                });
                eprintln!(
                    "[gallery] win_rect={:?} screen={:?} outer={:?} native_ppp={:?} monitor={:?} egui_ppp={}",
                    r.response.rect,
                    sr,
                    or,
                    npp,
                    mon,
                    ctx.pixels_per_point()
                );
            }
        }
        if !open {
            self.loaded = false; // 下次打开重新加载
            self.just_closed = true;
            return None;
        }
        toast
    }

    /// 待机路径:渲染进独立原生视口(viewport)。主窗口不做程序化 move+resize
    /// ——那会把 winit 的窗口 scale 搞成 2.0,绘制整体翻倍错位;独立视口按
    /// 创建时的显示器 scale 建窗,天然正确。自绘标题栏=拖动把手+×,Esc 也关闭。
    pub fn show_standby(&mut self, ui: &mut egui::Ui, tr: &Strings) -> Option<Result<String, String>> {
        if !self.loaded {
            self.reload();
        }
        let ctx = ui.ctx().clone();
        // Esc 关闭(视口持有焦点时事件走这里)
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.just_closed = true;
        }
        let mut toast: Option<Result<String, String>> = None;
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(ui.visuals().window_fill()))
            .show(ui, |ui| {
                // ---- 自绘标题栏:拖动把手 + × ----
                // 拖动用 OuterPosition 增量而不是 OS 级 StartDrag:egui-winit 只在
                // 窗口持有焦点时才执行 drag_window(),视口刚弹出/失焦时会静默失效。
                ui.horizontal(|ui| {
                    let w = ui.available_width();
                    let (grip_rect, grip) =
                        ui.allocate_exact_size(egui::vec2(w - 34.0, 28.0), egui::Sense::drag());
                    if grip.hovered() {
                        ctx.set_cursor_icon(egui::CursorIcon::Grab);
                    }
                    if grip.dragged() {
                        if let Some(outer) = ctx.input(|i| i.viewport().outer_rect) {
                            let d = ctx.input(|i| i.pointer.delta());
                            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(
                                outer.min + d,
                            ));
                        }
                    }
                    ui.painter().text(
                        Pos2::new(grip_rect.left() + 10.0, grip_rect.center().y),
                        Align2::LEFT_CENTER,
                        tr.gallery_title,
                        FontId::proportional(15.0),
                        ui.visuals().text_color(),
                    );
                    if ui
                        .add_sized(
                            [30.0, 26.0],
                            egui::Button::new(egui::RichText::new("×").size(15.0)),
                        )
                        .clicked()
                    {
                        self.just_closed = true;
                    }
                });
                ui.add_space(2.0);
                toast = self.body(&ctx, ui, tr);
            });
        toast
    }

    /// 搜索栏 + 主画 + 元数据 + OCR + 按钮 + 缩略图条(egui::Window 与待机全视口共用)
    fn body(&mut self, ctx: &egui::Context, ui: &mut egui::Ui, tr: &Strings) -> Option<Result<String, String>> {
        let mut toast: Option<Result<String, String>> = None;
        let mut search_changed = false;

        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text(tr.gallery_search_hint)
                    .desired_width(280.0),
            );
            if ui.button("🔍").clicked() {
                search_changed = true;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(crate::i18n::fill(
                    tr.gallery_count,
                    &[&self.items.len().to_string()],
                ));
            });
        });
        if search_changed {
            self.reload();
        }
        ui.add_space(4.0);
        if let Some(err) = &self.load_error {
            ui.label(egui::RichText::new(format!("DB: {err}")).color(Color32::RED));
            return None;
        }
        if self.items.is_empty() {
            ui.add_space(20.0);
            ui.label(if self.search.is_empty() {
                tr.gallery_empty
            } else {
                tr.gallery_no_result
            });
            return None;
        }

        // ---- 流行布局:上面主画,下面缩略图条(filmstrip)----
        let visuals = ui.visuals().clone();
        let total = ui.available_size();
        let strip_h = THUMB_H + 16.0 + 8.0; // 缩略图 + 日期行 + 余量
        let ocr_h = (total.y * 0.22).clamp(70.0, 120.0);
        let reserved = strip_h + ocr_h + 88.0; // 元数据行 + 按钮行 + 分隔线/间距
        let preview_h = (total.y - reserved).max(180.0);

        let Some(row) = self
            .selected
            .and_then(|id| self.items.iter().find(|r| r.id == id).cloned())
        else {
            return None;
        };
        self.load_preview(ctx, &row);

        // ---- 主画:占满剩余高度,等比缩放居中;双击进入全屏查看 ----
        let (pv_rect, pv_resp) = ui.allocate_exact_size(
            Vec2::new(ui.available_width(), preview_h),
            egui::Sense::click(),
        );
        {
            let p = ui.painter_at(pv_rect);
            p.rect_filled(pv_rect, 6.0, visuals.extreme_bg_color);
            match &self.preview {
                Some((id, tex)) if *id == row.id => {
                    p.image(
                        tex.id(),
                        fit_rect(tex.size_vec2(), pv_rect.shrink(4.0)),
                        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        Color32::WHITE,
                    );
                }
                _ => {
                    p.text(
                        pv_rect.center(),
                        Align2::CENTER_CENTER,
                        if row.file.is_file() { "…" } else { tr.gallery_missing_file },
                        FontId::proportional(13.0),
                        visuals.weak_text_color(),
                    );
                }
            }
        }
        if pv_resp.double_clicked() {
            self.want_viewer = true;
        }
        ui.add_space(4.0);
        let status_note = match row.ocr_status.as_str() {
            "done" => String::new(),
            other => format!(" · OCR:{other}"),
        };
        ui.label(
            egui::RichText::new(format!(
                "{} · {}×{} · {} · {}{}",
                format_local_ms(row.created_at),
                row.width,
                row.height,
                row.format,
                human_size(row.file_size as u64),
                status_note
            ))
            .weak()
            .small(),
        );
        // ---- 带序号 OCR 文本 ----
        egui::ScrollArea::vertical()
            .id_salt("gallery-ocr")
            .max_height(ocr_h)
            .show(ui, |ui| {
                if row.ocr_lines.is_empty() {
                    ui.label(egui::RichText::new(tr.gallery_no_ocr).weak());
                } else {
                    for l in &row.ocr_lines {
                        ui.label(format!("{} {}", circled(l.no), l.text));
                    }
                }
            });
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            if ui.button(tr.gallery_copy_image).clicked() {
                toast = Some(copy_image(&row.file).map(|_| tr.copied_tip.to_string()));
            }
            if ui.button(tr.gallery_copy_text).clicked() {
                let text = numbered_text(&row);
                toast = Some(clipboard_text(&text).map(|_| tr.ocr_copied.to_string()));
            }
            if ui.button(tr.gallery_open_folder).clicked() {
                let _ = std::process::Command::new("explorer")
                    .arg(format!("/select,{}", row.file.display()))
                    .spawn();
            }
            if ui
                .button(egui::RichText::new(tr.gallery_delete).color(Color32::from_rgb(183, 28, 28)))
                .clicked()
            {
                toast = Some(delete_shot(&row).map(|_| tr.gallery_deleted.to_string()));
                self.reload();
            }
        });
        ui.add_space(4.0);
        ui.separator();

        // ---- 缩略图条:底部横向滚动 ----
        egui::ScrollArea::horizontal()
            .id_salt("gallery-strip")
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (ti, row) in self.items.clone().iter().enumerate() {
                        self.load_thumb(ctx, row);
                        let tex = self.thumbs.get(&row.id);
                        let (rect, resp) = ui.allocate_exact_size(
                            Vec2::new(THUMB_W, THUMB_H + 14.0),
                            egui::Sense::click(),
                        );
                        let img_rect =
                            Rect::from_min_size(rect.min, Vec2::new(THUMB_W, THUMB_H));
                        let selected = self.selected == Some(row.id);
                        let p = ui.painter();
                        p.rect_filled(
                            img_rect,
                            4.0,
                            if selected {
                                visuals.selection.stroke.color
                            } else if resp.hovered() {
                                visuals.widgets.hovered.bg_fill
                            } else {
                                visuals.extreme_bg_color
                            },
                        );
                        match (tex, row.file.is_file()) {
                            (Some(t), _) => {
                                // 选中时内缩 2px,留出选色环
                                p.image(
                                    t.id(),
                                    fit_rect(
                                        t.size_vec2(),
                                        if selected {
                                            img_rect.shrink(2.0)
                                        } else {
                                            img_rect
                                        },
                                    ),
                                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                                    Color32::WHITE,
                                );
                            }
                            (None, true) => {
                                p.text(
                                    img_rect.center(),
                                    Align2::CENTER_CENTER,
                                    "…",
                                    FontId::proportional(14.0),
                                    Color32::GRAY,
                                );
                            }
                            (None, false) => {
                                p.text(
                                    img_rect.center(),
                                    Align2::CENTER_CENTER,
                                    "✕",
                                    FontId::proportional(14.0),
                                    Color32::GRAY,
                                );
                            }
                        }
                        let caption = format!(
                            "{} {}",
                            &format_local_ms(row.created_at)[5..16],
                            row.app_name.as_deref().unwrap_or("")
                        );
                        p.text(
                            Pos2::new(rect.min.x + 2.0, img_rect.bottom() + 2.0),
                            Align2::LEFT_TOP,
                            caption,
                            FontId::proportional(9.5),
                            visuals.weak_text_color(),
                        );
                        // 悬停快捷删除:右上角 ✕。悬停判定用指针包含(手动),
                        // 不依赖 egui 的 hover 优先级——叠放在 allocate 出的
                        // tile 上的小 interact 实测拿不到 hovered。
                        let pointer_pos = ctx.input(|i| i.pointer.latest_pos());
                        let hovering = pointer_pos.is_some_and(|pp| rect.contains(pp));
                        if ti == 0 && std::env::var("POINTTOFIX_GEOM_LOG").is_ok() {
                            eprintln!(
                                "[gallery] tile0 rect={rect:?} pointer={pointer_pos:?} hovering={hovering}"
                            );
                        }
                        let del_rect = Rect::from_min_size(
                            img_rect.right_top() - Vec2::new(18.0, 0.0),
                            Vec2::new(18.0, 18.0),
                        );
                        let del_resp = ui.interact(
                            del_rect,
                            Id::unique(("gal-del", row.id)),
                            egui::Sense::click(),
                        );
                        if hovering {
                            p.rect_filled(
                                del_rect,
                                4.0,
                                Color32::from_rgba_unmultiplied(178, 34, 34, 225),
                            );
                            p.text(
                                del_rect.center(),
                                Align2::CENTER_CENTER,
                                "✕",
                                FontId::proportional(11.0),
                                Color32::WHITE,
                            );
                        }
                        if del_resp.clicked() {
                            toast = Some(
                                delete_shot(row).map(|_| tr.gallery_deleted.to_string()),
                            );
                            self.reload();
                        }
                        if resp.clicked() {
                            self.selected = Some(row.id);
                        }
                    }
                });
            });
        toast
    }

    /// × 关闭信号(读后即清)。此前 open=false 被丢弃,画廊的 × 实际关不掉。
    pub fn take_closed(&mut self) -> bool {
        std::mem::take(&mut self.just_closed)
    }

    /// 主画双击请求(读后即清)
    pub fn take_want_viewer(&mut self) -> bool {
        std::mem::take(&mut self.want_viewer)
    }

    /// 全屏查看当前选中截图:黑底等比铺满所在视口(渲染进独立查看视口)。
    /// Esc 或点击返回 true——视口的创建/销毁由调用方负责。
    pub fn viewer_ui(&mut self, ui: &mut egui::Ui, tr: &Strings) -> bool {
        let ctx = ui.ctx().clone();
        let Some(row) = self
            .selected
            .and_then(|id| self.items.iter().find(|r| r.id == id).cloned())
        else {
            return true;
        };
        self.load_preview(&ctx, &row);
        let sr = ctx.input(|i| i.raw.screen_rect).unwrap_or(Rect::ZERO);
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            Id::unique("gallery-viewer"),
        ));
        painter.rect_filled(sr, 0.0, Color32::BLACK);
        match &self.preview {
            Some((id, tex)) if *id == row.id => {
                painter.image(
                    tex.id(),
                    fit_rect(tex.size_vec2(), sr.shrink(24.0)),
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
            _ => {
                painter.text(
                    sr.center(),
                    Align2::CENTER_CENTER,
                    if row.file.is_file() { "…" } else { tr.gallery_missing_file },
                    FontId::proportional(16.0),
                    Color32::GRAY,
                );
            }
        }
        painter.text(
            Pos2::new(sr.left() + 12.0, sr.bottom() - 24.0),
            Align2::LEFT_CENTER,
            tr.gallery_viewer_hint,
            FontId::proportional(12.0),
            Color32::GRAY,
        );
        let clicked = ctx.input(|i| i.pointer.primary_clicked());
        let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        esc || clicked
    }
}

fn fit_rect(content: Vec2, into: Rect) -> Rect {
    if content.x <= 0.0 || content.y <= 0.0 {
        return into;
    }
    let scale = (into.width() / content.x).min(into.height() / content.y).min(1.0);
    let size = content * scale;
    Rect::from_center_size(into.center(), size)
}

fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b < KB {
        format!("{bytes} B")
    } else if b < KB * KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{:.2} MB", b / (KB * KB))
    }
}

/// 带序号的全文(① 文本,每行一条)
fn numbered_text(row: &ShotRow) -> String {
    if !row.ocr_lines.is_empty() {
        row.ocr_lines
            .iter()
            .map(|l| format!("{} {}", circled(l.no), l.text))
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        row.ocr_text.clone()
    }
}

fn clipboard_text(text: &str) -> Result<(), String> {
    arboard::Clipboard::new()
        .and_then(|mut cb| cb.set_text(text.to_string()))
        .map_err(|e| e.to_string())
}

fn copy_image(path: &Path) -> Result<(), String> {
    let img = image::open(path).map_err(|e| e.to_string())?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    arboard::Clipboard::new()
        .and_then(|mut cb| {
            cb.set_image(arboard::ImageData {
                width: w as usize,
                height: h as usize,
                bytes: std::borrow::Cow::Owned(rgba.into_raw()),
            })
        })
        .map_err(|e| e.to_string())
}

/// 删除一条:图片进回收站 + 删缩略图 + 删 DB 行(触发器同步 FTS)
fn delete_shot(row: &ShotRow) -> Result<(), String> {
    trash::delete(&row.file).map_err(|e| format!("回收站: {e}"))?;
    if let Some(thumb) = &row.thumb {
        let _ = std::fs::remove_file(thumb);
    }
    delete_row(row.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn circled_numbers() {
        assert_eq!(circled(1), "①");
        assert_eq!(circled(20), "⑳");
        assert_eq!(circled(21), "21.");
    }

    #[test]
    fn rel_path_roundtrip() {
        let abs = gallery_dir().join("PointToFix_x.png");
        let rel = to_rel(&abs);
        assert!(!rel.contains('\\'));
        assert_eq!(from_rel(&rel), abs);
    }

    #[test]
    fn local_time_format_sane() {
        let s = format_local_ms(0);
        // 1970-01-01(时区偏移不影响格式)
        assert_eq!(s.len(), 19);
        assert!(s.starts_with("1970-01-01 ") || s.starts_with("1970-01-0"));
    }
}
