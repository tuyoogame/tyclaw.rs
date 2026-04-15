---
name: 微信公众号文章
description: 搜索任意公众号、获取文章列表和全文内容
triggers:
  - 搜索公众号
  - 公众号文章
  - 抓取文章
  - 微信文章
  - 获取公众号
  - 设置公众号
  - 设置公众号凭证
  - 公众号凭证
  - 绑定公众号
tool: skills/wechat-article/tool.py
default: false
credentials:
  key: wechat
  display_name: 微信公众号
  setup: oauth
---
# 微信公众号文章

搜索任意微信公众号，获取文章列表和全文内容。

## 前提

首次使用或凭证过期时，tool 会自动输出绑定链接。用户点击链接、用**公众号管理员微信**扫码即可完成绑定（与钉钉 OAuth 同一体验），凭证有效期约 4 天。

## 工具用法

`skills/wechat-article/tool.py`

### 常用命令

```bash
# 检查登录状态
python3 skills/wechat-article/tool.py status

# 搜索公众号
python3 skills/wechat-article/tool.py search --query "公众号名称"

# 获取文章列表（需先搜索获取 fakeid）
python3 skills/wechat-article/tool.py list --fakeid MjM5OTc2ODUxMw== --count 10

# 获取文章全文
python3 skills/wechat-article/tool.py read --url "https://mp.weixin.qq.com/s/xxxxx"

# 在公众号内搜索关键词
python3 skills/wechat-article/tool.py list --fakeid MjM5OTc2ODUxMw== --keyword "关键词"
```

### 参数速查

#### search 子命令

| 参数 | 说明 | 必须 |
|------|------|------|
| `--query` | 搜索关键词（公众号名称） | 是 |

#### list 子命令

| 参数 | 说明 | 必须 |
|------|------|------|
| `--fakeid` | 公众号的 FakeID（从 search 获取） | 是 |
| `--begin` | 偏移量，默认 0 | 否 |
| `--count` | 获取数量，默认 10，最大 100 | 否 |
| `--keyword` | 在该公众号内搜索关键词 | 否 |

#### read 子命令

| 参数 | 说明 | 必须 |
|------|------|------|
| `--url` | 微信文章链接 | 是 |
| `--format` | 输出格式：text（默认）或 html | 否 |

## 凭证设置

当用户说「设置公众号凭证」「绑定公众号」「公众号凭证」时，调用 `status` 子命令。如果未绑定或已过期，tool 会自动输出绑定链接，引导用户扫码完成授权。微信公众号凭证通过 OAuth 扫码授权设置，不支持手动填写。

## 典型对话流程

1. 用户：搜索公众号 游戏葡萄 → 调 search，返回 fakeid
2. 用户：获取最新文章 → 调 list，返回文章列表（含标题、链接）
3. 用户：帮我看看第一篇 → 调 read，返回全文内容供总结

## 安全约束

1. 仅读取公开发布的文章，不涉及私密信息
2. 凭证存储在用户个人 workspace，互不影响
3. 请勿高频请求，避免触发微信风控
