---
name: 凭证总览
description: 查看所有凭证配置状态、清除指定凭证
triggers:
  - 我的凭证
  - 我的账号
  - 查看凭证
  - 凭证总览
  - 凭证状态
  - 清除凭证
tool: tools/user_settings.py
default: true
---
# 凭证总览

查看用户所有凭证的配置状态（已配置/未配置/即将过期），以及清除指定凭证。

**凭证设置已迁移到各 Skill 自身**，本 Skill 仅负责只读总览和清除操作。

## 工具用法

`tools/user_settings.py`（从 TyClaw 项目根目录运行）

### 查看所有凭证状态

```bash
python3 tools/user_settings.py show
```

动态扫描已安装 Skill 的 credentials 声明，汇总展示所有凭证的配置状态。

### 清除凭证

```bash
python3 tools/user_settings.py clear --section ga
python3 tools/user_settings.py clear --section td
python3 tools/user_settings.py clear --section email
python3 tools/user_settings.py clear --section adx
python3 tools/user_settings.py clear --section cl
python3 tools/user_settings.py clear --section wechat
```

## 意图判断

- 用户说"查看我的凭证"、"我的账号"、"凭证状态" → 调用 `show`
- 用户说"清除XX凭证" → 调用 `clear --section <key>`
- **用户说"设置XX凭证"** → 不要在本 Skill 内处理设置。告知用户直接发送对应 Skill 的设置指令（如「设置GA凭证」「设置ADX凭证」「设置公众号凭证」），各 Skill 会自行引导完成设置流程

## 安全约束

1. staff_id 从环境变量 `TYCLAW_SENDER_STAFF_ID` 获取，禁止硬编码
2. 不要在答复中明文输出用户的密码或 token
3. 完成后直接输出结论
