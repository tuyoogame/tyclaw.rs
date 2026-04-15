"""
Teambition 任务操作工具（代理客户端）
实际 SDK 调用由 bot 侧代理完成，本工具仅解析 CLI 参数并转发。
"""

import argparse
import json
import os
import sys

import requests


_PROXY_URL = os.environ.get("_TYCLAW_TB_PROXY_URL", "")
_PROXY_TOKEN = os.environ.get("_TYCLAW_TB_PROXY_TOKEN", "")


def _proxy_call(action: str, **params) -> dict:
    if not _PROXY_URL:
        print("Error: _TYCLAW_TB_PROXY_URL not set", file=sys.stderr)
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


def main():
    parser = argparse.ArgumentParser(description="Teambition task operations")

    parser.add_argument("--project-id", required=True,
                        help="TB project ID")

    parser.add_argument("--title", default=None, help="Task title (max 500 chars)")
    parser.add_argument("--note", default="", help="Task note (markdown)")
    parser.add_argument("--priority", type=int, default=0, choices=[0, 1, 2],
                        help="Priority: 0=normal, 1=urgent, 2=very urgent")
    parser.add_argument("--sprint-id", default=None, help="Sprint ID")
    parser.add_argument("--tasklist-id", default=None,
                        help="Task group ID (tasklist)")
    parser.add_argument("--list-customfields", action="store_true",
                        help="List project custom field definitions (cf_id + name)")
    parser.add_argument("--list-task-types", action="store_true",
                        help="List all task types (scenariofieldconfig) and exit")
    parser.add_argument("--list-task-groups", action="store_true",
                        help="List all task groups and exit")
    parser.add_argument("--list-sprints", action="store_true",
                        help="List all sprints and exit")
    parser.add_argument("--list-members", action="store_true",
                        help="List known members (ding_id + name)")
    parser.add_argument("--list-statuses", action="store_true",
                        help="List all taskflow statuses (tfs_id + name)")
    parser.add_argument("--sprint-tasks", action="store_true",
                        help="Output normalized per-task list for sprint")
    parser.add_argument("--search-tasks", default=None, metavar="TQL",
                        help="Search tasks by TQL query (e.g. 'tfsId = \"xxx\"')")
    parser.add_argument("--executor-id", default=None,
                        help="Executor TB user ID (direct)")
    parser.add_argument("--executor-ding-id", default=None,
                        help="Executor DingTalk staff_id (auto-converted to TB userId)")
    parser.add_argument("--sfc-id", default=None,
                        help="scenariofieldconfig_id (task type)")
    parser.add_argument("--customfields", default=None,
                        help='Custom fields JSON array')
    parser.add_argument("--read-task", default=None, metavar="TASK_ID",
                        help="Read task details by task ID")
    parser.add_argument("--comment-task", default=None, metavar="TASK_ID",
                        help="Add a comment to a task")
    parser.add_argument("--comment", default=None,
                        help="Comment content (with --comment-task)")
    parser.add_argument("--images", nargs="*", default=None,
                        help="Image file paths to attach")
    args = parser.parse_args()

    base_params = {
        "project_id": args.project_id,
    }

    if args.read_task:
        result = _proxy_call("read-task", **base_params, task_id=args.read_task)
        print(json.dumps(result, ensure_ascii=False, indent=2))
        return

    if args.comment_task:
        if not args.comment:
            parser.error("--comment is required when using --comment-task")
        result = _proxy_call("comment-task", **base_params,
                             task_id=args.comment_task, comment=args.comment)
        print(json.dumps(result, ensure_ascii=False, indent=2))
        return

    if args.list_customfields:
        result = _proxy_call("list-customfields", **base_params)
        print(json.dumps(result, ensure_ascii=False, indent=2))
        return

    if args.list_task_types:
        result = _proxy_call("list-task-types", **base_params)
        print(json.dumps(result.get("task_types", []), ensure_ascii=False, indent=2))
        return

    if args.list_task_groups:
        result = _proxy_call("list-task-groups", **base_params)
        print(json.dumps(result.get("task_groups", []), ensure_ascii=False, indent=2))
        return

    if args.list_sprints:
        result = _proxy_call("list-sprints", **base_params)
        print(json.dumps(result.get("sprints", []), ensure_ascii=False, indent=2))
        return

    if args.list_members:
        result = _proxy_call("list-members", **base_params)
        print(json.dumps(result, ensure_ascii=False, indent=2))
        return

    if args.list_statuses:
        result = _proxy_call("list-statuses", **base_params)
        print(json.dumps(result.get("statuses", []), ensure_ascii=False, indent=2))
        return

    if args.search_tasks:
        result = _proxy_call("search-tasks", **base_params, tql=args.search_tasks)
        print(json.dumps(result, ensure_ascii=False, indent=2))
        return

    if args.sprint_tasks:
        if not args.sprint_id:
            parser.error("--sprint-id is required for --sprint-tasks")
        result = _proxy_call("sprint-tasks", **base_params, sprint_id=args.sprint_id)
        print(json.dumps(result, ensure_ascii=False, indent=2))
        return

    if not args.title:
        parser.error("--title is required when creating a task")

    create_params = {
        **base_params,
        "title": args.title,
        "note": args.note,
        "priority": args.priority,
    }
    if args.sprint_id:
        create_params["sprint_id"] = args.sprint_id
    if args.tasklist_id:
        create_params["tasklist_id"] = args.tasklist_id
    if args.executor_id:
        create_params["executor_id"] = args.executor_id
    if args.executor_ding_id:
        create_params["executor_ding_id"] = args.executor_ding_id
    if args.sfc_id:
        create_params["sfc_id"] = args.sfc_id
    if args.customfields:
        try:
            create_params["customfields"] = json.loads(args.customfields)
        except json.JSONDecodeError as e:
            parser.error(f"invalid --customfields JSON: {e}")
    if args.images:
        create_params["images"] = args.images

    result = _proxy_call("create-task", **create_params)
    print(json.dumps(result, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
