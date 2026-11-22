"""生成 RunAny_RS 的应用图标（R + A 字母标）。

思路：几何绘制（不是 AI 生图），所以 16px 下也是干净的 —— 应用图标最怕在
小尺寸糊成一团。做法是 4 倍超采样再缩小，边缘自带抗锯齿。

用法：python tools/make_icon.py
产出：assets/icon.png（1024）、assets/icon.ico（多尺寸）
"""

from PIL import Image, ImageDraw, ImageFont, ImageFilter
import os

# ---- 配色：跟软件默认主色（0x2f6feb）一家的蓝，右下压深一点做出体积感
TOP = (86, 148, 255)      # #5694FF
BOTTOM = (26, 71, 186)    # #1A47BA
WHITE = (255, 255, 255)

S = 4096                   # 超采样画布
MARGIN = int(S * 0.055)    # 圆角方块四周留一点透明边（Windows 图标习惯）
RADIUS = int(S * 0.215)

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
OUT_PNG = os.path.join(ROOT, "assets", "icon.png")
OUT_ICO = os.path.join(ROOT, "assets", "icon.ico")


def gradient(size, top, bottom):
    """竖直渐变（斜着会显得脏，图标还是干净点好）。"""
    img = Image.new("RGB", (1, size))
    px = img.load()
    for y in range(size):
        t = y / max(1, size - 1)
        px[0, y] = (
            round(top[0] + (bottom[0] - top[0]) * t),
            round(top[1] + (bottom[1] - top[1]) * t),
            round(top[2] + (bottom[2] - top[2]) * t),
        )
    return img.resize((size, size), Image.BICUBIC)


def rounded_mask(size, inset, radius):
    mask = Image.new("L", (size, size), 0)
    d = ImageDraw.Draw(mask)
    d.rounded_rectangle([inset, inset, size - 1 - inset, size - 1 - inset],
                        radius=radius, fill=255)
    return mask


def glyph(ch, font_path, px):
    """把单个字母渲染出来，并**裁到字形本身的包围盒**。

    PIL 的 `text()` 坐标是「行首左上角」，拿它算视觉居中很容易算歪（第一版就是
    字掉到底下去了）。裁到 bbox 之后只剩字形本身，摆位置就是纯几何，不会错。
    """
    f = ImageFont.truetype(font_path, px)
    pad = px
    tmp = Image.new("RGBA", (px * 2 + pad * 2, px * 2 + pad * 2), (0, 0, 0, 0))
    ImageDraw.Draw(tmp).text((pad, pad), ch, font=f, fill=(255, 255, 255, 255))
    box = tmp.getbbox()
    if box is None:
        raise RuntimeError(f"字形 {ch} 没画出来")
    return tmp.crop(box)


def draw_letters(img, box_w_ratio=0.76):
    """把 RA 两个字画上去。

    字距手工收紧（字体自带的 tracking 对字母标太松），A 稍微小一点点、低一点点 ——
    两个同高同宽的粗字母会像缩写牌，错开一点才像标。
    """
    font_path = "C:/Windows/Fonts/segoeuib.ttf"
    if not os.path.exists(font_path):
        font_path = "C:/Windows/Fonts/arialbd.ttf"

    g_r = glyph("R", font_path, 1400)
    g_a = glyph("A", font_path, 1400)

    # A 矮一点：视觉上更稳，也不会跟 R 抢
    a_h = int(g_a.height * 0.93)
    g_a = g_a.resize((int(g_a.width * a_h / g_a.height), a_h), Image.LANCZOS)

    gap = int(g_r.height * 0.01)            # 只留一丝缝：挨着但不粘
    total_w = g_r.width + gap + g_a.width
    target = int(S * box_w_ratio)
    scale = target / total_w
    g_r = g_r.resize((int(g_r.width * scale), int(g_r.height * scale)), Image.LANCZOS)
    g_a = g_a.resize((int(g_a.width * scale), int(g_a.height * scale)), Image.LANCZOS)
    gap = int(gap * scale)
    total_w = g_r.width + gap + g_a.width

    layer = Image.new("RGBA", (total_w, max(g_r.height, g_a.height)), (0, 0, 0, 0))
    mid_y = layer.height // 2
    layer.alpha_composite(g_r, (0, mid_y - g_r.height // 2))
    layer.alpha_composite(g_a, (g_r.width + gap, mid_y - g_a.height // 2 + int(S * 0.008)))

    # **不做投影**：白字压在蓝底上对比已经够，加一层灰边反而在 16/32px 下
    # 把笔画糊在一起（试过一版带投影的，小图标一看就是脏的）。
    x = (S - layer.width) // 2
    y = (S - layer.height) // 2 - int(S * 0.010)
    img.alpha_composite(layer, (x, y))


def main():
    # 1) 底：渐变 + 圆角
    base = gradient(S, TOP, BOTTOM).convert("RGBA")
    mask = rounded_mask(S, MARGIN, RADIUS)

    # 顶部一道很淡的高光，避免整块死平
    gloss = Image.new("L", (S, S), 0)
    gd = ImageDraw.Draw(gloss)
    gd.ellipse([-S * 0.3, -S * 0.9, S * 1.3, S * 0.42], fill=70)
    gloss = gloss.filter(ImageFilter.GaussianBlur(S * 0.05))
    white_layer = Image.new("RGBA", (S, S), (255, 255, 255, 0))
    white_layer.putalpha(gloss)

    tile = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    tile.paste(base, (0, 0), mask)
    tile.alpha_composite(Image.composite(
        white_layer, Image.new("RGBA", (S, S), (0, 0, 0, 0)), mask))

    # 2) 字
    draw_letters(tile)

    # 3) 收边：整体再蒙一遍圆角，把字的溢出切掉
    final = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    final.paste(tile, (0, 0), mask)

    # 4) 缩到 1024 出 png；ico 直接让 PIL 按多尺寸写
    png = final.resize((1024, 1024), Image.LANCZOS)
    os.makedirs(os.path.dirname(OUT_PNG), exist_ok=True)
    png.save(OUT_PNG)

    sizes = [(256, 256), (128, 128), (64, 64), (48, 48), (32, 32), (24, 24), (16, 16)]
    png.save(OUT_ICO, format="ICO", sizes=sizes)
    print("wrote", OUT_PNG)
    print("wrote", OUT_ICO)


if __name__ == "__main__":
    main()
