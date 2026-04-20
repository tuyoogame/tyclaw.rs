---
name: Figma
description: "Figma 设计文件查询：读取文件结构、导出界面截图、查看组件和样式"
triggers:
  - Figma
  - figma
  - 设计稿
  - UI设计
  - 界面截图
  - 导出设计
  - 设计组件
  - 设置Figma
  - 设置figma凭证
tool: tools/figma_api.py
default: false
credentials:
  key: figma
  display_name: Figma
  setup: manual
  fields:
    - {name: token, label: "Personal Access Token (file_content:read)", secret: true}
---
# Figma 设计文件（只读）

通过 Figma REST API 查询设计文件：读取文件结构、导出节点截图、查看已发布组件和样式。工具脚本：

- `tools/figma_api.py` — 8 个子命令（只读）

## 行为规则

### 写操作意图 → 告知不支持

用户要求创建 frame、修改设计、移动元素等**写入类意图**时，直接回复：

> Figma 功能目前仅支持查看和导出，不支持修改设计。请在 Figma 中直接操作。

### 凭证配置

使用前需先设置 Figma Personal Access Token。从 [Figma Settings](https://www.figma.com/settings) → Security → Personal access tokens 创建。

**创建 Token 时勾选 `File content (Read only)` scope**。

```bash
python3 tools/figma_api.py setup --token "figd_xxxxxxxxxxxxxxxxxxxx"
```

清除凭证：

```bash
python3 tools/figma_api.py clear-credentials
```

### file-key 参数

所有需要 `--file-key` 的子命令，可传 file key 或完整 Figma URL，工具自动提取 key：

- `--file-key 6BTJc8aXzQPgxVfmhEkB5K`
- `--file-key "https://www.figma.com/design/6BTJc8aXzQPgxVfmhEkB5K/MyFile"`

### 查询流程

1. 首次使用先引导用户设置 PAT
2. `get-file` 先了解文件结构（默认 depth=2，只返回页面 + 顶层 frame）
3. 确定目标节点 ID 后，用 `get-nodes` 看详细结构或 `export-images` 导出截图
4. 导出的图片在 `/tmp/tyclaw_{staff_id}_*` 目录下，作为附件回传用户

### 大文件注意

Figma 文件可能非常大。`get-file` 默认 `--depth 2` 避免输出过大。需要更深层次时逐步增加 depth 或用 `--node-ids` 过滤。

### 结合 HTML 可视化

导出的截图可结合 html-viz Skill 生成可交互的 HTML 原型：
1. `export-images` 导出现有界面元素
2. 将导出的图片作为素材，用 HTML/CSS 拼装新的 UI 布局
3. 通过 html-viz 的 `html_upload.py` 上传生成可分享链接

## 子命令参考

### setup — 设置凭证

```
python3 tools/figma_api.py setup --token "figd_xxxxxxxxxxxxxxxxxxxx"
```

| 参数 | 必填 | 说明 |
|------|------|------|
| --token | 是 | Personal Access Token（file_content:read scope） |

### clear-credentials — 清除凭证

```
python3 tools/figma_api.py clear-credentials
```

### get-file — 读取文件结构

```
python3 tools/figma_api.py get-file --file-key "https://www.figma.com/design/xxx/Name"
python3 tools/figma_api.py get-file --file-key xxx --depth 3
python3 tools/figma_api.py get-file --file-key xxx --node-ids "1:2,3:4"
```

| 参数 | 必填 | 说明 |
|------|------|------|
| --file-key | 是 | Figma file key 或完整 URL |
| --depth | 否 | 文档树深度，默认 2（页面 + 顶层 frame） |
| --node-ids | 否 | 逗号分隔的节点 ID，只返回指定节点及其子树 |

返回：文件名、页面列表、各页面下的 frame 列表（含 id/name/type/size）、已发布的组件和样式。

### get-nodes — 读取指定节点详情

```
python3 tools/figma_api.py get-nodes --file-key xxx --node-ids "19075:86"
python3 tools/figma_api.py get-nodes --file-key xxx --node-ids "19075:86,7230:9" --depth 3
```

| 参数 | 必填 | 说明 |
|------|------|------|
| --file-key | 是 | Figma file key 或完整 URL |
| --node-ids | 是 | 逗号分隔的节点 ID |
| --depth | 否 | 子树深度限制 |

### export-images — 导出节点截图

```
python3 tools/figma_api.py export-images --file-key xxx --node-ids "19075:86"
python3 tools/figma_api.py export-images --file-key xxx --node-ids "19075:86,7230:9" --format svg --scale 2
```

| 参数 | 必填 | 说明 |
|------|------|------|
| --file-key | 是 | Figma file key 或完整 URL |
| --node-ids | 是 | 逗号分隔的节点 ID |
| --format | 否 | 输出格式：png/svg/pdf/jpg（默认 png） |
| --scale | 否 | 缩放倍数 0.01-4（默认 1） |

输出：图片文件路径列表，保存在 `/tmp/tyclaw_{staff_id}_*_figma/` 目录。

### list-components — 列出文件中已发布的组件

```
python3 tools/figma_api.py list-components --file-key xxx
```

| 参数 | 必填 | 说明 |
|------|------|------|
| --file-key | 是 | Figma file key 或完整 URL |

返回：组件的 key、name、description、缩略图 URL。

### list-styles — 列出文件中已发布的样式

```
python3 tools/figma_api.py list-styles --file-key xxx
```

| 参数 | 必填 | 说明 |
|------|------|------|
| --file-key | 是 | Figma file key 或完整 URL |

返回：样式的 key、name、类型（FILL/TEXT/EFFECT/GRID）、缩略图 URL。

### list-team-components — 列出团队库组件

```
python3 tools/figma_api.py list-team-components --team-id 123456
```

| 参数 | 必填 | 说明 |
|------|------|------|
| --team-id | 是 | Figma 团队 ID |
| --page-size | 否 | 每页数量，默认 30，最大 100 |
| --after | 否 | 分页游标 |

## 典型场景

### 浏览设计文件

1. `get-file --file-key "URL"` → 获取页面和 frame 列表
2. 展示文件结构（页面名、frame 数量）

### 导出特定界面截图

1. `get-file` 定位目标 frame 的 node ID
2. `export-images --file-key xxx --node-ids "1:2" --format png`
3. 将图片作为附件回传

### 查看节点层级结构

1. `get-nodes --file-key xxx --node-ids "1:2" --depth 4`
2. 展示节点树（type/name/size/children）

### 参考现有设计拼新原型

1. `get-file` 了解文件结构
2. `export-images` 导出需要的素材（按钮/图标/背景）
3. 结合 html-viz Skill 生成 HTML mockup，用导出的图片作为 `<img>` 素材

## 安全约束

1. **纯只读**：工具不提供任何写入命令
2. **不修改文件**：不支持创建/移动/删除节点
3. **输出路径受控**：导出图片仅写入 `/tmp/tyclaw_{staff_id}_*` 前缀目录
