"""
Skill 分享工具（发布侧）
分享/取消分享/查看我的分享/修改可见范围

用法:
  python3 skills/skill-share/tool.py share --skill <name> --to-user <user>
  python3 skills/skill-share/tool.py share --skill <name> --to-department
  python3 skills/skill-share/tool.py share --skill <name> --to-all
  python3 skills/skill-share/tool.py my-shares
  python3 skills/skill-share/tool.py unshare <skill_name>
  python3 skills/skill-share/tool.py update-visibility <skill_name> [options]
"""

import argparse
import fcntl
import json
import os
import re
import sys
import time
from urllib.request import Request, urlopen
from urllib.error import HTTPError, URLError

import yaml

_PERSONAL_DIR = os.environ.get("TYCLAW_PERSONAL_DIR", "/workspace/_personal")
_STAFF_ID = os.environ.get("TYCLAW_SENDER_STAFF_ID", "")
_DEPT_IDS_RAW = os.environ.get("TYCLAW_SENDER_DEPT_IDS", "")
_PROXY_URL = os.environ.get("_TYCLAW_DT_PROXY_URL", "")
_PROXY_TOKEN = os.environ.get("_TYCLAW_DT_PROXY_TOKEN", "")

_PUBLISHED_PATH = os.path.join(_PERSONAL_DIR, "published_skills.json")
_MY_STATS_PATH = os.path.join(_PERSONAL_DIR, ".my_shares_stats.json")


def _my_dept_ids() -> set[int]:
    if not _DEPT_IDS_RAW:
        return set()
    return {int(d) for d in _DEPT_IDS_RAW.split(",") if d.strip()}


def _parse_frontmatter(content: str) -> dict:
    m = re.match(r"^---\s*\n(.*?)\n---\s*\n", content, re.DOTALL)
    if not m:
        return {}
    try:
        return yaml.safe_load(m.group(1)) or {}
    except yaml.YAMLError:
        return {}


# ── proxy-based user resolution ──────────────────────────────────────

def _resolve_user_via_proxy(name: str, retries: int = 3) -> dict:
    """通过 Bot 代理解析用户名 → staff_id，返回原始 JSON 响应。
    对瞬态错误（网络/5xx）自动重试，4xx 直接返回。
    """
    if not _PROXY_URL or not _PROXY_TOKEN:
        return {"error": True, "message": "Proxy not configured"}
    url = _PROXY_URL.rstrip("/").replace("/api/dingtalk-proxy", "") + "/api/resolve-user"
    payload = json.dumps({"token": _PROXY_TOKEN, "name": name}).encode("utf-8")

    last_err = ""
    for attempt in range(retries):
        req = Request(url, data=payload,
                      headers={"Content-Type": "application/json"})
        try:
            with urlopen(req, timeout=5) as resp:
                return json.loads(resp.read().decode("utf-8"))
        except HTTPError as exc:
            # 读取服务端返回的 JSON body
            try:
                body = json.loads(exc.read().decode("utf-8"))
            except Exception:
                body = {"error": True,
                        "message": f"HTTP {exc.code}: {exc.reason}"}
            if exc.code < 500:
                return body
            # 5xx 瞬态错误，重试
            last_err = body.get("message", f"HTTP {exc.code}")
        except URLError as exc:
            last_err = str(exc)
        except json.JSONDecodeError:
            return {"error": True, "message": "Invalid proxy response"}
        if attempt < retries - 1:
            time.sleep(0.5 * (2 ** attempt))

    return {"error": True, "message": f"Proxy request failed after "
            f"{retries} retries: {last_err}"}


def _resolve_dept_via_proxy(name: str) -> list[int] | None:
    """部门名 → dept_id 列表。有多个候选时打印列表并返回 None。"""
    if not _PROXY_URL or not _PROXY_TOKEN:
        print("Error: Proxy not configured, cannot resolve department name")
        return None
    url = _PROXY_URL.rstrip("/").replace("/api/dingtalk-proxy", "") + "/api/resolve-department"
    payload = json.dumps({"token": _PROXY_TOKEN, "name": name}).encode("utf-8")

    last_err = ""
    for attempt in range(3):
        req = Request(url, data=payload,
                      headers={"Content-Type": "application/json"})
        try:
            with urlopen(req, timeout=5) as resp:
                result = json.loads(resp.read().decode("utf-8"))
                break
        except HTTPError as exc:
            try:
                result = json.loads(exc.read().decode("utf-8"))
            except Exception:
                result = {"error": True, "message": f"HTTP {exc.code}"}
            if exc.code < 500:
                break
            last_err = result.get("message", f"HTTP {exc.code}")
        except URLError as exc:
            last_err = str(exc)
            result = None
        if attempt < 2:
            time.sleep(0.5 * (2 ** attempt))
    else:
        print(f"Error: Department resolution failed after retries: {last_err}")
        return None

    if result and result.get("dept_ids"):
        return result["dept_ids"]
    if result and result.get("candidates"):
        print(f"发现多个匹配「{name}」的部门，请用户确认：")
        for c in result["candidates"]:
            print(f"  - {c['name']}")
        return None
    msg = (result or {}).get("message", "Unknown error")
    print(f"Error: 未找到部门「{name}」- {msg}")
    return None


def _resolve_staff_id(name_or_id: str) -> str | None:
    """用户名 → staff_id。如果有重名，打印候选列表并返回 None。
    禁止向用户索要 staff_id——用户不知道这个值。
    """
    result = _resolve_user_via_proxy(name_or_id)
    if result.get("staff_id"):
        return result["staff_id"]
    if result.get("candidates"):
        print(f"发现多个匹配「{name_or_id}」的用户，请用户确认是哪一位：")
        for c in result["candidates"]:
            print(f"  - {c['name']}（{c.get('department', '?')}）")
        print("请让用户补充部门信息以区分，"
              "然后用 --to-user \"姓名\" 重试。")
        return None
    msg = result.get("message", "Unknown error")
    if "not found" in msg.lower():
        print(f"Error: 未找到用户「{name_or_id}」。"
              f"可能原因：1) 姓名输入有误；"
              f"2) 该用户尚未私聊过 TyClaw，不在用户目录中。")
    else:
        print(f"Error: 用户解析失败 - {msg}")
    print("注意：请勿要求用户提供 staff_id，用户无法获取该信息。"
          "请让用户确认姓名，或让对方先私聊 TyClaw 任意发一条消息后重试。")
    return None


# ── published_skills.json helpers (per-user fcntl.flock) ─────────────

def _load_published() -> list[dict]:
    if not os.path.exists(_PUBLISHED_PATH):
        return []
    try:
        with open(_PUBLISHED_PATH, "r", encoding="utf-8") as f:
            data = json.load(f)
        return data if isinstance(data, list) else []
    except (json.JSONDecodeError, OSError):
        return []


def _save_published(entries: list[dict]):
    os.makedirs(os.path.dirname(_PUBLISHED_PATH), exist_ok=True)
    tmp = _PUBLISHED_PATH + ".tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(entries, f, ensure_ascii=False, indent=2)
    os.replace(tmp, _PUBLISHED_PATH)


def _locked_update(fn):
    """读-改-写 published_skills.json，全程 flock"""
    os.makedirs(os.path.dirname(_PUBLISHED_PATH), exist_ok=True)
    lock_path = _PUBLISHED_PATH + ".lock"
    with open(lock_path, "w") as lock_f:
        fcntl.flock(lock_f, fcntl.LOCK_EX)
        try:
            entries = _load_published()
            entries = fn(entries)
            _save_published(entries)
        finally:
            fcntl.flock(lock_f, fcntl.LOCK_UN)


# ── snapshot readers ─────────────────────────────────────────────────

def _load_my_stats() -> dict:
    if not os.path.exists(_MY_STATS_PATH):
        return {}
    try:
        with open(_MY_STATS_PATH, "r", encoding="utf-8") as f:
            data = json.load(f)
        return data if isinstance(data, dict) else {}
    except (json.JSONDecodeError, OSError):
        return {}


# ── visibility display helper ────────────────────────────────────────

def _format_visibility(entry: dict) -> str:
    parts = []
    if entry.get("shared_to_all"):
        parts.append("全公司")
    dept_ids = entry.get("shared_to_departments") or []
    if dept_ids:
        parts.append("部门ID: " + ", ".join(str(d) for d in dept_ids))
    return " | ".join(parts) if parts else "指定用户"


# ── subcommands ──────────────────────────────────────────────────────

def cmd_share(args):
    if not _STAFF_ID:
        print("Error: TYCLAW_SENDER_STAFF_ID not set", file=sys.stderr)
        sys.exit(1)

    skill_name = args.skill
    skill_dir = os.path.join(_PERSONAL_DIR, "skills", skill_name)
    skill_md = os.path.join(skill_dir, "SKILL.md")

    if not os.path.exists(skill_md):
        print(f"Error: skill '{skill_name}' not found at {skill_dir}")
        sys.exit(1)

    with open(skill_md, "r", encoding="utf-8") as f:
        content = f.read()
    meta = _parse_frontmatter(content)
    display_name = meta.get("name", skill_name)
    description = meta.get("description", "")

    to_user = args.to_user
    to_dept = args.to_department
    to_all = args.to_all

    if not (to_user or to_dept or to_all):
        print("Error: must specify --to-user, --to-department, or --to-all")
        sys.exit(1)

    target_user_id = None
    if to_user:
        target_user_id = _resolve_staff_id(to_user)
        if not target_user_id:
            sys.exit(1)

    # 解析目标部门 ID 集合
    resolved_dept_ids: set[int] = set()
    dept_display = ""
    if to_dept is True:
        resolved_dept_ids = _my_dept_ids()
        dept_display = "我的部门"
    elif isinstance(to_dept, str) and to_dept:
        ids = _resolve_dept_via_proxy(to_dept)
        if not ids:
            sys.exit(1)
        resolved_dept_ids = set(ids)
        dept_display = f"部门「{to_dept}」"

    def _do_share(entries: list[dict]) -> list[dict]:
        existing = next(
            (e for e in entries if e.get("skill_name") == skill_name), None)
        if existing:
            if to_all:
                existing["shared_to_all"] = True
            if target_user_id:
                users = existing.get("shared_to_users") or []
                if target_user_id not in users:
                    users.append(target_user_id)
                existing["shared_to_users"] = users
            if resolved_dept_ids:
                depts = set(existing.get("shared_to_departments") or [])
                depts |= resolved_dept_ids
                existing["shared_to_departments"] = sorted(depts)
            existing["display_name"] = display_name
            existing["description"] = description
        else:
            from datetime import date
            entry = {
                "skill_name": skill_name,
                "display_name": display_name,
                "description": description,
                "shared_at": str(date.today()),
                "shared_to_all": bool(to_all),
                "shared_to_users": [target_user_id] if target_user_id else [],
                "shared_to_departments": sorted(resolved_dept_ids),
            }
            entries.append(entry)
        return entries

    _locked_update(_do_share)

    target_desc = []
    if to_all:
        target_desc.append("全公司")
    if target_user_id:
        target_desc.append(f"用户 {to_user}")
    if dept_display:
        target_desc.append(dept_display)

    key = f"{_STAFF_ID}--{skill_name}"
    print(f"已分享「{display_name}」给 {'、'.join(target_desc)}。")
    print(f"共享 Key: `{key}`")


def cmd_my_shares(_args):
    if not _STAFF_ID:
        print("Error: TYCLAW_SENDER_STAFF_ID not set", file=sys.stderr)
        sys.exit(1)

    published = _load_published()
    stats = _load_my_stats()

    if not published:
        print("你还没有分享任何 Skill。")
        return

    print("## 我分享的 Skill\n")
    for entry in published:
        skill_name = entry.get("skill_name", "?")
        display = entry.get("display_name", skill_name)
        vis = _format_visibility(entry)
        key = f"{_STAFF_ID}--{skill_name}"
        st = stats.get(skill_name, {})
        count = st.get("installed_count", 0)
        names = st.get("installed_by_names") or []

        usage_self = st.get("usage_count_self", 0)
        usage_others = st.get("usage_count_others", 0)

        print(f"- **{display}** (key: `{key}`)")
        print(f"  可见范围: {vis}")
        if names:
            print(f"  已安装 ({count}人): {', '.join(names)}")
        else:
            print(f"  暂无人安装")
        usage_parts = []
        if usage_self:
            usage_parts.append(f"自己 {usage_self} 次")
        if usage_others:
            usage_parts.append(f"他人 {usage_others} 次")
        if usage_parts:
            print(f"  使用次数: {', '.join(usage_parts)}")
        else:
            print(f"  暂无使用记录")


def cmd_unshare(args):
    if not _STAFF_ID:
        print("Error: TYCLAW_SENDER_STAFF_ID not set", file=sys.stderr)
        sys.exit(1)

    skill_name = args.skill_name
    display_name = skill_name
    removed = False

    def _do_unshare(entries: list[dict]) -> list[dict]:
        nonlocal display_name, removed
        new_entries = []
        for e in entries:
            if e.get("skill_name") == skill_name:
                display_name = e.get("display_name", skill_name)
                removed = True
                continue
            new_entries.append(e)
        return new_entries

    _locked_update(_do_unshare)

    if removed:
        print(f"已取消分享「{display_name}」。"
              f"已安装该 Skill 的用户将在下次对话时收到失效提示。")
    else:
        print(f"未找到你分享的 Skill「{skill_name}」。")


def cmd_update_visibility(args):
    if not _STAFF_ID:
        print("Error: TYCLAW_SENDER_STAFF_ID not set", file=sys.stderr)
        sys.exit(1)

    skill_name = args.skill_name
    found = False
    display_name = skill_name

    # 预解析部门 ID（在锁外完成网络调用）
    add_dept_ids: set[int] = set()
    remove_dept_ids: set[int] = set()
    if args.add_dept is True:
        add_dept_ids = _my_dept_ids()
    elif isinstance(args.add_dept, str) and args.add_dept:
        ids = _resolve_dept_via_proxy(args.add_dept)
        if ids:
            add_dept_ids = set(ids)
    if args.remove_dept is True:
        remove_dept_ids = _my_dept_ids()
    elif isinstance(args.remove_dept, str) and args.remove_dept:
        ids = _resolve_dept_via_proxy(args.remove_dept)
        if ids:
            remove_dept_ids = set(ids)

    def _do_update(entries: list[dict]) -> list[dict]:
        nonlocal found, display_name
        for e in entries:
            if e.get("skill_name") != skill_name:
                continue
            found = True
            display_name = e.get("display_name", skill_name)

            if args.to_all:
                e["shared_to_all"] = True
            if args.add_user:
                uid = _resolve_staff_id(args.add_user)
                if not uid:
                    print(f"Warning: user '{args.add_user}' not resolved")
                else:
                    users = e.get("shared_to_users") or []
                    if uid not in users:
                        users.append(uid)
                    e["shared_to_users"] = users
            if args.remove_user:
                uid = _resolve_staff_id(args.remove_user)
                if uid:
                    users = e.get("shared_to_users") or []
                    if uid in users:
                        users.remove(uid)
                    e["shared_to_users"] = users
            if add_dept_ids:
                depts = set(e.get("shared_to_departments") or [])
                depts |= add_dept_ids
                e["shared_to_departments"] = sorted(depts)
            if remove_dept_ids:
                depts = set(e.get("shared_to_departments") or [])
                depts -= remove_dept_ids
                e["shared_to_departments"] = sorted(depts)
            break
        return entries

    _locked_update(_do_update)

    if found:
        entries = _load_published()
        entry = next(
            (e for e in entries if e.get("skill_name") == skill_name), {})
        vis = _format_visibility(entry)
        print(f"已更新「{display_name}」的可见范围: {vis}")
    else:
        print(f"未找到你分享的 Skill「{skill_name}」。")


def main():
    parser = argparse.ArgumentParser(description="Skill 分享")
    sub = parser.add_subparsers(dest="command")

    p_share = sub.add_parser("share", help="Share a skill")
    p_share.add_argument("--skill", required=True, help="Skill directory name")
    p_share.add_argument("--to-user", default="",
                         help="Target user name or staff_id")
    p_share.add_argument("--to-department", nargs="?", const=True, default=None,
                         help="Share to departments (no arg=sender's dept, or specify name)")
    p_share.add_argument("--to-all", action="store_true",
                         help="Share to entire company")

    sub.add_parser("my-shares", help="List my shared skills")

    p_unshare = sub.add_parser("unshare", help="Unshare a skill")
    p_unshare.add_argument("skill_name", help="Skill directory name")

    p_vis = sub.add_parser("update-visibility", help="Update sharing scope")
    p_vis.add_argument("skill_name", help="Skill directory name")
    p_vis.add_argument("--add-user", default="",
                       help="Add user by name/id")
    p_vis.add_argument("--remove-user", default="",
                       help="Remove user by name/id")
    p_vis.add_argument("--add-dept", nargs="?", const=True, default=None,
                       help="Add departments (no arg=sender's dept, or specify name)")
    p_vis.add_argument("--remove-dept", nargs="?", const=True, default=None,
                       help="Remove departments (no arg=sender's dept, or specify name)")
    p_vis.add_argument("--to-all", action="store_true",
                       help="Set visible to entire company")

    args = parser.parse_args()
    cmd_map = {
        "share": cmd_share,
        "my-shares": cmd_my_shares,
        "unshare": cmd_unshare,
        "update-visibility": cmd_update_visibility,
    }
    handler = cmd_map.get(args.command)
    if handler:
        handler(args)
    else:
        parser.print_help()
        sys.exit(1)


if __name__ == "__main__":
    main()
