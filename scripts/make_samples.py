#!/usr/bin/env python3
"""
Generate synthetic 梦幻西游手游 screenshots that mimic the in-game 人物属性,
装备 and 背包 panels. These are used to exercise the OCR + feature pipeline
end-to-end without shipping copyrighted game art.

Outputs (into <outdir>, default `samples`):
  character_haitaotao.png   3-char nickname, full stat block
  character_yunzhonghe.png  3-char nickname, full stat block
  character_xiaolingdang.png 3-char nickname, full stat block
  character_qingfengxulai.png 4-char nickname variant
  equipment.png             装备 panel: all eight slots
  inventory.png             背包 grid with stack counts

Usage:
    python scripts/make_samples.py [outdir]
"""
from __future__ import annotations

import os
import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

# Resolve a CJK font that actually exists on this machine.
FONT_CANDIDATES = [
    r"C:\Windows\Fonts\msyh.ttc",
    r"C:\Windows\Fonts\msyhbd.ttc",
    r"C:\Windows\Fonts\simhei.ttf",
    r"C:\Windows\Fonts\Deng.ttf",
    r"C:\Windows\Fonts\simsun.ttc",
]


def load_font(size: int, bold: bool = False) -> ImageFont.FreeTypeFont:
    for path in FONT_CANDIDATES:
        if os.path.exists(path):
            try:
                index = 0
                if path.endswith("msyhbd.ttc") or bold:
                    index = 1 if path.endswith(".ttc") else 0
                return ImageFont.truetype(path, size, index=index)
            except Exception:
                continue
    return ImageFont.load_default()


# Theme colours lifted from the design spec.
BG = (24, 26, 32)
PANEL = (38, 41, 50)
PANEL_LIGHT = (48, 52, 63)
TITLE = (247, 199, 96)
LABEL = (170, 178, 194)
VALUE = (240, 243, 250)
HP = (226, 82, 66)
MP = (66, 150, 236)
GOLD = (246, 184, 43)


def make_attributes(nickname: str, game_id: str, level: int, out: Path) -> None:
    img = Image.new("RGB", (1280, 800), BG)
    d = ImageDraw.Draw(img)

    # Header: nickname / level / 游戏ID
    d.rounded_rectangle([40, 30, 1240, 170], radius=16, fill=PANEL_LIGHT)
    d.text((110, 52), nickname, font=load_font(40, True), fill=TITLE)
    d.text((110, 108), f"Lv.{level}", font=load_font(26), fill=(236, 240, 248))
    d.text((330, 112), f"游戏ID {game_id}", font=load_font(24), fill=LABEL)
    d.text((330, 52), "门派：大唐官府", font=load_font(24), fill=LABEL)

    # HP / MP bars with values
    def bar(y: int, label: str, cur: int, mx: int, colour) -> None:
        d.text((60, y), label, font=load_font(26, True), fill=LABEL)
        d.rounded_rectangle([170, y + 4, 900, y + 34], radius=15, fill=(20, 22, 28))
        frac = cur / mx
        d.rounded_rectangle([170, y + 4, 170 + int(730 * frac), y + 34], radius=15, fill=colour)
        text = f"{cur:,} / {mx:,}"
        tw = d.textlength(text, font=load_font(21, True))
        # Keep the label fully inside the bar so OCR sees it against the fill.
        d.text((890 - tw, y + 7), text, font=load_font(21, True), fill=(255, 255, 255))

    bar(190, "气血", 7721, 7721, HP)
    bar(244, "魔法", 2215, 2215, MP)

    # Combat stats, two columns like the game's attribute screen.
    left = [
        ("物理伤害", "758"),
        ("法术伤害", "582"),
        ("速度", "892"),
        ("治疗强度", "408"),
    ]
    right = [
        ("物理防御", "1,714"),
        ("法术防御", "948"),
        ("封印命中", "512"),
        ("抵抗封印", "486"),
    ]
    d.rounded_rectangle([40, 300, 620, 680], radius=14, fill=PANEL)
    d.rounded_rectangle([660, 300, 1240, 680], radius=14, fill=PANEL)
    d.text((70, 322), "战斗属性", font=load_font(24, True), fill=TITLE)
    d.text((690, 322), "防御属性", font=load_font(24, True), fill=TITLE)
    f_l = load_font(23)
    f_v = load_font(25, True)
    for i, (label, value) in enumerate(left):
        ry = 380 + i * 70
        d.text((70, ry), label, font=f_l, fill=LABEL)
        tw = d.textlength(value, font=f_v)
        d.text((590 - tw, ry - 1), value, font=f_v, fill=VALUE)
    for i, (label, value) in enumerate(right):
        ry = 380 + i * 70
        d.text((690, ry), label, font=f_l, fill=LABEL)
        tw = d.textlength(value, font=f_v)
        d.text((1210 - tw, ry - 1), value, font=f_v, fill=VALUE)

    # Currencies
    d.rounded_rectangle([40, 710, 1240, 780], radius=12, fill=PANEL_LIGHT)
    d.text((70, 728), "金币", font=load_font(24, True), fill=GOLD)
    d.text((200, 728), "2,942,685", font=load_font(25, True), fill=VALUE)
    d.text((640, 728), "银币", font=load_font(24, True), fill=(150, 158, 176))
    d.text((760, 728), "14,297,169", font=load_font(25, True), fill=VALUE)

    img.save(out, "PNG")
    print(f"wrote {out}")


# Eight equipment slots of 梦幻西游手游, in the panel's own top-to-bottom order.
EQUIPMENT_SLOTS = [
    ("武器", "霜冷九州"),
    ("头盔", "玄冰冠"),
    ("衣服", "龙鳞甲"),
    ("项链", "焚天玉"),
    ("腰带", "九霄束"),
    ("鞋子", "踏云靴"),
    ("戒指", "三界戒"),
    ("饰品", "混沌珠"),
]


def make_equipment(out: Path) -> None:
    """装备 panel: one row per slot.

    Each row is drawn as a SINGLE text run, "武器 霜冷九州". PP-OCRv4 emits one
    text box per detected run, so this is exactly the shape `extract_equipment`
    expects: the slot vocabulary plus the item name on the same OCR line. (Draw
    the slot and the name as two separate runs and the detector returns two
    boxes, which the extractor — reasonably — ignores.)
    """
    img = Image.new("RGB", (1280, 800), BG)
    d = ImageDraw.Draw(img)
    d.text((40, 26), "装备", font=load_font(34, True), fill=TITLE)
    d.text((40, 82), "评分 41,208", font=load_font(22), fill=LABEL)

    d.rounded_rectangle([40, 130, 1240, 716], radius=14, fill=PANEL)
    f_row = load_font(25, True)
    f_level = load_font(20)
    for i, (slot, name) in enumerate(EQUIPMENT_SLOTS):
        ry = 168 + i * 66
        if i % 2 == 0:
            d.rounded_rectangle([56, ry - 8, 1224, ry + 48], radius=8, fill=PANEL_LIGHT)
        d.text((84, ry), f"{slot} {name}", font=f_row, fill=VALUE)
        # Enhancement level on the far right ("+9" style).
        suffix = f"+{9 - i}"
        tw = d.textlength(suffix, font=f_level)
        d.text((1196 - tw, ry + 3), suffix, font=f_level, fill=LABEL)

    d.text((70, 742), "已装备 8 件 / 8 件", font=load_font(22), fill=LABEL)
    img.save(out, "PNG")
    print(f"wrote {out}")


def make_inventory(out: Path) -> None:
    img = Image.new("RGB", (1280, 800), BG)
    d = ImageDraw.Draw(img)
    d.text((40, 30), "背包", font=load_font(34, True), fill=TITLE)
    d.text((40, 84), "容量 36 / 60", font=load_font(22), fill=LABEL)

    items = [
        ("金柳露", 12), ("玄天残卷", 3), ("五宝", 1), ("九转金丹", 8),
        ("风月宝鉴", 2), ("高级藏宝图", 5), ("金刚石", 24), ("定神香", 9),
        ("超级金柳露", 6), ("百岁香", 14), ("夜光珠", 2), ("灵宝", 7),
    ]
    # A tight, contiguous cell grid like the real 背包: cells share borders,
    # which is what makes the panel read as a *grid* (many evenly spaced
    # vertical edges) instead of a few wide panel rows.
    #
    # GEOMETRY MATTERS. `extract_inventory` pairs an item name with the stack
    # count printed BELOW it, accepting a vertical gap of up to ~2.2x the
    # name's text height. A roomy cell pushes the count out of that window and
    # the pairing silently drops to 0 items, so the cell stays compact and the
    # count sits directly under the name, right-aligned — which is also how the
    # in-game 背包 grid is laid out.
    cols, cell, gap = 6, 132, 8
    ox, oy = 40, 140
    f_name = load_font(21, True)
    f_cnt = load_font(23, True)
    for idx, (name, count) in enumerate(items):
        r, c = divmod(idx, cols)
        x = ox + c * (cell + gap)
        y = oy + r * (cell + gap)
        d.rectangle([x, y, x + cell, y + cell], fill=PANEL, outline=(92, 99, 118), width=2)
        tw = d.textlength(name, font=f_name)
        d.text((x + (cell - tw) / 2, y + 22), name, font=f_name, fill=VALUE)
        # Stack count: directly beneath the name, centred in the cell. Centred
        # rather than right-aligned so a single narrow digit ("1", "7", "9")
        # never sits on top of the cell's 2px outline, which otherwise merges
        # with the glyph and costs the detector that box entirely.
        label = str(count)
        tw2 = d.textlength(label, font=f_cnt)
        d.text((x + (cell - tw2) / 2, y + 56), label, font=f_cnt, fill=VALUE)

    img.save(out, "PNG")
    print(f"wrote {out}")


def main() -> int:
    outdir = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("samples")
    outdir.mkdir(parents=True, exist_ok=True)

    make_attributes("海淘淘", "429277013", 89, outdir / "character_haitaotao.png")
    make_attributes("云中鹤", "412870335", 76, outdir / "character_yunzhonghe.png")
    make_attributes("小铃铛", "518204467", 64, outdir / "character_xiaolingdang.png")
    # Four-character nickname variant: exercises nickname extraction when the
    # name is longer than the three-character cases above.
    make_attributes("清风徐来", "520131478", 72, outdir / "character_qingfengxulai.png")
    make_equipment(outdir / "equipment.png")
    make_inventory(outdir / "inventory.png")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
