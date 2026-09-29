# 使驾皮肤开发指南 / Skin Development Guide

使驾支持通过「皮肤包」自定义应用背景、悬浮在窗口左下角的人物看板图，以及
按钮、文本、面板等 UI 元素的主题配色。内置了两套预设皮肤（命运石之门、地狱乐），
你完全可以按下面的格式做出自己的皮肤并以 zip 包导入。

## 皮肤包格式

皮肤包是一个 `.zip` 压缩包，根目录（或任意一层目录）包含 `skin.json`：

```json
{
  "id": "my-skin",
  "name": "我的皮肤",
  "background": "bg.png",
  "character": "character.png",
  "vars": {
    "--bg": "#101014",
    "--bg-panel": "#16161c",
    "--bg-elev": "#1c1c24",
    "--border": "#39394a",
    "--border-soft": "#282833",
    "--text": "#e6e6f0",
    "--text-dim": "#a8a8bc",
    "--text-faint": "#76768a",
    "--accent": "#7ec8e3",
    "--accent-soft": "#1f3440",
    "--danger": "#d83b3b",
    "--code-bg": "#0d0d12",
    "--radius": "6px"
  }
}
```

同目录下放置 `background` / `character` 引用的图片文件（png / jpg / webp / gif，文件内容必须与扩展名一致）。

**体积限制**：单张图片 ≤ 8 MiB，整包解压后 ≤ 32 MiB，文件数 ≤ 128。图片会被原样读进内存再以 `blob:` URL 显示，建议背景用 JPEG/WebP（2560×1440 约 300–600 KB），立绘用带透明通道的 WebP 或 PNG（1024×1536）。

## 字段说明

| 字段 | 必填 | 说明 |
|---|---|---|
| `id` | ✔ | 皮肤唯一标识：小写字母 / 数字 / `-` / `_`，不能与内置皮肤同名。**同 id 不会覆盖**——已安装的皮肤需先在皮肤卡片上删除（右上角 ✕）再导入 |
| `name` |  | 显示名称，默认用 id |
| `background` |  | 整个软件的背景图（建议横版 ≥1600×800） |
| `character` |  | 悬浮在窗口左下角顶层的人物看板图，不拦截点击（建议竖版透明底 png） |
| `vars` |  | 覆盖 CSS 变量（见下表），未提供的变量沿用当前主题 |

## CSS 变量一览

颜色值只接受 `#rgb` / `#rrggbb`（不支持带透明度的 8 位 hex、`rgb()` 等写法，否则该变量会被忽略）；`--radius` 写成 `Npx`（超过 18px 按 18px 处理）。背景图的清晰度由用户在设置里的「皮肤不透明度」滑块控制。

| 变量 | 用途 |
|---|---|
| `--bg` | 应用底色 |
| `--bg-panel` | 面板背景（侧栏、对话框等） |
| `--bg-elev` | 元素浮起背景（按钮、卡片） |
| `--border` / `--border-soft` | 边框 |
| `--text` / `--text-dim` / `--text-faint` | 文本三级 |
| `--accent` / `--accent-soft` | 强调色（按钮、选中、连线） |
| `--danger` | 危险操作色 |
| `--code-bg` | 代码块背景 |
| `--radius` | 全局圆角 |

## 使用方法

1. 设置 → 外观主题 → 勾选「启用皮肤」；
2. 导入：把 zip **直接拖进窗口**（皮肤设置打开时会出现高亮的投放区），或点「选择 zip 文件」用系统文件选择器；可一次导入多个；
3. 在皮肤卡片中选择即可应用；关闭开关则回到标准主题配色；
4. 删除：把鼠标移到自定义皮肤卡片上，点右上角的 ✕ 并确认。正在使用的皮肤被删除后自动回到基础主题。

皮肤安装在 `%APPDATA%\com.shidrive.desktop\skins\<id>\`，删除即移除该目录。

## 示例：内置皮肤

内置皮肤以相同机制实现（`app/src/themes.css`），可当作参考实现：

- **命运石之门**：复古 CRT 实验室终端——深黑蓝底、黄绿荧光强调、扫描线与闪烁动效、等宽字体；
- **地狱乐**：和风妖异——墨黑底、朱红强调、雾气流动动效。

## 进阶（CSS 注入）

`skin.json` 支持可选的 `"css"` 字段：一段自定义 CSS 文本，会在皮肤启用时注入
`<style>` 元素，可用于添加装饰线条、SVG 背景、专属动效等。写法示例：

```json
{
  "id": "retro",
  "css": ".titlebar { border-bottom: 2px solid #b7d36b; } .btn { text-transform: uppercase; letter-spacing: .06em; }"
}
```

请遵守各皮肤素材的版权许可；人物立绘等素材请使用你有权分发的文件。

## 动态皮肤（scene / globalFx）

v0.3.18 起 `skin.json` 可声明两个可选块，**都是纯数据，皮肤包里不允许任何脚本**：

### `scene`：分层动态看板

替代左下角静态人物图：身体网格按遮罩飘动（头发 / 衣摆 / 呼吸），自动眨眼、视线跟随鼠标，并在事件时切换表情。

```json
"scene": {
  "body": { "src": "body.png", "size": [768, 1376], "grid": [40, 72] },
  "mask": "mask.png",
  "pivot": { "neck": [370, 330], "chest": [400, 480] },
  "layers": [
    { "id": "closed", "src": "face_closed.png", "rect": [266,146,194,172], "bind": "blink" },
    { "id": "talk",   "src": "face_talk.png",   "rect": [266,126,168,193], "bind": "talk" }
  ],
  "idle":  { "wind": 1, "breath": 1, "blink": { "every": [2, 6], "double": 0.15 }, "gaze": { "maxRot": 0.045 } },
  "clips": {
    "talk":  { "dur": 2400, "keys": { "talk": "flap(100..170)" } },
    "smug":  { "dur": 1700, "keys": { "smug": [[0,0],[150,1],[1400,1],[1700,0]], "nod": [[0,0],[180,1],[520,0]] } }
  },
  "on": { "message.send": "smug", "agent.streaming": "talk" }
}
```

- `size`：坐标系（设计尺寸）；`rect` 与 `pivot` 都用这个坐标。贴图实际像素可以更小，按比例采样。
- `mask.png`：R = 头发风力、G = 衣摆风力、B = 呼吸、A = 头部跟随。
- `layers`：表情补丁，`bind` 为参数名，参数 0→1 控制不透明度（`slide` 可选位移）。
- 内置参数：`blink`（自动眨眼驱动）、`nod`、`jolt`、`blinkHold`（>0.3 时暂停眨眼）。
- `clips.keys`：`[[毫秒, 值]]` 关键帧（值 -1..1），或 `"flap(最短..最长)"` 口型开合。
- 事件：`message.send`、`agent.streaming`（回复开始，flap 片段会持续到 `agent.done`）、`agent.done`、`agent.error`、`skin.enter`。
- 上限：图层 16、片段 16、贴图单张 8 MiB / 总计 32 MiB。所有被引用文件必须在包内且是真实图片。
- **贴图尺寸**：按显示尺寸的 1.2–1.5 倍出图（看板约 300×540 CSS 像素 → body 约 450×800 即可）；不生成 mipmap，过大反而发糊、占显存。
- 可用 `tools/make_scene.py`（动态皮肤工具包）从一张立绘 + 表情图生成 mask 与 scene.json。

### `globalFx`：全局特效

```json
"globalFx": {
  "presets": { "frost": [ { "at": 0, "fx": "vignette", "color": "#7fc4ff", "opacity": 0.35, "dur": 900 } ] },
  "on": { "agent.done": "frost" }
}
```

原语：`shake`、`flash`、`slices`、`rgbSplit`、`hueShift`、`vignette`、`overlayText`。**引擎不内置任何预设**，`on` 只能引用本皮肤 `presets` 里定义的名字。例如“世界线跳跃”：

```json
"worldline": [
  { "at": 0, "fx": "slices", "n": 9, "dur": 1100, "color": "#ffb347" },
  { "at": 0, "fx": "rgbSplit", "px": 7, "dur": 1100 },
  { "at": 0, "fx": "shake", "px": 6, "dur": 1100, "steps": true },
  { "at": 0, "fx": "overlayText", "style": "nixie", "roll": 1100, "dur": 2600 },
  { "at": 1100, "fx": "flash", "color": "#ffb347", "opacity": 0.28, "dur": 300 }
]
```

每个预设 ≤16 步、`at` ≤2000ms；同一预设 1.2s 冷却，闪光每秒最多 3 次。

### 动效档位

设置 → 皮肤插件 → 皮肤动效：**完整 / 轻量 / 关闭**。轻量：关闭飘动与呼吸（仍眨眼/换表情），全局特效只保留 `vignette`、`overlayText`；系统开启“减少动态效果”时完整自动降为轻量。关闭或 WebGL 不可用时看板回退为 `body` 静态图。窗口隐藏时暂停渲染。
