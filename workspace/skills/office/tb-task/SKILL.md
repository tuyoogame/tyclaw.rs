---
name: Teambition任务
description: Teambition 任务操作：创建任务、读取任务、评论任务、搜索任务
triggers:
  - TB
  - Teambition
  - 提单
  - 创建任务
  - 读取任务
  - 评论任务
  - 搜索任务
  - 迭代任务
  - sprint
  - 按状态查询
tool: tools/tb_task.py
default: false
---

# Teambition Skill

Teambition 任务操作：创建任务（提单）、读取任务内容、评论任务、按任意条件搜索任务。

## 前置条件

1. `config.yaml` 中已配置 `teambition` 段（`app_id`、`app_secret`、`tenant_id`），由 Bot 侧代理持有，**工具无需访问凭据**
2. 用户需提供以下信息（通过 CLI 参数传入）：
   - `project_id` — 项目 ID

所有 TB API 调用通过 Bot 侧代理自动完成身份验证。**`operator_id` 由代理根据当前用户的钉钉身份自动注入**，无需手动传入（传入也会被忽略）。

## CLI 用法

所有命令都需要必填参数 `--project-id`（`tenant_id` 从 config 读取，`operator_id` 由代理自动注入）。

```bash
# 创建任务（指定迭代）
python3 tools/tb_task.py \
  --project-id "<项目ID>" \
  --title "<任务标题>" \
  --note "<任务备注>" \
  --priority 0 \
  --sprint-id "<迭代ID>"

# 创建任务（不指定迭代）
python3 tools/tb_task.py --project-id "<项目>" \
  --title "<任务标题>"

# 读取任务详情和备注内容
python3 tools/tb_task.py --project-id "<项目>" \
  --read-task "<任务ID>"

# 评论任务
python3 tools/tb_task.py --project-id "<项目>" \
  --comment-task "<任务ID>" --comment "<评论内容>"

# 列出项目自定义字段定义（cf_id → 字段名 + 选项）
python3 tools/tb_task.py \
  --operator-id "<操作者>" --project-id "<项目>" \
  --list-customfields

# 列出项目下所有任务类型（用于获取 sfc-id）
python3 tools/tb_task.py --project-id "<项目>" \
  --list-task-types

# 列出项目下所有任务分组（用于获取 tasklist-id）
python3 tools/tb_task.py --project-id "<项目>" \
  --list-task-groups

# 列出所有迭代（用于选择 sprint_id）
python3 tools/tb_task.py --project-id "<项目>" \
  --list-sprints

# 列出已知成员（ding_id → 姓名映射，来自 users.json）
python3 tools/tb_task.py --project-id "<项目>" \
  --list-members

# 列出所有工作流状态（用于获取 tfs_id，按状态搜索时需要）
python3 tools/tb_task.py --project-id "<项目>" \
  --list-statuses

# 查询指定迭代任务列表（sprint-id 必填）
python3 tools/tb_task.py --project-id "<项目>" \
  --sprint-tasks --sprint-id "<迭代ID>"

# 通用搜索：按 TQL 查询全项目任务
python3 tools/tb_task.py --project-id "<项目>" \
  --search-tasks 'tfsId = "<状态ID>"'
```

## 参数速查

### 全局参数（必填）

| 参数 | 说明 |
|------|------|
| `--project-id` | 目标项目 ID |

### 操作参数

| 参数 | 必填 | 说明 |
|------|------|------|
| `--title` | 创建时必填 | 任务标题（最多 500 字符） |
| `--note` | 否 | 任务备注（支持 markdown） |
| `--priority` | 否 | 0=普通（默认）/ 1=紧急 / 2=非常紧急 |
| `--sprint-id` | `--sprint-tasks` 时必填 | 迭代 ID（可通过 `--list-sprints` 查询） |
| `--tasklist-id` | 否 | 任务分组 ID（可通过 `--list-task-groups` 查询） |
| `--executor-id` | 否 | 执行者 TB 用户 ID（直接指定） |
| `--executor-ding-id` | 否 | 执行者钉钉 staff_id（自动转换为 TB userId） |
| `--sfc-id` | 否 | scenariofieldconfig_id（任务类型，可通过 `--list-task-types` 查询） |
| `--customfields` | 否 | 自定义字段 JSON，如 `[{"cf_id":"x","value_title":"y"}]` |
| `--images` | 否 | 附件图片路径（空格分隔多个） |
| `--read-task` | 否 | 读取任务详情（传入任务 ID） |
| `--comment-task` | 否 | 给任务添加评论（传入任务 ID，需配合 `--comment`） |
| `--comment` | 否 | 评论内容（与 `--comment-task` 配合，支持 markdown） |
| `--list-customfields` | 否 | 列出项目自定义字段定义（cf_id + 字段名 + 选项值） |
| `--list-task-types` | 否 | 列出项目下所有任务类型（scenariofieldconfig） |
| `--list-task-groups` | 否 | 列出项目下所有任务分组 |
| `--list-sprints` | 否 | 列出所有迭代（含状态） |
| `--list-members` | 否 | 列出已知成员（ding_id + 姓名，来自 users.json） |
| `--list-statuses` | 否 | 列出所有工作流状态（tfs_id + 名称 + 所属工作流） |
| `--sprint-tasks` | 否 | 输出迭代内所有任务列表 |
| `--search-tasks` | 否 | 通用搜索，传入 TQL 查询语句（搜索全项目） |

## TQL 查询语法

`--search-tasks` 接受 TQL 表达式，支持按任意条件搜索全项目任务。

### 可用字段

| 字段 | 运算符 | 示例 | 说明 |
|------|--------|------|------|
| `tfsId` | `=` | `tfsId = "xxx"` | 按工作流状态 ID 筛选 |
| `_sprintId` | `=` | `_sprintId = "xxx"` | 按迭代 ID 筛选 |
| `isDone` | `=` | `isDone = false` | 按完成状态 |
| `priority` | `=` | `priority = 2` | 按优先级（0/1/2） |
| `executorId` | `=` | `executorId = "xxx"` | 按执行者 |
| `content` | `~` | `content ~ "关键词"` | 按标题模糊搜索 |

### 组合条件

用 `AND` 连接多个条件：

```
isDone = false AND priority = 2
tfsId = "xxx" AND executorId = "yyy"
```

### 典型用法

按自定义字段统计任务的流程：

1. 任务输出的 `customfields` 已自动解析 `cf_name`（如"优先级（P）"、"缺陷分类"）
2. 如需查看项目所有字段定义及选项值，用 `--list-customfields`

按执行人分类任务的流程：

1. 查询任务时 `executor_name` 字段会自动填充（仅限 users.json 中已登记的用户）
2. 如需完整映射表，先 `--list-members` 获取所有已知 ding_id→姓名

按状态查任务的流程：

1. 先 `--list-statuses` 查询所有状态，找到目标状态的 `tfs_id`
2. 用 `--search-tasks 'tfsId = "<tfs_id>"'` 搜索

## 优先级选择

| 条件 | 优先级 |
|------|--------|
| 全服影响 / 持续报错 / 核心功能异常 | 2（非常紧急） |
| 部分玩家受影响 / 非核心功能 | 1（紧急） |
| 低频 / 边缘场景 / 已有临时方案 | 0（普通） |

## 输出

创建任务成功时 stdout 输出 JSON：

```json
{
  "task_id": "xxx",
  "task_url": "https://www.teambition.com/project/{project_id}/sprint/section/{sprint_id}/task/{task_id}",
  "content": "任务标题",
  "is_done": false,
  "executor": "tb_user_id 或 unassigned",
  "attachments": 0
}
```

`--read-task` 输出：

```json
{
  "task_id": "xxx",
  "task_url": "https://www.teambition.com/project/{project_id}/task/{task_id}",
  "content": "任务标题",
  "is_done": false,
  "priority": "紧急",
  "note_html": "<p>富文本 HTML</p>",
  "note_text": "纯文本内容"
}
```

`--comment-task` 输出：

```json
{
  "comment_id": "xxx",
  "task_id": "xxx",
  "task_url": "...",
  "operator_id": "评论者 TB 用户 ID"
}
```

`--search-tasks` / `--sprint-tasks` 输出：

```json
{
  "total": 35,
  "tasks": [
    {
      "id": "xxx",
      "content": "任务标题",
      "is_done": false,
      "priority": 1,
      "priority_label": "紧急",
      "status": "修复中",
      "status_kind": "进行中",
      "executor_id": "tb_user_id",
      "executor_ding_id": "staff_id",
      "executor_name": "张三",
      "creator_id": "tb_user_id",
      "creator_ding_id": "staff_id",
      "creator_name": "李四",
      "involve_members": [
        {"tb_user_id": "tb_uid_1", "ding_id": "staff_id_1", "name": "李四"},
        {"tb_user_id": "tb_uid_2", "ding_id": "staff_id_2", "name": ""}
      ],
      "customfields": [
        {"cf_id": "xxx", "cf_name": "优先级（P）", "type": "dropDown", "values": ["P1"]},
        {"cf_id": "yyy", "cf_name": "缺陷分类", "type": "commongroup", "values": ["弟子系统"]}
      ],
      "parent_task_id": "",
      "progress": 0,
      "updated": "2026-04-10T08:00:00Z",
      "created": "2026-04-01T10:00:00Z",
      "accomplish_time": ""
    }
  ]
}
```

过程日志输出到 stderr，可忽略。

## 使用示例

```bash
# 查询迭代列表
python3 tools/tb_task.py --project-id "proj789" --list-sprints

# 查询所有工作流状态
python3 tools/tb_task.py --project-id "proj789" --list-statuses

# 按状态搜索全项目任务
python3 tools/tb_task.py --project-id "proj789" \
  --search-tasks 'tfsId = "66b9819c1e9f89fed73ae0d3"'

# 按状态 + 未完成组合搜索
python3 tools/tb_task.py --project-id "proj789" \
  --search-tasks 'tfsId = "xxx" AND isDone = false'

# 按标题关键词搜索
python3 tools/tb_task.py --project-id "proj789" \
  --search-tasks 'content ~ "登录"'

# 创建任务到指定迭代
python3 tools/tb_task.py --project-id "proj789" \
  --title "商店道具数量显示为0" \
  --priority 1 \
  --sprint-id "sprint_abc123"

# 读取任务
python3 tools/tb_task.py --project-id "proj789" \
  --read-task "680b3a0bfb04be42d58a3ce1"

# 评论任务
python3 tools/tb_task.py --project-id "proj789" \
  --comment-task "680b3a0bfb04be42d58a3ce1" \
  --comment "已排查，根因是配表缺失。"
```
