"""
钉钉文档工具
通过钉钉开放平台 API 操作在线文档，支持文档内容读取、内容覆写、块元素 CRUD、段落追加、图片上传

用法示例:
  python tools/dingtalk_doc.py read-doc --doc-id <id>
  python tools/dingtalk_doc.py read-doc --doc-id <id> --format json
  python tools/dingtalk_doc.py overwrite-doc --doc-id <id> --content "# Hello"
  python tools/dingtalk_doc.py get-blocks --doc-id <id>
  python tools/dingtalk_doc.py insert-block --doc-id <id> --block-type paragraph --text "内容"
  python tools/dingtalk_doc.py upload-image --doc-id <id> --file /path/to/image.png
"""

import argparse
import json
import os
import sys

import requests

from dingtalk_auth import require_operator_id
from utils import load_config, format_json, get_dingtalk_token

BASE_URL = "https://api.dingtalk.com"


# ---------------------------------------------------------------------------
# HTTP helpers（与 dingtalk_sheet.py 一致）
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


def _api_request_quiet(token, method, path, data=None, params=None):
    """HTTP request that returns (ok, data) instead of sys.exit on error."""
    try:
        if _PROXY_URL:
            resp = requests.post(_PROXY_URL, json={
                "token": _PROXY_TOKEN, "method": method,
                "path": path, "data": data, "params": params,
            }, timeout=30)
        else:
            fn = {"GET": requests.get, "POST": requests.post,
                  "PUT": requests.put, "DELETE": requests.delete}[method]
            kwargs = {"headers": _headers(token), "params": params, "timeout": 30}
            if method in ("POST", "PUT"):
                kwargs["json"] = data
            resp = fn(f"{BASE_URL}{path}", **kwargs)
        if resp.ok:
            return True, resp.json() if resp.text else {}
        try:
            return False, resp.json()
        except Exception:
            return False, {"code": str(resp.status_code), "message": resp.text[:500]}
    except Exception as e:
        return False, {"code": "exception", "message": str(e)}


def _api_post_quiet(token, path, data=None, params=None):
    """POST that returns (ok, data) instead of sys.exit on error."""
    return _api_request_quiet(token, "POST", path, data=data, params=params)



# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _get_operator_id(config, args):
    return require_operator_id(config, args, scope_label="钉钉文档")


# ---------------------------------------------------------------------------
# 文档读取 helpers
# ---------------------------------------------------------------------------

_HACK_READABLE_TYPES = frozenset({"unorderedList", "orderedList", "blockquote"})


def _get_all_blocks(token, doc_id, oid):
    """获取文档所有块（自动分页）"""
    all_blocks = []
    next_token = None
    while True:
        params = {"operatorId": oid, "maxResults": 200}
        if next_token:
            params["nextToken"] = next_token
        result = _api_get(
            token, f"/v1.0/doc/suites/documents/{doc_id}/blocks", params,
        )
        data = result.get("result", {})
        all_blocks.extend(data.get("data", []))
        next_token = data.get("nextToken")
        if not next_token:
            break
    return all_blocks


def _read_block_text(token, doc_id, block, oid):
    """提取单个块文本。标准字段优先，list/blockquote fallback appendText("") hack。

    Returns (text, method): text=None 表示不可读, method 为 "standard"/"hack"/None
    """
    bt = block.get("blockType", "")
    bid = block.get("id", "")

    if bt == "paragraph":
        return block.get("paragraph", {}).get("text", ""), "standard"
    if bt == "heading":
        return block.get("heading", {}).get("text", ""), "standard"

    if bt in _HACK_READABLE_TYPES:
        ok, resp = _api_post_quiet(
            token,
            f"/v1.0/doc/suites/documents/{doc_id}/blocks/{bid}/paragraph/appendText",
            data={"text": ""},
            params={"operatorId": oid},
        )
        if ok:
            return resp.get("result", {}).get("data", {}).get("text", ""), "hack"
        return None, None

    return None, None


def _block_placeholder(block):
    """为不可读块生成占位文本，返回 None 表示跳过"""
    bt = block.get("blockType", "unknown")
    if bt == "table":
        t = block.get("table", {})
        return f"[表格 {t.get('rowSize', '?')}行x{t.get('colSize', '?')}列]"
    if bt == "unknown":
        return None
    return f"[{bt}]"


# ---------------------------------------------------------------------------
# 文档子命令
# ---------------------------------------------------------------------------

def cmd_overwrite_doc(config, args):
    """覆写文档 — 用 Markdown 或纯文本替换整篇文档内容"""
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)

    content = args.content
    if args.content_file:
        with open(args.content_file, "r", encoding="utf-8") as f:
            content = f.read()
    if not content:
        print("Error: --content or --content-file is required", file=sys.stderr)
        sys.exit(1)

    result = _api_post(
        token,
        f"/v1.0/doc/suites/documents/{args.doc_id}/overwriteContent",
        data={"content": content, "operatorId": oid},
    )
    print(format_json(result))


def cmd_get_blocks(config, args):
    """查询块元素列表"""
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)
    params = {"operatorId": oid}
    if args.max_results:
        params["maxResults"] = args.max_results

    result = _api_get(
        token,
        f"/v1.0/doc/suites/documents/{args.doc_id}/blocks",
        params,
    )

    fmt = getattr(args, "format", "json")
    if fmt == "summary":
        blocks = result.get("result", {}).get("data", [])
        for b in blocks:
            bid = b.get("id", "?")
            bt = b.get("blockType", "?")
            text = ""
            if bt == "paragraph":
                text = b.get("paragraph", {}).get("text", "")
            elif bt == "heading":
                text = b.get("heading", {}).get("text", "")
            idx = b.get("index", "?")
            print(f"[{idx}] {bt:20s} id={bid}  {text[:60]}")
    else:
        print(format_json(result))


def cmd_insert_content(config, args):
    """插入内容 — 在文档中插入 Markdown 内容"""
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)

    content = args.content
    if args.content_file:
        with open(args.content_file, "r", encoding="utf-8") as f:
            content = f.read()
    if not content:
        print("Error: --content or --content-file is required", file=sys.stderr)
        sys.exit(1)

    body = {"content": {"type": "markdown", "content": content}, "operatorId": oid}
    if args.index is not None:
        body["index"] = args.index

    result = _api_post(
        token,
        f"/v1.0/doc/suites/documents/{args.doc_id}/content",
        data=body,
    )
    print(format_json(result))


def cmd_insert_block(config, args):
    """插入块元素 — 在文档中插入段落、标题等块"""
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)

    element = {"blockType": args.block_type}
    if args.block_type == "paragraph":
        element["paragraph"] = {"text": args.text or ""}
    elif args.block_type == "heading":
        element["heading"] = {"level": args.level or 1, "text": args.text or ""}
    else:
        if args.body:
            element[args.block_type] = json.loads(args.body)

    body = {"element": element, "operatorId": oid}
    if args.index is not None:
        body["index"] = args.index

    result = _api_post(
        token,
        f"/v1.0/doc/suites/documents/{args.doc_id}/blocks",
        data=body,
    )
    print(format_json(result))


def cmd_update_block(config, args):
    """更新块元素"""
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)

    element = {"blockType": args.block_type}
    if args.block_type == "paragraph":
        element["paragraph"] = {"text": args.text or ""}
    elif args.block_type == "heading":
        element["heading"] = {"level": args.level or 1, "text": args.text or ""}
    else:
        if args.body:
            element[args.block_type] = json.loads(args.body)

    result = _api_put(
        token,
        f"/v1.0/doc/suites/documents/{args.doc_id}/blocks/{args.block_id}",
        data={"element": element, "operatorId": oid},
    )
    print(format_json(result))


def cmd_delete_block(config, args):
    """删除块元素"""
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)
    result = _api_delete(
        token,
        f"/v1.0/doc/suites/documents/{args.doc_id}/blocks/{args.block_id}",
        params={"operatorId": oid},
    )
    print(format_json(result))


def cmd_append_text(config, args):
    """在段落末尾追加纯文本"""
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)
    result = _api_post(
        token,
        f"/v1.0/doc/suites/documents/{args.doc_id}/blocks/{args.block_id}/paragraph/appendText",
        data={"text": args.text},
        params={"operatorId": oid},
    )
    print(format_json(result))


def cmd_append_element(config, args):
    """在段落末尾追加行内元素（支持样式）"""
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)

    properties = {"text": args.text}
    if args.style:
        try:
            properties["style"] = json.loads(args.style)
        except json.JSONDecodeError as e:
            print(f"Error: invalid --style JSON: {e}", file=sys.stderr)
            sys.exit(1)

    result = _api_post(
        token,
        f"/v1.0/doc/suites/documents/{args.doc_id}/blocks/{args.block_id}/paragraph/appendElement",
        data={"elementType": args.element_type, "properties": properties},
        params={"operatorId": oid},
    )
    print(format_json(result))


def cmd_upload_image(config, args):
    """上传图片到文档 — 获取上传 URL + PUT 上传 + 返回资源 ID"""
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)

    import mimetypes
    file_path = args.file
    if not os.path.exists(file_path):
        print(f"Error: file not found: {file_path}", file=sys.stderr)
        sys.exit(1)

    file_size = os.path.getsize(file_path)
    file_name = os.path.basename(file_path)
    media_type = mimetypes.guess_type(file_path)[0] or "image/png"

    # Step 1: 获取上传信息
    upload_resp = _api_post(
        token,
        f"/v1.0/doc/docs/resources/{args.doc_id}/uploadInfos/query",
        data={"mediaType": media_type, "resourceName": file_name, "size": file_size},
        params={"operatorId": oid},
    )

    result_data = upload_resp.get("result", upload_resp)
    upload_url = result_data.get("uploadUrl", "")
    resource_id = result_data.get("resourceId", "")
    upload_headers = result_data.get("headers", {})

    if not upload_url:
        print(format_json({"error": True, "message": "No uploadUrl in response", "response": upload_resp}),
              file=sys.stderr)
        sys.exit(1)

    # Step 2: PUT 上传文件
    with open(file_path, "rb") as f:
        put_headers = dict(upload_headers)
        put_headers["Content-Type"] = media_type
        put_resp = requests.put(upload_url, headers=put_headers, data=f, timeout=60)

    if not put_resp.ok:
        print(json.dumps({"error": True, "status": put_resp.status_code, "body": put_resp.text[:500]},
                         ensure_ascii=False, indent=2), file=sys.stderr)
        sys.exit(1)

    print(format_json({
        "resourceId": resource_id,
        "mediaType": media_type,
        "fileName": file_name,
        "fileSize": file_size,
        "message": "Upload successful. Use resourceId in insert-block or append-element to embed the image.",
    }))



def cmd_read_doc(config, args):
    """读取文档内容 — 组合 blocks query + appendText hack 最大化提取文本"""
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)

    blocks = _get_all_blocks(token, args.doc_id, oid)

    fmt = getattr(args, "format", "text")

    if fmt == "json":
        items = []
        for b in blocks:
            text, method = _read_block_text(token, args.doc_id, b, oid)
            entry = {
                "index": b.get("index"),
                "blockType": b.get("blockType", "unknown"),
                "id": b.get("id", ""),
                "text": text,
                "readable": text is not None,
            }
            if method:
                entry["method"] = method
            bt = b.get("blockType", "")
            if bt == "heading" and text is not None:
                entry["level"] = b.get("heading", {}).get("level")
            if bt == "table":
                t = b.get("table", {})
                entry["meta"] = {"rowSize": t.get("rowSize"), "colSize": t.get("colSize")}
            items.append(entry)
        print(format_json(items))
        return

    # --format text: best-effort markdown
    lines = []
    ol_counter = 0
    prev_bt = None
    for b in blocks:
        bt = b.get("blockType", "")
        text, method = _read_block_text(token, args.doc_id, b, oid)

        line = None
        if bt == "heading" and text is not None:
            level_raw = b.get("heading", {}).get("level", "heading-1")
            try:
                level = int(str(level_raw).split("-")[-1])
            except (ValueError, IndexError):
                level = 1
            line = f"{'#' * level} {text}"
            ol_counter = 0
        elif bt == "paragraph" and text is not None:
            line = text
            ol_counter = 0
        elif bt == "unorderedList" and text is not None:
            line = f"- {text}"
            ol_counter = 0
        elif bt == "orderedList" and text is not None:
            ol_counter += 1
            line = f"{ol_counter}. {text}"
        elif bt == "blockquote" and text is not None:
            line = f"> {text}"
            ol_counter = 0
        else:
            ol_counter = 0
            line = _block_placeholder(b)

        if line is not None:
            same_group = (bt in ("unorderedList", "orderedList", "blockquote")
                          and bt == prev_bt)
            if lines and not same_group:
                lines.append("")
            lines.append(line)
        prev_bt = bt if line is not None else prev_bt

    if lines:
        print("\n".join(lines))
    else:
        print("[文档为空或所有内容不可读]")


# ---------------------------------------------------------------------------
# argparse
# ---------------------------------------------------------------------------

def _add_common(p):
    p.add_argument("--doc-id", required=True, help="文档 ID（docKey 或 dentryUuid）")
    p.add_argument("--operator-id", help="操作人 unionId（直接指定）")
    p.add_argument("--user-id", help="操作人 userId，从 credentials.yaml 查找 unionId")


def _add_block_id(p):
    p.add_argument("--block-id", required=True, help="块元素 ID")


def _add_block_type(p):
    p.add_argument("--block-type", required=True, help="块类型（paragraph / heading 等）")
    p.add_argument("--text", help="文本内容（paragraph / heading 用）")
    p.add_argument("--level", type=int, help="标题级别 1-6（heading 用）")
    p.add_argument("--body", help="块内容 JSON（其他类型用）")


def main():
    parser = argparse.ArgumentParser(description="钉钉文档工具")
    parser.add_argument("--config", help="配置文件路径")
    sub = parser.add_subparsers(dest="command", required=True)

    # overwrite-doc
    p = sub.add_parser("overwrite-doc", help="覆写文档（markdown/text 替换全部内容）")
    _add_common(p)
    g = p.add_mutually_exclusive_group(required=True)
    g.add_argument("--content", help="文档内容（内联）")
    g.add_argument("--content-file", help="从文件读取内容")

    # get-blocks
    p = sub.add_parser("get-blocks", help="查询块元素列表")
    _add_common(p)
    p.add_argument("--max-results", type=int, help="最大返回数量")
    p.add_argument("--format", choices=["json", "summary"], default="json", help="输出格式")

    # insert-content
    p = sub.add_parser("insert-content", help="插入 Markdown 内容")
    _add_common(p)
    g = p.add_mutually_exclusive_group(required=True)
    g.add_argument("--content", help="Markdown 内容（内联）")
    g.add_argument("--content-file", help="从文件读取内容")
    p.add_argument("--index", type=int, help="插入位置索引（不指定则追加到末尾）")

    # insert-block
    p = sub.add_parser("insert-block", help="插入块元素（段落/标题等）")
    _add_common(p)
    _add_block_type(p)
    p.add_argument("--index", type=int, help="插入位置索引")

    # update-block
    p = sub.add_parser("update-block", help="更新块元素")
    _add_common(p)
    _add_block_id(p)
    _add_block_type(p)

    # delete-block
    p = sub.add_parser("delete-block", help="删除块元素")
    _add_common(p)
    _add_block_id(p)

    # append-text
    p = sub.add_parser("append-text", help="在段落末尾追加纯文本")
    _add_common(p)
    _add_block_id(p)
    p.add_argument("--text", required=True, help="追加的文本内容")

    # append-element
    p = sub.add_parser("append-element", help="在段落末尾追加行内元素（支持样式）")
    _add_common(p)
    _add_block_id(p)
    p.add_argument("--text", required=True, help="元素文本内容")
    p.add_argument("--element-type", default="text", help="元素类型（默认 text）")
    p.add_argument("--style", help="样式 JSON，如 {\"bold\":true}")

    # upload-image
    p = sub.add_parser("upload-image", help="上传图片到文档（获取URL + 上传 + 返回资源ID）")
    _add_common(p)
    p.add_argument("--file", required=True, help="本地图片文件路径")

    # read-doc
    p = sub.add_parser("read-doc", help="读取文档内容（标准读取 + 列表/引用 hack 补全）")
    _add_common(p)
    p.add_argument("--format", choices=["text", "json"], default="text",
                   help="输出格式：text(默认,markdown风格) / json(结构化)")

    args = parser.parse_args()
    config = load_config(getattr(args, "config", None))

    dispatch = {
        "read-doc": cmd_read_doc,
        "overwrite-doc": cmd_overwrite_doc,
        "get-blocks": cmd_get_blocks,
        "insert-content": cmd_insert_content,
        "insert-block": cmd_insert_block,
        "update-block": cmd_update_block,
        "delete-block": cmd_delete_block,
        "append-text": cmd_append_text,
        "append-element": cmd_append_element,
        "upload-image": cmd_upload_image,
    }
    dispatch[args.command](config, args)


if __name__ == "__main__":
    main()
