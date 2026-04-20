---
name: HTML可视化
description: "生成交互式 HTML 可视化（图表、流程图、UI原型、数据仪表盘、演示页等）"
triggers:
  - 画个图
  - 可视化
  - 做个图表
  - 流程图
  - UI原型
  - 仪表盘
  - 数据图表
  - 做个演示
  - 情绪曲线
  - 布局设计
  - mockup
default: false
---
# HTML 可视化

## 功能

生成自包含的交互式 HTML 页面，用于数据图表、流程图、UI 原型、游戏设计文档、演示幻灯片等可视化场景。生成后上传到静态服务获取公开链接，用户在浏览器中查看。

## 工作流程

### Step 1: 生成 HTML

将完整的可视化内容写入一个 self-contained HTML 文件：

```python
staff_id = os.environ.get("TYCLAW_SENDER_STAFF_ID", "unknown")
import time
output_path = f"/tmp/tyclaw_{staff_id}_{int(time.time())}_viz/viz.html"
os.makedirs(os.path.dirname(output_path), exist_ok=True)
# 写入 HTML 内容
with open(output_path, "w", encoding="utf-8") as f:
    f.write(html_content)
```

### Step 2: 上传获取链接

```bash
python3 tools/html_upload.py --file /tmp/tyclaw_xxx_viz/viz.html
```

返回 JSON：`{"url": "https://...", "path": "...", "size": 12345}`

### Step 3: 回复用户

在回复中包含链接，让用户点击查看：

```
已生成可视化页面，点击查看：
{url}
```

### Step 4: 保存用于迭代

将 HTML 内容同时保存到 `_personal/viz_latest.html`，用户后续说"改一下颜色"等迭代请求时，先读取此文件作为修改基础。

## HTML 生成规范

### 基本要求

- **单文件自包含**：所有 CSS 和 JS 写在 HTML 内（inline），不依赖外部文件
- **CDN 库可引用**：允许通过 `<script src>` / `<link href>` 引用白名单 CDN 库
- **响应式设计**：适配桌面和移动端
- **编码声明**：`<meta charset="UTF-8">`

### CDN 白名单

| 库 | 用途 | CDN |
|---|---|---|
| ECharts | 图表（折线/柱状/饼图/雷达/热力/漏斗等） | `cdn.jsdelivr.net/npm/echarts@5/dist/echarts.min.js` |
| Chart.js | 轻量图表 | `cdn.jsdelivr.net/npm/chart.js` |
| Mermaid.js | 流程图/状态机/时序图/甘特图 | `cdn.jsdelivr.net/npm/mermaid/dist/mermaid.min.js` |
| D3.js | 高度定制数据可视化 | `cdn.jsdelivr.net/npm/d3@7` |
| Tailwind CSS | 快速布局和样式 | `cdn.tailwindcss.com` |
| Anime.js | 动画效果 | `cdn.jsdelivr.net/npm/animejs@3/lib/anime.min.js` |

优先使用 ECharts（功能最全、中文支持好）。简单布局优先用 inline CSS 而非引入 Tailwind。

### 默认暗色主题

所有可视化统一使用暗色主题，保持专业视觉风格：

```css
:root {
  --bg-primary: #1a1a2e;
  --bg-secondary: #16213e;
  --bg-card: #0f3460;
  --text-primary: #e8e8e8;
  --text-secondary: #a0a0b0;
  --accent-orange: #e94560;
  --accent-blue: #00adb5;
  --accent-green: #4ecca3;
  --accent-purple: #7b68ee;
  --accent-yellow: #ffc107;
  --border-color: #2a2a4a;
}
body {
  background: var(--bg-primary);
  color: var(--text-primary);
  font-family: -apple-system, "Segoe UI", "PingFang SC", "Microsoft YaHei", sans-serif;
  margin: 0;
  padding: 20px;
}
```

ECharts 配色：用 `backgroundColor: '#1a1a2e'`，文字 `#e8e8e8`，坐标轴 `#555`。

### 安全约束

- 禁止 `fetch` / `XMLHttpRequest` 访问非 CDN 外部地址
- 禁止使用 `localStorage` / `sessionStorage` / `indexedDB`
- 禁止 `window.open` / `window.location` 跳转
- 禁止内嵌 `<iframe>`

## 场景模式

根据用户需求选择合适的模式：

### 1. 数据图表

适用：折线图、柱状图、饼图、雷达图、漏斗图、热力图、散点图等

- 使用 ECharts，配置 `tooltip`、`legend`、`toolbox`（下载图片按钮）
- 数据量大时启用 `dataZoom`（滑动缩放）
- 多图表用 grid 布局并排或上下排列

### 2. 时间线 / 流程

适用：故事线、项目进度、事件时间轴、流程图

- 时间线用 HTML/CSS flexbox + 圆点 + 连线
- 彩色标签用 `<span>` + `border-radius` + 场景对应颜色
- 流程图优先用 Mermaid.js（`graph TD` / `stateDiagram`）

### 3. UI 原型 / Mockup

适用：游戏界面原型、App 页面 Mockup、交互设计

- 用 CSS flexbox/grid 做布局
- 虚线框 `border: 2px dashed` 表示占位/目标区域
- 可用 JS 实现简单交互（点击高亮、拖拽归位）
- 9:16 竖屏布局用 `max-width: 360px; margin: 0 auto;`

### 4. 空间布局 / 地图

适用：场景布局、建筑排布、地图标注

- 用 SVG 画路径线、标注点、区域框
- CSS absolute 定位放置建筑/元素标签
- 左图右文的双栏布局：`display: grid; grid-template-columns: 1fr 1fr;`

### 5. 演示页

适用：方案演示、数据报告、分析总结

- 用 CSS 分页：每个 section 高度 100vh，`scroll-snap-type: y mandatory`
- 页面切换动画
- 大标题 + 要点列表 + 配图/图表

## 迭代修改

用户说"改一下"时：

1. 读取 `_personal/viz_latest.html`
2. 根据用户要求修改对应部分
3. 写回文件 + 重新上传获取新链接
4. 更新 `_personal/viz_latest.html`

常见迭代指令示例：
- "换个配色" → 修改 CSS 变量
- "加个标题" → 插入 `<h1>`
- "柱状图改折线图" → 改 ECharts type
- "加上交互" → 添加 JS 事件处理
- "数据改一下" → 修改 data 数组
