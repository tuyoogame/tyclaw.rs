---
name: GitLab
description: GitLab 代码仓库数据查询：提交记录、Merge Request、项目成员、用户活跃度
triggers:
  - GitLab
  - GL
  - 代码提交
  - 提交记录
  - commit
  - MR
  - merge request
  - 合并请求
  - 设置GitLab
  - 设置GL凭证
tool: tools/gitlab_api.py
default: false
credentials:
  key: gitlab
  display_name: GitLab
  setup: manual
  fields:
    - {name: token, label: Personal Access Token (read_api), secret: true}
---
# GitLab 代码仓库（只读）

通过 GitLab REST API 查询代码仓库数据：提交记录、Merge Request、项目成员、用户活动事件。工具脚本：

- `tools/gitlab_api.py` — 8 个子命令（只读）

## 行为规则

### 写操作意图 → 告知不支持

用户要求创建 MR、评论、approve、push 代码、创建 Issue 等**写入类意图**时，直接回复：

> GitLab 功能目前仅支持查看，不支持写入操作。请在 GitLab 中直接操作。

### 凭证配置

使用前需先设置 GitLab Personal Access Token。Token 从 [tygit PAT 页面](https://tygit.tuyoo.com/-/user_settings/personal_access_tokens) 创建。

**创建 Token 时只需勾选 `read_api` 一个 scope**（只读访问 API），不要勾选 `api`（权限过大，包含写操作）、`read_repository`（用于 git clone，本 Skill 不拉代码）。

```bash
python3 tools/gitlab_api.py setup --token "glpat-xxxxxxxxxxxxxxxxxxxx"
```

清除凭证：

```bash
python3 tools/gitlab_api.py clear-credentials
```

### 查询类意图 → 先确认凭证再执行

收到 GitLab 查询需求时：

1. 首次使用先引导用户设置 PAT（提醒只需 `read_api` scope，给出创建链接）
2. 查询前先用 `list-projects` 了解用户可访问的项目范围
3. 用户提到项目名时，从 `list-projects` 结果匹配 `id` 或 `path_with_namespace`

### project 参数

所有需要 `--project` 的子命令，可传项目 ID（数字）或 URL 编码的 `namespace/project` 路径。路径中的 `/` 需替换为 `%2F`，如 `star-studio%2Fclient`。

建议先 `list-projects` 获取 ID，后续用 ID 更简单。

## 子命令参考

### setup — 设置凭证

```
python3 tools/gitlab_api.py setup --token "glpat-xxxxxxxxxxxxxxxxxxxx"
```

| 参数 | 必填 | 说明 |
|------|------|------|
| --token | 是 | Personal Access Token（只需 read_api scope） |

### clear-credentials — 清除凭证

```
python3 tools/gitlab_api.py clear-credentials
```

无参数。

### list-projects — 列出可访问的项目

```
python3 tools/gitlab_api.py list-projects
python3 tools/gitlab_api.py list-projects --search "client"
python3 tools/gitlab_api.py list-projects --owned
```

| 参数 | 必填 | 说明 |
|------|------|------|
| --search | 否 | 按关键词搜索项目名 |
| --owned | 否 | 只列出自己拥有的项目 |
| --membership | 否 | 只列出自己是成员的项目（默认行为） |
| --per-page | 否 | 每页数量，默认 20，最大 100 |
| --page | 否 | 页码，默认 1 |

### commits — 查询提交记录

```
python3 tools/gitlab_api.py commits --project 42
python3 tools/gitlab_api.py commits --project 42 --author "zhangsan@example.com" --since 2026-04-11
python3 tools/gitlab_api.py commits --project 42 --ref develop --since 2026-04-01 --until 2026-04-18
```

| 参数 | 必填 | 说明 |
|------|------|------|
| --project | 是 | 项目 ID 或 URL 编码路径 |
| --author | 否 | 按作者过滤（邮箱或姓名） |
| --since | 否 | 起始日期（YYYY-MM-DD） |
| --until | 否 | 截止日期（YYYY-MM-DD） |
| --ref | 否 | 分支名或 tag，默认项目默认分支 |
| --per-page | 否 | 每页数量，默认 20，最大 100 |
| --page | 否 | 页码，默认 1 |

### list-mrs — 列出 Merge Request

```
python3 tools/gitlab_api.py list-mrs --project 42
python3 tools/gitlab_api.py list-mrs --project 42 --state opened
python3 tools/gitlab_api.py list-mrs --project 42 --state merged --author zhangsan --since 2026-04-01
```

| 参数 | 必填 | 说明 |
|------|------|------|
| --project | 是 | 项目 ID 或 URL 编码路径 |
| --state | 否 | 状态过滤：opened / merged / closed / all（默认 all） |
| --author | 否 | 按作者用户名过滤 |
| --reviewer | 否 | 按 reviewer 用户名过滤 |
| --since | 否 | 创建时间起始（YYYY-MM-DD） |
| --per-page | 否 | 每页数量，默认 20，最大 100 |
| --page | 否 | 页码，默认 1 |

### mr-detail — MR 详情

```
python3 tools/gitlab_api.py mr-detail --project 42 --mr-iid 123
python3 tools/gitlab_api.py mr-detail --project 42 --mr-iid 123 --with-changes --with-comments
```

| 参数 | 必填 | 说明 |
|------|------|------|
| --project | 是 | 项目 ID 或 URL 编码路径 |
| --mr-iid | 是 | MR 的项目内编号（iid） |
| --with-changes | 否 | 包含变更文件列表（文件路径 + 增删行数，不含 diff 正文） |
| --with-comments | 否 | 包含评论/讨论 |

### user-events — 用户活动事件

```
python3 tools/gitlab_api.py user-events
python3 tools/gitlab_api.py user-events --username zhangsan --since 2026-04-11
```

| 参数 | 必填 | 说明 |
|------|------|------|
| --username | 否 | 目标用户名（不传查当前用户） |
| --since | 否 | 起始日期（YYYY-MM-DD） |
| --until | 否 | 截止日期（YYYY-MM-DD） |
| --per-page | 否 | 每页数量，默认 20，最大 100 |
| --page | 否 | 页码，默认 1 |

### list-members — 列出项目成员

```
python3 tools/gitlab_api.py list-members --project 42
```

| 参数 | 必填 | 说明 |
|------|------|------|
| --project | 是 | 项目 ID 或 URL 编码路径 |
| --per-page | 否 | 每页数量，默认 20，最大 100 |
| --page | 否 | 页码，默认 1 |

## 典型场景

### 查看某人最近的提交

1. `list-projects --search "项目名"` → 获取项目 ID
2. `commits --project <id> --author "张三" --since 2026-04-11`
3. 格式化展示（提交信息、时间、修改文件数）

### 查看待合并的 MR

1. `list-mrs --project <id> --state opened`
2. 展示各 MR 的标题、作者、创建时间

### 了解 MR 变更内容

1. `mr-detail --project <id> --mr-iid 123 --with-changes --with-comments`
2. 展示描述、变更文件统计、reviewer 评论

### 查看团队活跃度

1. `list-members --project <id>` → 获取成员列表
2. `commits --project <id> --since 2026-04-01 --per-page 100` → 按 author 汇总
3. 分析各成员提交频次

### 查看个人近期工作概览

1. `user-events --since 2026-04-11` → 查看自己的 push/MR/comment 等活动
2. 汇总展示近期工作内容

## 安全约束

1. **纯只读**：工具不提供任何写入命令
2. **不访问文件内容**：`--with-changes` 只返回文件路径和增删行数统计，不返回 diff 正文
3. **不支持 git 操作**：不支持 clone / pull / push / checkout 等 git 命令
