---
name: 创建Skill
description: 描述需求，AI 自动创建可复用的 Skill
triggers:
  - 帮我做个工具
  - 帮我做个
  - 创建一个工具
  - 写一个功能
  - 我想要一个能
  - 帮我改一下
  - 修改参数
  - 删除能力
  - 删除功能
---
# Skill Manager — 元 Skill

## 职责

管理用户的个人 Skill：创建、修改、删除。
- **创建**：接收自然语言需求，自动生成 tool.py + SKILL.md 两件套
- **修改**：根据用户要求修改已有 Skill 的代码或配置
- **删除**：删除用户指定的个人 Skill

## 意图判断

根据用户消息判断操作类型：
- 创建意图：做/写/创建一个工具、我想要一个能XX的功能 → 执行「创建流程」
- 修改意图：改一下XX、修改XX的参数/比例、更新XX → 执行「修改流程」
- 删除意图：删除XX能力/功能 → 执行「删除流程」

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

**反例**：收到第一条文字回复就急着开始创建，后面用户发文件时又要返工
**正例**：收到文字回答 → "收到，请发输入样本文件" → 收到文件 → "还有输出模板吗？没有的话我按默认格式生成" → 开始创建

### Step 2: 确定命名

- `skill_name`：小写字母+连字符，如 `excel-converter`、`json-formatter`
- `staff_id`：从环境变量 `TYCLAW_SENDER_STAFF_ID` 获取（用于临时输出路径，文件路径无需包含）

### Step 3: 生成两件套

按以下路径和格式创建文件（相对于当前工作目录 `/workspace`）：

#### 3a. 工具脚本

路径：`work/_personal/skills/{skill_name}/tool.py`

要求：
- 使用 argparse 解析命令行参数
- 输出文件写到临时路径，**从环境变量获取 staff_id**：
  ```python
  staff_id = os.environ.get("TYCLAW_SENDER_STAFF_ID", "unknown")
  output_dir = f"/tmp/tyclaw_{staff_id}_{timestamp}_{skill_name}/"
  ```
- 优先使用容器中已安装的包：requests, pyyaml, 标准库
- 如需其他包，在脚本开头注释说明 `# requires: xxx`
- 包含完整的错误处理和日志
- 脚本可独立运行：`python3 work/_personal/skills/{skill_name}/tool.py --help`

#### 3b. Skill 文档

路径：`work/_personal/skills/{skill_name}/SKILL.md`

格式（必须包含 YAML frontmatter）：
```markdown
---
name: {Skill 中文名}
description: {一句话描述}
triggers:
  - {触发关键词1}
  - {触发关键词2}
  - {触发关键词3}
tool: tool.py
---
# {Skill 中文名}

## 功能
{一句话描述}

## 使用方式
{用户如何触发，示例消息}

## 工具
- `tool.py`：{简述}

## 参数
{工具脚本的参数说明}

## 输出
{输出格式和位置}
```

frontmatter 字段说明：
- `name`：Skill 的中文名称
- `description`：一句话描述，会显示在用户的功能列表中
- `triggers`：触发关键词列表，帮助 AI 路由到此 Skill
- `tool`：工具脚本的相对路径（通常为 `tool.py`）

### Step 4: 确认回复

创建完成后，回复格式：

```
Skill「{Skill 中文名}」已创建！

你现在可以直接使用它，试试发送：{示例消息}

创建的文件：
- 工具脚本：work/_personal/skills/{skill_name}/tool.py
- Skill 文档：work/_personal/skills/{skill_name}/SKILL.md
```

---

## 二、修改流程

用户要求修改已有 Skill 时（改参数、改逻辑、改配置等），按以下步骤执行：

### Step 1: 定位 Skill

- 根据用户描述匹配 `work/_personal/skills/` 下的 Skill 目录
- 用 `read_file` 读取该 Skill 的 `SKILL.md` 和 `tool.py`，理解现有逻辑

### Step 2: 确认修改范围

- 向用户确认要修改的内容，避免误改
- 如果修改较大（如整体重写），建议用户确认

### Step 3: 执行修改

- 用 `edit_file` 或 `write_file` 修改 `work/_personal/skills/{skill_name}/tool.py` 和/或 `SKILL.md`
- 保持原有文件结构和 frontmatter 格式不变
- 如果修改涉及 SKILL.md 中记录的参数/逻辑说明，同步更新文档

### Step 4: 确认回复

```
Skill「{Skill 中文名}」已更新！

修改内容：{简述修改了什么}
```

---

## 三、删除流程

用户要求删除个人 Skill 时，按以下步骤执行：

### Step 1: 确认目标

- 根据用户描述匹配 `work/_personal/skills/` 下的 Skill
- **如果用户要删除的是 builtin 能力（如"创建Skill"本身），拒绝并说明**：

```
「{能力名}」是系统内置功能，无法删除。你只能管理自己创建的个人 Skill。
```

### Step 2: 确认删除

- 告诉用户即将删除的 Skill 名称和目录，请用户确认

### Step 3: 执行删除

- 删除整个 `work/_personal/skills/{skill_name}/` 目录（包括 tool.py 和 SKILL.md）
- 使用 `exec` 工具执行 `rm -r work/_personal/skills/{skill_name}/`

### Step 4: 确认回复

```
Skill「{Skill 中文名}」已删除。
```

---

## 安全约束

1. 只能在 `work/_personal/skills/{skill_name}/` 下创建/修改/删除文件
2. 禁止修改或删除 builtin 能力（`skills/` 目录下的内容为只读）
3. 工具脚本禁止包含网络请求到外部未知服务（内网 API 除外）
4. 工具脚本禁止执行危险操作（rm -rf /、系统命令等）
5. 工具脚本的输出路径必须使用环境变量 `TYCLAW_SENDER_STAFF_ID`，禁止硬编码 staff_id

## 可用工具

Agent 提供以下工具用于创建和管理 Skill：
- `read_file`：读取文件内容
- `write_file`：创建或覆盖文件（自动创建父目录）
- `edit_file`：编辑已有文件（替换指定文本）
- `delete_file`：删除文件
- `exec`：执行 shell 命令（如运行 Python 脚本、删除目录）
- `list_dir`：列出目录内容
- `send_file`：将生成的文件发送给用户

## 容器可用包

以下包在容器中可直接使用：
- requests（HTTP 调用）
- pyyaml（YAML 处理）
- json, csv, re, os, sys, pathlib, argparse, datetime, shutil（标准库）
