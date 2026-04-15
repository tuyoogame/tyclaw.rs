---
name: Skill商店
description: 浏览、搜索、安装、卸载所有 Skill（系统/共享/个人）
triggers:
  - Skill商店
  - 功能商店
  - Skill目录
  - 安装功能
  - 安装Skill
  - 安装
  - 卸载功能
  - 卸载Skill
  - 卸载
  - 有什么功能
  - 有什么Skill
  - 可用功能
  - 功能列表
  - 搜索Skill
  - 搜索功能
  - 探索Skill
default: true
tool: skills/skill-store/tool.py
---
# Skill 商店

统一的 Skill 目录，管理所有类型 Skill 的浏览、搜索、安装与卸载。包括系统 Skill、其他用户共享的 Skill，以及展示用户自己创建的 Skill。

## 工具用法

`skills/skill-store/tool.py`（从 TyClaw 项目根目录运行）

### 浏览 Skill 目录

```bash
python3 skills/skill-store/tool.py list
```

展示完整 Skill 目录，分为：我的 Skill、已安装、可安装（系统 + 共享），附带安装数和使用次数统计。共享 Skill 按安装数从高到低排序。

### 搜索 Skill

```bash
python3 skills/skill-store/tool.py list --keyword <关键词>
```

在所有 Skill（名称、描述、作者）中搜索匹配的关键词。

### 安装 Skill

```bash
python3 skills/skill-store/tool.py install <skill_key>
```

- 系统 Skill 的 key 如 `ga-query`、`td-query`
- 共享 Skill 的 key 格式为 `作者ID--skill名`（目录列表中会列出）

### 卸载 Skill

```bash
python3 skills/skill-store/tool.py uninstall <skill_key>
```

默认 Skill（`default: true`）无法卸载。

## 交互规则

1. 用户说"Skill商店"、"有什么功能"、"有什么Skill"时，先调用 `list`，将结果格式化后展示
2. 用户说"搜索XX"、"有没有XX功能"时，调用 `list --keyword <关键词>` 搜索
3. 用户说"安装 XX"时，如果 key 已知则直接 `install <key>`；否则先调用 `list --keyword` 搜索匹配的 skill_key，再安装
4. 用户说"卸载 XX"时，同理调用 `uninstall <key>`
5. 安装/卸载完成后，告知用户结果。安装完成后立即读取对应 Skill 的 SKILL.md，在当前对话中即可使用
6. key 中含 `--` 的是共享 Skill，不含的是系统 Skill，安装/卸载命令相同，无需区分
7. **禁止依赖对话记忆**：每次安装或搜索请求都必须重新执行工具命令，不要因为之前搜索过就跳过。Skill 目录会在对话过程中变化（例如其他用户新分享了 Skill）
