# 使驾皮肤开发指南 / Skin Development Guide

使驾支持通过「皮肤包」自定义应用背景、工作流画布右下角的人物看板图，以及
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
    "--bg-panel": "#16161cf0",
    "--bg-elev": "#1c1c24e8",
    "--border": "#39394a",
    "--border-soft": "#282833",
    "--text": "#e6e6f0",
    "--text-dim": "#a8a8bc",
    "--text-faint": "#76768a",
    "--accent": "#7ec8e3",
    "--accent-soft": "#7ec8e326",
    "--danger": "#d83b3b",
    "--code-bg": "#0d0d12",
    "--radius": "6px"
  },
  "bg_opacity": 0.3
}
```

同目录下放置 `background` / `character` 引用的图片文件（png/jpg/webp）。

## 字段说明

| 字段 | 必填 | 说明 |
|---|---|---|
| `id` | ✔ | 皮肤唯一标识（字母数字连字符），同 id 导入时覆盖 |
| `name` |  | 显示名称，默认用 id |
| `background` |  | 整个软件的背景图（建议横版 ≥1600×800） |
| `character` |  | 工作流画布右下角的人物看板图（建议竖版透明底 png） |
| `vars` |  | 覆盖 CSS 变量（见下表），未提供的变量沿用当前主题 |
| `bg_opacity` |  | 背景图不透明度，默认 0.3 |

## CSS 变量一览

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

1. 设置 → 外观主题 → 勾选「使用皮肤插件」；
2. 点「导入皮肤包（zip）」选择你的 zip；
3. 在皮肤卡片中选择即可应用；关闭开关则回到标准主题配色。

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
