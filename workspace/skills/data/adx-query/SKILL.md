---
name: ADX广告数据查询
description: 查询 DataEye AdXray 广告透视平台数据，包括产品投放趋势、媒体分布、热门排行、新品排行、搜索产品等
triggers:
  - ADX
  - AdXray
  - DataEye
  - 广告透视
  - 投放素材
  - 媒体分布
  - 热门排行
  - 竞品分析
  - 广告情报
  - 设置ADX
  - 设置ADX凭证
  - ADX账号
tool: tools/adx_query.py
default: false
credentials:
  key: adx
  display_name: ADX (DataEye)
  setup: manual
  fields:
    - {name: email, label: AdXray 登录邮箱}
    - {name: password, label: 登录密码, secret: true}
---
# ADX 广告数据查询

通过 DataEye AdXray 平台 API 查询手游广告情报数据，包括产品信息、投放趋势、媒体分布、排行榜等。

> 本文件位于 `skills/adx-query/`，下文中的相对路径均基于项目根目录。

## 前置依赖

```bash
pip install requests ddddocr
```

`ddddocr` 用于自动识别登录验证码（简单 4 位字母数字），识别失败会自动重试最多 5 次。

## 凭证配置

使用前需先设置 ADX 凭证（DataEye AdXray 平台的登录邮箱和密码）：

```bash
python3 tools/adx_query.py setup --email "user@tuyoogame.com" --password "your-password"
```

清除凭证：

```bash
python3 tools/adx_query.py clear-credentials
```

凭证通过环境变量 `_TYCLAW_ADX_EMAIL` / `_TYCLAW_ADX_PASSWORD` 自动注入，也可通过 `--email` / `--password` 命令行参数手动传入（优先级高于环境变量）。

当用户说「设置ADX凭证」「ADX账号」时，询问用户提供 DataEye AdXray 平台的登录邮箱和密码，然后调用 `setup`。

## 工具用法

`tools/adx_query.py`（从 TyClaw 项目根目录运行）

脚本自动处理验证码识别、登录、session 缓存和 API 签名。

### 常用命令

```bash
# 搜索产品（凭证已通过 setup 预配置）
python3 tools/adx_query.py search --keyword "次神"

# 查询产品基本信息
python3 tools/adx_query.py product_info --product-id 36528

# 查询投放趋势（最近 7 天）
python3 tools/adx_query.py trend --product-id 36528 --days 7

# 查询投放趋势（指定日期范围）
python3 tools/adx_query.py trend --product-id 36528 --start-date 2026-03-01 --end-date 2026-03-20

# 查询媒体分布
python3 tools/adx_query.py media_dist --product-id 36528 --days 7

# 热门产品排行
python3 tools/adx_query.py hot_ranking --days 7 --page 1 --size 20

# 新品排行
python3 tools/adx_query.py new_ranking --days 7

# 手动传入凭证（覆盖环境变量）
python3 tools/adx_query.py --email user@example.com --password pass123 search --keyword "次神"
```

### 参数速查

| 参数 | 说明 | 必须 |
|------|------|------|
| `--email` | AdXray 账号邮箱 | 可选，优先于环境变量 |
| `--password` | 账号密码 | 可选，优先于环境变量 |

子命令：

| 子命令 | 说明 | 关键参数 |
|--------|------|----------|
| `search` | 搜索产品/媒体/发行商 | `--keyword` |
| `product_info` | 产品基本信息 | `--product-id` |
| `trend` | 投放趋势（素材数/计划数） | `--product-id`, `--days` 或 `--start-date`/`--end-date` |
| `media_dist` | 媒体分布 | `--product-id`, `--days` |
| `hot_ranking` | 热门产品排行 | `--days`, `--page`, `--size` |
| `new_ranking` | 新品排行 | `--days`, `--page`, `--size` |

## 可用 API 端点

### 产品相关

| 端点 | 说明 | 关键参数 |
|------|------|----------|
| `/product/getProductInfo` | 产品详情（名称、公司、素材数、排名） | `productId` |
| `/product/listTrendByProduct` | 投放趋势（素材数/计划数按天） | `productId, startDate, endDate` |
| `/product/listMediaDistributionV2` | 媒体分布 | `productId, startDate, endDate` |
| `/product/listPositionDistributionV2` | 广告位分布 | `productId, startDate, endDate` |
| `/product/audienceAnalysis` | 受众分析 | `productId, productName` |
| `/product/getProductVideo` | 视频素材 | `productId` |

### 排行榜

排行榜接口使用 `pageId`（非 `pageNo`），需要 `searchType=1, top=500, adForm=INFO_FLOW`。

| 端点 | 说明 |
|------|------|
| `/product/listHotProductRanking` | 热门产品排行 |
| `/product/listNewProductRanking` | 新品排行 |
| `/product/listTopNewProduct` | Top 新品（简洁列表） |

### 搜索

| 端点 | 说明 | 关键参数 |
|------|------|----------|
| `/search/quickSearch` | 搜索产品/媒体/发行商 | `searchKey, top, isHighLight` |

搜索结果字段为 `id` 和 `name`，`id` 可作为产品端点的 `productId` 使用。

## 认证流程

1. `GET /user/getVerifyCode` → PNG 验证码图片（80×30，4 位字母数字）
2. `POST /user/login`：`accountId`（邮箱）+ `password`（MD5）+ `vCode`（验证码）
3. 登录后从首页提取 `App.userKey` 作为后续请求的 token
4. Session 缓存到 `~/.cache/adxray_session.json`，1 小时内免重复登录

## API 签名算法

每个数据请求需要 `sign` 和 `token` 参数：

1. 收集所有请求参数（排除 sign 和 token）
2. 按 key 字母序排列，拼为 `key1=value1&key2=value2&...`
3. 末尾追加 `&key=g:%w0k7&q1v9^tRnLz!M`
4. MD5 → 转大写 = `sign`
5. `token` = 登录后获取的 `App.userKey`

POST 请求需要请求头 `s: MD5(今日日期 "YYYY/M/D")`，以及 `thisTimes: int(time()*1000/100)` 参数。

## 常见问题

- **509 "Error Null sign"**：sign 计算错误，检查参数排序和密钥拼接
- **412 "验证码错误"**：验证码 OCR 失败或 session 过期，自动重试
- **401 "Unauthorized"**：session 过期，删除缓存文件重新登录
- **404**：部分旧接口已下线，使用 V2 版本

## 安全约束

1. 此工具仅做只读查询，无写入操作
2. 完成后直接输出结果
