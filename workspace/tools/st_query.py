#!/usr/bin/env python3
"""Sensor Tower 数据查询（代理客户端）
实际 API 调用由 bot 侧代理完成（含缓存 + 频率限制），本工具仅解析 CLI 参数并转发。
"""

import argparse
import json
import os
import sys

import requests

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from utils import save_user_credentials, sync_credential_env, \
    clear_user_credentials, clear_credential_env

_PROXY_URL = os.environ.get("_TYCLAW_ST_PROXY_URL", "")
_PROXY_TOKEN = os.environ.get("_TYCLAW_ST_PROXY_TOKEN", "")


def _proxy_call(action: str, **params) -> dict:
    if not _PROXY_URL:
        print("Error: _TYCLAW_ST_PROXY_URL not set. "
              "Sensor Tower queries require the bot-side proxy.",
              file=sys.stderr)
        sys.exit(1)
    resp = requests.post(_PROXY_URL, json={
        "token": _PROXY_TOKEN,
        "action": action,
        **params,
    }, timeout=120)
    try:
        data = resp.json()
    except Exception:
        print(f"Error: proxy returned non-JSON: {resp.status_code} {resp.text[:500]}",
              file=sys.stderr)
        sys.exit(1)
    if data.get("error"):
        print(json.dumps(data, ensure_ascii=False, indent=2), file=sys.stderr)
        sys.exit(1)
    return data


def _cmd_setup(args):
    staff_id = os.environ.get("TYCLAW_SENDER_STAFF_ID", "")
    if not staff_id:
        print("Error: TYCLAW_SENDER_STAFF_ID env var required", file=sys.stderr)
        sys.exit(1)
    data = {"token": args.token}
    save_user_credentials(staff_id, "st", data)
    sync_credential_env("st", data)
    print(f"Sensor Tower API token saved for {staff_id}")


def _cmd_clear_credentials(_args):
    staff_id = os.environ.get("TYCLAW_SENDER_STAFF_ID", "")
    if not staff_id:
        print("Error: TYCLAW_SENDER_STAFF_ID env var required", file=sys.stderr)
        sys.exit(1)
    if clear_user_credentials(staff_id, "st"):
        clear_credential_env("st")
        print(f"Sensor Tower credentials cleared for {staff_id}")
    else:
        print(f"No Sensor Tower credentials found for {staff_id}")


def main():
    parser = argparse.ArgumentParser(description="Sensor Tower data query")
    sub = parser.add_subparsers(dest="action", required=True)

    # ── setup / clear ──
    p_setup = sub.add_parser("setup", help="Set Sensor Tower API token")
    p_setup.add_argument("--token", required=True,
                         help="API token from Sensor Tower dashboard")
    sub.add_parser("clear-credentials",
                   help="Clear Sensor Tower credentials")

    # ── search ──
    p_search = sub.add_parser("search",
                              help="Search apps or publishers by name")
    p_search.add_argument("--term", required=True, help="Search keyword")
    p_search.add_argument("--entity-type", default="app",
                          choices=["app", "publisher"],
                          help="Entity type (default: app)")
    p_search.add_argument("--os", default="unified",
                          choices=["ios", "android", "unified"],
                          dest="app_store",
                          help="App store (default: unified)")
    p_search.add_argument("--limit", type=int, default=10)

    # ── sales ──
    p_sales = sub.add_parser("sales",
                             help="Download & revenue estimates for apps")
    p_sales.add_argument("--app-ids", required=True,
                         help="App IDs (comma-separated)")
    p_sales.add_argument("--os", default="unified",
                         choices=["ios", "android", "unified"])
    p_sales.add_argument("--countries", default="WW",
                         help="Country codes, comma-separated (default: WW)")
    p_sales.add_argument("--start-date", required=True,
                         help="Start date (YYYY-MM-DD)")
    p_sales.add_argument("--end-date", required=True,
                         help="End date (YYYY-MM-DD)")
    p_sales.add_argument("--date-granularity", default="daily",
                         choices=["daily", "weekly", "monthly", "quarterly"])

    # ── top-charts ──
    p_top = sub.add_parser("top-charts",
                           help="Top apps by revenue, downloads, or active users")
    p_top.add_argument("--os", default="unified",
                       choices=["ios", "android", "unified"])
    p_top.add_argument("--measure", default="revenue",
                       choices=["revenue", "units", "DAU", "WAU", "MAU"],
                       help="Metric to rank by (default: revenue)")
    p_top.add_argument("--category", required=True,
                       help="Category ID (use st_categories for lookup)")
    p_top.add_argument("--regions", default="WW",
                       help="Region codes, comma-separated (default: WW)")
    p_top.add_argument("--time-range", default="month",
                       choices=["day", "week", "month", "quarter"])
    p_top.add_argument("--date",
                       help="Start date (YYYY-MM-DD, default: current month)")
    p_top.add_argument("--limit", type=int, default=20)
    p_top.add_argument("--comparison-attribute", default="absolute",
                       choices=["absolute", "delta", "transformed_delta"])

    # ── app-info ──
    p_info = sub.add_parser("app-info",
                            help="App details and metadata")
    p_info.add_argument("--app-ids", required=True,
                        help="App IDs (comma-separated)")
    p_info.add_argument("--os", default="unified",
                        choices=["ios", "android", "unified"])

    # ── usage ──
    p_usage = sub.add_parser("usage",
                             help="Active user metrics (DAU/WAU/MAU)")
    p_usage.add_argument("--app-ids", required=True,
                         help="App IDs (comma-separated)")
    p_usage.add_argument("--os", default="unified",
                         choices=["ios", "android", "unified"])
    p_usage.add_argument("--countries", default="WW",
                         help="Country codes, comma-separated (default: WW)")
    p_usage.add_argument("--start-date", required=True,
                         help="Start date (YYYY-MM-DD)")
    p_usage.add_argument("--end-date", required=True,
                         help="End date (YYYY-MM-DD)")
    p_usage.add_argument("--date-granularity", default="monthly",
                         choices=["daily", "weekly", "monthly", "quarterly"])

    args = parser.parse_args()

    if args.action == "setup":
        _cmd_setup(args)
        return
    if args.action == "clear-credentials":
        _cmd_clear_credentials(args)
        return

    # 构建代理调用参数
    params: dict = {}

    if args.action == "search":
        params = {
            "term": args.term,
            "entity_type": args.entity_type,
            "app_store": args.app_store,
            "limit": args.limit,
        }

    elif args.action == "sales":
        params = {
            "os": args.os,
            "app_ids": args.app_ids,
            "countries": args.countries,
            "start_date": args.start_date,
            "end_date": args.end_date,
            "date_granularity": args.date_granularity,
        }

    elif args.action == "top-charts":
        params = {
            "os": args.os,
            "measure": args.measure,
            "category": args.category,
            "regions": args.regions,
            "time_range": args.time_range,
            "comparison_attribute": args.comparison_attribute,
            "limit": args.limit,
        }
        if args.date:
            params["date"] = args.date

    elif args.action == "app-info":
        params = {
            "os": args.os,
            "app_ids": args.app_ids,
        }

    elif args.action == "usage":
        params = {
            "os": args.os,
            "app_ids": args.app_ids,
            "countries": args.countries,
            "start_date": args.start_date,
            "end_date": args.end_date,
            "date_granularity": args.date_granularity,
        }

    result = _proxy_call(args.action, **params)
    json.dump(result, sys.stdout, ensure_ascii=False, indent=2)
    print()


if __name__ == "__main__":
    main()
