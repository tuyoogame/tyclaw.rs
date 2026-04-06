---
name: 创建Skill
description: 描述需求，AI 自动创建可复用的 Skill
category: meta
tags: [skill, automation, creation]
triggers:
  - 创建
  - 做一个
  - 写一个工具
  - 新建skill
tool: null
risk_level: write
---
# Skill Manager — 元 Skill

## 职责

管理用户的个人 Skill：创建、修改、删除。
- **创建**：接收自然语言需求，自动生成 tool.py + SKILL.md 两件套
- **修改**：根据用户要求修改已有 Skill 的代码或配置
- **删除**：删除用户指定的个人 Skill

## 意图判断

根据用户消息判断操作类型：
- 创建意图：做/写/创建一个工具、我想要一个能XX的功能 → **先执行 Step 2.5 检查是否已存在**，已存在则走「修改流程」，否则走「创建流程」
- 修改意图：改一下XX、修改XX的参数/比例、更新XX → 执行「修改流程」
- 删除意图：删除XX能力/功能 → 执行「删除流程」

⚠️ **重要**：即使用户明确说"创建"，如果同名 Skill 已存在，也必须走修改流程。在 Step 2 确定命名后立即检查。

---

## 一、创建流程

收到用户创建 Skill 的请求后，按以下步骤执行：

### Step 1: 需求澄清

如果需求不够明确（缺少输入/输出/格式），向用户追问。明确后继续。

#### 钉钉交互优化（重要）

钉钉限制：文字和文件只能分开发送，一次只能发一个文件。用户必须分多条消息回复。

**原则 1：一次列清所有需要的东西**

把所有需要澄清的问题 + 需要的文件，合并到一次回复中列出，不要分批追问。包括：
- 所有业务参数（比例、规则、维度等）
- 输入样本文件（用于确认列名格式）
- 输出模板文件（如果涉及生成文件，主动问"是否有期望的输出格式模板"）
- 对有合理默认值的参数，给出默认值让用户确认，减少需要回复的内容

**原则 2：分条收集，到齐再动手**

用户会分多条消息回复（文字一条、文件一条、再一个文件又一条），AI 每收到一条都要：
1. 确认收到了什么
2. 对照清单检查：还缺什么？
3. 如果还缺 → 回复"收到 XX，还需要 YY"
4. 如果全部到齐 → 开始创建

### Step 2: 确定命名

- `skill_name`：小写字母+连字符，如 `excel-converter`、`json-formatter`
- `staff_id`：从环境变量 `TYCLAW_SENDER_STAFF_ID` 获取，或从 prompt 上下文中提取

### Step 2.5: 检查是否已存在

用 `read_file` 尝试读取 `skills/{skill_name}/SKILL.md`：
- **文件存在** → 已有 Skill，切换到修改流程（第二节），不要重新创建。
- **文件不存在** → 继续创建流程。

### Step 3: 生成两件套

**V2 路径变更**：`skills/{skill_name}/`

#### 3a. 工具脚本

路径：`skills/{skill_name}/tool.py`

要求：
- 使用 argparse 解析命令行参数
- 输出文件写到临时路径：
  ```python
  staff_id = os.environ.get("TYCLAW_SENDER_STAFF_ID", "unknown")
  output_dir = f"/tmp/tyclaw_{staff_id}_{timestamp}_{skill_name}/"
  ```
- 优先使用共享 venv 已安装的包：pandas, openpyxl, requests, pyyaml, 标准库
- 包含完整的错误处理和日志
- 脚本可独立运行

#### 3b. Skill 文档

路径：`skills/{skill_name}/SKILL.md`

格式（必须包含 YAML frontmatter）：
```markdown
---
name: {Skill 中文名}
description: {一句话描述}
triggers:
  - {触发关键词1}
  - {触发关键词2}
tool: tool.py
---
# {Skill 中文名}

## 功能
{一句话描述}

## 使用方式
{用户如何触发，示例消息}

## 参数
{工具脚本的参数说明}

## 输出
{输出格式和位置}
```

### Step 4: 确认回复

创建完成后回复：已创建 Skill + 使用示例 + 文件列表。

---

## 二、修改流程

### Step 1: 定位 Skill
- 根据用户描述匹配 `skills/` 下的 Skill 目录
- 读取 `SKILL.md` 和 `tool.py`，理解现有逻辑

### Step 2: 确认修改范围
- 向用户确认要修改的内容，避免误改

### Step 3: 执行修改
- 修改 tool.py 和/或 SKILL.md
- 保持 frontmatter 格式不变
- 同步更新文档

### Step 4: 确认回复

---

## 三、删除流程

### Step 1: 确认目标
- 匹配 `skills/` 下的 Skill
- 如果目标是 builtin 能力，拒绝删除

### Step 2: 确认删除
- 告知用户将删除的 Skill，请用户确认

### Step 3: 执行删除
- 删除整个 Skill 目录

### Step 4: 确认回复

---

## 安全约束

1. 只能在 `skills/{skill_name}/` 下创建/修改/删除文件
2. 禁止读取或修改其他用户的文件
3. 禁止修改或删除 builtin 能力
4. 工具脚本禁止包含到外部未知服务的网络请求（内网 API 除外）
5. 工具脚本禁止执行危险操作
6. 输出路径必须使用环境变量 `TYCLAW_SENDER_STAFF_ID`，禁止硬编码 staff_id

## 共享 venv 可用包

pandas, openpyxl, requests, pyyaml, json, csv, re, os, sys, pathlib, argparse, datetime, shutil
