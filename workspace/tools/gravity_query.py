#!/usr/bin/env python3
# requires: pycryptodome
"""
引力引擎排行榜数据查询工具
支持微信/抖音小游戏的排行榜、App趋势、搜索、竞品分析
"""

import argparse
import base64
import hashlib
import json
import os
import random
import sys
import time
from pathlib import Path

import requests

_PROJECT_ROOT = Path(__file__).resolve().parent.parent
if str(_PROJECT_ROOT) not in sys.path:
    sys.path.insert(0, str(_PROJECT_ROOT))

from tools.utils import (
    get_injected_credential,
    save_user_credentials,
    sync_credential_env,
    clear_user_credentials,
    clear_credential_env,
)

API_BASE = "https://api-insight.gravity-engine.com"
APPRANK_V1 = f"{API_BASE}/apprank/api/v1"
ACCOUNT_V1 = f"{API_BASE}/account_center/api/v1"

# ---------------------------------------------------------------------------
# 加密 / 解密
# ---------------------------------------------------------------------------

def _make_request_headers(body_str: str, jwt: str) -> tuple[dict, str]:
    """构造带加密签名的请求头，返回 (headers, aes_key)"""
    ts = int(time.time() * 1000)
    ts_str = str(ts)
    rand_hex = "".join(random.choice("0123456789abcdef") for _ in range(5))
    g = "etg" + rand_hex
    session = base64.b64encode(g.encode()).decode()

    sig_input = ts_str[3:8] + "11" + session + body_str
    signature = hashlib.md5(sig_input.encode()).hexdigest()
    aes_key = g + "gv" + ts_str[7:11] + "00"  # 16 bytes → AES-128

    headers = {
        "accept": "application/json, text/plain, */*",
        "content-type": "application/json",
        "Authorization": jwt,
        "origin": "https://rank.gravity-engine.com",
        "user-agent": "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) "
                      "AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36",
        "gravity-timestamp": ts_str,
        "gravity-session": session,
        "gravity-signature": signature,
    }
    return headers, aes_key


def _decrypt_response(encrypted_text: str, aes_key: str) -> dict:
    """AES-128-ECB 解密响应"""
    from Crypto.Cipher import AES
    from Crypto.Util.Padding import unpad

    key_bytes = aes_key.encode("utf-8")[:16]
    cipher = AES.new(key_bytes, AES.MODE_ECB)
    raw = base64.b64decode(encrypted_text)
    decrypted = unpad(cipher.decrypt(raw), AES.block_size)
    return json.loads(decrypted.decode("utf-8"))


def _api_post(path: str, body: dict, jwt: str) -> dict:
    """发起加密 POST 请求并解密响应"""
    body_str = json.dumps(body, separators=(",", ":"), ensure_ascii=False)
    headers, aes_key = _make_request_headers(body_str, jwt)

    resp = requests.post(f"{APPRANK_V1}{path}", headers=headers, data=body_str.encode("utf-8"), timeout=15)
    resp.raise_for_status()
    data = resp.json()

    if data.get("code") != 0:
        extra = data.get("extra") or {}
        raise RuntimeError(f"API error {data.get('code')}: {data.get('msg')} "
                           f"{extra.get('error', '')}")

    payload = data.get("data", {})
    if isinstance(payload, dict) and "text" in payload:
        return _decrypt_response(payload["text"], aes_key)
    return payload

# ---------------------------------------------------------------------------
# 凭证管理
# ---------------------------------------------------------------------------

def _get_jwt() -> str:
    jwt = get_injected_credential("gravity", "jwt")
    if not jwt:
        print("Error: Gravity Engine credentials not configured. "
              "Run: python3 tools/gravity_query.py send-code --phone <phone>", file=sys.stderr)
        sys.exit(1)
    expire_ms = get_injected_credential("gravity", "expire_time") or "0"
    if int(expire_ms) / 1000 < time.time():
        print("Error: Gravity Engine JWT has expired. Please re-login with send-code + login.", file=sys.stderr)
        sys.exit(1)
    return jwt

# ---------------------------------------------------------------------------
# 子命令
# ---------------------------------------------------------------------------

def cmd_send_code(args):
    """发送短信验证码"""
    body = {"username": args.phone, "action_type": "login", "code_type": "phone"}
    resp = requests.post(
        f"{ACCOUNT_V1}/get_verify_code/v2/",
        headers={"content-type": "application/json", "origin": "https://rank.gravity-engine.com"},
        json=body, timeout=10,
    )
    data = resp.json()
    if data.get("code") == 0:
        print(json.dumps({"status": "ok", "message": f"Verification code sent to {args.phone}"},
                          ensure_ascii=False))
    else:
        print(json.dumps({"status": "error", "message": data.get("msg", "Unknown error")},
                          ensure_ascii=False))
        sys.exit(1)


def cmd_login(args):
    """用验证码登录"""
    body = {
        "action_type": "phone",
        "username": args.phone,
        "code": int(args.code),
        "product_name": "turbo",
        "free_login_day": 7,
    }
    resp = requests.post(
        f"{ACCOUNT_V1}/user_login/v2/",
        headers={"content-type": "application/json", "origin": "https://rank.gravity-engine.com"},
        json=body, timeout=10,
    )
    data = resp.json()
    if data.get("code") != 0:
        print(json.dumps({"status": "error", "message": data.get("msg", "Login failed")},
                          ensure_ascii=False))
        sys.exit(1)

    user = data["data"]["user"]
    jwt = user["Authorization"]
    free_days = data["data"].get("day", 7)
    expire_time_ms = str(int((time.time() + free_days * 86400) * 1000))

    staff_id = os.environ.get("TYCLAW_SENDER_STAFF_ID", "local")
    cred_data = {"phone": args.phone, "jwt": jwt, "expire_time": expire_time_ms}
    save_user_credentials(staff_id, "gravity", cred_data)
    sync_credential_env("gravity", cred_data)

    print(json.dumps({
        "status": "ok",
        "message": f"Logged in as {user.get('name', args.phone)}, "
                   f"JWT valid for {free_days} days",
        "company": user.get("company_name", ""),
    }, ensure_ascii=False))


def cmd_clear_credentials(args):
    staff_id = os.environ.get("TYCLAW_SENDER_STAFF_ID", "local")
    changed = clear_user_credentials(staff_id, "gravity")
    clear_credential_env("gravity")
    print(json.dumps({"status": "ok", "cleared": changed}, ensure_ascii=False))


def _resolve_rank_date(period, explicit_date, rank_type, rank_genre, jwt):
    """周榜/月榜未指定日期时，从 public_list 获取最新可用日期"""
    if explicit_date:
        return explicit_date
    if period == "day":
        from datetime import date as _date
        return _date.today().isoformat()
    probe = {
        "page": 1, "page_size": 1,
        "filters": [
            {"field": "period_type", "operator": 1, "values": [period]},
            {"field": "rank_type", "operator": 1, "values": [rank_type]},
            {"field": "rank_genre", "operator": 1, "values": [rank_genre]},
        ]
    }
    data = _api_post("/rank/public_list/", probe, jwt)
    items = data.get("list", [])
    if items:
        return items[0].get("stat_datetime", "")
    from datetime import date as _date
    return _date.today().isoformat()


def cmd_rank(args):
    """查询排行榜"""
    jwt = _get_jwt()

    period = getattr(args, "period", None) or "day"
    query_date = _resolve_rank_date(period, args.date, args.rank_type, args.rank_genre, jwt)

    filters = [
        {"field": "stat_datetime", "operator": 1, "values": [query_date]},
        {"field": "period_type", "operator": 1, "values": [period]},
        {"field": "rank_type", "operator": 1, "values": [args.rank_type]},
        {"field": "rank_genre", "operator": 1, "values": [args.rank_genre]},
    ]
    if args.game_type:
        filters.append({"field": "game_type_main_name", "operator": 1, "values": [args.game_type]})

    body = {
        "page": args.page,
        "page_size": args.page_size,
        "extra_fields": {"change_label": True, "app_genre_ranking": True},
        "filters": filters,
    }

    data = _api_post("/rank/list/", body, jwt)
    items = data.get("list", [])
    page_info = data.get("page_info", {})

    if args.format == "json":
        print(json.dumps({"page_info": page_info, "list": items}, ensure_ascii=False, indent=2))
        return

    period_label = {"day": "日榜", "week": "周榜", "month": "月榜"}.get(period, period)
    _print_rank_table(items, args.rank_type, args.rank_genre)
    total = page_info.get("total_number", len(items))
    print(f"\n{period_label} | 数据日期 {query_date} | 共 {total} 条 | 第 {args.page}/{page_info.get('total_page', '?')} 页")


def cmd_trend(args):
    """查询 App 排名趋势"""
    jwt = _get_jwt()

    rank_type_list = args.rank_type_list.split(",") if args.rank_type_list else ["popularity", "bestseller"]

    body = {
        "app_id": args.app_id,
        "date_list": [args.start_date, args.end_date],
        "rank_genre": args.rank_genre,
        "rank_type_list": rank_type_list,
    }
    data = _api_post("/rank/app_trend/public_list/", body, jwt)

    if args.format == "json":
        print(json.dumps(data, ensure_ascii=False, indent=2))
        return

    trends = data.get("list", {})
    app_info = data.get("app_info", {})
    app_name = app_info.get("app_name", str(args.app_id))
    print(f"## {app_name} 排名趋势\n")

    type_names = {"popularity": "人气榜", "bestseller": "畅销榜",
                  "most_played": "畅玩榜", "fresh_game": "新游榜"}

    for rt, entries in trends.items():
        if not entries:
            continue
        label = type_names.get(rt, rt)
        print(f"### {label}\n")
        print("| 日期 | 排名 | 变化 |")
        print("|------|------|------|")
        for e in entries:
            rank = e.get("ranking", "-")
            if rank == 999:
                rank = "未上榜"
            diff = e.get("ranking_diff", 0)
            diff_str = f"+{diff}" if diff > 0 else str(diff) if diff < 0 else "-"
            print(f"| {e['stat_datetime']} | {rank} | {diff_str} |")
        print()


def cmd_search(args):
    """搜索 App"""
    jwt = _get_jwt()

    filters = [{"field": "name", "operator": 8, "values": [args.keyword]}]  # 8=contains
    if args.app_os:
        filters.append({"field": "app_os", "operator": 1, "values": [int(args.app_os)]})

    body = {
        "filters": filters,
        "page": args.page,
        "page_size": args.page_size,
    }
    data = _api_post("/app/public_list/", body, jwt)

    if args.format == "json":
        print(json.dumps(data, ensure_ascii=False, indent=2))
        return

    items = data.get("list", [])
    page_info = data.get("page_info", {})

    if not items:
        print("未找到匹配的应用")
        return

    print("| # | App ID | 名称 | 发行商 | 平台 | 分类 |")
    print("|---|--------|------|--------|------|------|")
    for idx, app in enumerate(items, 1):
        os_label = {3: "微信", 6: "抖音"}.get(app.get("app_os"), str(app.get("app_os", "")))
        genre = app.get("game_type_main_name", "")
        sub = app.get("game_type_sub_name", "")
        genre_str = f"{genre}/{sub}" if sub else genre
        print(f"| {idx} | {app.get('id', '')} | {app.get('name', '')} | "
              f"{app.get('publisher_name', '-')} | {os_label} | {genre_str} |")

    print(f"\n共 {page_info.get('total_number', '?')} 条 | 第 {args.page} 页")


def cmd_publisher(args):
    """搜索发行商"""
    jwt = _get_jwt()

    filters = [{"field": "name", "operator": 8, "values": [args.keyword]}]
    body = {"filters": filters, "page": args.page, "page_size": args.page_size}
    data = _api_post("/publisher/public_list/", body, jwt)

    if args.format == "json":
        print(json.dumps(data, ensure_ascii=False, indent=2))
        return

    items = data.get("list", [])
    if not items:
        print("未找到匹配的发行商")
        return

    print("| # | ID | 发行商 | 应用数 |")
    print("|---|------|--------|--------|")
    for idx, p in enumerate(items, 1):
        print(f"| {idx} | {p.get('id', '')} | {p.get('name', '')} | {p.get('app_count', 0)} |")


def cmd_competition(args):
    """竞品趋势"""
    jwt = _get_jwt()

    body = {
        "app_id": args.app_id,
        "rank_type": args.rank_type,
        "rank_genre": args.rank_genre,
        "date_list": [args.start_date, args.end_date],
    }
    data = _api_post("/rank/competition_trends/", body, jwt)

    if args.format == "json":
        print(json.dumps(data, ensure_ascii=False, indent=2))
        return

    date_list = data.get("date_list", [])
    trends = data.get("trends", {})

    if not trends:
        print("无竞品趋势数据")
        return

    # 取最近一天的排名列表
    latest_date = date_list[-1] if date_list else ""
    latest_items = trends.get(latest_date, [])

    type_names = {"popularity": "人气榜", "bestseller": "畅销榜",
                  "most_played": "畅玩榜", "fresh_game": "新游榜"}
    rtype = type_names.get(args.rank_type, args.rank_type)
    print(f"## 竞品趋势 - {rtype}（{latest_date}）\n")

    print("| 排名 | 应用名 | 发行商 | 变化 |")
    print("|------|--------|--------|------|")
    for item in latest_items[:args.top]:
        info = item.get("app_info", {})
        change = item.get("change", 0)
        change_str = f"+{change}" if change > 0 else str(change) if change < 0 else "-"
        print(f"| {item.get('ranking', '-')} | {info.get('app_name', '?')} | "
              f"{info.get('publisher_name', '-')} | {change_str} |")


# ---------------------------------------------------------------------------
# 格式化辅助
# ---------------------------------------------------------------------------

def _print_rank_table(items: list, rank_type: str, rank_genre: str):
    type_names = {"popularity": "人气榜", "bestseller": "畅销榜",
                  "most_played": "畅玩榜", "fresh_game": "新游榜"}
    genre_names = {"wx_minigame": "微信小游戏", "dy_minigame": "抖音小游戏"}
    title = f"{genre_names.get(rank_genre, rank_genre)} {type_names.get(rank_type, rank_type)}"
    print(f"## {title}\n")

    if not items:
        print("暂无数据")
        return

    print("| 排名 | 应用名 | 发行商 | 分类 | 变化 | 霸榜 |")
    print("|------|--------|--------|------|------|------|")
    for item in items:
        info = item.get("app_info", {})
        change = item.get("change", 0)
        change_str = f"↑{abs(change)}" if change < 0 else f"↓{change}" if change > 0 else "-"
        label = item.get("change_label") or {}
        badge = label.get("first_msg", "")
        genre = info.get("game_type_main_name", "")
        sub = info.get("game_type_sub_name", "")
        genre_str = f"{genre}/{sub}" if sub else genre
        print(f"| {item.get('ranking', '-')} | {info.get('app_name', '?')} | "
              f"{info.get('publisher_name', '-')} | {genre_str} | {change_str} | {badge} |")


# ---------------------------------------------------------------------------
# CLI 入口
# ---------------------------------------------------------------------------

def main():
    parser = argparse.ArgumentParser(description="引力引擎排行榜查询工具")
    parser.add_argument("--format", choices=["markdown", "json"], default="markdown")
    subs = parser.add_subparsers(dest="command")

    # --- send-code ---
    p_sc = subs.add_parser("send-code", help="发送短信验证码")
    p_sc.add_argument("--phone", required=True, help="手机号")
    p_sc.set_defaults(func=cmd_send_code)

    # --- login ---
    p_login = subs.add_parser("login", help="用验证码登录")
    p_login.add_argument("--phone", required=True, help="手机号")
    p_login.add_argument("--code", required=True, help="短信验证码")
    p_login.set_defaults(func=cmd_login)

    # --- clear-credentials ---
    p_cc = subs.add_parser("clear-credentials", help="清除已保存的凭证")
    p_cc.set_defaults(func=cmd_clear_credentials)

    # --- rank ---
    p_rank = subs.add_parser("rank", help="查询排行榜")
    p_rank.add_argument("--rank-type", default="popularity",
                        choices=["popularity", "bestseller", "most_played", "fresh_game"],
                        help="榜单类型")
    p_rank.add_argument("--rank-genre", default="wx_minigame",
                        choices=["wx_minigame", "dy_minigame"],
                        help="平台")
    p_rank.add_argument("--page", type=int, default=1)
    p_rank.add_argument("--page-size", type=int, default=20)
    p_rank.add_argument("--period", default="day",
                        choices=["day", "week", "month"],
                        help="榜单周期：day=日榜 week=周榜 month=月榜")
    p_rank.add_argument("--date", help="日期 YYYY-MM-DD，默认今天")
    p_rank.add_argument("--game-type", help="游戏分类筛选，如 休闲、竞技、棋牌")
    p_rank.set_defaults(func=cmd_rank)

    # --- trend ---
    p_trend = subs.add_parser("trend", help="查询 App 排名趋势")
    p_trend.add_argument("--app-id", type=int, required=True, help="App ID")
    p_trend.add_argument("--start-date", required=True, help="开始日期 YYYY-MM-DD")
    p_trend.add_argument("--end-date", required=True, help="结束日期 YYYY-MM-DD")
    p_trend.add_argument("--rank-genre", default="wx_minigame",
                         choices=["wx_minigame", "dy_minigame"])
    p_trend.add_argument("--rank-type-list", default="popularity,bestseller",
                         help="榜单类型，逗号分隔")
    p_trend.set_defaults(func=cmd_trend)

    # --- search ---
    p_search = subs.add_parser("search", help="搜索应用")
    p_search.add_argument("--keyword", required=True, help="搜索关键词")
    p_search.add_argument("--app-os", choices=["3", "6"], default="",
                          help="平台筛选：3=微信, 6=抖音")
    p_search.add_argument("--page", type=int, default=1)
    p_search.add_argument("--page-size", type=int, default=20)
    p_search.set_defaults(func=cmd_search)

    # --- publisher ---
    p_pub = subs.add_parser("publisher", help="搜索发行商")
    p_pub.add_argument("--keyword", required=True, help="搜索关键词")
    p_pub.add_argument("--page", type=int, default=1)
    p_pub.add_argument("--page-size", type=int, default=20)
    p_pub.set_defaults(func=cmd_publisher)

    # --- competition ---
    p_comp = subs.add_parser("competition", help="竞品趋势")
    p_comp.add_argument("--app-id", type=int, required=True, help="App ID（作为基准）")
    p_comp.add_argument("--rank-type", default="popularity",
                        choices=["popularity", "bestseller", "most_played", "fresh_game"])
    p_comp.add_argument("--rank-genre", default="wx_minigame",
                        choices=["wx_minigame", "dy_minigame"])
    p_comp.add_argument("--start-date", required=True, help="开始日期 YYYY-MM-DD")
    p_comp.add_argument("--end-date", required=True, help="结束日期 YYYY-MM-DD")
    p_comp.add_argument("--top", type=int, default=20, help="显示前N名")
    p_comp.set_defaults(func=cmd_competition)

    args = parser.parse_args()
    if not args.command:
        parser.print_help()
        sys.exit(1)
    args.func(args)


if __name__ == "__main__":
    main()
