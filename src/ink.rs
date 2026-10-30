//! 墨迹模型:所有标注都是矢量图形,存进列表、每帧用 epaint 重绘。
//! 撤销/重做双向栈;橡皮擦是圆形笔刷——沿拖动路径只擦掉圈内的那部分墨迹。

use eframe::egui::{
    epaint::CornerRadius, epaint::StrokeKind, Align2, Color32, FontId, Painter, Pos2, Rect, Shape,
    Stroke, Vec2,
};
use eframe::egui::epaint::EllipseShape;
use eframe::egui::epaint::QuadraticBezierShape;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool {
    Pen,
    Eraser,
    Line,
    Arrow,
    DoubleArrow,
    Rect,
    RectFilled,
    Ellipse,
    EllipseFilled,
    Text,
    /// 打勾盖章(绿色)
    CheckStamp,
    /// 打叉盖章(红色)
    CrossStamp,
    /// 时间章(点击时把"日期+整点"盖在画布上)
    Time,
    /// 签名章(文字在设置里自定义)
    Signature,
}

#[derive(Clone, Copy)]
pub struct Spec {
    pub color: Color32,
    pub width: f32,
}

#[derive(Clone)]
pub enum Ink {
    Freehand { pts: Vec<Pos2>, spec: Spec },
    Line { a: Pos2, b: Pos2, spec: Spec },
    Arrow { a: Pos2, b: Pos2, spec: Spec, double: bool },
    Rect { a: Pos2, b: Pos2, spec: Spec, filled: bool },
    Ellipse { a: Pos2, b: Pos2, spec: Spec, filled: bool },
    Text { pos: Pos2, text: String, size: f32, color: Color32 },
    /// 勾/叉盖章,size 由当前笔宽档位决定
    Stamp { pos: Pos2, size: f32, color: Color32, check: bool },
    /// 印章:圆角边框 + 文本(时间章/签名章共用,文本在点击那刻定格)
    Seal { pos: Pos2, size: f32, color: Color32, text: String },
    /// 实心图形被橡皮擦切开后剩下的填充薄片(只填充、不描边):
    /// epaint 不支持带洞的填充,用扫描线薄片近似圆形缺口。
    FillRect { a: Pos2, b: Pos2, color: Color32 },
    /// 填充多边形(擦除后保留下来的箭头头部等)
    Poly { pts: Vec<Pos2>, color: Color32 },
}

impl Ink {
    pub fn paint(&self, p: &Painter) {
        match self {
            Ink::Freehand { pts, spec } => {
                if pts.len() >= 2 {
                    p.add(Shape::line(pts.clone(), Stroke::new(spec.width, spec.color)));
                } else if let Some(&pt) = pts.first() {
                    p.add(Shape::line(vec![pt, pt + Vec2::X], Stroke::new(spec.width, spec.color)));
                }
            }
            Ink::Line { a, b, spec } => {
                p.add(Shape::line(vec![*a, *b], Stroke::new(spec.width, spec.color)));
            }
            Ink::Arrow { a, b, spec, double } => {
                let Some(g) = arrow_geom(*a, *b, *spec, *double) else { return };
                let stroke = Stroke::new(spec.width, spec.color);
                // 主线两端都缩进到箭头底部,避免粗线的方形端帽从尖端戳出(平头感)
                if g.line_a.distance(g.line_b) > 0.5 {
                    p.add(Shape::line(vec![g.line_a, g.line_b], stroke));
                }
                // 尖端三角形:顶点正好在 b / a
                p.add(Shape::convex_polygon(
                    vec![
                        *b,
                        g.line_b + g.perp * g.head_half,
                        g.line_b - g.perp * g.head_half,
                    ],
                    spec.color,
                    Stroke::NONE,
                ));
                if *double {
                    p.add(Shape::convex_polygon(
                        vec![
                            *a,
                            g.line_a + g.perp * g.head_half,
                            g.line_a - g.perp * g.head_half,
                        ],
                        spec.color,
                        Stroke::NONE,
                    ));
                }
            }
            Ink::Rect { a, b, spec, filled } => {
                let r = Rect::from_two_pos(*a, *b);
                if *filled {
                    p.add(Shape::rect_filled(r, CornerRadius::same(0), spec.color));
                }
                let pts = vec![
                    r.left_top(),
                    r.right_top(),
                    r.right_bottom(),
                    r.left_bottom(),
                    r.left_top(),
                ];
                p.add(Shape::line(pts, Stroke::new(spec.width, spec.color)));
            }
            Ink::Ellipse { a, b, spec, filled } => {
                let (c, radius) = ellipse_params(*a, *b);
                let shape = if *filled {
                    Shape::Ellipse(EllipseShape::filled(c, radius, spec.color))
                } else {
                    Shape::Ellipse(EllipseShape::stroke(c, radius, Stroke::new(spec.width, spec.color)))
                };
                p.add(shape);
            }
            Ink::Text { pos, text, size, color } => {
                if !text.is_empty() {
                    p.text(*pos, Align2::LEFT_TOP, text.clone(), FontId::proportional(*size), *color);
                }
            }
            Ink::Stamp { pos, size, color, check } => {
                let c = *pos;
                let h = size * 0.5;
                let w = (size * 0.17).max(4.0);
                let stroke = Stroke::new(w, *color);
                let cap = w * 0.5;
                if *check {
                    // 圆头笔画 + 长边微微外弧,接近马克笔手写效果
                    let a = Pos2::new(c.x - h * 0.92, c.y + h * 0.02);
                    let bpt = Pos2::new(c.x - h * 0.2, c.y + h * 0.68);
                    let cpt = Pos2::new(c.x + h * 0.98, c.y - h * 0.86);
                    p.add(QuadraticBezierShape::from_points_stroke(
                        [a, a + (bpt - a) * 0.5 + Vec2::new(0.0, h * 0.05), bpt],
                        false,
                        Color32::TRANSPARENT,
                        stroke,
                    ));
                    let mid = bpt + (cpt - bpt) * 0.5 + Vec2::new(h * 0.08, -h * 0.05);
                    p.add(QuadraticBezierShape::from_points_stroke(
                        [bpt, mid, cpt],
                        false,
                        Color32::TRANSPARENT,
                        stroke,
                    ));
                    for pt in [a, bpt, cpt] {
                        p.circle_filled(pt, cap, *color);
                    }
                } else {
                    // 交叉:两条略弯的对角线 + 四端圆帽
                    let r = h * 0.8;
                    let segs = [
                        (Pos2::new(c.x - r, c.y - r), Pos2::new(c.x + r, c.y + r)),
                        (Pos2::new(c.x + r, c.y - r), Pos2::new(c.x - r, c.y + r)),
                    ];
                    for (pa, pb) in segs {
                        let d = pb - pa;
                        let mid = pa + d * 0.5 + Vec2::new(d.y * 0.05, -d.x * 0.05);
                        p.add(QuadraticBezierShape::from_points_stroke(
                            [pa, mid, pb],
                            false,
                            Color32::TRANSPARENT,
                            stroke,
                        ));
                        p.circle_filled(pa, cap, *color);
                        p.circle_filled(pb, cap, *color);
                    }
                }
            }
            Ink::Seal { pos, size, color, text } => {
                if !text.is_empty() {
                    let font = FontId::proportional(*size);
                    let galley = p.layout_no_wrap(text.clone(), font, *color);
                    let pad_x = size * 0.42;
                    let pad_y = size * 0.26;
                    let r = Rect::from_min_size(
                        *pos,
                        galley.size() + Vec2::new(pad_x * 2.0, pad_y * 2.0),
                    );
                    // 印章感:圆角边框,线宽随字号
                    p.rect_stroke(
                        r,
                        CornerRadius::same((*size * 0.16).round().max(2.0) as u8),
                        Stroke::new((*size * 0.09).max(1.5), *color),
                        StrokeKind::Inside,
                    );
                    p.galley(r.min + Vec2::new(pad_x, pad_y), galley, *color);
                }
            }
            Ink::FillRect { a, b, color } => {
                p.add(Shape::rect_filled(
                    Rect::from_two_pos(*a, *b),
                    CornerRadius::same(0),
                    *color,
                ));
            }
            Ink::Poly { pts, color } => {
                if pts.len() >= 3 {
                    p.add(Shape::convex_polygon(pts.clone(), *color, Stroke::NONE));
                }
            }
        }
    }

    /// 退化图形(没画出面积的)不入栈,避免 undo 里全是垃圾
    pub fn is_degenerate(&self) -> bool {
        match self {
            Ink::Freehand { pts, .. } => pts.len() < 2,
            Ink::Line { a, b, .. } | Ink::Arrow { a, b, .. } => a.distance(*b) < 2.0,
            Ink::Rect { a, b, .. } | Ink::Ellipse { a, b, .. } => {
                (a.x - b.x).abs() < 2.0 || (a.y - b.y).abs() < 2.0
            }
            Ink::Text { text, .. } => text.trim().is_empty(),
            Ink::Stamp { .. } => false,
            Ink::Seal { text, .. } => text.trim().is_empty(),
            Ink::FillRect { a, b, .. } => {
                (a.x - b.x).abs() < 0.5 || (a.y - b.y).abs() < 0.5
            }
            Ink::Poly { pts, .. } => pts.len() < 3,
        }
    }

    /// 橡皮命中检测用的采样点
    fn sample_points(&self) -> Vec<Pos2> {
        match self {
            Ink::Freehand { pts, .. } => pts.clone(),
            Ink::Line { a, b, .. } | Ink::Arrow { a, b, .. } => {
                let mut v = vec![*a, *b];
                for t in [0.25f32, 0.5, 0.75] {
                    v.push(*a + (*b - *a) * t);
                }
                v
            }
            Ink::Rect { a, b, .. } => {
                let r = Rect::from_two_pos(*a, *b);
                vec![
                    r.left_top(),
                    r.right_top(),
                    r.right_bottom(),
                    r.left_bottom(),
                    r.center_top(),
                    r.center_bottom(),
                    r.left_center(),
                    r.right_center(),
                ]
            }
            Ink::Ellipse { a, b, .. } => {
                let (c, radius) = ellipse_params(*a, *b);
                (0..16)
                    .map(|i| {
                        let ang = std::f32::consts::TAU * i as f32 / 16.0;
                        c + Vec2::new(ang.cos() * radius.x, ang.sin() * radius.y)
                    })
                    .collect()
            }
            Ink::Text { pos, size, .. } => vec![
                *pos,
                *pos + Vec2::new(*size * 2.0, 0.0),
                *pos + Vec2::new(0.0, *size),
                *pos + Vec2::new(*size * 2.0, *size),
            ],
            Ink::Stamp { pos, size, .. } => {
                let h = size * 0.5 * 0.7;
                vec![
                    *pos,
                    *pos + Vec2::new(h, h),
                    *pos + Vec2::new(-h, -h),
                    *pos + Vec2::new(h, -h),
                    *pos + Vec2::new(-h, h),
                ]
            }
            Ink::Seal { pos, size, text, .. } => {
                // 近似包围盒:字宽按 0.62 倍字号估,加上内边距与边框
                let w = text.chars().count() as f32 * size * 0.62 + size * 1.4;
                let h = size * 1.55;
                vec![
                    *pos,
                    *pos + Vec2::new(w, 0.0),
                    *pos + Vec2::new(0.0, h),
                    *pos + Vec2::new(w, h),
                    *pos + Vec2::new(w * 0.5, h * 0.5),
                ]
            }
            Ink::FillRect { a, b, .. } => {
                let r = Rect::from_two_pos(*a, *b);
                vec![
                    r.center(),
                    r.left_top(),
                    r.right_top(),
                    r.right_bottom(),
                    r.left_bottom(),
                ]
            }
            Ink::Poly { pts, .. } => pts.clone(),
        }
    }

    /// 文字/勾叉没有可切分的路径:圆圈碰到就整笔移除
    fn hits_eraser(&self, from: Pos2, to: Pos2, radius: f32) -> bool {
        self.sample_points()
            .iter()
            .any(|p| dist_to_segment(*p, from, to) <= radius)
    }
}

/// 画布:已完成图形 + 正在画的图形,撤销/重做双向栈
#[derive(Default)]
pub struct Canvas {
    pub shapes: Vec<Ink>,
    pub current: Option<Ink>,
    pub redo_stack: Vec<Ink>,
}

impl Canvas {
    pub fn paint(&self, p: &Painter) {
        for s in &self.shapes {
            s.paint(p);
        }
        if let Some(c) = &self.current {
            c.paint(p);
        }
    }

    pub fn commit(&mut self) {
        if let Some(c) = self.current.take() {
            if !c.is_degenerate() {
                self.shapes.push(c);
                self.redo_stack.clear();
            }
        }
    }

    /// 一次性图形(勾/叉/时间章):点击即完成,不经过拖拽管线。
    /// 与 commit 一样清空 redo 栈,否则撤销盖章后重做会把更早的图形一起带回来。
    pub fn push_instant(&mut self, ink: Ink) {
        if !ink.is_degenerate() {
            self.shapes.push(ink);
            self.redo_stack.clear();
        }
    }

    pub fn undo(&mut self) {
        if let Some(s) = self.shapes.pop() {
            self.redo_stack.push(s);
        }
    }

    pub fn redo(&mut self) {
        if let Some(s) = self.redo_stack.pop() {
            self.shapes.push(s);
        }
    }

    pub fn clear(&mut self) {
        self.shapes.clear();
        self.current = None;
        self.redo_stack.clear();
    }

    /// 橡皮擦:圆形笔刷沿 from→to 扫过,只擦掉圈内的那部分墨迹(所有图形按路径切断)。
    /// 实心图形无法直接抠洞,用扫描线薄片近似圆形缺口;文字/勾叉保持整笔移除。
    pub fn erase(&mut self, from: Pos2, to: Pos2, radius: f32) {
        let mut out: Vec<Ink> = Vec::with_capacity(self.shapes.len());
        for shape in self.shapes.drain(..) {
            erase_shape(shape, from, to, radius, &mut out);
        }
        self.shapes = out;
        self.redo_stack.clear();
    }
}

pub fn freehand(spec: Spec, pos: Pos2) -> Ink {
    Ink::Freehand { pts: vec![pos], spec }
}

// ---------- 圆形橡皮的几何工具 ----------

/// 实心图形抠洞时扫描线薄片的高度(逻辑像素,越小缺口越圆滑)
const FILL_STRIP_H: f32 = 0.5;

/// 箭头几何:箭杆两端缩进到箭头底部
struct ArrowGeom {
    perp: Vec2,
    line_a: Pos2,
    line_b: Pos2,
    head_half: f32,
}

fn arrow_geom(a: Pos2, b: Pos2, spec: Spec, double: bool) -> Option<ArrowGeom> {
    let d = b - a;
    let len = d.length();
    if len < 1.0 {
        return None;
    }
    let dir = d / len;
    let perp = Vec2::new(-dir.y, dir.x);
    let head_len = (spec.width * 3.4).max(13.0).min(len * 0.55);
    Some(ArrowGeom {
        perp,
        line_a: if double { a + dir * head_len } else { a },
        line_b: b - dir * head_len,
        head_half: head_len * 0.52,
    })
}

/// 把一个图形按圆形橡皮切成若干残余图形
fn erase_shape(shape: Ink, from: Pos2, to: Pos2, radius: f32, out: &mut Vec<Ink>) {
    match shape {
        Ink::Freehand { pts, spec } => {
            let cut = radius + spec.width * 0.5;
            let runs = clip_polyline(&pts, from, to, cut);
            if runs.len() == 1 && runs[0] == pts {
                out.push(Ink::Freehand { pts, spec });
            } else {
                for run in runs {
                    out.push(Ink::Freehand { pts: run, spec });
                }
            }
        }
        Ink::Line { a, b, spec } => {
            let cut = radius + spec.width * 0.5;
            let runs = clip_polyline(&[a, b], from, to, cut);
            if runs.len() == 1 && runs[0].len() == 2 && runs[0][0] == a && runs[0][1] == b {
                out.push(Ink::Line { a, b, spec });
            } else {
                for run in runs {
                    if let (Some(&ra), Some(&rb)) = (run.first(), run.last()) {
                        if ra.distance(rb) > 0.5 {
                            out.push(Ink::Line { a: ra, b: rb, spec });
                        }
                    }
                }
            }
        }
        Ink::Arrow { a, b, spec, double } => {
            let Some(g) = arrow_geom(a, b, spec, double) else {
                out.push(Ink::Arrow { a, b, spec, double });
                return;
            };
            let cut = radius + spec.width * 0.5;
            let runs = clip_polyline(&[g.line_a, g.line_b], from, to, cut);
            let shaft_hit = !(runs.len() == 1
                && runs[0].len() == 2
                && runs[0][0] == g.line_a
                && runs[0][1] == g.line_b);
            let head_b = [
                b,
                g.line_b + g.perp * g.head_half,
                g.line_b - g.perp * g.head_half,
            ];
            let head_a = [
                a,
                g.line_a + g.perp * g.head_half,
                g.line_a - g.perp * g.head_half,
            ];
            let head_b_hit = tri_hits_capsule(head_b, from, to, radius);
            let head_a_hit = double && tri_hits_capsule(head_a, from, to, radius);
            if !shaft_hit && !head_b_hit && !head_a_hit {
                out.push(Ink::Arrow { a, b, spec, double });
                return;
            }
            // 箭杆按圆切成若干直线段;箭头不是圆形可切的形状,碰到就整个去掉
            for run in runs {
                if let (Some(&ra), Some(&rb)) = (run.first(), run.last()) {
                    if ra.distance(rb) > 0.5 {
                        out.push(Ink::Line { a: ra, b: rb, spec });
                    }
                }
            }
            if !head_b_hit {
                out.push(Ink::Poly { pts: head_b.to_vec(), color: spec.color });
            }
            if double && !head_a_hit {
                out.push(Ink::Poly { pts: head_a.to_vec(), color: spec.color });
            }
        }
        Ink::Rect { a, b, spec, filled } => {
            let r = Rect::from_two_pos(a, b);
            let outline = vec![
                r.left_top(),
                r.right_top(),
                r.right_bottom(),
                r.left_bottom(),
                r.left_top(),
            ];
            let cut = radius + spec.width * 0.5;
            let runs = clip_polyline(&outline, from, to, cut);
            let outline_hit = !(runs.len() == 1 && runs[0] == outline);
            let fill_hit =
                filled && (outline_hit || r.contains(from) || r.contains(to));
            if !outline_hit && !fill_hit {
                out.push(Ink::Rect { a, b, spec, filled });
            } else {
                if filled {
                    emit_rect_fill(r, from, to, radius, spec.color, out);
                }
                if outline_hit {
                    for run in runs {
                        out.push(Ink::Freehand { pts: run, spec });
                    }
                } else {
                    out.push(Ink::Rect { a, b, spec, filled: false });
                }
            }
        }
        Ink::Ellipse { a, b, spec, filled } => {
            let (c, rad) = ellipse_params(a, b);
            let outline = ellipse_outline(c, rad);
            let cut = radius + spec.width * 0.5;
            let runs = clip_polyline(&outline, from, to, cut);
            let outline_hit = !(runs.len() == 1 && runs[0] == outline);
            let fill_hit = filled
                && (outline_hit
                    || point_in_ellipse(from, c, rad)
                    || point_in_ellipse(to, c, rad));
            if !outline_hit && !fill_hit {
                out.push(Ink::Ellipse { a, b, spec, filled });
            } else {
                if filled {
                    emit_ellipse_fill(c, rad, from, to, radius, spec.color, out);
                }
                if outline_hit {
                    for run in runs {
                        out.push(Ink::Freehand { pts: run, spec });
                    }
                } else {
                    out.push(Ink::Ellipse { a, b, spec, filled: false });
                }
            }
        }
        Ink::FillRect { a, b, color } => {
            let r = Rect::from_two_pos(a, b);
            if segment_rect_dist(from, to, r) <= radius {
                emit_rect_fill(r, from, to, radius, color, out);
            } else {
                out.push(Ink::FillRect { a, b, color });
            }
        }
        Ink::Poly { pts, color } => {
            if !poly_hits_capsule(&pts, from, to, radius) {
                out.push(Ink::Poly { pts, color });
            }
        }
        other @ (Ink::Text { .. } | Ink::Stamp { .. } | Ink::Seal { .. }) => {
            if !other.hits_eraser(from, to, radius) {
                out.push(other);
            }
        }
    }
}

/// 把折线按“圆形笔刷扫过的胶囊”切成若干段(保留圈外部分),切割处按 1px 采样
fn clip_polyline(pts: &[Pos2], from: Pos2, to: Pos2, radius: f32) -> Vec<Vec<Pos2>> {
    let mut runs: Vec<Vec<Pos2>> = Vec::new();
    let Some(&first) = pts.first() else { return runs };
    if pts.len() == 1 {
        if dist_to_segment(first, from, to) > radius {
            runs.push(vec![first]);
        }
        return runs;
    }

    let mut run: Vec<Pos2> = Vec::new();
    let mut prev = first;
    if dist_to_segment(prev, from, to) > radius {
        run.push(prev);
    }
    for &p in &pts[1..] {
        if segment_segment_dist(prev, p, from, to) > radius {
            // 整段都在圈外:保留原始点,不重采样(没碰到的图形保持原样)
            run.push(p);
        } else {
            let d = p - prev;
            let steps = (d.length() / 1.0).ceil().max(1.0) as usize;
            for i in 1..=steps {
                let q = prev + d * (i as f32 / steps as f32);
                if dist_to_segment(q, from, to) > radius {
                    run.push(q);
                } else if !run.is_empty() {
                    if run.len() >= 2 {
                        runs.push(std::mem::take(&mut run));
                    } else {
                        run.clear();
                    }
                }
            }
        }
        prev = p;
    }
    if run.len() >= 2 {
        runs.push(run);
    }
    runs
}

/// 实心矩形:胶囊包围盒外的四条整块 + 包围盒内用扫描线抠出圆形缺口
fn emit_rect_fill(r: Rect, from: Pos2, to: Pos2, radius: f32, color: Color32, out: &mut Vec<Ink>) {
    let cap = Rect::from_two_pos(from, to).expand(radius);
    let hole = r.intersect(cap);
    if hole.width() <= 0.0 || hole.height() <= 0.0 {
        push_fill_rect(out, r.left(), r.right(), r.top(), r.bottom(), color);
        return;
    }
    if hole.top() > r.top() {
        push_fill_rect(out, r.left(), r.right(), r.top(), hole.top(), color);
    }
    if hole.bottom() < r.bottom() {
        push_fill_rect(out, r.left(), r.right(), hole.bottom(), r.bottom(), color);
    }
    if hole.left() > r.left() {
        push_fill_rect(out, r.left(), hole.left(), hole.top(), hole.bottom(), color);
    }
    if hole.right() < r.right() {
        push_fill_rect(out, hole.right(), r.right(), hole.top(), hole.bottom(), color);
    }
    let mut y = hole.top();
    while y < hole.bottom() {
        let y1 = (y + FILL_STRIP_H).min(hole.bottom());
        let ym = (y + y1) * 0.5;
        let cuts = capsule_cuts_at(ym, from, to, radius);
        for (x0, x1) in subtract_spans(&[(hole.left(), hole.right())], &cuts) {
            push_fill_rect(out, x0, x1, y, y1, color);
        }
        y = y1;
    }
}

/// 实心椭圆:整块用扫描线表示(椭圆边界是曲线),逐条抠掉圆形缺口
fn emit_ellipse_fill(
    c: Pos2,
    rad: Vec2,
    from: Pos2,
    to: Pos2,
    radius: f32,
    color: Color32,
    out: &mut Vec<Ink>,
) {
    if rad.x <= 0.05 || rad.y <= 0.05 {
        return;
    }
    let top = c.y - rad.y;
    let bottom = c.y + rad.y;
    let mut y = top;
    while y < bottom {
        let y1 = (y + FILL_STRIP_H).min(bottom);
        let ym = (y + y1) * 0.5;
        let dy = ((ym - c.y) / rad.y).clamp(-1.0, 1.0);
        let dx = rad.x * (1.0 - dy * dy).max(0.0).sqrt();
        if dx > 0.05 {
            let cuts = capsule_cuts_at(ym, from, to, radius);
            for (x0, x1) in subtract_spans(&[(c.x - dx, c.x + dx)], &cuts) {
                push_fill_rect(out, x0, x1, y, y1, color);
            }
        }
        y = y1;
    }
}

fn push_fill_rect(out: &mut Vec<Ink>, x0: f32, x1: f32, y0: f32, y1: f32, color: Color32) {
    if x1 - x0 > 0.05 && y1 - y0 > 0.05 {
        out.push(Ink::FillRect {
            a: Pos2::new(x0, y0),
            b: Pos2::new(x1, y1),
            color,
        });
    }
}

/// 圆笔刷(胶囊)在水平线 y 上覆盖的 x 区间:沿路径采样一串重叠圆盘后合并
fn capsule_cuts_at(y: f32, from: Pos2, to: Pos2, radius: f32) -> Vec<(f32, f32)> {
    let len = from.distance(to);
    let n = ((len / (radius * 0.5)).ceil() as usize).max(1);
    let mut spans: Vec<(f32, f32)> = Vec::with_capacity(n + 1);
    for i in 0..=n {
        let t = i as f32 / n as f32;
        let c = from + (to - from) * t;
        let dy = y - c.y;
        if dy.abs() < radius {
            let dx = (radius * radius - dy * dy).max(0.0).sqrt();
            spans.push((c.x - dx, c.x + dx));
        }
    }
    spans.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut merged: Vec<(f32, f32)> = Vec::with_capacity(spans.len());
    for (a, b) in spans {
        if b <= a {
            continue;
        }
        if let Some(last) = merged.last_mut() {
            if a <= last.1 {
                last.1 = last.1.max(b);
                continue;
            }
        }
        merged.push((a, b));
    }
    merged
}

fn subtract_spans(base: &[(f32, f32)], cuts: &[(f32, f32)]) -> Vec<(f32, f32)> {
    let mut out = Vec::new();
    for &(a, b) in base {
        let mut cur = a;
        for &(ca, cb) in cuts {
            if cb <= cur || ca >= b {
                continue;
            }
            if ca > cur {
                out.push((cur, ca.min(b)));
            }
            cur = cur.max(cb);
            if cur >= b {
                break;
            }
        }
        if cur < b {
            out.push((cur, b));
        }
    }
    out
}

/// 椭圆周长采样(闭合成环),擦除后作为折线绘制
fn ellipse_outline(c: Pos2, rad: Vec2) -> Vec<Pos2> {
    let rx = rad.x.abs();
    let ry = rad.y.abs();
    let peri =
        std::f32::consts::PI * (3.0 * (rx + ry) - ((3.0 * rx + ry) * (rx + 3.0 * ry)).sqrt());
    let n = ((peri / 2.0).ceil() as usize).clamp(48, 1024);
    let mut pts = Vec::with_capacity(n + 1);
    for i in 0..=n {
        let a = std::f32::consts::TAU * i as f32 / n as f32;
        pts.push(c + Vec2::new(a.cos() * rx, a.sin() * ry));
    }
    pts
}

fn point_in_ellipse(p: Pos2, c: Pos2, rad: Vec2) -> bool {
    if rad.x.abs() < 0.001 || rad.y.abs() < 0.001 {
        return false;
    }
    let dx = (p.x - c.x) / rad.x;
    let dy = (p.y - c.y) / rad.y;
    dx * dx + dy * dy <= 1.0
}

fn point_in_triangle(p: Pos2, tri: [Pos2; 3]) -> bool {
    let side = |a: Pos2, b: Pos2| (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x);
    let d1 = side(tri[0], tri[1]);
    let d2 = side(tri[1], tri[2]);
    let d3 = side(tri[2], tri[0]);
    !((d1 < 0.0 || d2 < 0.0 || d3 < 0.0) && (d1 > 0.0 || d2 > 0.0 || d3 > 0.0))
}

fn tri_hits_capsule(tri: [Pos2; 3], from: Pos2, to: Pos2, radius: f32) -> bool {
    for i in 0..3 {
        if segment_segment_dist(tri[i], tri[(i + 1) % 3], from, to) <= radius {
            return true;
        }
    }
    point_in_triangle(from, tri) || point_in_triangle(to, tri)
}

fn poly_hits_capsule(pts: &[Pos2], from: Pos2, to: Pos2, radius: f32) -> bool {
    if pts.len() == 3 {
        return tri_hits_capsule([pts[0], pts[1], pts[2]], from, to, radius);
    }
    (0..pts.len()).any(|i| {
        segment_segment_dist(pts[i], pts[(i + 1) % pts.len()], from, to) <= radius
    })
}

/// 线段到矩形的最近距离(线段在矩形内为 0)
fn segment_rect_dist(a: Pos2, b: Pos2, r: Rect) -> f32 {
    if r.contains(a) || r.contains(b) {
        return 0.0;
    }
    let edges = [
        (r.left_top(), r.right_top()),
        (r.right_top(), r.right_bottom()),
        (r.right_bottom(), r.left_bottom()),
        (r.left_bottom(), r.left_top()),
    ];
    edges
        .iter()
        .map(|&(p, q)| segment_segment_dist(a, b, p, q))
        .fold(f32::INFINITY, f32::min)
}

fn segment_segment_dist(a: Pos2, b: Pos2, c: Pos2, d: Pos2) -> f32 {
    if segments_intersect(a, b, c, d) {
        return 0.0;
    }
    dist_to_segment(a, c, d)
        .min(dist_to_segment(b, c, d))
        .min(dist_to_segment(c, a, b))
        .min(dist_to_segment(d, a, b))
}

fn segments_intersect(a: Pos2, b: Pos2, c: Pos2, d: Pos2) -> bool {
    let cross = |o: Pos2, p: Pos2, q: Pos2| {
        (p.x - o.x) * (q.y - o.y) - (p.y - o.y) * (q.x - o.x)
    };
    let d1 = cross(c, d, a);
    let d2 = cross(c, d, b);
    let d3 = cross(a, b, c);
    let d4 = cross(a, b, d);
    ((d1 > 0.0) != (d2 > 0.0)) && ((d3 > 0.0) != (d4 > 0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> Spec {
        Spec { color: Color32::RED, width: 4.0 }
    }

    fn erase_one(mut shapes: Vec<Ink>, from: Pos2, to: Pos2, radius: f32) -> Vec<Ink> {
        let mut canvas = Canvas::default();
        canvas.shapes = std::mem::take(&mut shapes);
        canvas.erase(from, to, radius);
        canvas.shapes
    }

    #[test]
    fn eraser_cuts_freehand_into_runs() {
        let pts = (0..=40).map(|i| Pos2::new(i as f32 * 10.0, 100.0)).collect();
        let out = erase_one(
            vec![Ink::Freehand { pts, spec: spec() }],
            Pos2::new(200.0, 100.0),
            Pos2::new(200.0, 100.0),
            30.0,
        );
        assert_eq!(out.len(), 2, "中间擦掉后应剩左右两段");
        for shape in &out {
            if let Ink::Freehand { pts, .. } = shape {
                assert!(pts.iter().all(|p| p.distance(Pos2::new(200.0, 100.0)) > 29.0));
            } else {
                panic!("应保持 Freehand");
            }
        }
    }

    #[test]
    fn eraser_splits_line_instead_of_deleting_it() {
        let out = erase_one(
            vec![Ink::Line {
                a: Pos2::new(0.0, 100.0),
                b: Pos2::new(400.0, 100.0),
                spec: spec(),
            }],
            Pos2::new(200.0, 100.0),
            Pos2::new(200.0, 100.0),
            30.0,
        );
        assert_eq!(out.len(), 2);
        let mut xs = vec![];
        for shape in &out {
            if let Ink::Line { a, b, .. } = shape {
                xs.push(a.x);
                xs.push(b.x);
            } else {
                panic!("应切成两条 Line");
            }
        }
        assert!(xs.iter().any(|x| *x <= 171.0), "左段应到圈边");
        assert!(xs.iter().any(|x| *x >= 229.0), "右段应从圈边开始");
    }

    #[test]
    fn eraser_keeps_shapes_outside_the_circle() {
        let rect = Ink::Rect {
            a: Pos2::new(100.0, 100.0),
            b: Pos2::new(300.0, 200.0),
            spec: spec(),
            filled: false,
        };
        let out = erase_one(vec![rect], Pos2::new(900.0, 900.0), Pos2::new(900.0, 900.0), 30.0);
        assert_eq!(out.len(), 1);
        assert!(matches!(out[0], Ink::Rect { .. }), "没碰到时应原样保留");
    }

    #[test]
    fn eraser_bites_rect_outline_only_where_circle_is() {
        let out = erase_one(
            vec![Ink::Rect {
                a: Pos2::new(100.0, 100.0),
                b: Pos2::new(300.0, 200.0),
                spec: spec(),
                filled: false,
            }],
            Pos2::new(100.0, 150.0),
            Pos2::new(100.0, 150.0),
            20.0,
        );
        assert!(out.iter().all(|s| !matches!(s, Ink::Rect { .. })), "被擦后应转为路径段");
        let right_kept = out.iter().any(|s| match s {
            Ink::Freehand { pts, .. } => pts.iter().any(|p| p.x > 290.0),
            _ => false,
        });
        assert!(right_kept, "另一边(右侧)应保留");
    }

    #[test]
    fn eraser_punches_hole_in_filled_rect() {
        let out = erase_one(
            vec![Ink::Rect {
                a: Pos2::new(100.0, 100.0),
                b: Pos2::new(300.0, 200.0),
                spec: spec(),
                filled: true,
            }],
            Pos2::new(200.0, 150.0),
            Pos2::new(200.0, 150.0),
            30.0,
        );
        let center = Pos2::new(200.0, 150.0);
        let mut pieces = 0;
        for shape in &out {
            if let Ink::FillRect { a, b, .. } = shape {
                pieces += 1;
                assert!(
                    !Rect::from_two_pos(*a, *b).contains(center),
                    "圈内不应留下填充碎片"
                );
            }
        }
        assert!(pieces >= 4, "圈外填充应保留(上下左右)");
        assert!(
            out.iter().any(|s| matches!(s, Ink::Rect { filled: false, .. })),
            "没被碰到的描边应保留"
        );
    }

    #[test]
    fn eraser_cuts_arrow_shaft_and_keeps_heads() {
        let out = erase_one(
            vec![Ink::Arrow {
                a: Pos2::new(100.0, 100.0),
                b: Pos2::new(300.0, 100.0),
                spec: spec(),
                double: false,
            }],
            Pos2::new(200.0, 100.0),
            Pos2::new(200.0, 100.0),
            20.0,
        );
        assert!(out.iter().all(|s| !matches!(s, Ink::Arrow { .. })));
        assert_eq!(
            out.iter().filter(|s| matches!(s, Ink::Line { .. })).count(),
            2,
            "箭杆应切成两段"
        );
        assert_eq!(
            out.iter().filter(|s| matches!(s, Ink::Poly { .. })).count(),
            1,
            "箭头应保留"
        );
    }

    #[test]
    fn eraser_removes_arrow_head_when_covered() {
        let out = erase_one(
            vec![Ink::Arrow {
                a: Pos2::new(100.0, 100.0),
                b: Pos2::new(300.0, 100.0),
                spec: spec(),
                double: false,
            }],
            Pos2::new(300.0, 100.0),
            Pos2::new(300.0, 100.0),
            20.0,
        );
        assert!(out.iter().all(|s| !matches!(s, Ink::Poly { .. })), "箭头被圈住应去掉");
        assert!(out.iter().any(|s| matches!(s, Ink::Line { .. })), "箭杆残余应保留");
    }

    #[test]
    fn eraser_cuts_ellipse_outline() {
        let out = erase_one(
            vec![Ink::Ellipse {
                a: Pos2::new(100.0, 100.0),
                b: Pos2::new(300.0, 200.0),
                spec: spec(),
                filled: false,
            }],
            Pos2::new(100.0, 150.0),
            Pos2::new(100.0, 150.0),
            20.0,
        );
        assert!(out.iter().all(|s| !matches!(s, Ink::Ellipse { .. })));
        assert!(out.iter().any(|s| matches!(s, Ink::Freehand { .. })));
    }
}

/// 给基础色加透明度(透明色列 = 马克笔效果)
pub fn with_alpha(c: Color32, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a)
}

fn ellipse_params(a: Pos2, b: Pos2) -> (Pos2, Vec2) {
    let center = Pos2::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
    let radius = Vec2::new((a.x - b.x).abs() / 2.0, (a.y - b.y).abs() / 2.0);
    (center, radius)
}

fn dist_to_segment(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let len2 = ab.length_sq();
    if len2 < 0.0001 {
        return p.distance(a);
    }
    let t = (((p - a).dot(ab)) / len2).clamp(0.0, 1.0);
    p.distance(a + ab * t)
}
