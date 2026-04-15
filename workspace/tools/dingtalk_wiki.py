"""
钉钉知识库管理工具
通过钉钉开放平台 API 管理知识库、创建文档/表格/文件夹、搜索文档、查询权限

用法示例:
  python tools/dingtalk_wiki.py list-wikis
  python tools/dingtalk_wiki.py search-docs --keyword "周报"
  python tools/dingtalk_wiki.py create-node --workspace-id <id> --name "新文档" --doc-type DOC
  python tools/dingtalk_wiki.py query-permissions --dentry-uuid <id>
"""

import argparse
import json
import os
import sys

import requests

from dingtalk_auth import require_operator_id
from utils import load_config, format_json, format_markdown_table, get_dingtalk_token

BASE_URL = "https://api.dingtalk.com"


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


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _get_operator_id(config, args):
    return require_operator_id(config, args, scope_label="钉钉知识库")


# ---------------------------------------------------------------------------
# 知识库子命令
# ---------------------------------------------------------------------------

def cmd_get_wiki(config, args):
    """获取知识库详情"""
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)
    result = _api_get(
        token,
        f"/v2.0/wiki/workspaces/{args.workspace_id}",
        {"operatorId": oid},
    )
    print(format_json(result))


def cmd_list_wikis(config, args):
    """获取知识库列表"""
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)
    params = {"operatorId": oid}
    if args.max_results:
        params["maxResults"] = args.max_results

    result = _api_get(token, "/v2.0/wiki/workspaces", params)

    fmt = getattr(args, "format", "json")
    workspaces = result.get("workspaces", [])
    if fmt == "markdown" and workspaces:
        headers = ["workspaceId", "name", "type", "url"]
        rows = [[str(w.get(h, "")) for h in headers] for w in workspaces]
        print(format_markdown_table(headers, rows))
    else:
        print(format_json(result))


def cmd_my_wiki(config, args):
    """获取我的文档知识库（个人空间）"""
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)
    result = _api_get(token, "/v2.0/wiki/mineWorkspaces", {"operatorId": oid})
    print(format_json(result))


# ---------------------------------------------------------------------------
# 节点子命令
# ---------------------------------------------------------------------------

def cmd_create_node(config, args):
    """创建文档/表格/文件夹"""
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)

    body = {
        "name": args.name,
        "docType": args.doc_type,
        "operatorId": oid,
    }
    if args.parent_node_id:
        body["parentNodeId"] = args.parent_node_id

    result = _api_post(
        token,
        f"/v1.0/doc/workspaces/{args.workspace_id}/docs",
        data=body,
    )
    print(format_json(result))


# ---------------------------------------------------------------------------
# 搜索子命令
# ---------------------------------------------------------------------------

def cmd_search_docs(config, args):
    """全局搜索文档（全文搜索，范围为 operatorId 可见的全部文档）"""
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)

    body = {
        "keyword": args.keyword,
        "operatorId": oid,
    }
    if args.next_token:
        body["nextToken"] = args.next_token

    result = _api_post(
        token,
        "/v2.0/storage/dentries/search",
        data=body,
        params={"unionId": oid},
    )

    fmt = getattr(args, "format", "json")
    items = result.get("items", [])
    if fmt == "markdown" and items:
        headers = ["dentryUuid", "name", "creator", "modifier"]
        rows = [[str(item.get(h, "")) for h in headers] for item in items]
        print(format_markdown_table(headers, rows))
    else:
        print(format_json(result))


# ---------------------------------------------------------------------------
# 权限子命令
# ---------------------------------------------------------------------------

def cmd_query_permissions(config, args):
    """查询文档权限列表"""
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)

    body = {}
    if args.filter_role_ids:
        body["filterRoleIds"] = args.filter_role_ids

    result = _api_post(
        token,
        f"/v2.0/storage/spaces/dentries/{args.dentry_uuid}/permissions/query",
        data=body,
        params={"unionId": oid},
    )

    fmt = getattr(args, "format", "json")
    permissions = result.get("permissions", [])
    if fmt == "markdown" and permissions:
        headers = ["role", "memberType", "memberId", "memberName"]
        rows = []
        for p in permissions:
            role = p.get("role", {}).get("id", "")
            member = p.get("member", {})
            rows.append([role, member.get("type", ""), member.get("id", ""), member.get("name", "")])
        print(format_markdown_table(headers, rows))
    else:
        print(format_json(result))


# ---------------------------------------------------------------------------
# argparse
# ---------------------------------------------------------------------------

def _add_common(p):
    p.add_argument("--operator-id", help="操作人 unionId（直接指定）")
    p.add_argument("--user-id", help="操作人 userId，从 credentials.yaml 查找 unionId")


def _add_format(p):
    p.add_argument("--format", choices=["json", "markdown"], default="json", help="输出格式")


def main():
    parser = argparse.ArgumentParser(description="钉钉知识库管理工具")
    parser.add_argument("--config", help="配置文件路径")
    sub = parser.add_subparsers(dest="command", required=True)

    # get-wiki
    p = sub.add_parser("get-wiki", help="获取知识库详情")
    _add_common(p)
    p.add_argument("--workspace-id", required=True, help="知识库 ID")

    # list-wikis
    p = sub.add_parser("list-wikis", help="获取知识库列表")
    _add_common(p)
    p.add_argument("--max-results", type=int, help="最大返回数量")
    _add_format(p)

    # my-wiki
    p = sub.add_parser("my-wiki", help="获取我的文档知识库（个人空间）")
    _add_common(p)

    # create-node
    p = sub.add_parser("create-node", help="创建文档/表格/文件夹")
    _add_common(p)
    p.add_argument("--workspace-id", required=True, help="知识库 ID")
    p.add_argument("--name", required=True, help="名称")
    p.add_argument("--doc-type", required=True, choices=["DOC", "WORKBOOK", "NOTABLE", "FOLDER"],
                   help="类型：DOC(文档) / WORKBOOK(表格) / NOTABLE(多维表格) / FOLDER(文件夹)")
    p.add_argument("--parent-node-id", help="父节点 ID（不填则在根目录）")

    # search-docs
    p = sub.add_parser("search-docs", help="全局搜索文档（全文搜索）")
    _add_common(p)
    p.add_argument("--keyword", required=True, help="搜索关键词")
    p.add_argument("--next-token", help="分页令牌")
    _add_format(p)

    # query-permissions
    p = sub.add_parser("query-permissions", help="查询文档权限列表")
    _add_common(p)
    p.add_argument("--dentry-uuid", required=True, help="文档 dentryUuid")
    p.add_argument("--filter-role-ids", nargs="*", help="过滤角色，如 OWNER EDITOR")
    _add_format(p)

    args = parser.parse_args()
    config = load_config(getattr(args, "config", None))

    dispatch = {
        "get-wiki": cmd_get_wiki,
        "list-wikis": cmd_list_wikis,
        "my-wiki": cmd_my_wiki,
        "create-node": cmd_create_node,
        "search-docs": cmd_search_docs,
        "query-permissions": cmd_query_permissions,
    }
    dispatch[args.command](config, args)


if __name__ == "__main__":
    main()
