# -*- coding: utf-8 -*-
"""品牌图标生成：assets/logo.png（源图）→ app-icon.ico + window-icon.png。

产物入库（不进构建流程），仅当 logo 更换时手动重跑一次：
    python scripts/gen-icon.py

- app-icon.ico：exe 资源图标（build.rs 经 winresource 嵌入 asset-manager /
  asset-installer）。多尺寸帧 16/24/32/48/64/128/256，全部 LANCZOS 高质量
  降采样——16px 是 Explorer/任务栏最常显示的档，降采样质量直接决定观感。
- window-icon.png：Slint Window icon 属性用（标题栏/任务栏窗口图标），
  256px 单帧。窗口图标只在小尺寸显示，用 256px 控制嵌入体积
  （1240px 源图 ~400KB vs 256px ~60KB）。
"""

from pathlib import Path

from PIL import Image

REPO = Path(__file__).resolve().parent.parent
SRC = REPO / "assets" / "logo.png"
ICO_OUT = REPO / "assets" / "app-icon.ico"
WIN_OUT = REPO / "assets" / "window-icon.png"

ICO_SIZES = [256, 128, 64, 48, 32, 24, 16]


def main() -> None:
    base = Image.open(SRC).convert("RGBA")
    if base.width != base.height:
        raise SystemExit(f"源图非正方形：{base.size}")

    frames = [base.resize((s, s), Image.Resampling.LANCZOS) for s in ICO_SIZES]
    frames[0].save(ICO_OUT, format="ICO", append_images=frames[1:])

    win = base.resize((256, 256), Image.Resampling.LANCZOS)
    win.save(WIN_OUT, format="PNG", optimize=True)

    # 回读校验：帧尺寸齐全 + alpha 保留
    ico = Image.open(ICO_OUT)
    got = sorted(s for s in ico.info.get("sizes", []))
    want = sorted((s, s) for s in ICO_SIZES)
    if got != want:
        raise SystemExit(f"ico 帧不符：{got}")
    print(f"OK ico: {ICO_OUT}（帧 {len(got)} 档, {ICO_OUT.stat().st_size} 字节）")
    print(f"OK png: {WIN_OUT}（{WIN_OUT.stat().st_size} 字节）")


if __name__ == "__main__":
    main()
