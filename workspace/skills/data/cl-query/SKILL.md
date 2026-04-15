---
name: 创量智投数据查询
description: 查询创量智投平台的媒体账户、素材库、巨量广告项目列表与状态变更
triggers:
  - 创量
  - 创量智投
  - 媒体账户
  - 广告账户
  - 广告项目
  - 广告计划
  - 投放状态
  - 素材库
  - 巨量广告
  - 腾讯广告
  - 磁力智投
  - 千川
  - 设置创量
  - 设置创量凭证
  - 创量账号
tool: tools/cl_query.py
default: false
credentials:
  key: cl
  display_name: 创量智投
  setup: manual
  fields:
    - {name: email, label: 创量智投登录邮箱}
    - {name: password, label: 登录密码, secret: true}
---
# 创量智投数据查询

通过创量智投 (Chuangliang) 平台 API 查询媒体账户、素材库和巨量广告投放数据。

> 本文件位于 `skills/cl-query/`，下文中的相对路径均基于项目根目录。

## 前置依赖

```bash
pip install requests
```

## 凭证配置

使用前需先设置创量凭证（创量平台的登录邮箱和密码）：

```bash
python3 tools/cl_query.py setup --email "user@tuyoogame.com" --password "your-password"
```

清除凭证：

```bash
python3 tools/cl_query.py clear-credentials
```

凭证通过环境变量 `_TYCLAW_CL_EMAIL` / `_TYCLAW_CL_PASSWORD` 自动注入，也可通过 `--email` / `--password` 命令行参数手动传入。

当用户说「设置创量凭证」「创量账号」时，询问用户提供创量智投平台 (cl.mobgi.com) 的登录邮箱和密码，然后调用 `setup`。

## 工具用法

`tools/cl_query.py`（从 TyClaw 项目根目录运行）

### 账户管理

```bash
# 列出所有媒体账户
python3 tools/cl_query.py accounts --media-type all

# 按媒体类型筛选
python3 tools/cl_query.py accounts --media-type toutiao

# 查看当前用户菜单
python3 tools/cl_query.py menu

# 查看优化师列表
python3 tools/cl_query.py users

# 查看支持的媒体类型
python3 tools/cl_query.py media_types
```

### 账户报表（含预算/余额/消耗/ROI）

```bash
# 巨量广告全部账户（当日）
python3 tools/cl_query.py account_report --media-type toutiao_upgrade

# 按广告主 ID 搜索
python3 tools/cl_query.py account_report --keyword 1840847899078345 --search-field advertiser_id

# 腾讯广告账户
python3 tools/cl_query.py account_report --media-type gdt_upgrade

# 按账户名称搜索
python3 tools/cl_query.py account_report --keyword "三国冰河" --search-field advertiser_nick

# 指定日期
python3 tools/cl_query.py account_report --start-date 2026-04-01 --end-date 2026-04-12
```

> **巨量**返回：`budget`（预算）、`balance`（余额）、`stat_cost`（消耗）、`attribution_game_in_app_ltv_1day`（当日付费金额）、`attribution_game_in_app_roi_1day`（当日付费ROI）等。
> **腾讯**返回：`daily_budget`（日预算）、`balance`（余额）、`cost`（消耗）、`first_day_pay_amount`（首日付费金额）、`roi_activated_d1/d3/d7`（首日/3日/7日ROI）等。

### 素材管理

```bash
# 素材列表
python3 tools/cl_query.py materials --page 1 --page-size 20

# 按分组查看
python3 tools/cl_query.py materials --group-id 12345

# 搜索素材
python3 tools/cl_query.py material_search --keyword "视频名称"
```

### 素材报表

```bash
# 全媒体汇总（默认），当日数据
python3 tools/cl_query.py material_report --days 1

# 巨量广告素材
python3 tools/cl_query.py material_report --media-type toutiao_upgrade --days 7

# 腾讯广告素材
python3 tools/cl_query.py material_report --media-type gdt_upgrade --days 7

# 按素材名称搜索
python3 tools/cl_query.py material_report --keyword "翊" --days 1

# 指定日期范围
python3 tools/cl_query.py material_report --start-date 2026-04-01 --end-date 2026-04-09
```

> 返回字段包含 `material_name`、`material_id`、消耗（巨量为 `stat_cost`，汇总/腾讯为 `cost`）等。

### 巨量广告项目列表

```bash
# 最近 7 天的项目
python3 tools/cl_query.py projects

# 最近 30 天
python3 tools/cl_query.py projects --days 30

# 指定日期范围
python3 tools/cl_query.py projects --start-date 2026-03-01 --end-date 2026-04-08

# 按名称搜索
python3 tools/cl_query.py projects --keyword "三国"

# 按状态筛选（enable=投放中, disable=已暂停）
python3 tools/cl_query.py projects --status enable
```

### 腾讯广告列表

```bash
# 最近 7 天
python3 tools/cl_query.py gdt_ads

# 最近 30 天
python3 tools/cl_query.py gdt_ads --days 30

# 指定日期范围
python3 tools/cl_query.py gdt_ads --start-date 2026-03-01 --end-date 2026-04-08
```

### 批量修改腾讯广告投放日期

```bash
# adgroup-ids 和 advertiser-ids 需一一对应（从 ads --media-type gdt 获取）
python3 tools/cl_query.py gdt_update_dates \
  --adgroup-ids 91883998316,91883996686 \
  --advertiser-ids 64411558,64411593 \
  --begin-date 2026-04-09 --end-date 2026-04-30
```

### 批量修改巨量广告项目投放时段

```bash
# 指定时段：周二到周四 05:00-10:30
python3 tools/cl_query.py toutiao_update_project_schedule \
  --project-ids 7623633328285646867,7623633341118119974 \
  --media-account-ids 12445101462,12445101457 \
  --schedule "tue-thu:05:00-10:30"

# 多段时间
python3 tools/cl_query.py toutiao_update_project_schedule \
  --project-ids <ids> --media-account-ids <mids> \
  --schedule "mon-fri:09:00-12:00,sat:10:00-14:00"

# 不限时段
python3 tools/cl_query.py toutiao_update_project_schedule \
  --project-ids <ids> --media-account-ids <mids> \
  --no-limit
```

> **时段格式**: `day_range:HH:MM-HH:MM`，多段逗号分隔。day_range 可以是 `mon`/`tue`/.../`sun`、`mon-fri` 范围、或 `all`。时间精度 30 分钟。`--no-limit` 表示全时段不限。
> project-ids 和 media-account-ids 从 `projects` 命令获取（`project_id` 和 `media_account_id` 字段），需一一对应。

### 批量修改巨量广告账户预算

```bash
# 日预算（默认）
python3 tools/cl_query.py toutiao_update_budget \
  --media-account-ids 12445101462,12445101457 \
  --budgets 15000,20000

# 总预算
python3 tools/cl_query.py toutiao_update_budget \
  --media-account-ids 12445101462 \
  --budgets 50000 --budget-mode BUDGET_MODE_TOTAL
```

> `--budgets 0` 表示不限预算。media-account-ids 从 `accounts` 或 `projects` 获取。

### 批量修改腾讯广告账户日预算

```bash
python3 tools/cl_query.py gdt_update_budget \
  --media-account-ids 12537086110,12537086111 \
  --daily-budgets 11111,22222
```

> `--daily-budgets 0` 表示不限预算。

### 巨量广告项目状态变更

```bash
# 启用项目
python3 tools/cl_query.py update_status --ids 7623633328285646867 --status enable

# 暂停项目（多个用逗号分隔）
python3 tools/cl_query.py update_status --ids ID1,ID2,ID3 --status disable
```

> 项目 ID 从 `projects` 命令获取（`project_id` 字段）。

### 站内信

```bash
python3 tools/cl_query.py messages --page 1 --page-size 10
```

### 参数速查

全局参数：

| 参数 | 说明 |
|---|---|
| `--email` | 登录邮箱（可选，优先于环境变量） |
| `--password` | 登录密码（可选，优先于环境变量） |

通用参数：

| 参数 | 说明 | 默认值 |
|---|---|---|
| `--page` | 页码 | 1 |
| `--page-size` | 每页条数 | 20 |
| `--days` | 查询天数（projects 用） | 7 |
| `--start-date` | 开始日期（覆盖 --days） | - |
| `--end-date` | 结束日期 | - |

## 支持的媒体类型

| media_type | 中文名 |
|---|---|
| toutiao | 巨量广告 |
| kuaishou | 磁力智投 |
| gdt | 腾讯广告 |
| baidu | 百度信息流 |
| baidu_search | 百度搜索 |
| bilibili | B站营销 |
| jinniu | 磁力金牛 |
| baidushop | 百度电商 |
| qianchuan_universe | 巨量千川(全域) |
| localads | 巨量本地推 |
| redbook | 小红书 |

## 认证流程

1. `POST /User/AdminUser/loginInfo` → 预登录，获取可用 product_version
2. `POST /User/AdminUser/login` → 正式登录，获取 session cookie
3. 密码需 MD5 加密后传输
4. Session 通过 cookie（`chuangliang_session` + `userId`）维持
5. Session 缓存到 `~/.cache/cl_session.json`，2 小时内免重复登录
