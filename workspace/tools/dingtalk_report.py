"""
钉钉日志（Report）工具 — 只读
通过钉钉旧版 OAPI 查询日志：模板、日志列表、统计、评论等

用法示例:
  python tools/dingtalk_report.py my-scope
  python tools/dingtalk_report.py list-templates
  python tools/dingtalk_report.py get-template --name 日报
  python tools/dingtalk_report.py list-reports --days 7
  python tools/dingtalk_report.py list-reports --department --days 7
  python tools/dingtalk_report.py get-statistics --report-id <id>
  python tools/dingtalk_report.py list-related-users --report-id <id> --type 0
  python tools/dingtalk_report.py list-receivers --report-id <id>
  python tools/dingtalk_report.py get-comments --report-id <id>
  python tools/dingtalk_report.py get-unread
"""

import argparse
import json
import os
import sys
import time

import requests

from utils import load_config, format_json

OAPI_BASE = "https://oapi.dingtalk.com"

_PROXY_URL = os.environ.get("_TYCLAW_DT_PROXY_URL", "")
_PROXY_TOKEN = os.environ.get("_TYCLAW_DT_PROXY_TOKEN", "")


# ---------------------------------------------------------------------------
# HTTP helpers
# ---------------------------------------------------------------------------

def _handle_response(resp):
    if resp.ok:
        if resp.status_code == 204 or not resp.text:
            return {}
        data = resp.json()
        if data.get("errcode", 0) != 0:
            print(json.dumps({"error": True, "errcode": data.get("errcode"),
                              "errmsg": data.get("errmsg", "")},
                             ensure_ascii=False, indent=2), file=sys.stderr)
            sys.exit(1)
        return data
    try:
        err = resp.json()
    except Exception:
        err = {"status": resp.status_code, "body": resp.text}
    print(json.dumps({"error": True, **err}, ensure_ascii=False, indent=2), file=sys.stderr)
    sys.exit(1)


def _proxy_forward(path, data=None):
    resp = requests.post(_PROXY_URL, json={
        "token": _PROXY_TOKEN, "method": "POST",
        "path": path, "data": data,
    }, timeout=30)
    return _handle_response(resp)


def _get_oapi_token(config) -> str:
    """直连模式下获取旧版 access_token。"""
    dt = config.get("dingtalk", {})
    app_key = dt.get("client_id", "")
    app_secret = dt.get("client_secret", "")
    resp = requests.get(f"{OAPI_BASE}/gettoken",
                        params={"appkey": app_key, "appsecret": app_secret},
                        timeout=10)
    data = resp.json()
    if data.get("errcode") != 0:
        print(json.dumps({"error": True, "message": f"gettoken failed: {data}"},
                         ensure_ascii=False), file=sys.stderr)
        sys.exit(1)
    return data["access_token"]


def _oapi_post(config, path, data=None):
    if _PROXY_URL:
        return _proxy_forward(path, data=data)
    token = _get_oapi_token(config)
    resp = requests.post(f"{OAPI_BASE}{path}",
                         params={"access_token": token},
                         json=data, timeout=15)
    return _handle_response(resp)


def _get_userid() -> str:
    uid = os.environ.get("TYCLAW_SENDER_STAFF_ID", "")
    if not uid:
        print(json.dumps({"error": True, "message": "TYCLAW_SENDER_STAFF_ID not set"},
                         ensure_ascii=False), file=sys.stderr)
        sys.exit(1)
    return uid


# ---------------------------------------------------------------------------
# 子命令
# ---------------------------------------------------------------------------

def cmd_my_scope(config, args):
    """查询当前用户的日志查询权限范围"""
    uid = _get_userid()
    scope: dict = {"self": {"userid": uid}}
    if _PROXY_URL:
        try:
            info = _proxy_rpc("/api/report-dept-members")
            members = info.get("members", [])
            dept_names = info.get("dept_names", [])
            if members:
                scope["departments"] = dept_names
                scope["total_members"] = len(members)
                scope["members"] = members
        except SystemExit:
            pass
    print(format_json(scope))


def cmd_list_templates(config, args):
    """列出可用的日志模板"""
    body: dict = {"offset": args.offset or 0, "size": args.size or 50}
    if args.userid:
        body["userid"] = args.userid
    result = _oapi_post(config, "/topapi/report/template/listbyuserid", body)
    tpl_list = result.get("result", {}).get("template_list", [])
    print(format_json({"templates": tpl_list,
                       "next_cursor": result.get("result", {}).get("next_cursor")}))


def cmd_get_template(config, args):
    """获取模板详情（字段列表、默认接收人）"""
    userid = args.userid or _get_userid()
    result = _oapi_post(config, "/topapi/report/template/getbyname", {
        "template_name": args.name,
        "userid": userid,
    })
    detail = result.get("result", {})
    print(format_json(detail))


def _proxy_rpc(endpoint: str, payload: dict | None = None):
    """Call a Bot-side proxy RPC endpoint (e.g. /api/report-dept-members)."""
    if not _PROXY_URL:
        print(json.dumps({"error": True,
                          "message": "Proxy URL not configured, --department requires proxy mode"},
                         ensure_ascii=False), file=sys.stderr)
        sys.exit(1)
    base = _PROXY_URL.rsplit("/api/", 1)[0]
    url = f"{base}{endpoint}"
    body = {"token": _PROXY_TOKEN}
    if payload:
        body.update(payload)
    resp = requests.post(url, json=body, timeout=15)
    if not resp.ok:
        try:
            err = resp.json()
        except Exception:
            err = {"status": resp.status_code, "body": resp.text}
        print(json.dumps({"error": True, **err}, ensure_ascii=False, indent=2),
              file=sys.stderr)
        sys.exit(1)
    return resp.json()


def _query_reports_for_user(config, userid, args):
    """Query all reports for a single userid (auto-paginates).

    When template_name contains multiple values, queries each template separately
    and merges results (deduplicated by report_id).
    """
    now_ms = int(time.time() * 1000)
    days = args.days or 7
    start_time = args.start_time or (now_ms - days * 86400 * 1000)
    end_time = args.end_time or now_ms
    api_path = "/topapi/report/simplelist" if args.simple else "/topapi/report/list"

    template_names = args.template_name or [None]

    seen_ids: set = set()
    merged: list = []
    last_has_more = False
    last_cursor = None

    for tpl in template_names:
        cursor = 0
        while True:
            body: dict = {
                "userid": userid,
                "start_time": start_time,
                "end_time": end_time,
                "cursor": cursor,
                "size": args.size or 20,
            }
            if tpl is not None:
                body["template_name"] = tpl

            result = _oapi_post(config, api_path, body)
            r = result.get("result", {})
            for item in r.get("data_list", []):
                rid = item.get("report_id")
                if rid and rid not in seen_ids:
                    seen_ids.add(rid)
                    merged.append(item)
            if not r.get("has_more", False):
                break
            cursor = r.get("next_cursor", 0)

    return merged


def cmd_list_reports(config, args):
    """查询日志列表（自己 / 指定用户 / 部门）"""
    if args.department:
        info = _proxy_rpc("/api/report-dept-members")
        all_members = info.get("members", [])
        dept_names = info.get("dept_names", [])
        if not all_members:
            print(json.dumps({"error": True,
                              "message": "You are not a department manager, "
                                         "or no department members found"},
                             ensure_ascii=False), file=sys.stderr)
            sys.exit(1)

        total_members = len(all_members)
        offset = args.member_offset or 0
        limit = args.member_limit
        batch = all_members[offset:]
        if limit and limit > 0:
            batch = batch[:limit]

        all_reports: list = []
        for m in batch:
            reports = _query_reports_for_user(config, m["userid"], args)
            for r in reports:
                r["_member_name"] = m["name"]
            all_reports.extend(reports)

        all_reports.sort(key=lambda r: r.get("create_time", 0), reverse=True)
        output: dict = {
            "dept_names": dept_names,
            "total_members": total_members,
            "member_offset": offset,
            "members_queried": len(batch),
            "reports": all_reports,
            "total_reports": len(all_reports),
        }
        if offset + len(batch) < total_members:
            output["has_more_members"] = True
            output["next_member_offset"] = offset + len(batch)
        print(format_json(output))
        return

    userid = args.userid or _get_userid()
    data_list = _query_reports_for_user(config, userid, args)
    data_list.sort(key=lambda r: r.get("create_time", 0), reverse=True)
    print(format_json({"reports": data_list, "total": len(data_list)}))


def cmd_get_statistics(config, args):
    """获取日志统计数据（已读/评论/点赞数）"""
    result = _oapi_post(config, "/topapi/report/statistics", {
        "report_id": args.report_id,
    })
    print(format_json(result.get("result", {})))


def cmd_list_related_users(config, args):
    """获取日志相关人员列表（已读/评论/点赞用户）"""
    result = _oapi_post(config, "/topapi/report/statistics/listbytype", {
        "report_id": args.report_id,
        "type": args.type,
        "offset": args.offset or 0,
        "size": args.size or 100,
    })
    r = result.get("result", {})
    print(format_json({
        "userid_list": r.get("userid_list", []),
        "has_more": r.get("has_more", False),
        "next_cursor": r.get("next_cursor"),
    }))


def cmd_list_receivers(config, args):
    """获取日志接收人员列表"""
    result = _oapi_post(config, "/topapi/report/receiver/list", {
        "report_id": args.report_id,
        "offset": args.offset or 0,
        "size": args.size or 100,
    })
    r = result.get("result", {})
    print(format_json({
        "userid_list": r.get("userid_list", []),
        "has_more": r.get("has_more", False),
        "next_cursor": r.get("next_cursor"),
    }))


def cmd_get_comments(config, args):
    """获取日志评论详情"""
    result = _oapi_post(config, "/topapi/report/comment/list", {
        "report_id": args.report_id,
        "offset": args.offset or 0,
        "size": args.size or 20,
    })
    r = result.get("result", {})
    print(format_json({
        "comments": r.get("comments", []),
        "has_more": r.get("has_more", False),
        "next_cursor": r.get("next_cursor"),
    }))


def cmd_get_unread(config, args):
    """获取用户日志未读数"""
    userid = args.userid or _get_userid()
    result = _oapi_post(config, "/topapi/report/getunreadcount", {
        "userid": userid,
    })
    print(format_json({"count": result.get("count", 0)}))


# ---------------------------------------------------------------------------
# argparse
# ---------------------------------------------------------------------------

def _add_paging(p, default_size=20):
    p.add_argument("--offset", type=int, default=0, help="分页游标")
    p.add_argument("--size", type=int, default=default_size, help="每页大小")


def main():
    parser = argparse.ArgumentParser(description="钉钉日志工具")
    parser.add_argument("--config", help="配置文件路径")
    sub = parser.add_subparsers(dest="command", required=True)

    # my-scope
    sub.add_parser("my-scope", help="查询当前用户的日志查询权限范围")

    # list-templates
    p = sub.add_parser("list-templates", help="列出可用的日志模板")
    p.add_argument("--userid", help="员工 userId（不传则返回所有模板）")
    _add_paging(p, default_size=50)

    # get-template
    p = sub.add_parser("get-template", help="获取模板详情")
    p.add_argument("--name", required=True, help="模板名称（如: 日报）")
    p.add_argument("--userid", help="操作员工 userId（不传则使用当前用户）")

    # list-reports
    p = sub.add_parser("list-reports", help="查询日志列表")
    p.add_argument("--userid", help="员工 userId（不传默认查当前用户，Leader 可查部门成员）")
    p.add_argument("--department", action="store_true",
                   help="查询自己管理的部门所有成员的日志（仅 Leader 可用）")
    p.add_argument("--member-limit", type=int, default=0,
                   help="与 --department 配合：每批最多查询的成员数（默认不限）")
    p.add_argument("--member-offset", type=int, default=0,
                   help="与 --department 配合：跳过前 N 个成员（默认 0）")
    p.add_argument("--template-name", nargs="+", help="模板名称过滤（支持多个，分别查询后合并去重）")
    p.add_argument("--days", type=int, default=7, help="查询最近天数（默认 7）")
    p.add_argument("--start-time", type=int, help="起始时间（ms 时间戳，覆盖 --days）")
    p.add_argument("--end-time", type=int, help="结束时间（ms 时间戳）")
    p.add_argument("--size", type=int, default=20, help="每页大小（API 侧最大 20，工具自动翻页）")
    p.add_argument("--simple", action="store_true",
                   help="只返回概要（不含日志正文和修改时间）")

    # get-statistics
    p = sub.add_parser("get-statistics", help="获取日志统计数据")
    p.add_argument("--report-id", required=True, help="日志 ID")

    # list-related-users
    p = sub.add_parser("list-related-users", help="获取日志相关人员列表")
    p.add_argument("--report-id", required=True, help="日志 ID")
    p.add_argument("--type", type=int, required=True, choices=[0, 1, 2],
                   help="类型: 0=已读 1=评论 2=点赞")
    _add_paging(p, default_size=100)

    # list-receivers
    p = sub.add_parser("list-receivers", help="获取日志接收人员列表")
    p.add_argument("--report-id", required=True, help="日志 ID")
    _add_paging(p, default_size=100)

    # get-comments
    p = sub.add_parser("get-comments", help="获取日志评论详情")
    p.add_argument("--report-id", required=True, help="日志 ID")
    _add_paging(p, default_size=20)

    # get-unread
    p = sub.add_parser("get-unread", help="获取用户日志未读数")
    p.add_argument("--userid", help="员工 userId（不传则使用当前用户）")

    args = parser.parse_args()
    config = load_config(getattr(args, "config", None))

    dispatch = {
        "my-scope": cmd_my_scope,
        "list-templates": cmd_list_templates,
        "get-template": cmd_get_template,
        "list-reports": cmd_list_reports,
        "get-statistics": cmd_get_statistics,
        "list-related-users": cmd_list_related_users,
        "list-receivers": cmd_list_receivers,
        "get-comments": cmd_get_comments,
        "get-unread": cmd_get_unread,
    }
    dispatch[args.command](config, args)


if __name__ == "__main__":
    main()
