#!/usr/bin/env python3
"""
像素对比: 对比 current/ vs baseline/ 的每张截图。
差异 > 阈值 → 生成 diff/ 下的红框差异图。
用法: python3 pixel-diff.py <baseline_dir> <current_dir> <diff_dir>
"""

import sys
import os
from pathlib import Path
from PIL import Image, ImageChops, ImageDraw

THRESHOLD = 0.005  # 0.5% 差异阈值


def pixel_diff(baseline: Path, current: Path, diff_output: Path) -> float | None:
    """
    对比两张图片，返回差异百分比。
    差异 > 阈值时生成 diff 图（红框标出差异区域）。
    返回 None 表示无法对比（尺寸不同/文件损坏）。
    """
    try:
        a = Image.open(baseline).convert("RGB")
        b = Image.open(current).convert("RGB")
    except Exception as e:
        print(f"  ⚠️  无法打开图片: {e}")
        return None

    if a.size != b.size:
        print(f"  ⚠️  尺寸不同: {a.size} vs {b.size}, 跳过对比")
        return None

    # 像素差异
    diff = ImageChops.difference(a, b)
    total_pixels = a.size[0] * a.size[1]
    diff_pixels = sum(
        1 for i in range(total_pixels)
        if diff.getdata()[i] != (0, 0, 0)
    )
    pct = diff_pixels / total_pixels

    if pct > THRESHOLD:
        # 生成红框差异图
        diff_output.parent.mkdir(parents=True, exist_ok=True)
        # 叠加差异高亮
        overlay = diff.point(lambda p: 255 if p > 30 else 0).convert("L")
        # 在 current 上画红框
        result = b.copy()
        draw = ImageDraw.Draw(result)
        # 找差异区域边框
        from PIL import ImageFilter
        edges = overlay.filter(ImageFilter.FIND_EDGES)
        bbox = edges.getbbox()
        if bbox:
            draw.rectangle(bbox, outline="red", width=2)
        result.save(diff_output)

        thumb = diff_output.with_suffix(diff_output.suffix.replace(".png", ".thumb.png"))
        result.resize((320, 240)).save(thumb)

    return pct


def main():
    if len(sys.argv) < 4:
        print("用法: python3 pixel-diff.py <baseline_dir> <current_dir> <diff_dir>")
        sys.exit(1)

    baseline_dir = Path(sys.argv[1])
    current_dir = Path(sys.argv[2])
    diff_dir = Path(sys.argv[3])

    if not baseline_dir.exists():
        print("  ⚠️  baseline 目录不存在, 跳过像素对比")
        sys.exit(0)

    regressions = 0
    total = 0

    for bimg in sorted(baseline_dir.rglob("*.png")):
        rel = bimg.relative_to(baseline_dir)
        cimg = current_dir / rel
        if not cimg.exists():
            continue

        dimg = diff_dir / rel
        total += 1
        pct = pixel_diff(bimg, cimg, dimg)

        if pct is None:
            regressions += 1
            print(f"  ⚠️  无法对比: {rel}")
        elif pct > THRESHOLD:
            regressions += 1
            print(f"  ⚠️  VISUAL REGRESSION: {rel} ({pct:.2%} diff)")
        else:
            # 差异太小，删除 diff 图 (如果之前有)
            if dimg.exists():
                dimg.unlink()

    if regressions == 0:
        print(f"  ✅ 全部 {total} 张截图无视觉回归")
    else:
        print(f"  ⚠️  {regressions}/{total} 张截图有视觉回归, 差异图已保存到 {diff_dir}")


if __name__ == "__main__":
    main()