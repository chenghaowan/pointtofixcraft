//! 工具条图标:全部用 epaint 矢量绘制,不依赖系统字体的符号字形。

use eframe::egui::{
    epaint::CornerRadius, Align2, Color32, FontId, Painter, Pos2, Rect, Shape, Stroke, Vec2,
};

#[derive(Clone, Copy)]
pub enum Icon {
    /// 设置(齿轮)
    Settings,
    /// 笔宽档位 1..=4(圆点逐渐变大)
    Dot(u8),
    Pen,
    /// 橡皮擦
    Eraser,
    Line,
    Arrow,
    DoubleArrow,
    Rect,
    RectFilled,
    Ellipse,
    EllipseFilled,
    Text,
    /// 绿勾盖章工具
    CheckStamp,
    /// 红叉盖章工具
    CrossStamp,
    /// 时间章工具(时钟)
    TimeStamp,
    /// 签名章工具(印章)
    Signature,
    Region,
    /// 选区去背(背景变白)
    Debak,
    ZoomIn,
    ZoomOut,
    /// 撤销(逆时针回退)
    UndoArrow,
    /// 清空全部
    ClearAll,
    Print,
    Save,
    Copy,
}

const STROKE: f32 = 2.0;

pub fn draw(p: &Painter, icon: Icon, rect: Rect, color: Color32, bg: Color32) {
    let c = rect.center();
    let h = rect.width().min(rect.height()) * 0.42;
    let stroke = Stroke::new(STROKE, color);
    match icon {
        Icon::Settings => {
            // 齿轮:内圈 + 8 根放射齿
            let r = h * 0.66;
            p.circle_stroke(c, r, Stroke::new(1.7, color));
            p.circle_stroke(c, h * 0.26, Stroke::new(1.4, color));
            for i in 0..8 {
                let a = i as f32 * std::f32::consts::TAU / 8.0;
                let d = Vec2::new(a.cos(), a.sin());
                p.line_segment(
                    [c + d * (r + 0.6), c + d * (r + h * 0.4)],
                    Stroke::new(2.6, color),
                );
            }
        }
        Icon::Dot(level) => {
            let r = match level {
                1 => 0.22,
                2 => 0.38,
                3 => 0.58,
                _ => 0.8,
            };
            p.circle_filled(c, h * r, color);
        }
        Icon::Pen => {
            // 笔杆(斜向矩形)+ 实心笔尖,一眼能区别于“直线”
            let dir = Vec2::new(1.0, -1.0).normalized();
            let perp = Vec2::new(1.0, 1.0).normalized();
            let half_w = h * 0.2;
            let tip = c + Vec2::new(-1.0, 1.0) * h;
            let shoulder = tip + dir * (h * 0.62);
            let s1 = shoulder + perp * half_w;
            let s2 = shoulder - perp * half_w;
            let e1 = shoulder + dir * (h * 1.55) + perp * half_w;
            let e2 = shoulder + dir * (h * 1.55) - perp * half_w;
            p.add(Shape::closed_line(vec![s1, e1, e2, s2], Stroke::new(1.8, color)));
            p.add(Shape::convex_polygon(vec![tip, s1, s2], color, Stroke::NONE));
            // 笔杆上的笔夹/箍环
            let band = shoulder + dir * (h * 0.2);
            p.line_segment(
                [band + perp * half_w, band - perp * half_w],
                Stroke::new(1.2, color),
            );
        }
        Icon::Eraser => {
            // 倾斜橡皮块(线稿):轮廓 + 橡皮套分界 + 下方擦除弧线
            let ang = -40f32.to_radians();
            let u = Vec2::new(ang.cos(), ang.sin());
            let v = Vec2::new(-ang.sin(), ang.cos());
            let corner = |uu: f32, vv: f32| c + u * (h * uu) + v * (h * vv);
            p.add(Shape::closed_line(
                vec![
                    corner(-0.95, -0.45),
                    corner(0.95, -0.45),
                    corner(0.95, 0.45),
                    corner(-0.95, 0.45),
                ],
                Stroke::new(1.8, color),
            ));
            p.line_segment(
                [corner(-0.35, -0.45), corner(-0.35, 0.45)],
                Stroke::new(1.5, color),
            );
            p.add(eframe::epaint::QuadraticBezierShape::from_points_stroke(
                [
                    c + Vec2::new(-h * 0.2, h * 0.95),
                    c + Vec2::new(h * 0.25, h * 1.08),
                    c + Vec2::new(h * 0.85, h * 0.8),
                ],
                false,
                Color32::TRANSPARENT,
                Stroke::new(1.5, color),
            ));
        }
        Icon::Debak => {
            // 魔术棒:斜杆 + 实心菱形棒尖 + 小十字火花(去背=魔棒)
            let a = Pos2::new(c.x - h * 0.85, c.y + h * 0.85);
            let b = Pos2::new(c.x + h * 0.38, c.y - h * 0.38);
            p.line_segment([a, b], Stroke::new(2.0, color));
            let tip = Pos2::new(c.x + h * 0.55, c.y - h * 0.55);
            let r = h * 0.42;
            p.add(Shape::convex_polygon(
                vec![
                    tip + Vec2::new(0.0, -r),
                    tip + Vec2::new(r, 0.0),
                    tip + Vec2::new(0.0, r),
                    tip + Vec2::new(-r, 0.0),
                ],
                color,
                Stroke::NONE,
            ));
            let s = Pos2::new(c.x - h * 0.45, c.y - h * 0.8);
            let arm = h * 0.2;
            p.line_segment(
                [s + Vec2::new(-arm, 0.0), s + Vec2::new(arm, 0.0)],
                Stroke::new(1.6, color),
            );
            p.line_segment(
                [s + Vec2::new(0.0, -arm), s + Vec2::new(0.0, arm)],
                Stroke::new(1.6, color),
            );
        }
        Icon::Line => {
            p.line_segment([Pos2::new(c.x - h, c.y + h), Pos2::new(c.x + h, c.y - h)], stroke);
        }
        Icon::Arrow => {
            let a = Pos2::new(c.x - h, c.y + h);
            let b = Pos2::new(c.x + h, c.y - h);
            let base = arrow_head(p, a, b, h, color);
            p.line_segment([a, base], stroke);
        }
        Icon::DoubleArrow => {
            let a = Pos2::new(c.x - h, c.y + h);
            let b = Pos2::new(c.x + h, c.y - h);
            let base_b = arrow_head(p, a, b, h, color);
            let base_a = arrow_head(p, b, a, h, color);
            p.line_segment([base_a, base_b], stroke);
        }
        Icon::Rect => {
            p.rect_stroke(
                rect.shrink(h * 0.45),
                0.0,
                stroke,
                eframe::epaint::StrokeKind::Inside,
            );
        }
        Icon::RectFilled => {
            p.rect_filled(rect.shrink(h * 0.45), 0.0, color);
        }
        Icon::Ellipse => {
            p.add(eframe::epaint::EllipseShape::stroke(
                c,
                Vec2::new(h * 0.95, h * 0.65),
                stroke,
            ));
        }
        Icon::EllipseFilled => {
            p.add(eframe::epaint::EllipseShape::filled(
                c,
                Vec2::new(h * 0.95, h * 0.65),
                color,
            ));
        }
        Icon::Text => {
            p.text(c, Align2::CENTER_CENTER, "A", FontId::proportional(rect.height() * 0.95), color);
        }
        Icon::CheckStamp => {
            // 粗壮的绿色对勾(圆头笔画)
            let green = Color32::from_rgb(34, 197, 94);
            let hh = h * 1.15;
            let a = Pos2::new(c.x - hh * 0.9, c.y + hh * 0.05);
            let m = Pos2::new(c.x - hh * 0.15, c.y + hh * 0.75);
            let b = Pos2::new(c.x + hh * 1.0, c.y - hh * 0.75);
            p.line_segment([a, m], Stroke::new(3.4, green));
            p.line_segment([m, b], Stroke::new(3.4, green));
            for pt in [a, m, b] {
                p.circle_filled(pt, 1.7, green);
            }
        }
        Icon::CrossStamp => {
            // 粗壮的红色叉(圆头笔画)
            let red = Color32::from_rgb(220, 38, 38);
            let hh = h * 1.15;
            let d1a = Pos2::new(c.x - hh, c.y - hh);
            let d1b = Pos2::new(c.x + hh, c.y + hh);
            let d2a = Pos2::new(c.x + hh, c.y - hh);
            let d2b = Pos2::new(c.x - hh, c.y + hh);
            for (pa, pb) in [(d1a, d1b), (d2a, d2b)] {
                p.line_segment([pa, pb], Stroke::new(3.4, red));
                p.circle_filled(pa, 1.7, red);
                p.circle_filled(pb, 1.7, red);
            }
        }
        Icon::TimeStamp => {
            // 时钟:表圈 + 时针(10 点方向)+ 分针(12 点方向)+ 12 点刻度
            p.circle_stroke(c, h * 0.95, Stroke::new(1.9, color));
            p.line_segment(
                [Pos2::new(c.x, c.y - h * 0.95), Pos2::new(c.x, c.y - h * 0.62)],
                Stroke::new(1.9, color),
            );
            p.line_segment(
                [c, Pos2::new(c.x - h * 0.34, c.y + h * 0.1)],
                Stroke::new(2.0, color),
            );
            p.line_segment(
                [c, Pos2::new(c.x + h * 0.08, c.y - h * 0.58)],
                Stroke::new(2.0, color),
            );
            p.circle_filled(c, 1.3, color);
        }
        Icon::Signature => {
            // 印章:圆角方框 + 抽象印文(左列"横+竖"、右列"竖+底横"),红色描边
            let red = Color32::from_rgb(220, 38, 38);
            let r = Rect::from_center_size(c, Vec2::splat(h * 1.9));
            p.rect_stroke(
                r,
                CornerRadius::same(3),
                Stroke::new(2.2, red),
                eframe::epaint::StrokeKind::Inside,
            );
            let ink = Stroke::new(1.8, red);
            // 左列:顶横 + 中竖
            p.line_segment(
                [
                    Pos2::new(c.x - h * 0.55, c.y - h * 0.45),
                    Pos2::new(c.x + h * 0.05, c.y - h * 0.45),
                ],
                ink,
            );
            p.line_segment(
                [
                    Pos2::new(c.x - h * 0.25, c.y - h * 0.45),
                    Pos2::new(c.x - h * 0.25, c.y + h * 0.5),
                ],
                ink,
            );
            // 右列:竖 + 底横(折笔)
            p.line_segment(
                [
                    Pos2::new(c.x + h * 0.3, c.y - h * 0.5),
                    Pos2::new(c.x + h * 0.3, c.y + h * 0.2),
                ],
                ink,
            );
            p.line_segment(
                [
                    Pos2::new(c.x + h * 0.3, c.y + h * 0.5),
                    Pos2::new(c.x - h * 0.05, c.y + h * 0.5),
                ],
                ink,
            );
        }
        Icon::Region => {
            let r = rect.shrink(h * 0.5);
            dashed_segment(p, Pos2::new(r.left(), r.top()), Pos2::new(r.right(), r.top()), color);
            dashed_segment(p, Pos2::new(r.right(), r.top()), Pos2::new(r.right(), r.bottom()), color);
            dashed_segment(p, Pos2::new(r.right(), r.bottom()), Pos2::new(r.left(), r.bottom()), color);
            dashed_segment(p, Pos2::new(r.left(), r.bottom()), Pos2::new(r.left(), r.top()), color);
        }
        Icon::ZoomIn | Icon::ZoomOut => {
            // 放大镜 + / −
            let lens = Pos2::new(c.x - h * 0.12, c.y - h * 0.12);
            let r = h * 0.62;
            p.circle_stroke(lens, r, Stroke::new(1.9, color));
            let diag = Vec2::new(0.707, 0.707);
            p.line_segment(
                [lens + diag * r, lens + diag * (r + h * 0.55)],
                Stroke::new(2.4, color),
            );
            let arm = r * 0.5;
            p.line_segment(
                [
                    lens + Vec2::new(-arm, 0.0),
                    lens + Vec2::new(arm, 0.0),
                ],
                Stroke::new(1.8, color),
            );
            if matches!(icon, Icon::ZoomIn) {
                p.line_segment(
                    [
                        lens + Vec2::new(0.0, -arm),
                        lens + Vec2::new(0.0, arm),
                    ],
                    Stroke::new(1.8, color),
                );
            }
        }
        Icon::UndoArrow => {
            // ↩ 形回退箭头:左向实心箭头 + 立柱(比弧线更小尺寸也清晰)
            let tip = Pos2::new(c.x - h * 0.95, c.y - h * 0.25);
            p.add(Shape::convex_polygon(
                vec![
                    tip,
                    Pos2::new(c.x - h * 0.45, c.y - h * 0.55),
                    Pos2::new(c.x - h * 0.45, c.y + h * 0.05),
                ],
                color,
                Stroke::NONE,
            ));
            p.line_segment(
                [
                    Pos2::new(c.x - h * 0.5, c.y - h * 0.25),
                    Pos2::new(c.x + h * 0.55, c.y - h * 0.25),
                ],
                Stroke::new(2.0, color),
            );
            p.line_segment(
                [
                    Pos2::new(c.x + h * 0.55, c.y - h * 0.25),
                    Pos2::new(c.x + h * 0.55, c.y + h * 0.65),
                ],
                Stroke::new(2.0, color),
            );
        }
        Icon::ClearAll => {
            // 垃圾桶:提手 + 盖线 + 梯形桶身 + 两道竖线
            p.add(Shape::line(
                vec![
                    Pos2::new(c.x - h * 0.32, c.y - h * 0.55),
                    Pos2::new(c.x - h * 0.26, c.y - h * 0.8),
                    Pos2::new(c.x + h * 0.26, c.y - h * 0.8),
                    Pos2::new(c.x + h * 0.32, c.y - h * 0.55),
                ],
                Stroke::new(1.6, color),
            ));
            p.line_segment(
                [
                    Pos2::new(c.x - h * 0.82, c.y - h * 0.55),
                    Pos2::new(c.x + h * 0.82, c.y - h * 0.55),
                ],
                Stroke::new(1.8, color),
            );
            p.add(Shape::closed_line(
                vec![
                    Pos2::new(c.x - h * 0.62, c.y - h * 0.42),
                    Pos2::new(c.x + h * 0.62, c.y - h * 0.42),
                    Pos2::new(c.x + h * 0.48, c.y + h * 0.9),
                    Pos2::new(c.x - h * 0.48, c.y + h * 0.9),
                ],
                Stroke::new(1.8, color),
            ));
            for dx in [-0.2f32, 0.2] {
                p.line_segment(
                    [
                        Pos2::new(c.x + h * dx, c.y - h * 0.2),
                        Pos2::new(c.x + h * dx * 0.8, c.y + h * 0.65),
                    ],
                    Stroke::new(1.3, color),
                );
            }
        }
        Icon::Print => {
            // 打印机:纸张在机身后面,机身用按钮底色遮挡纸张下半部分
            let paper = Rect::from_min_max(
                Pos2::new(c.x - h * 0.65, c.y - h),
                Pos2::new(c.x + h * 0.65, c.y - h * 0.3),
            );
            p.rect_filled(paper, CornerRadius::same(1), bg);
            p.rect_stroke(
                paper,
                CornerRadius::same(1),
                Stroke::new(1.4, color),
                eframe::epaint::StrokeKind::Inside,
            );
            let body = Rect::from_min_max(
                Pos2::new(c.x - h, c.y - h * 0.5),
                Pos2::new(c.x + h, c.y + h * 0.35),
            );
            p.rect_filled(body, CornerRadius::same(2), bg);
            p.rect_stroke(
                body,
                CornerRadius::same(2),
                Stroke::new(1.8, color),
                eframe::epaint::StrokeKind::Inside,
            );
            // 进纸口
            p.line_segment(
                [
                    Pos2::new(c.x - h * 0.62, c.y - h * 0.2),
                    Pos2::new(c.x + h * 0.62, c.y - h * 0.2),
                ],
                Stroke::new(1.4, color),
            );
            // 出纸(在机身前面)
            let out = Rect::from_min_max(
                Pos2::new(c.x - h * 0.6, c.y + h * 0.28),
                Pos2::new(c.x + h * 0.6, c.y + h * 0.9),
            );
            p.rect_filled(out, CornerRadius::same(1), bg);
            p.rect_stroke(
                out,
                CornerRadius::same(1),
                Stroke::new(1.4, color),
                eframe::epaint::StrokeKind::Inside,
            );
            p.line_segment(
                [
                    Pos2::new(c.x - h * 0.3, c.y + h * 0.56),
                    Pos2::new(c.x + h * 0.3, c.y + h * 0.56),
                ],
                Stroke::new(1.1, color),
            );
        }
        Icon::Save => {
            // 软盘:方轮廓 + 实心金属滑盖 + 下方标签(白线=标签文字)
            let x0 = c.x - h;
            let x1 = c.x + h;
            let y0 = c.y - h;
            let y1 = c.y + h;
            p.rect_stroke(
                Rect::from_min_max(Pos2::new(x0, y0), Pos2::new(x1, y1)),
                CornerRadius::same(1),
                Stroke::new(1.8, color),
                eframe::epaint::StrokeKind::Inside,
            );
            p.rect_filled(
                Rect::from_min_max(
                    Pos2::new(c.x - h * 0.4, y0 + h * 0.3),
                    Pos2::new(c.x + h * 0.4, y0 + h * 0.58),
                ),
                CornerRadius::same(1),
                color,
            );
            let label = Rect::from_min_max(
                Pos2::new(c.x - h * 0.62, y1 - h * 0.6),
                Pos2::new(c.x + h * 0.62, y1 - h * 0.32),
            );
            p.rect_filled(label, CornerRadius::same(1), color);
            p.line_segment(
                [
                    Pos2::new(c.x - h * 0.36, y1 - h * 0.46),
                    Pos2::new(c.x + h * 0.36, y1 - h * 0.46),
                ],
                Stroke::new(1.0, Color32::WHITE),
            );
        }
        Icon::Copy => {
            // 两页纸:后页在左上,前页用按钮底色盖住重叠的角
            let back = Rect::from_min_max(
                Pos2::new(c.x - h, c.y - h),
                Pos2::new(c.x + h * 0.35, c.y + h * 0.35),
            );
            let front = Rect::from_min_max(
                Pos2::new(c.x - h * 0.35, c.y - h * 0.35),
                Pos2::new(c.x + h, c.y + h),
            );
            p.rect_stroke(
                back,
                CornerRadius::same(1),
                Stroke::new(1.5, color),
                eframe::epaint::StrokeKind::Inside,
            );
            p.rect_filled(front, CornerRadius::same(1), bg);
            p.rect_stroke(
                front,
                CornerRadius::same(1),
                Stroke::new(1.6, color),
                eframe::epaint::StrokeKind::Inside,
            );
        }
    }
}

/// 画箭头三角形,返回箭杆应结束的位置(缩进到箭头底部,保证尖端是尖的)
fn arrow_head(p: &Painter, from: Pos2, tip: Pos2, h: f32, color: Color32) -> Pos2 {
    let d = tip - from;
    let len = d.length();
    if len < 0.001 {
        return tip;
    }
    let dir = d / len;
    let base = tip - dir * (h * 0.8);
    let perp = Vec2::new(-dir.y, dir.x);
    let half = h * 0.45;
    p.add(eframe::epaint::Shape::convex_polygon(
        vec![tip, base + perp * half, base - perp * half],
        color,
        Stroke::NONE,
    ));
    base
}

fn dashed_segment(p: &Painter, a: Pos2, b: Pos2, color: Color32) {
    let d = b - a;
    let n = 3;
    for i in 0..n {
        let t0 = i as f32 / n as f32;
        let t1 = t0 + 0.55 / n as f32;
        p.line_segment([a + d * t0, a + d * t1], Stroke::new(STROKE, color));
    }
}
