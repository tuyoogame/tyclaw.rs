"""
定时任务管理工具 — 供 Cursor CLI 中的 AI 调用
子命令: add / list / update / remove / toggle

环境变量:
  TYCLAW_SENDER_STAFF_ID   — 当前用户 staff_id
  TYCLAW_PERSONAL_DIR      — 用户个人目录（容器内 /workspace/_personal）
  TYCLAW_CONVERSATION_TYPE — 会话类型 ("1" 私聊 / "2" 群聊)
  TYCLAW_CONVERSATION_ID   — 群聊会话 ID（私聊为空）
"""

import argparse
import json
import os
import sys
from pathlib import Path

from schedule_store import ScheduleStore


def _get_staff_id() -> str:
    sid = os.environ.get("TYCLAW_SENDER_STAFF_ID", "")
    if not sid:
        print("ERROR: TYCLAW_SENDER_STAFF_ID not set")
        sys.exit(1)
    return sid


def _get_schedules_path() -> Path:
    """用户 schedules.json 路径：优先用 TYCLAW_PERSONAL_DIR 环境变量"""
    personal_dir = os.environ.get("TYCLAW_PERSONAL_DIR", "")
    if personal_dir:
        return Path(personal_dir) / "schedules.json"
    return Path(__file__).resolve().parent.parent / "_personal" / "schedules.json"


def _get_store() -> ScheduleStore:
    return ScheduleStore(str(_get_schedules_path()), staff_id=_get_staff_id())


def _get_conversation_context() -> tuple[str, str, str]:
    conv_type = os.environ.get("TYCLAW_CONVERSATION_TYPE", "1")
    conv_id = os.environ.get("TYCLAW_CONVERSATION_ID", "")
    conv_title = os.environ.get("TYCLAW_CONVERSATION_TITLE", "")
    return conv_type, conv_id, conv_title


def cmd_add(args):
    staff_id = _get_staff_id()
    conv_type, conv_id, conv_title = _get_conversation_context()
    store = _get_store()

    sched = store.add(
        staff_id, args.name, args.cron, args.message,
        conversation_type=conv_type,
        conversation_id=conv_id,
        conversation_title=conv_title,
        end_at=args.end_at or "",
    )
    if sched is None:
        print(f"ERROR: 定时任务已达上限，无法添加更多")
        sys.exit(1)

    print(f"OK: 定时任务已创建")
    print(f"  ID: {sched['id']}")
    print(f"  名称: {sched['name']}")
    print(f"  Cron: {sched['cron']}")
    print(f"  消息: {sched['message']}")
    if sched.get("end_at"):
        print(f"  结束时间: {sched['end_at']}")


def cmd_list(args):
    staff_id = _get_staff_id()
    store = _get_store()
    schedules = store.get_user_schedules(staff_id)

    if not schedules:
        print("当前没有定时任务。")
        return

    print(f"共 {len(schedules)} 个定时任务：\n")
    for s in schedules:
        status = "启用" if s.get("enabled", True) else "已暂停"
        last = s.get("last_run") or "从未执行"
        conv_type = s.get("conversation_type", "1")
        conv_title = s.get("conversation_title", "")
        if conv_type == "2" and conv_title:
            push_target = f"群聊「{conv_title}」"
        elif conv_type == "2":
            push_target = "群聊"
        else:
            push_target = "私聊"
        end_at = s.get("end_at") or "无"
        print(f"  [{s['id']}] {s['name']} ({status})")
        print(f"    Cron: {s['cron']}")
        print(f"    消息: {s['message']}")
        print(f"    推送目标: {push_target}")
        print(f"    结束时间: {end_at}")
        print(f"    上次执行: {last}")
        print()


def cmd_remove(args):
    staff_id = _get_staff_id()
    store = _get_store()

    if store.remove(staff_id, args.id):
        print(f"OK: 定时任务 {args.id} 已删除")
    else:
        print(f"ERROR: 未找到 ID 为 {args.id} 的定时任务")
        sys.exit(1)


def cmd_update(args):
    staff_id = _get_staff_id()
    store = _get_store()

    kwargs = {}
    if args.name is not None:
        kwargs["name"] = args.name
    if args.cron is not None:
        kwargs["cron"] = args.cron
    if args.message is not None:
        kwargs["message"] = args.message
    if args.end_at is not None:
        kwargs["end_at"] = args.end_at

    if not kwargs:
        print("ERROR: 至少提供 --name / --cron / --message / --end-at 中的一个")
        sys.exit(1)

    sched = store.update(staff_id, args.id, **kwargs)
    if sched is None:
        print(f"ERROR: 未找到 ID 为 {args.id} 的定时任务")
        sys.exit(1)

    print(f"OK: 定时任务已更新")
    print(f"  ID: {sched['id']}")
    print(f"  名称: {sched['name']}")
    print(f"  Cron: {sched['cron']}")
    print(f"  消息: {sched['message']}")
    if sched.get("end_at"):
        print(f"  结束时间: {sched['end_at']}")


def cmd_toggle(args):
    staff_id = _get_staff_id()
    store = _get_store()

    sched = store.toggle(staff_id, args.id)
    if sched is None:
        print(f"ERROR: 未找到 ID 为 {args.id} 的定时任务")
        sys.exit(1)

    status = "启用" if sched["enabled"] else "已暂停"
    print(f"OK: 定时任务「{sched['name']}」已切换为{status}")


def main():
    parser = argparse.ArgumentParser(
        description="TyClaw 定时任务管理工具")
    sub = parser.add_subparsers(dest="command", required=True)

    p_add = sub.add_parser("add", help="添加定时任务")
    p_add.add_argument("--name", required=True, help="任务名称")
    p_add.add_argument("--cron", required=True,
                       help="Cron 表达式 (5 段)")
    p_add.add_argument("--message", required=True,
                       help="定时执行的消息内容")
    p_add.add_argument("--end-at", default=None,
                       help="可选，ISO 格式截止时间（如 2025-04-11T10:00:00），到期自动停用")
    p_add.set_defaults(func=cmd_add)

    p_list = sub.add_parser("list", help="列出定时任务")
    p_list.set_defaults(func=cmd_list)

    p_rm = sub.add_parser("remove", help="删除定时任务")
    p_rm.add_argument("--id", required=True, help="任务 ID")
    p_rm.set_defaults(func=cmd_remove)

    p_update = sub.add_parser("update", help="修改定时任务")
    p_update.add_argument("--id", required=True, help="任务 ID")
    p_update.add_argument("--name", default=None, help="新名称")
    p_update.add_argument("--cron", default=None, help="新 Cron 表达式")
    p_update.add_argument("--message", default=None, help="新消息内容")
    p_update.add_argument("--end-at", default=None,
                          help="新截止时间（ISO 格式），传空字符串可清除")
    p_update.set_defaults(func=cmd_update)

    p_toggle = sub.add_parser("toggle", help="启用/禁用定时任务")
    p_toggle.add_argument("--id", required=True, help="任务 ID")
    p_toggle.set_defaults(func=cmd_toggle)

    args = parser.parse_args()
    args.func(args)


if __name__ == "__main__":
    main()
