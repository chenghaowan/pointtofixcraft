"""生成 PointToFix 图标:Pointofix 风格四色圆盘 + 红色马克笔 + 鼠标指针,右下角 Rust 齿轮角标。

用法: python scripts/make_icon.py
输出: assets/pointtofixcraft.png (256x256) 与 assets/app.ico (多尺寸)
"""

from pathlib import Path

from PIL import Image, ImageDraw, ImageFont, ImageOps

ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "assets"
S = 1024  # 先在 4x 尺寸绘制,再降采样得到平滑边缘

BLUE = ((89, 174, 240), (146, 207, 251))
GREEN = ((144, 212, 144), (188, 236, 188))
YELLOW = ((255, 201, 145), (255, 235, 183))
GRAY = ((192, 192, 192), (224, 224, 224))
MARKER = (214, 38, 52, 205)
MARKER_DOT = (243, 37, 29, 255)
RUST_ORANGE = (211, 69, 22, 255)


def radial_circle(diameter: int, dark, light) -> Image.Image:
    """径向渐变圆:中心 light,边缘 dark。"""
    grad = Image.radial_gradient("L").resize((diameter, diameter), Image.Resampling.LANCZOS)
    body = ImageOps.colorize(grad, black=light, white=dark).convert("RGBA")
    mask = Image.new("L", (diameter, diameter), 0)
    ImageDraw.Draw(mask).ellipse((0, 0, diameter - 1, diameter - 1), fill=255)
    body.putalpha(mask)
    return body


def draw_gear(draw: ImageDraw.ImageDraw, center, outer: float, root: float, teeth: int = 12):
    import math

    def smoothstep(t: float) -> float:
        t = min(max(t, 0.0), 1.0)
        return t * t * (3.0 - 2.0 * t)

    cx, cy = center
    pts = []
    steps = 1440
    pitch = 2 * math.pi / teeth
    for i in range(steps):
        a = 2 * math.pi * i / steps
        phase = (a % pitch) / pitch
        if phase < 0.08:
            k = smoothstep(phase / 0.08)
        elif phase < 0.46:
            k = 1.0
        elif phase < 0.54:
            k = 1.0 - smoothstep((phase - 0.46) / 0.08)
        else:
            k = 0.0
        r = root + (outer - root) * k
        pts.append((cx + math.cos(a) * r, cy + math.sin(a) * r))
    draw.polygon(pts, fill=RUST_ORANGE)


def build() -> Image.Image:
    canvas = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    r = S // 4
    overlap = 10  # 圆盘略微重叠,避免降采样后在接缝处透出白线
    centers = [(r, r), (3 * r, r), (r, 3 * r), (3 * r, 3 * r)]
    for (cx, cy), (dark, light) in zip(centers, (BLUE, GREEN, YELLOW, GRAY)):
        d = 2 * r + overlap
        disc = radial_circle(d, dark, light)
        canvas.alpha_composite(disc, (cx - d // 2, cy - d // 2))

    layer = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    d = ImageDraw.Draw(layer)
    # 红色马克笔:从左上角斜向中心
    w = int(0.165 * S)
    d.line([(0.01 * S, 0.01 * S), (0.50 * S, 0.50 * S)], fill=MARKER, width=w)
    for p in ((0.01 * S, 0.01 * S), (0.50 * S, 0.50 * S)):
        d.ellipse((p[0] - w / 2, p[1] - w / 2, p[0] + w / 2, p[1] + w / 2), fill=MARKER)
    # 笔尖红点
    dr = 0.085 * S
    cx = cy = 0.505 * S
    d.ellipse((cx - dr, cy - dr, cx + dr, cy + dr), fill=MARKER_DOT)
    canvas.alpha_composite(layer)

    # 鼠标指针(经典 NW 箭头,白填充黑描边)
    pts = [
        (0.530, 0.500),
        (0.530, 0.950),
        (0.635, 0.830),
        (0.720, 0.970),
        (0.785, 0.940),
        (0.700, 0.800),
        (0.850, 0.800),
    ]
    pts = [(x * S, y * S) for x, y in pts]
    cursor = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    cd = ImageDraw.Draw(cursor)
    cd.polygon(pts, fill=(255, 255, 255, 255), outline=(20, 20, 20, 255), width=int(0.013 * S))
    canvas.alpha_composite(cursor)

    # 右下角 Rust 角标:白圈 + 橙色齿轮 + 白色 R
    bx = by = 0.855 * S
    badge = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    bd = ImageDraw.Draw(badge)
    ring = 0.138 * S
    bd.ellipse(
        (bx - ring, by - ring, bx + ring, by + ring),
        fill=(255, 255, 255, 255),
        outline=(150, 150, 150, 190),
        width=4,
    )
    draw_gear(bd, (bx, by), outer=0.124 * S, root=0.106 * S)
    font_path = Path("C:/Windows/Fonts/seguibl.ttf")
    if not font_path.exists():
        font_path = Path("C:/Windows/Fonts/arialbd.ttf")
    font = ImageFont.truetype(str(font_path), int(0.19 * S))
    bd.text((bx, by + 0.004 * S), "R", font=font, fill=(255, 255, 255, 255), anchor="mm")
    canvas.alpha_composite(badge)
    return canvas


def main():
    ASSETS.mkdir(exist_ok=True)
    master = build()
    icon256 = master.resize((256, 256), Image.Resampling.LANCZOS)
    icon256.save(ASSETS / "pointtofixcraft.png")
    icon256.save(
        ASSETS / "app.ico",
        format="ICO",
        sizes=[(16, 16), (20, 20), (24, 24), (32, 32), (40, 40), (48, 48), (64, 64), (128, 128), (256, 256)],
    )
    print("wrote", ASSETS / "pointtofixcraft.png")
    print("wrote", ASSETS / "app.ico")


if __name__ == "__main__":
    main()
