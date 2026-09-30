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

## 动态皮肤（scene / globalFx / timeline）

`skin.json` 可声明三个可选块，**全部是纯数据，皮肤包里不允许任何脚本**。引擎不认识具体角色、表情或特效，所有表演都写在皮肤包里；新增皮肤不需要改代码。

### 事件

| 事件 | 触发时机 |
|---|---|
| `message.send` | 用户发送消息 |
| `agent.streaming` | 当前可见会话开始回复 |
| `agent.done` | 回复完成（失败的回合不会触发） |
| `agent.error` | 回合失败 |
| `character.click` | 点击人物的不透明区域（点在按钮、输入框等控件上时不算） |
| `skin.enter` | 皮肤刚启用 |

### `scene`：分层动态看板

```jsonc
"scene": {
  "body": { "src": "body.png", "size": [1024, 1536], "grid": [40, 72] },
  "mask": "mask.png",
  "pivot": { "neck": [484, 379], "chest": [484, 609] },
  "layers": [
    { "id": "closed", "src": "face_closed.png", "rect": [372,101,225,263], "bind": "blink" },
    { "id": "talk",   "src": "face_talk.png",   "rect": [372,101,225,263], "bind": "talk" }
  ],
  "idle": {
    "wind": 1, "breath": 1,
    "hairAmp": 32, "coatAmp": 20,        // 飘动幅度（0–90 / 0–60）
    "gust": 0.6,                         // 阵风强度 0–1：风力会慢慢起伏，偶尔来一阵强风
    "lag": 1.2,                          // 发梢滞后 0–3：值越大，波浪沿发丝传得越明显
    "windDir": 0.3,                      // 风向偏置 -1..1
    "blink": { "every": [2, 6], "double": 0.15 }, "gaze": { "maxRot": 0.04 },
    "actions": { "every": [35, 80], "clips": ["rest", "smirk"] }   // 空闲时随机做一个小动作
  },
  "clips": {
    "talk":  { "dur": 2400, "maxDur": 30000,
               "keys": { "talk": { "flap": [90, 170], "burst": [900, 2400], "rest": [500, 1400] } } },
    "smug":  { "dur": 2200, "keys": { "smug": [[0,0],[160,1],[1800,1],[2200,0]], "nod": [[0,0],[200,1],[560,0]] } },
    "poke":  { "dur": 1500, "cooldown": 2500, "keys": { "surprise": [[0,0],[60,1],[1200,1],[1500,0]], "jolt": [[0,0],[50,1],[300,0]] } }
  },
  "on": { "agent.streaming": "talk", "agent.done": "smug", "character.click": "poke", "message.send": ["ack", "smirk"] }
}
```

- `size` 是坐标系（原图像素），`rect` 和 `pivot` 都按这个坐标写；贴图本身可以更小，渲染时按比例采样。
- `mask.png` 的四个通道：R = 头发风力，G = 衣摆风力，B = 呼吸，A = 头部跟随。
- 内置参数：`blink`（自动眨眼）、`nod`（点头）、`jolt`（受惊一抖）、`blinkHold`（>0.3 时暂停眨眼）。
- **口型** `{"flap":[开合间隔ms], "burst":[连续说话时长], "rest":[停顿时长]}`：说一阵、停一阵，不会一直张嘴；`burst: null` 表示不停顿。旧写法 `"flap(100..170)"` 也会自动带上默认停顿。
- `until: "<事件>"` 让片段持续到该事件发生（上限 `maxDur`）；绑定在 `agent.streaming` 上的口型片段默认持续到 `agent.done`。
- `cooldown`：同一片段两次触发之间的最短间隔（例如防止连续点击时不停惊吓）。
- `on` 的值可以是片段名，也可以是数组（随机挑一个）。多个表情同时生效时，最后开始的片段决定显示哪张脸；有表情时隐藏眨眼层。
- 上限：图层 16，片段 24，每个片段 16 个参数；贴图单张 8 MiB，总计 32 MiB。
- **贴图尺寸**：按显示尺寸的 1.2–1.5 倍出图即可（看板约 340×600 CSS 像素）。

### `globalFx`：全局特效

```jsonc
"globalFx": {
  "presets": {
    "worldline": [
      { "at": 0,    "fx": "tint", "color": "#ff9a3c", "opacity": 0.18, "blend": "overlay", "dur": 1500 },
      { "at": 0,    "fx": "slices", "n": 10, "dur": 1000, "color": "#ffb347" },
      { "at": 0,    "fx": "rgbSplit", "px": 7, "dur": 1000 },
      { "at": 80,   "fx": "divergence", "values": ["1.048596", "0.571024"], "roll": 900, "lock": 110, "dur": 3000 },
      { "at": 1050, "fx": "ring", "x": 0.5, "y": 0.2, "size": 0.8, "dur": 900 }
    ]
  },
  "on": { "agent.done": "worldline", "agent.error": ["glitch"] },
  "ambient": { "every": [150, 360], "play": "flicker" }      // 空闲时偶尔播放（窗口需在前台）
}
```

| 原语 | 参数 |
|---|---|
| `shake` | px, steps, dur |
| `flash` | color, opacity, dur（每秒最多 3 次） |
| `slices` | n, color, dur：横向撕裂条 |
| `rgbSplit` | px, dur：RGB 错位 |
| `hueShift` | deg, dur |
| `vignette` | color, opacity, dur |
| `overlayText` | text, style(nixie/plain), roll, color, dur |
| `divergence` | values[], roll, lock, size, y, color, dur：辉光管读数，先滚动，再从左到右逐位定格 |
| `scanlines` | color, opacity, gap, dur |
| `noise` | opacity, dur：胶片颗粒 |
| `tint` | color, opacity, blend(color/overlay/multiply/screen/soft-light/…), dur：整体调色 |
| `ring` | x, y (0–1), size, width, color, dur：扩散光环 |
| `particles` | shape(dot/spark/digit), n ≤40, dir(up/down), color, dur |

**引擎不内置任何预设**。上限：每个皮肤 12 个预设，每个预设 24 步，`at` ≤3000ms；同一预设 1.2s 冷却。

### `timeline`：会话时间轴装饰

```json
"timeline": { "art": "timeline_art.svg", "mark": "timeline_mark.svg", "height": 160, "railColor": "#6b5039" }
```

- `art`：时间轴背后的竖条装饰，宽 18px，`height` 可设 40–480（默认 130）。
- `mark`：替换消息节点圆点的图标（显示为 12×12）。
- 支持 PNG/JPEG/WebP/GIF/**SVG**。SVG 只作为 `<img>` 渲染，导入时会被拒绝的内容：脚本、事件属性、`foreignObject`、`<use>`、实体声明、外链（`http(s):`），文件不得超过 512 KiB。

### 动效档位

设置 → 皮肤插件 → 皮肤动效：**完整 / 轻量 / 关闭**。
- 轻量：关闭飘动和呼吸，仍然眨眼、换表情；全局特效只保留 `vignette`、`overlayText`、`divergence`、`tint`。
- 系统开启"减少动态效果"时，完整自动降为轻量。
- 关闭，或 WebGL 不可用时，看板显示 `body` 静态图。
- 窗口隐藏时暂停渲染。
