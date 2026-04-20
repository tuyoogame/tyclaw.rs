#!/usr/bin/env python3
"""GitLab 代码仓库数据工具（只读）
通过 GitLab REST API v4 查询项目、提交、MR、用户活动等数据。
凭证通过环境变量注入（_TYCLAW_GL_TOKEN）或回退读 credentials.yaml。
"""

import argparse
import json
import os
import sys
import urllib.error
import urllib.parse
import urllib.request

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from utils import (save_user_credentials, sync_credential_env,
                    clear_user_credentials, clear_credential_env,
                    get_injected_credential)

GITLAB_URL = "https://tygit.tuyoo.com"
API_BASE = f"{GITLAB_URL}/api/v4"
_TIMEOUT = 30


def _get_token() -> str:
    token = get_injected_credential("gitlab", "token")
    if not token:
        print("Error: GitLab token not configured.\n"
              "Run: python3 tools/gitlab_api.py setup --token YOUR_PAT\n"
              f"Create PAT at: {GITLAB_URL}/-/user_settings/personal_access_tokens\n"
              "Only check the 'read_api' scope.",
              file=sys.stderr)
        sys.exit(1)
    return token


def _api_get(token: str, path: str, params: dict | None = None) -> dict | list:
    url = f"{API_BASE}{path}"
    if params:
        filtered = {k: v for k, v in params.items() if v is not None}
        if filtered:
            url += "?" + urllib.parse.urlencode(filtered)
    req = urllib.request.Request(url, headers={"PRIVATE-TOKEN": token})
    try:
        with urllib.request.urlopen(req, timeout=_TIMEOUT) as resp:
            return json.loads(resp.read().decode("utf-8"))
    except urllib.error.HTTPError as e:
        body = ""
        try:
            body = e.read().decode("utf-8", errors="replace")[:500]
        except Exception:
            pass
        if e.code == 401:
            print("Error: GitLab 401 Unauthorized. Token may be invalid or expired.\n"
                  "Run: python3 tools/gitlab_api.py setup --token NEW_PAT",
                  file=sys.stderr)
        elif e.code == 403:
            print(f"Error: GitLab 403 Forbidden. Insufficient permissions.\n{body}",
                  file=sys.stderr)
        elif e.code == 404:
            print(f"Error: GitLab 404 Not Found. Check project ID or path.\n{body}",
                  file=sys.stderr)
        else:
            print(f"Error: GitLab API {e.code}: {body}", file=sys.stderr)
        sys.exit(1)
    except urllib.error.URLError as e:
        print(f"Error: Failed to connect to GitLab: {e.reason}", file=sys.stderr)
        sys.exit(1)


def _date_to_iso(date_str: str) -> str:
    """YYYY-MM-DD -> ISO 8601 (YYYY-MM-DDT00:00:00Z)"""
    return f"{date_str}T00:00:00Z"


# ── Commands ──

def _cmd_setup(args):
    staff_id = os.environ.get("TYCLAW_SENDER_STAFF_ID", "")
    if not staff_id:
        print("Error: TYCLAW_SENDER_STAFF_ID env var required", file=sys.stderr)
        sys.exit(1)
    data = {"token": args.token}
    save_user_credentials(staff_id, "gitlab", data)
    sync_credential_env("gitlab", data)
    print(f"GitLab credentials saved for {staff_id}")


def _cmd_clear_credentials(_args):
    staff_id = os.environ.get("TYCLAW_SENDER_STAFF_ID", "")
    if not staff_id:
        print("Error: TYCLAW_SENDER_STAFF_ID env var required", file=sys.stderr)
        sys.exit(1)
    if clear_user_credentials(staff_id, "gitlab"):
        clear_credential_env("gitlab")
        print(f"GitLab credentials cleared for {staff_id}")
    else:
        print(f"No GitLab credentials found for {staff_id}")


def _cmd_list_projects(args):
    token = _get_token()
    params: dict = {
        "per_page": min(args.per_page, 100),
        "page": args.page,
        "order_by": "last_activity_at",
        "sort": "desc",
    }
    if args.search:
        params["search"] = args.search
    if args.owned:
        params["owned"] = "true"
    else:
        params["membership"] = "true"

    data = _api_get(token, "/projects", params)
    projects = []
    for p in data:
        projects.append({
            "id": p.get("id"),
            "name": p.get("name"),
            "path_with_namespace": p.get("path_with_namespace"),
            "description": (p.get("description") or "")[:200],
            "default_branch": p.get("default_branch"),
            "web_url": p.get("web_url"),
            "last_activity_at": p.get("last_activity_at"),
            "star_count": p.get("star_count", 0),
            "forks_count": p.get("forks_count", 0),
        })
    print(json.dumps({"total": len(projects), "projects": projects},
                     ensure_ascii=False, indent=2))


def _cmd_commits(args):
    token = _get_token()
    params: dict = {
        "per_page": min(args.per_page, 100),
        "page": args.page,
        "with_stats": "true",
    }
    if args.author:
        params["author"] = args.author
    if args.since:
        params["since"] = _date_to_iso(args.since)
    if args.until:
        params["until"] = _date_to_iso(args.until)
    if args.ref:
        params["ref_name"] = args.ref

    project = urllib.parse.quote(str(args.project), safe="")
    data = _api_get(token, f"/projects/{project}/repository/commits", params)
    commits = []
    for c in data:
        stats = c.get("stats") or {}
        commits.append({
            "id": c.get("short_id"),
            "full_id": c.get("id"),
            "title": c.get("title"),
            "message": c.get("message", "").strip(),
            "author_name": c.get("author_name"),
            "author_email": c.get("author_email"),
            "authored_date": c.get("authored_date"),
            "additions": stats.get("additions", 0),
            "deletions": stats.get("deletions", 0),
            "total": stats.get("total", 0),
            "web_url": c.get("web_url"),
        })
    print(json.dumps({"total": len(commits), "commits": commits},
                     ensure_ascii=False, indent=2))


def _cmd_list_mrs(args):
    token = _get_token()
    params: dict = {
        "per_page": min(args.per_page, 100),
        "page": args.page,
        "state": args.state,
        "order_by": "updated_at",
        "sort": "desc",
    }
    if args.author:
        params["author_username"] = args.author
    if args.reviewer:
        params["reviewer_username"] = args.reviewer
    if args.since:
        params["created_after"] = _date_to_iso(args.since)

    project = urllib.parse.quote(str(args.project), safe="")
    data = _api_get(token, f"/projects/{project}/merge_requests", params)
    mrs = []
    for m in data:
        author = m.get("author") or {}
        mrs.append({
            "iid": m.get("iid"),
            "title": m.get("title"),
            "state": m.get("state"),
            "author": author.get("username"),
            "author_name": author.get("name"),
            "source_branch": m.get("source_branch"),
            "target_branch": m.get("target_branch"),
            "created_at": m.get("created_at"),
            "updated_at": m.get("updated_at"),
            "merged_at": m.get("merged_at"),
            "web_url": m.get("web_url"),
            "upvotes": m.get("upvotes", 0),
            "downvotes": m.get("downvotes", 0),
            "labels": m.get("labels", []),
        })
    print(json.dumps({"total": len(mrs), "merge_requests": mrs},
                     ensure_ascii=False, indent=2))


def _cmd_mr_detail(args):
    token = _get_token()
    project = urllib.parse.quote(str(args.project), safe="")
    base_path = f"/projects/{project}/merge_requests/{args.mr_iid}"

    mr = _api_get(token, base_path)
    author = mr.get("author") or {}
    merged_by = mr.get("merged_by") or {}
    result: dict = {
        "iid": mr.get("iid"),
        "title": mr.get("title"),
        "description": mr.get("description"),
        "state": mr.get("state"),
        "author": author.get("username"),
        "author_name": author.get("name"),
        "source_branch": mr.get("source_branch"),
        "target_branch": mr.get("target_branch"),
        "created_at": mr.get("created_at"),
        "updated_at": mr.get("updated_at"),
        "merged_at": mr.get("merged_at"),
        "merged_by": merged_by.get("username"),
        "web_url": mr.get("web_url"),
        "upvotes": mr.get("upvotes", 0),
        "downvotes": mr.get("downvotes", 0),
        "labels": mr.get("labels", []),
        "changes_count": mr.get("changes_count"),
        "reviewers": [{"username": r.get("username"), "name": r.get("name")}
                      for r in (mr.get("reviewers") or [])],
    }

    if args.with_changes:
        changes_data = _api_get(token, f"{base_path}/changes")
        changes = []
        for ch in changes_data.get("changes", []):
            changes.append({
                "old_path": ch.get("old_path"),
                "new_path": ch.get("new_path"),
                "new_file": ch.get("new_file", False),
                "renamed_file": ch.get("renamed_file", False),
                "deleted_file": ch.get("deleted_file", False),
            })
        result["changes"] = changes
        result["changes_file_count"] = len(changes)

    if args.with_comments:
        notes = _api_get(token, f"{base_path}/notes",
                         {"per_page": 100, "sort": "asc"})
        comments = []
        for n in notes:
            if n.get("system"):
                continue
            note_author = n.get("author") or {}
            comments.append({
                "id": n.get("id"),
                "author": note_author.get("username"),
                "author_name": note_author.get("name"),
                "body": n.get("body"),
                "created_at": n.get("created_at"),
                "resolvable": n.get("resolvable", False),
                "resolved": n.get("resolved", False),
            })
        result["comments"] = comments
        result["comments_count"] = len(comments)

    print(json.dumps(result, ensure_ascii=False, indent=2))


def _cmd_user_events(args):
    token = _get_token()
    params: dict = {
        "per_page": min(args.per_page, 100),
        "page": args.page,
        "sort": "desc",
    }
    if args.since:
        params["after"] = args.since
    if args.until:
        params["before"] = args.until

    if args.username:
        users = _api_get(token, "/users", {"username": args.username})
        if not users:
            print(f"Error: User '{args.username}' not found", file=sys.stderr)
            sys.exit(1)
        user_id = users[0]["id"]
        path = f"/users/{user_id}/events"
    else:
        path = "/events"

    data = _api_get(token, path, params)
    events = []
    for e in data:
        event = {
            "action_name": e.get("action_name"),
            "target_type": e.get("target_type"),
            "target_title": e.get("target_title"),
            "target_iid": e.get("target_iid"),
            "created_at": e.get("created_at"),
            "push_data": None,
            "project_id": e.get("project_id"),
        }
        if e.get("push_data"):
            pd = e["push_data"]
            event["push_data"] = {
                "ref": pd.get("ref"),
                "ref_type": pd.get("ref_type"),
                "commit_count": pd.get("commit_count", 0),
                "commit_title": pd.get("commit_title"),
            }
        events.append(event)
    print(json.dumps({"total": len(events), "events": events},
                     ensure_ascii=False, indent=2))


def _cmd_list_members(args):
    token = _get_token()
    project = urllib.parse.quote(str(args.project), safe="")
    params: dict = {
        "per_page": min(args.per_page, 100),
        "page": args.page,
    }
    data = _api_get(token, f"/projects/{project}/members/all", params)
    members = []
    access_levels = {10: "Guest", 20: "Reporter", 30: "Developer",
                     40: "Maintainer", 50: "Owner"}
    for m in data:
        members.append({
            "id": m.get("id"),
            "username": m.get("username"),
            "name": m.get("name"),
            "state": m.get("state"),
            "access_level": m.get("access_level"),
            "access_level_name": access_levels.get(m.get("access_level"), "Unknown"),
            "web_url": m.get("web_url"),
        })
    print(json.dumps({"total": len(members), "members": members},
                     ensure_ascii=False, indent=2))


def main():
    parser = argparse.ArgumentParser(description="GitLab repository data tool (read-only)")
    sub = parser.add_subparsers(dest="action", required=True)

    # ── setup / clear ──
    p_setup = sub.add_parser("setup", help="Set GitLab Personal Access Token")
    p_setup.add_argument("--token", required=True,
                         help="Personal Access Token (only read_api scope needed)")
    sub.add_parser("clear-credentials", help="Clear GitLab credentials")

    # ── list-projects ──
    p_proj = sub.add_parser("list-projects", help="List accessible projects")
    p_proj.add_argument("--search", help="Search projects by keyword")
    p_proj.add_argument("--owned", action="store_true",
                        help="Only list projects owned by you")
    p_proj.add_argument("--membership", action="store_true",
                        help="Only list projects you are a member of (default)")
    p_proj.add_argument("--per-page", type=int, default=20, help="Results per page (max 100)")
    p_proj.add_argument("--page", type=int, default=1, help="Page number")

    # ── commits ──
    p_commits = sub.add_parser("commits", help="List project commits")
    p_commits.add_argument("--project", required=True,
                           help="Project ID or URL-encoded path (e.g. 42 or ns%%2Fproj)")
    p_commits.add_argument("--author", help="Filter by author email or name")
    p_commits.add_argument("--since", help="Start date (YYYY-MM-DD)")
    p_commits.add_argument("--until", help="End date (YYYY-MM-DD)")
    p_commits.add_argument("--ref", help="Branch or tag name")
    p_commits.add_argument("--per-page", type=int, default=20, help="Results per page (max 100)")
    p_commits.add_argument("--page", type=int, default=1, help="Page number")

    # ── list-mrs ──
    p_mrs = sub.add_parser("list-mrs", help="List merge requests")
    p_mrs.add_argument("--project", required=True,
                       help="Project ID or URL-encoded path")
    p_mrs.add_argument("--state", default="all",
                       choices=["opened", "merged", "closed", "all"],
                       help="MR state filter (default: all)")
    p_mrs.add_argument("--author", help="Filter by author username")
    p_mrs.add_argument("--reviewer", help="Filter by reviewer username")
    p_mrs.add_argument("--since", help="Created after date (YYYY-MM-DD)")
    p_mrs.add_argument("--per-page", type=int, default=20, help="Results per page (max 100)")
    p_mrs.add_argument("--page", type=int, default=1, help="Page number")

    # ── mr-detail ──
    p_mr = sub.add_parser("mr-detail", help="Get merge request details")
    p_mr.add_argument("--project", required=True,
                      help="Project ID or URL-encoded path")
    p_mr.add_argument("--mr-iid", required=True, type=int,
                      help="Merge request IID (project-scoped number)")
    p_mr.add_argument("--with-changes", action="store_true",
                      help="Include changed files list (paths + stats, no diff)")
    p_mr.add_argument("--with-comments", action="store_true",
                      help="Include discussion comments")

    # ── user-events ──
    p_events = sub.add_parser("user-events", help="List user activity events")
    p_events.add_argument("--username", help="Target username (default: current user)")
    p_events.add_argument("--since", help="Start date (YYYY-MM-DD)")
    p_events.add_argument("--until", help="End date (YYYY-MM-DD)")
    p_events.add_argument("--per-page", type=int, default=20,
                          help="Results per page (max 100)")
    p_events.add_argument("--page", type=int, default=1, help="Page number")

    # ── list-members ──
    p_mem = sub.add_parser("list-members", help="List project members")
    p_mem.add_argument("--project", required=True,
                       help="Project ID or URL-encoded path")
    p_mem.add_argument("--per-page", type=int, default=20, help="Results per page (max 100)")
    p_mem.add_argument("--page", type=int, default=1, help="Page number")

    args = parser.parse_args()

    dispatch = {
        "setup": _cmd_setup,
        "clear-credentials": _cmd_clear_credentials,
        "list-projects": _cmd_list_projects,
        "commits": _cmd_commits,
        "list-mrs": _cmd_list_mrs,
        "mr-detail": _cmd_mr_detail,
        "user-events": _cmd_user_events,
        "list-members": _cmd_list_members,
    }
    dispatch[args.action](args)


if __name__ == "__main__":
    main()
