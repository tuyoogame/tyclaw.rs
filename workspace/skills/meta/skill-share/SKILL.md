---
name: Skill分享
description: 将自己的 Skill 分享给同事或部门，管理已分享的 Skill
triggers:
  - 分享Skill
  - 分享给
  - 共享
  - 取消分享
  - 我的分享
  - 我分享了
default: true
tool: skills/skill-share/tool.py
---
# Skill 分享

管理 Skill 的发布与可见范围。用户可以把自己创建的 Skill 分享给指定同事、部门或全公司。

搜索、安装、卸载共享 Skill 请使用「Skill 商店」。

## 工具用法

`skills/skill-share/tool.py`（从 TyClaw 项目根目录运行）

### 分享 Skill

```bash
# 分享给指定用户（支持用户名）
python3 skills/skill-share/tool.py share --skill <skill_name> --to-user <用户名或staff_id>

# 分享给自己的部门
python3 skills/skill-share/tool.py share --skill <skill_name> --to-department

# 分享给指定部门（按部门名称，支持模糊匹配）
python3 skills/skill-share/tool.py share --skill <skill_name> --to-department <部门名>

# 分享给全公司
python3 skills/skill-share/tool.py share --skill <skill_name> --to-all
```

`skill_name` 是用户 `_personal/skills/` 下的目录名。

### 查看我分享的 Skill

```bash
python3 skills/skill-share/tool.py my-shares
```

展示自己发布的 Skill 列表及安装者信息。

### 取消分享

```bash
python3 skills/skill-share/tool.py unshare <skill_name>
```

### 修改可见范围

```bash
python3 skills/skill-share/tool.py update-visibility <skill_name> --add-user <用户名>
python3 skills/skill-share/tool.py update-visibility <skill_name> --remove-user <用户名>
python3 skills/skill-share/tool.py update-visibility <skill_name> --add-dept [部门名]
python3 skills/skill-share/tool.py update-visibility <skill_name> --remove-dept [部门名]
python3 skills/skill-share/tool.py update-visibility <skill_name> --to-all
```

## 交互规则

1. 用户说"分享 XX 给 YY"时，根据上下文匹配 skill_name 和目标（用户名用 `--to-user`，部门名用 `--to-department <name>`），调用 `share`
2. 用户说"我的分享"时，调用 `my-shares`
3. 用户说"取消分享 XX"时，调用 `unshare`
4. 用户说"搜索Skill"或"安装XX"时，引导用户使用「Skill 商店」
