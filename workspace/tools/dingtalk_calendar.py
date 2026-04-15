"""
钉钉日程工具
通过钉钉开放平台 API 管理日程、参与者、忙闲查询、会议室

用法示例:
  python tools/dingtalk_calendar.py create-event --summary "周会" --start 2026-04-11T14:00:00+08:00 --end 2026-04-11T15:00:00+08:00
  python tools/dingtalk_calendar.py list-events --time-min 2026-04-10T00:00:00+08:00 --time-max 2026-04-11T00:00:00+08:00
  python tools/dingtalk_calendar.py get-event --event-id <id>
  python tools/dingtalk_calendar.py view-events --time-min 2026-04-10T00:00:00+08:00 --time-max 2026-04-17T00:00:00+08:00
  python tools/dingtalk_calendar.py update-event --event-id <id> --summary "新标题"
  python tools/dingtalk_calendar.py delete-event --event-id <id>
  python tools/dingtalk_calendar.py list-attendees --event-id <id>
  python tools/dingtalk_calendar.py add-attendees --event-id <id> --staff-ids 011533646711841213
  python tools/dingtalk_calendar.py remove-attendees --event-id <id> --staff-ids 011533646711841213
  python tools/dingtalk_calendar.py respond-event --event-id <id> --status accepted
  python tools/dingtalk_calendar.py query-schedule --staff-ids 621942314220944463 --start 2026-04-10T00:00:00+08:00 --end 2026-04-11T00:00:00+08:00
  python tools/dingtalk_calendar.py list-rooms
  python tools/dingtalk_calendar.py query-room-schedule --room-ids <id1> <id2> --start ... --end ...
  python tools/dingtalk_calendar.py book-room --event-id <id> --room-ids <roomId>
  python tools/dingtalk_calendar.py cancel-room --event-id <id> --room-ids <roomId>
"""

import argparse
import json
import os
import re
import sys
from datetime import datetime, timedelta

import requests

from dingtalk_auth import require_operator_id
from utils import load_config, format_json, get_dingtalk_token

BASE_URL = "https://api.dingtalk.com"
CALENDAR_ID = "primary"

# __self__ 占位符：代理模式下由 Bot Proxy 替换为当前用户 unionId
_SELF_PLACEHOLDER = "__self__"


# ---------------------------------------------------------------------------
# HTTP helpers
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


def _api_get(token, path, params=None):
    if _PROXY_URL:
        return _proxy_forward("GET", path, params=params)
    resp = requests.get(f"{BASE_URL}{path}", headers=_headers(token), params=params, timeout=30)
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


def _api_delete(token, path, params=None):
    if _PROXY_URL:
        return _proxy_forward("DELETE", path, params=params)
    resp = requests.delete(f"{BASE_URL}{path}", headers=_headers(token), params=params, timeout=30)
    return _handle_response(resp)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

_current_uid: str = ""


def _get_operator_id(config, args):
    global _current_uid
    uid = require_operator_id(config, args, scope_label="钉钉日程")
    _current_uid = uid
    return uid


def _calendar_prefix(uid: str) -> str:
    return f"/v1.0/calendar/users/{uid}/calendars/{CALENDAR_ID}"


def _user_placeholder() -> str:
    """代理模式用 __self__（代理替换），直接模式用实际 unionId。"""
    if _PROXY_URL:
        return _SELF_PLACEHOLDER
    return _current_uid


def _self_prefix() -> str:
    return _calendar_prefix(_user_placeholder())


_DATE_RE = re.compile(r"^\d{4}-\d{2}-\d{2}$")


def _is_date_only(s: str) -> bool:
    return bool(_DATE_RE.match(s))


def _build_time_obj(dt_str: str, tz: str = "Asia/Shanghai") -> dict:
    return {"dateTime": dt_str, "timeZone": tz}


def _build_date_obj(date_str: str) -> dict:
    return {"date": date_str}


def _next_day(date_str: str) -> str:
    """yyyy-MM-dd → T+1"""
    dt = datetime.strptime(date_str, "%Y-%m-%d")
    return (dt + timedelta(days=1)).strftime("%Y-%m-%d")


def _staff_ids_to_placeholders(staff_ids: list[str]) -> list[str]:
    """将 staff_id 列表转为 __staff:xxx__ 占位符，代理侧自动解析为 unionId。"""
    if not staff_ids:
        return []
    return [f"__staff:{sid}__" for sid in staff_ids]


# ---------------------------------------------------------------------------
# 日程基础操作（6 个子命令）
# ---------------------------------------------------------------------------

def cmd_create_event(config, args):
    token = get_dingtalk_token(config)
    _get_operator_id(config, args)

    is_all_day = args.all_day or _is_date_only(args.start)

    body: dict = {
        "summary": args.summary,
    }

    if is_all_day:
        body["isAllDay"] = True
        body["start"] = _build_date_obj(args.start)
        if args.end:
            end_date = args.end
            if end_date == args.start:
                end_date = _next_day(end_date)
            body["end"] = _build_date_obj(end_date)
        else:
            body["end"] = _build_date_obj(_next_day(args.start))
    else:
        body["isAllDay"] = False
        body["start"] = _build_time_obj(args.start)
        if args.end:
            body["end"] = _build_time_obj(args.end)

    if args.description:
        body["description"] = args.description
    if args.location:
        body["location"] = {"displayName": args.location}

    if args.reminders is not None:
        if args.reminders:
            body["reminders"] = [{"method": "dingtalk", "minutes": int(m)} for m in args.reminders]
        else:
            body["reminders"] = []

    if args.staff_ids:
        union_ids = _staff_ids_to_placeholders(args.staff_ids)
        body["attendees"] = [{"id": uid} for uid in union_ids]

    if args.online_meeting:
        body["onlineMeetingInfo"] = {"type": "dingtalk"}

    extra = {}
    if args.no_push:
        extra["noPushNotification"] = "true"
    if args.no_chat:
        extra["noChatNotification"] = "true"
    if extra:
        body["extra"] = extra

    result = _api_post(token, f"{_self_prefix()}/events", data=body)
    print(format_json(result))


def cmd_list_events(config, args):
    token = get_dingtalk_token(config)
    _get_operator_id(config, args)

    params = {}
    if args.time_min:
        params["timeMin"] = args.time_min
    if args.time_max:
        params["timeMax"] = args.time_max
    if args.max_results:
        params["maxResults"] = args.max_results
    if args.next_token:
        params["nextToken"] = args.next_token

    result = _api_get(token, f"{_self_prefix()}/events", params=params)
    print(format_json(result))


def cmd_get_event(config, args):
    token = get_dingtalk_token(config)
    _get_operator_id(config, args)

    result = _api_get(token, f"{_self_prefix()}/events/{args.event_id}")
    print(format_json(result))


def cmd_view_events(config, args):
    token = get_dingtalk_token(config)
    _get_operator_id(config, args)

    params = {}
    if args.time_min:
        params["timeMin"] = args.time_min
    if args.time_max:
        params["timeMax"] = args.time_max
    if args.max_results:
        params["maxResults"] = args.max_results
    if args.next_token:
        params["nextToken"] = args.next_token

    result = _api_get(token, f"{_self_prefix()}/eventsview", params=params)
    print(format_json(result))


def cmd_update_event(config, args):
    token = get_dingtalk_token(config)
    _get_operator_id(config, args)

    body: dict = {"id": args.event_id}

    if args.summary is not None:
        body["summary"] = args.summary
    if args.description is not None:
        body["description"] = args.description
    if args.location is not None:
        body["location"] = {"displayName": args.location}

    if args.all_day:
        is_all_day = True
    elif getattr(args, "no_all_day", False):
        is_all_day = False
    elif args.start and _is_date_only(args.start):
        is_all_day = True
    else:
        is_all_day = None

    if args.start:
        if is_all_day:
            body["start"] = _build_date_obj(args.start)
        else:
            body["start"] = _build_time_obj(args.start)
    if args.end:
        if is_all_day:
            end_date = args.end
            if args.start and end_date == args.start:
                end_date = _next_day(end_date)
            body["end"] = _build_date_obj(end_date)
        else:
            body["end"] = _build_time_obj(args.end)

    if is_all_day is not None:
        body["isAllDay"] = is_all_day

    extra = {}
    if getattr(args, "no_push", False):
        extra["noPushNotification"] = "true"
    if getattr(args, "no_chat", False):
        extra["noChatNotification"] = "true"
    if extra:
        body["extra"] = extra

    result = _api_put(token, f"{_self_prefix()}/events/{args.event_id}", data=body)
    print(format_json(result))


def cmd_delete_event(config, args):
    token = get_dingtalk_token(config)
    _get_operator_id(config, args)

    params = {}
    if args.push_notification is not None:
        params["pushNotification"] = str(args.push_notification).lower()

    result = _api_delete(token, f"{_self_prefix()}/events/{args.event_id}", params=params)
    print(format_json(result))


# ---------------------------------------------------------------------------
# 参与者管理（4 个子命令）
# ---------------------------------------------------------------------------

def cmd_list_attendees(config, args):
    token = get_dingtalk_token(config)
    _get_operator_id(config, args)

    params = {}
    if args.max_results:
        params["maxResults"] = args.max_results
    if args.next_token:
        params["nextToken"] = args.next_token

    result = _api_get(token, f"{_self_prefix()}/events/{args.event_id}/attendees", params=params)
    print(format_json(result))


def cmd_add_attendees(config, args):
    token = get_dingtalk_token(config)
    _get_operator_id(config, args)

    union_ids = _staff_ids_to_placeholders(args.staff_ids)
    body: dict = {
        "attendeesToAdd": [{"id": uid} for uid in union_ids],
    }
    if args.push_notification is not None:
        body["pushNotification"] = args.push_notification
    if args.chat_notification is not None:
        body["chatNotification"] = args.chat_notification

    result = _api_post(token, f"{_self_prefix()}/events/{args.event_id}/attendees", data=body)
    print(format_json(result))


def cmd_remove_attendees(config, args):
    token = get_dingtalk_token(config)
    _get_operator_id(config, args)

    union_ids = _staff_ids_to_placeholders(args.staff_ids)
    body = {
        "attendeesToRemove": [{"id": uid} for uid in union_ids],
    }

    result = _api_post(
        token, f"{_self_prefix()}/events/{args.event_id}/attendees/batchRemove", data=body,
    )
    print(format_json(result))


def cmd_respond_event(config, args):
    token = get_dingtalk_token(config)
    _get_operator_id(config, args)

    result = _api_post(
        token, f"{_self_prefix()}/events/{args.event_id}/respond",
        data={"responseStatus": args.status},
    )
    print(format_json(result))


# ---------------------------------------------------------------------------
# 忙闲查询（1 个子命令）
# ---------------------------------------------------------------------------

def cmd_query_schedule(config, args):
    token = get_dingtalk_token(config)
    _get_operator_id(config, args)

    union_ids = _staff_ids_to_placeholders(args.staff_ids)
    body = {
        "userIds": union_ids,
        "startTime": args.start,
        "endTime": args.end,
    }

    result = _api_post(
        token, f"/v1.0/calendar/users/{_user_placeholder()}/querySchedule", data=body,
    )
    print(format_json(result))


# ---------------------------------------------------------------------------
# 会议室（4 个子命令）
# ---------------------------------------------------------------------------

def cmd_list_rooms(config, args):
    token = get_dingtalk_token(config)
    _get_operator_id(config, args)

    params = {"unionId": _user_placeholder()}
    if args.max_results:
        params["maxResults"] = args.max_results
    if args.next_token:
        params["nextToken"] = args.next_token

    result = _api_get(token, "/v1.0/rooms/meetingRoomLists", params=params)
    print(format_json(result))


def cmd_query_room_schedule(config, args):
    token = get_dingtalk_token(config)
    _get_operator_id(config, args)

    body = {
        "roomIds": args.room_ids,
        "startTime": args.start,
        "endTime": args.end,
    }

    result = _api_post(
        token,
        f"/v1.0/calendar/users/{_user_placeholder()}/meetingRooms/schedules/query",
        data=body,
    )
    print(format_json(result))


def cmd_book_room(config, args):
    token = get_dingtalk_token(config)
    _get_operator_id(config, args)

    body = {
        "meetingRoomsToAdd": [{"roomId": rid} for rid in args.room_ids],
    }

    result = _api_post(
        token, f"{_self_prefix()}/events/{args.event_id}/meetingRooms", data=body,
    )
    print(format_json(result))


def cmd_cancel_room(config, args):
    token = get_dingtalk_token(config)
    _get_operator_id(config, args)

    body = {
        "meetingRoomsToRemove": [{"roomId": rid} for rid in args.room_ids],
    }

    result = _api_post(
        token,
        f"{_self_prefix()}/events/{args.event_id}/meetingRooms/batchRemove",
        data=body,
    )
    print(format_json(result))


# ---------------------------------------------------------------------------
# argparse
# ---------------------------------------------------------------------------

def _add_common(p):
    p.add_argument("--operator-id", help="操作人 unionId（直接指定）")
    p.add_argument("--user-id", help="操作人 userId，从 credentials.yaml 查找 unionId")


def _add_event_id(p):
    p.add_argument("--event-id", required=True, help="日程 ID")


def _add_time_range(p, required=False):
    p.add_argument("--time-min", required=required, help="开始时间（ISO-8601，如 2026-04-10T00:00:00+08:00）")
    p.add_argument("--time-max", required=required, help="结束时间（ISO-8601）")


def _add_pagination(p):
    p.add_argument("--max-results", type=int, help="最大返回数量")
    p.add_argument("--next-token", help="分页游标")


def main():
    parser = argparse.ArgumentParser(description="钉钉日程工具")
    parser.add_argument("--config", help="配置文件路径")
    sub = parser.add_subparsers(dest="command", required=True)

    # --- 日程基础操作 ---

    p = sub.add_parser("create-event", help="创建日程")
    _add_common(p)
    p.add_argument("--summary", required=True, help="日程标题")
    p.add_argument("--start", required=True, help="开始时间（ISO-8601）或日期（yyyy-MM-dd，全天日程）")
    p.add_argument("--end", help="结束时间/日期（全天日程的结束日期需 T+1）")
    p.add_argument("--description", help="日程描述")
    p.add_argument("--location", help="地点名称")
    p.add_argument("--all-day", action="store_true", default=False, help="全天日程")
    p.add_argument("--staff-ids", nargs="+", help="参与者 staff_id 列表（自动解析为 unionId）")
    p.add_argument("--reminders", nargs="*", type=int,
                   help="提前提醒分钟数列表（如 15 30）；不传=默认15分钟；传空=不提醒")
    p.add_argument("--online-meeting", action="store_true", default=False, help="同时创建钉钉视频会议")
    p.add_argument("--no-push", action="store_true", default=False, help="不发钉钉推送通知")
    p.add_argument("--no-chat", action="store_true", default=False, help="不发单聊卡片通知")

    p = sub.add_parser("list-events", help="查询日程列表")
    _add_common(p)
    _add_time_range(p)
    _add_pagination(p)

    p = sub.add_parser("get-event", help="查询单个日程详情")
    _add_common(p)
    _add_event_id(p)

    p = sub.add_parser("view-events", help="查询日程视图（展开循环日程）")
    _add_common(p)
    _add_time_range(p)
    _add_pagination(p)

    p = sub.add_parser("update-event", help="修改日程（仅组织者可修改）")
    _add_common(p)
    _add_event_id(p)
    p.add_argument("--summary", help="日程标题")
    p.add_argument("--start", help="开始时间（ISO-8601）或日期")
    p.add_argument("--end", help="结束时间/日期")
    p.add_argument("--description", help="日程描述")
    p.add_argument("--location", help="地点名称")
    g = p.add_mutually_exclusive_group()
    g.add_argument("--all-day", action="store_true", default=None, help="设为全天日程")
    g.add_argument("--no-all-day", action="store_true", help="取消全天，改为定时日程")
    p.add_argument("--no-push", action="store_true", default=False, help="不发钉钉推送通知")
    p.add_argument("--no-chat", action="store_true", default=False, help="不发单聊卡片通知")

    p = sub.add_parser("delete-event", help="删除日程")
    _add_common(p)
    _add_event_id(p)
    p.add_argument("--push-notification", action="store_true", default=None,
                   help="发送取消通知给参与者")

    # --- 参与者管理 ---

    p = sub.add_parser("list-attendees", help="获取日程参与者列表")
    _add_common(p)
    _add_event_id(p)
    _add_pagination(p)

    p = sub.add_parser("add-attendees", help="添加日程参与者")
    _add_common(p)
    _add_event_id(p)
    p.add_argument("--staff-ids", nargs="+", required=True, help="参与者 staff_id 列表")
    p.add_argument("--push-notification", action="store_true", default=None,
                   help="弹窗提醒参与者")
    p.add_argument("--chat-notification", action="store_true", default=None,
                   help="单聊卡片提醒参与者")

    p = sub.add_parser("remove-attendees", help="移除日程参与者（仅组织者可操作）")
    _add_common(p)
    _add_event_id(p)
    p.add_argument("--staff-ids", nargs="+", required=True, help="要移除的参与者 staff_id 列表")

    p = sub.add_parser("respond-event", help="响应日程邀请（接受/拒绝/暂定）")
    _add_common(p)
    _add_event_id(p)
    p.add_argument("--status", required=True,
                   choices=["accepted", "declined", "tentative", "needsAction"],
                   help="响应状态")

    # --- 忙闲查询 ---

    p = sub.add_parser("query-schedule", help="查询用户忙闲状态")
    _add_common(p)
    p.add_argument("--staff-ids", nargs="+", required=True,
                   help="查询目标用户 staff_id 列表（最多 20 个）")
    p.add_argument("--start", required=True, help="查询开始时间（ISO-8601）")
    p.add_argument("--end", required=True, help="查询结束时间（ISO-8601）")

    # --- 会议室 ---

    p = sub.add_parser("list-rooms", help="列出可用会议室")
    _add_common(p)
    _add_pagination(p)

    p = sub.add_parser("query-room-schedule", help="查询会议室忙闲")
    _add_common(p)
    p.add_argument("--room-ids", nargs="+", required=True, help="会议室 roomId 列表（建议不超过 5 个）")
    p.add_argument("--start", required=True, help="查询开始时间（ISO-8601）")
    p.add_argument("--end", required=True, help="查询结束时间（ISO-8601）")

    p = sub.add_parser("book-room", help="预定会议室（绑定到已有日程）")
    _add_common(p)
    _add_event_id(p)
    p.add_argument("--room-ids", nargs="+", required=True, help="会议室 roomId 列表（最多 5 个）")

    p = sub.add_parser("cancel-room", help="取消预定会议室")
    _add_common(p)
    _add_event_id(p)
    p.add_argument("--room-ids", nargs="+", required=True, help="要取消的会议室 roomId 列表")

    args = parser.parse_args()
    config = load_config(getattr(args, "config", None))

    dispatch = {
        "create-event": cmd_create_event,
        "list-events": cmd_list_events,
        "get-event": cmd_get_event,
        "view-events": cmd_view_events,
        "update-event": cmd_update_event,
        "delete-event": cmd_delete_event,
        "list-attendees": cmd_list_attendees,
        "add-attendees": cmd_add_attendees,
        "remove-attendees": cmd_remove_attendees,
        "respond-event": cmd_respond_event,
        "query-schedule": cmd_query_schedule,
        "list-rooms": cmd_list_rooms,
        "query-room-schedule": cmd_query_room_schedule,
        "book-room": cmd_book_room,
        "cancel-room": cmd_cancel_room,
    }
    dispatch[args.command](config, args)


if __name__ == "__main__":
    main()
