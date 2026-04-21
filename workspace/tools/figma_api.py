#!/usr/bin/env python3
"""Figma 设计文件工具（只读）
通过 Figma REST API 查询文件结构、导出截图、读取组件和样式。
凭证通过环境变量注入（_TYCLAW_FIGMA_TOKEN）或回退读 credentials.yaml。
"""

import argparse
import json
import os
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from utils import (save_user_credentials, sync_credential_env,
                    clear_user_credentials, clear_credential_env,
                    get_injected_credential)

API_BASE = "https://api.figma.com/v1"
_TIMEOUT = 30

# Figma URL → file key 的正则
_FIGMA_URL_RE = re.compile(
    r"(?:https?://)?(?:www\.)?figma\.com/(?:file|design|proto)/([a-zA-Z0-9]+)"
)


def _parse_file_key(raw: str) -> str:
    """从 Figma URL 或纯 key 中提取 file_key"""
    m = _FIGMA_URL_RE.search(raw)
    if m:
        return m.group(1)
    return raw.strip()


def _get_token() -> str:
    token = get_injected_credential("figma", "token")
    if not token:
        print("Error: Figma token not configured.\n"
              "Run: python3 tools/figma_api.py setup --token YOUR_PAT\n"
              "Create PAT at: https://www.figma.com/settings → Security → Personal access tokens\n"
              "Scope: check 'File content (Read only)'.",
              file=sys.stderr)
        sys.exit(1)
    return token


def _api_get(token: str, path: str, params: dict | None = None,
             timeout: int = _TIMEOUT) -> dict | list:
    url = f"{API_BASE}{path}"
    if params:
        filtered = {k: v for k, v in params.items() if v is not None}
        if filtered:
            url += "?" + urllib.parse.urlencode(filtered)
    req = urllib.request.Request(url, headers={"X-Figma-Token": token})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return json.loads(resp.read().decode("utf-8"))
    except urllib.error.HTTPError as e:
        body = ""
        try:
            body = e.read().decode("utf-8", errors="replace")[:500]
        except Exception:
            pass
        if e.code == 403:
            print("Error: Figma 403 Forbidden. Token may be invalid or expired.\n"
                  "Run: python3 tools/figma_api.py setup --token NEW_PAT",
                  file=sys.stderr)
        elif e.code == 404:
            print(f"Error: Figma 404 Not Found. Check file key.\n{body}",
                  file=sys.stderr)
        else:
            print(f"Error: Figma API {e.code}: {body}", file=sys.stderr)
        sys.exit(1)
    except urllib.error.URLError as e:
        print(f"Error: Failed to connect to Figma: {e.reason}", file=sys.stderr)
        sys.exit(1)


def _download_file(url: str, dest: str, timeout: int = 30):
    req = urllib.request.Request(url)
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        with open(dest, "wb") as f:
            while True:
                chunk = resp.read(65536)
                if not chunk:
                    break
                f.write(chunk)


def _summarize_node(node: dict, max_children: int = 30) -> dict:
    """精简节点信息，避免输出过大"""
    result: dict = {
        "id": node.get("id"),
        "name": node.get("name"),
        "type": node.get("type"),
    }
    bb = node.get("absoluteBoundingBox")
    if bb:
        result["size"] = f"{int(bb.get('width', 0))}x{int(bb.get('height', 0))}"

    fills = node.get("fills", [])
    for f in fills:
        if f.get("type") == "IMAGE":
            result["has_image"] = True
            break

    children = node.get("children", [])
    if children:
        result["children_count"] = len(children)
        result["children"] = [_summarize_node(c, max_children) for c in children[:max_children]]
        if len(children) > max_children:
            result["children_truncated"] = True
    return result


# ── Commands ──

def _cmd_setup(args):
    staff_id = os.environ.get("TYCLAW_SENDER_STAFF_ID", "")
    if not staff_id:
        print("Error: TYCLAW_SENDER_STAFF_ID env var required", file=sys.stderr)
        sys.exit(1)
    data = {"token": args.token}
    save_user_credentials(staff_id, "figma", data)
    sync_credential_env("figma", data)
    print(f"Figma credentials saved for {staff_id}")


def _cmd_clear_credentials(_args):
    staff_id = os.environ.get("TYCLAW_SENDER_STAFF_ID", "")
    if not staff_id:
        print("Error: TYCLAW_SENDER_STAFF_ID env var required", file=sys.stderr)
        sys.exit(1)
    if clear_user_credentials(staff_id, "figma"):
        clear_credential_env("figma")
        print(f"Figma credentials cleared for {staff_id}")
    else:
        print(f"No Figma credentials found for {staff_id}")


def _cmd_get_file(args):
    token = _get_token()
    file_key = _parse_file_key(args.file_key)
    params: dict = {}
    if args.depth is not None:
        params["depth"] = args.depth
    if args.node_ids:
        params["ids"] = args.node_ids

    data = _api_get(token, f"/files/{file_key}", params, timeout=60)

    result: dict = {
        "name": data.get("name"),
        "lastModified": data.get("lastModified"),
        "editorType": data.get("editorType"),
        "version": data.get("version"),
    }

    doc = data.get("document", {})
    pages = doc.get("children", [])
    result["pages"] = []
    for p in pages:
        page_info = _summarize_node(p)
        result["pages"].append(page_info)

    comps = data.get("components", {})
    if comps:
        result["components_count"] = len(comps)
        result["components"] = [
            {"node_id": k, "name": v.get("name"), "description": v.get("description", "")}
            for k, v in list(comps.items())[:50]
        ]

    styles = data.get("styles", {})
    if styles:
        result["styles_count"] = len(styles)
        result["styles"] = [
            {"node_id": k, "name": v.get("name"), "type": v.get("styleType")}
            for k, v in list(styles.items())[:50]
        ]

    print(json.dumps(result, ensure_ascii=False, indent=2))


def _cmd_get_nodes(args):
    token = _get_token()
    file_key = _parse_file_key(args.file_key)
    params: dict = {"ids": args.node_ids}
    if args.depth is not None:
        params["depth"] = args.depth

    data = _api_get(token, f"/files/{file_key}/nodes", params, timeout=60)

    nodes = data.get("nodes", {})
    result = []
    for nid, ndata in nodes.items():
        if ndata is None:
            result.append({"id": nid, "error": "Node not found"})
            continue
        doc = ndata.get("document", {})
        result.append(_summarize_node(doc))

    print(json.dumps({"nodes": result}, ensure_ascii=False, indent=2))


def _cmd_export_images(args):
    token = _get_token()
    file_key = _parse_file_key(args.file_key)
    fmt = args.format or "png"
    scale = args.scale or 1

    params: dict = {
        "ids": args.node_ids,
        "format": fmt,
        "scale": scale,
    }

    data = _api_get(token, f"/images/{file_key}", params, timeout=60)
    images = data.get("images", {})

    if not images:
        print("No images returned.", file=sys.stderr)
        sys.exit(1)

    staff_id = os.environ.get("TYCLAW_SENDER_STAFF_ID", "unknown")
    ts = int(time.time())
    output_dir = f"/tmp/tyclaw_{staff_id}_{ts}_figma"
    os.makedirs(output_dir, exist_ok=True)

    exported = []
    for node_id, url in images.items():
        if not url:
            exported.append({"node_id": node_id, "error": "Render failed (null URL)"})
            continue
        safe_name = node_id.replace(":", "_")
        filename = f"{safe_name}.{fmt}"
        dest = os.path.join(output_dir, filename)
        try:
            _download_file(url, dest, timeout=30)
            size = os.path.getsize(dest)
            exported.append({
                "node_id": node_id,
                "file": dest,
                "size": size,
                "format": fmt,
            })
        except Exception as e:
            exported.append({"node_id": node_id, "error": str(e)})

    print(json.dumps({"output_dir": output_dir, "images": exported},
                     ensure_ascii=False, indent=2))


def _cmd_list_components(args):
    token = _get_token()
    file_key = _parse_file_key(args.file_key)

    data = _api_get(token, f"/files/{file_key}/components", timeout=60)
    meta = data.get("meta", {})
    components = meta.get("components", [])

    result = []
    for c in components:
        result.append({
            "key": c.get("key"),
            "name": c.get("name"),
            "description": c.get("description", ""),
            "node_id": c.get("node_id"),
            "thumbnail_url": c.get("thumbnail_url"),
            "containing_frame": c.get("containing_frame", {}).get("name"),
            "created_at": c.get("created_at"),
            "updated_at": c.get("updated_at"),
        })
    print(json.dumps({"total": len(result), "components": result},
                     ensure_ascii=False, indent=2))


def _cmd_list_styles(args):
    token = _get_token()
    file_key = _parse_file_key(args.file_key)

    data = _api_get(token, f"/files/{file_key}/styles", timeout=60)
    meta = data.get("meta", {})
    styles = meta.get("styles", [])

    result = []
    for s in styles:
        result.append({
            "key": s.get("key"),
            "name": s.get("name"),
            "description": s.get("description", ""),
            "style_type": s.get("style_type"),
            "node_id": s.get("node_id"),
            "thumbnail_url": s.get("thumbnail_url"),
            "created_at": s.get("created_at"),
            "updated_at": s.get("updated_at"),
        })
    print(json.dumps({"total": len(result), "styles": result},
                     ensure_ascii=False, indent=2))


def _cmd_list_team_components(args):
    token = _get_token()
    params: dict = {
        "page_size": min(args.page_size, 100),
    }
    if args.after:
        params["after"] = args.after

    data = _api_get(token, f"/teams/{args.team_id}/components", params, timeout=60)
    meta = data.get("meta", {})
    components = meta.get("components", [])

    result = []
    for c in components:
        result.append({
            "key": c.get("key"),
            "name": c.get("name"),
            "description": c.get("description", ""),
            "file_key": c.get("file_key"),
            "node_id": c.get("node_id"),
            "thumbnail_url": c.get("thumbnail_url"),
            "containing_frame": c.get("containing_frame", {}).get("name"),
        })

    cursor = meta.get("cursor", {})
    print(json.dumps({
        "total": len(result),
        "components": result,
        "cursor_after": cursor.get("after"),
    }, ensure_ascii=False, indent=2))


def main():
    parser = argparse.ArgumentParser(description="Figma design file tool (read-only)")
    sub = parser.add_subparsers(dest="action", required=True)

    # ── setup / clear ──
    p_setup = sub.add_parser("setup", help="Set Figma Personal Access Token")
    p_setup.add_argument("--token", required=True,
                         help="Personal Access Token (file_content:read scope)")
    sub.add_parser("clear-credentials", help="Clear Figma credentials")

    # ── get-file ──
    p_file = sub.add_parser("get-file", help="Get file structure")
    p_file.add_argument("--file-key", required=True,
                        help="Figma file key or full URL")
    p_file.add_argument("--depth", type=int, default=2,
                        help="Document tree depth (default: 2, pages + top frames)")
    p_file.add_argument("--node-ids",
                        help="Comma-separated node IDs to filter")

    # ── get-nodes ──
    p_nodes = sub.add_parser("get-nodes", help="Get specific node details")
    p_nodes.add_argument("--file-key", required=True,
                         help="Figma file key or full URL")
    p_nodes.add_argument("--node-ids", required=True,
                         help="Comma-separated node IDs (e.g. '1:2,3:4')")
    p_nodes.add_argument("--depth", type=int,
                         help="Subtree depth limit")

    # ── export-images ──
    p_img = sub.add_parser("export-images", help="Export nodes as images")
    p_img.add_argument("--file-key", required=True,
                       help="Figma file key or full URL")
    p_img.add_argument("--node-ids", required=True,
                       help="Comma-separated node IDs to export")
    p_img.add_argument("--format", choices=["png", "svg", "pdf", "jpg"],
                       default="png", help="Image format (default: png)")
    p_img.add_argument("--scale", type=float, default=1,
                       help="Scale factor 0.01-4 (default: 1)")

    # ── list-components ──
    p_comp = sub.add_parser("list-components",
                            help="List published components in a file")
    p_comp.add_argument("--file-key", required=True,
                        help="Figma file key or full URL")

    # ── list-styles ──
    p_style = sub.add_parser("list-styles",
                             help="List published styles in a file")
    p_style.add_argument("--file-key", required=True,
                         help="Figma file key or full URL")

    # ── list-team-components ──
    p_team = sub.add_parser("list-team-components",
                            help="List team library components")
    p_team.add_argument("--team-id", required=True,
                        help="Figma team ID")
    p_team.add_argument("--page-size", type=int, default=30,
                        help="Results per page (max 100)")
    p_team.add_argument("--after", help="Cursor for pagination")

    args = parser.parse_args()

    dispatch = {
        "setup": _cmd_setup,
        "clear-credentials": _cmd_clear_credentials,
        "get-file": _cmd_get_file,
        "get-nodes": _cmd_get_nodes,
        "export-images": _cmd_export_images,
        "list-components": _cmd_list_components,
        "list-styles": _cmd_list_styles,
        "list-team-components": _cmd_list_team_components,
    }
    dispatch[args.action](args)


if __name__ == "__main__":
    main()
