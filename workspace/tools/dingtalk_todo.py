"""
钉钉待办工具
通过钉钉开放平台 API 管理待办任务

用法示例:
  python tools/dingtalk_todo.py create-task --subject "修复登录 bug" --executors 621942314220944463
  python tools/dingtalk_todo.py update-task --task-id <id> --subject "新标题" --done true
  python tools/dingtalk_todo.py update-executor-status --task-id <id> --executors 621942314220944463 --is-done true
  python tools/dingtalk_todo.py query-tasks --is-done false
"""

import argparse
import json
import os
import sys

import requests

from dingtalk_auth import require_operator_id
from utils import load_config, format_json, get_dingtalk_token

BASE_URL = "https://api.dingtalk.com"

_SELF_PLACEHOLDER = "__self__"


# ---------------------------------------------------------------------------
# HTTP helpers（与 dingtalk_calendar.py 同构）
# ---------------------------------------------------------------------------

def _headers(token):
    return {
        "x-acs-dingtalk-access-token": token,
        "Content-Type": "application/json",
    }


def _handle_response(resp):
    if resp.ok:
        if resp.status_code == 204 or not resp.text:
            return {}
        return resp.json()
    try:
        err = resp.json()
    except Exception:
        err = {"status": resp.status_code, "body": resp.text}
    print(json.dumps({"error": True, **err}, ensure_ascii=False, indent=2), file=sys.stderr)
    sys.exit(1)


_PROXY_URL = os.environ.get("_TYCLAW_DT_PROXY_URL", "")
_PROXY_TOKEN = os.environ.get("_TYCLAW_DT_PROXY_TOKEN", "")


def _proxy_forward(method, path, data=None, params=None):
    resp = requests.post(_PROXY_URL, json={
        "token": _PROXY_TOKEN, "method": method,
        "path": path, "data": data, "params": params,
    }, timeout=30)
    return _handle_response(resp)


def _api_post(token, path, data=None, params=None):
    if _PROXY_URL:
        return _proxy_forward("POST", path, data=data, params=params)
    resp = requests.post(
        f"{BASE_URL}{path}", headers=_headers(token), json=data, params=params, timeout=30,
    )
    return _handle_response(resp)


def _api_put(token, path, data=None, params=None):
    if _PROXY_URL:
        return _proxy_forward("PUT", path, data=data, params=params)
    resp = requests.put(
        f"{BASE_URL}{path}", headers=_headers(token), json=data, params=params, timeout=30,
    )
    return _handle_response(resp)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

_current_uid: str = ""


def _get_operator_id(config, args):
    global _current_uid
    uid = require_operator_id(config, args, scope_label="钉钉待办")
    _current_uid = uid
    return uid


def _user_placeholder() -> str:
    if _PROXY_URL:
        return _SELF_PLACEHOLDER
    return _current_uid


def _staff_ids_to_placeholders(staff_ids: list[str]) -> list[str]:
    if not staff_ids:
        return []
    if _PROXY_URL:
        return [f"__staff:{sid}__" for sid in staff_ids]
    return list(staff_ids)


def _parse_due_time(s: str) -> int:
    """接受毫秒时间戳或 ISO-8601 格式，统一返回毫秒时间戳。"""
    if s.isdigit():
        return int(s)
    from datetime import datetime, timezone
    try:
        dt = datetime.fromisoformat(s)
        return int(dt.timestamp() * 1000)
    except ValueError:
        print(json.dumps({"error": True, "message": f"Invalid due-time format: {s}"}, ensure_ascii=False), file=sys.stderr)
        sys.exit(1)


# ---------------------------------------------------------------------------
# 子命令
# ---------------------------------------------------------------------------

def cmd_create_task(config, args):
    token = get_dingtalk_token(config)
    _get_operator_id(config, args)

    body: dict = {"subject": args.subject}

    if args.description:
        body["description"] = args.description
    if args.due_time:
        body["dueTime"] = _parse_due_time(args.due_time)
    if args.priority is not None:
        body["priority"] = args.priority

    if args.executors:
        body["executorIds"] = _staff_ids_to_placeholders(args.executors)
    if args.participants:
        body["participantIds"] = _staff_ids_to_placeholders(args.participants)

    if args.creator:
        body["creatorId"] = f"__staff:{args.creator}__" if _PROXY_URL else args.creator

    if args.source_id:
        body["sourceId"] = args.source_id
    if args.source_title:
        body["sourceTitle"] = args.source_title

    if args.detail_app_url or args.detail_pc_url:
        detail = {}
        if args.detail_app_url:
            detail["appUrl"] = args.detail_app_url
        if args.detail_pc_url:
            detail["pcUrl"] = args.detail_pc_url
        body["detailUrl"] = detail

    if args.todo_type:
        body["todoType"] = args.todo_type

    if args.executor_only:
        body["isOnlyShowExecutor"] = True

    notify = {}
    if args.notify_ding:
        notify["dingNotify"] = "1"
    if notify:
        body["notifyConfigs"] = notify

    uid = _user_placeholder()
    result = _api_post(token, f"/v1.0/todo/users/{uid}/tasks", data=body)
    print(format_json(result))


def cmd_update_task(config, args):
    token = get_dingtalk_token(config)
    _get_operator_id(config, args)

    body: dict = {}

    if args.subject is not None:
        body["subject"] = args.subject
    if args.description is not None:
        body["description"] = args.description
    if args.due_time is not None:
        body["dueTime"] = _parse_due_time(args.due_time)
    if args.done is not None:
        body["done"] = args.done.lower() in ("true", "1", "yes")
    if args.priority is not None:
        body["priority"] = args.priority

    if args.executors:
        body["executorIds"] = _staff_ids_to_placeholders(args.executors)
    if args.participants:
        body["participantIds"] = _staff_ids_to_placeholders(args.participants)

    if not body:
        print(json.dumps({"error": True, "message": "No fields to update"}, ensure_ascii=False), file=sys.stderr)
        sys.exit(1)

    uid = _user_placeholder()
    result = _api_put(token, f"/v1.0/todo/users/{uid}/tasks/{args.task_id}", data=body)
    print(format_json(result))


def cmd_update_executor_status(config, args):
    token = get_dingtalk_token(config)
    _get_operator_id(config, args)

    is_done = args.is_done.lower() in ("true", "1", "yes")
    union_ids = _staff_ids_to_placeholders(args.executors)

    body = {
        "executorStatusList": [{"id": uid, "isDone": is_done} for uid in union_ids],
    }

    uid = _user_placeholder()
    result = _api_put(
        token,
        f"/v1.0/todo/users/{uid}/tasks/{args.task_id}/executorStatus",
        data=body,
    )
    print(format_json(result))


def cmd_query_tasks(config, args):
    token = get_dingtalk_token(config)
    _get_operator_id(config, args)

    body: dict = {}
    if args.next_token is not None:
        body["nextToken"] = args.next_token
    if args.is_done is not None:
        body["isDone"] = args.is_done.lower() in ("true", "1", "yes")
    if args.todo_type:
        body["todoType"] = args.todo_type
    if args.role_types:
        body["roleTypes"] = [[r] for r in args.role_types]

    uid = _user_placeholder()
    result = _api_post(token, f"/v1.0/todo/users/{uid}/org/tasks/query", data=body)
    print(format_json(result))


# ---------------------------------------------------------------------------
# argparse
# ---------------------------------------------------------------------------

def _add_common(p):
    p.add_argument("--operator-id", help="操作人 unionId（直接指定）")
    p.add_argument("--user-id", help="操作人 userId，从 credentials.yaml 查找 unionId")


def main():
    parser = argparse.ArgumentParser(description="钉钉待办工具")
    parser.add_argument("--config", help="配置文件路径")
    sub = parser.add_subparsers(dest="command", required=True)

    # create-task
    p = sub.add_parser("create-task", help="创建待办任务")
    _add_common(p)
    p.add_argument("--subject", required=True, help="待办标题（最大 1024 字符）")
    p.add_argument("--description", help="待办备注（最大 4096 字符）")
    p.add_argument("--due-time", help="截止时间（毫秒时间戳或 ISO-8601）")
    p.add_argument("--priority", type=int, choices=[10, 20, 30, 40],
                   help="优先级: 10=低 20=普通 30=紧急 40=非常紧急")
    p.add_argument("--executors", nargs="+", help="执行者 staff_id 列表")
    p.add_argument("--participants", nargs="+", help="参与者 staff_id 列表")
    p.add_argument("--creator", help="创建者 staff_id（不传则使用当前用户）")
    p.add_argument("--source-id", help="业务侧唯一标识（幂等）")
    p.add_argument("--source-title", help="来源标题")
    p.add_argument("--detail-app-url", help="移动端跳转链接")
    p.add_argument("--detail-pc-url", help="PC 端跳转链接")
    p.add_argument("--todo-type", choices=["TODO", "READ"], help="业务类型")
    p.add_argument("--executor-only", action="store_true", default=False,
                   help="仅执行者可见")
    p.add_argument("--notify-ding", action="store_true", default=False,
                   help="发送 ding 通知")

    # update-task
    p = sub.add_parser("update-task", help="更新待办任务")
    _add_common(p)
    p.add_argument("--task-id", required=True, help="待办任务 ID")
    p.add_argument("--subject", help="新标题")
    p.add_argument("--description", help="新备注")
    p.add_argument("--due-time", help="新截止时间")
    p.add_argument("--done", help="完成状态: true/false")
    p.add_argument("--priority", type=int, choices=[10, 20, 30, 40], help="优先级")
    p.add_argument("--executors", nargs="+", help="新执行者 staff_id 列表")
    p.add_argument("--participants", nargs="+", help="新参与者 staff_id 列表")

    # update-executor-status
    p = sub.add_parser("update-executor-status", help="更新执行者完成状态")
    _add_common(p)
    p.add_argument("--task-id", required=True, help="待办任务 ID")
    p.add_argument("--executors", nargs="+", required=True,
                   help="执行者 staff_id 列表")
    p.add_argument("--is-done", required=True, help="完成状态: true/false")

    # query-tasks
    p = sub.add_parser("query-tasks", help="查询企业下用户待办列表")
    _add_common(p)
    p.add_argument("--next-token", help="分页游标")
    p.add_argument("--is-done", help="完成状态筛选: true/false")
    p.add_argument("--todo-type", choices=["TODO", "READ"], help="业务类型筛选")
    p.add_argument("--role-types", nargs="+",
                   choices=["executor", "creator", "participant"],
                   help="角色筛选（多选，外层 OR）")

    args = parser.parse_args()
    config = load_config(getattr(args, "config", None))

    dispatch = {
        "create-task": cmd_create_task,
        "update-task": cmd_update_task,
        "update-executor-status": cmd_update_executor_status,
        "query-tasks": cmd_query_tasks,
    }
    dispatch[args.command](config, args)


if __name__ == "__main__":
    main()
