---
name: 企业邮箱
description: 通过企业邮箱发送、读取和搜索邮件
triggers:
  - 发邮件
  - 发个邮件
  - 发送邮件
  - 帮我发封邮件
  - 邮件通知
  - 发一封邮件
  - 查邮件
  - 看邮件
  - 读邮件
  - 收件箱
  - 搜索邮件
  - 找邮件
  - 有没有邮件
  - 最近的邮件
  - 设置邮箱
  - 设置邮箱凭证
  - 邮箱账号
tool: tools/email_sender.py
default: false
credentials:
  key: email
  display_name: 企业邮箱
  setup: manual
  fields:
    - {name: address, label: 企业邮箱地址}
    - {name: password, label: 第三方客户端安全密码, secret: true}
---
# 企业邮箱

通过阿里企业邮箱（钉钉企业邮箱）发送、读取和搜索邮件。

## 前提

用户必须先设置邮箱凭证，提供邮箱地址和第三方客户端安全密码。

```bash
python3 tools/email_sender.py setup --address "xxxx@tuyoogame.com" --password "第三方客户端安全密码"
```

清除凭证：

```bash
python3 tools/email_sender.py clear-credentials
```

当用户说「设置邮箱凭证」「设置邮箱」「邮箱账号」时，先告知密码获取方式：打开钉钉企业邮箱 → 账户与安全 → 账户安全 → 三方客户端安全密码 → 生成密码。然后询问用户提供邮箱地址（@tuyoogame.com）和生成的安全密码，调用 `setup`。

## 一、发送邮件

工具：`tools/email_sender.py`

### 发送纯文本邮件

```bash
python3 tools/email_sender.py --to "recipient@tuyoogame.com" --subject "标题" --body "正文内容"
```

### 发送 HTML 邮件

```bash
python3 tools/email_sender.py --to "recipient@tuyoogame.com" --subject "标题" --body "<h1>标题</h1><p>正文</p>" --html
```

### 发送给多人 + 抄送 + 附件

```bash
python3 tools/email_sender.py --to "a@tuyoogame.com,b@tuyoogame.com" --cc "c@tuyoogame.com" --subject "报告" --body "请查收附件" --attachment "/tmp/report.xlsx"
```

### 发送内嵌图片邮件（图片直接显示在正文中）

```bash
python3 tools/email_sender.py --to "a@tuyoogame.com" --subject "日报" --inline-image "/tmp/a.png,/tmp/b.png"
```

图片会按顺序嵌入邮件正文，文件名作为标题。可同时搭配 `--body` 添加文字说明。

### 发送参数速查

| 参数 | 说明 | 必须 |
|------|------|------|
| `--to` | 收件人，多个用逗号分隔 | 是 |
| `--subject` | 邮件主题 | 是 |
| `--body` | 邮件正文 | 是（无 `--inline-image` 时） |
| `--cc` | 抄送，多个用逗号分隔 | 否 |
| `--attachment` | 附件路径，多个用逗号分隔 | 否 |
| `--inline-image` | 内嵌图片路径，多个用逗号分隔，图片显示在正文中 | 否 |
| `--html` | 正文为 HTML 格式 | 否 |

## 二、读取邮件

工具：`tools/email_reader.py`（只读，不会删除或修改任何邮件）

### 列出收件箱最近邮件

```bash
python3 tools/email_reader.py list
python3 tools/email_reader.py list --limit 20
python3 tools/email_reader.py list --folder "Sent Messages"
```

### 读取指定邮件

```bash
python3 tools/email_reader.py read --id 123
```

`--id` 的值来自 `list` 或 `search` 输出中方括号里的序号。

### 搜索邮件

```bash
python3 tools/email_reader.py search --subject "周报"
python3 tools/email_reader.py search --from "zhang@tuyoogame.com"
python3 tools/email_reader.py search --since 2025-01-01 --before 2025-02-01
python3 tools/email_reader.py search --from "li@tuyoogame.com" --subject "数据" --limit 5
```

### 读取参数速查

| 子命令 | 参数 | 说明 |
|--------|------|------|
| `list` | `--limit N` | 显示条数，默认 10 |
| `list` | `--folder` | 邮箱文件夹，默认 INBOX |
| `read` | `--id` | 邮件序号（必须） |
| `search` | `--from` | 发件人地址 |
| `search` | `--subject` | 主题关键词 |
| `search` | `--since` | 起始日期 YYYY-MM-DD |
| `search` | `--before` | 截止日期 YYYY-MM-DD |
| `search` | `--limit N` | 最大结果数，默认 10 |

## 意图判断

发送意图（"帮我发个邮件给 XXX"、"发邮件通知 XXX"）：
1. 确认收件人邮箱地址（如果用户只给了名字，询问邮箱地址）
2. 确认邮件主题和正文内容
3. 如果有附件需求，确认附件路径
4. 参数齐全后调用 `tools/email_sender.py` 发送

读取意图（"看看我的邮件"、"最近有什么邮件"）：
1. 先用 `tools/email_reader.py list` 列出最近邮件
2. 用户指定某封后用 `read --id` 读取详情

搜索意图（"找一下 XXX 发的邮件"、"搜一下关于 XXX 的邮件"）：
1. 根据用户描述组合 `--from`/`--subject`/`--since`/`--before` 参数
2. 调用 `tools/email_reader.py search` 搜索

## 安全约束

1. staff_id 从环境变量 `TYCLAW_SENDER_STAFF_ID` 获取，禁止硬编码
2. 不要在答复中明文输出用户的邮箱密码
3. 完成后直接输出结果
4. **发送仅允许 @tuyoogame.com 邮箱**。如果用户要求发给外部邮箱，直接拒绝并说明原因，不要尝试调用工具
5. **读取工具为只读模式**，不支持也不允许删除、移动、标记任何邮件
