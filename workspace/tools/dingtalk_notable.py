"""
钉钉多维表格（AI 表格 / Notable）读写工具
通过钉钉开放平台 API 操作多维表格，支持数据表管理、字段管理、记录 CRUD、附件上传

用法示例:
  python tools/dingtalk_notable.py list-sheets --base-id <id>
  python tools/dingtalk_notable.py list-fields --base-id <id> --sheet-id <id>
  python tools/dingtalk_notable.py create-records --base-id <id> --sheet-id <id> --records '[{"fields":{"名称":"test"}}]'
"""

import argparse
import json
import mimetypes
import os
import sys

import requests

from dingtalk_auth import require_operator_id
from utils import load_config, format_json, format_markdown_table, get_dingtalk_token

BASE_URL = "https://api.dingtalk.com"


# ---------------------------------------------------------------------------
# HTTP helpers (与 dingtalk_sheet.py 同构，支持 proxy 双模式)
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

def _get_operator_id(config, args):
    return require_operator_id(config, args, scope_label="钉钉多维表格")


def _prefix(args):
    return f"/v1.0/notable/bases/{args.base_id}"


def _load_json_arg(raw, arg_name):
    """解析 JSON 字符串参数"""
    if raw is None:
        return None
    try:
        return json.loads(raw)
    except json.JSONDecodeError as e:
        print(f"Error: invalid JSON for {arg_name}: {e}", file=sys.stderr)
        sys.exit(1)


def _load_json_required(raw, file_path, arg_name):
    """从 --xxx 或 --xxx-file 加载 JSON，至少有一个"""
    if file_path:
        with open(file_path, "r", encoding="utf-8") as f:
            raw = f.read()
    if raw is None:
        print(f"Error: --{arg_name} or --{arg_name}-file is required", file=sys.stderr)
        sys.exit(1)
    return _load_json_arg(raw, arg_name)


# ---------------------------------------------------------------------------
# 数据表子命令 (5)
# ---------------------------------------------------------------------------

def cmd_list_sheets(config, args):
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)
    result = _api_get(token, f"{_prefix(args)}/sheets", {"operatorId": oid})

    fmt = getattr(args, "format", "json")
    sheets = result.get("value", [])
    if fmt == "markdown":
        headers = ["id", "name"]
        rows = [[str(s.get(h, "")) for h in headers] for s in sheets]
        print(format_markdown_table(headers, rows))
    else:
        print(format_json(result))


def cmd_get_sheet(config, args):
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)
    result = _api_get(
        token, f"{_prefix(args)}/sheets/{args.sheet_id}", {"operatorId": oid},
    )
    print(format_json(result))


def cmd_create_sheet(config, args):
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)
    data = {"name": args.name}
    fields = _load_json_arg(args.fields, "fields")
    if fields:
        data["fields"] = fields
    result = _api_post(
        token, f"{_prefix(args)}/sheets", data=data, params={"operatorId": oid},
    )
    print(format_json(result))


def cmd_update_sheet(config, args):
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)
    result = _api_put(
        token, f"{_prefix(args)}/sheets/{args.sheet_id}",
        data={"name": args.name}, params={"operatorId": oid},
    )
    print(format_json(result))


def cmd_delete_sheet(config, args):
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)
    result = _api_delete(
        token, f"{_prefix(args)}/sheets/{args.sheet_id}", params={"operatorId": oid},
    )
    print(format_json(result))


# ---------------------------------------------------------------------------
# 字段子命令 (4)
# ---------------------------------------------------------------------------

def cmd_list_fields(config, args):
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)
    result = _api_get(
        token, f"{_prefix(args)}/sheets/{args.sheet_id}/fields", {"operatorId": oid},
    )

    fmt = getattr(args, "format", "json")
    fields = result.get("value", [])
    if fmt == "markdown":
        headers = ["id", "name", "type"]
        rows = [[str(f.get(h, "")) for h in headers] for f in fields]
        print(format_markdown_table(headers, rows))
    else:
        print(format_json(result))


def cmd_create_field(config, args):
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)
    data = {"name": args.name, "type": args.type}
    prop = _load_json_arg(args.property, "property")
    if prop:
        data["property"] = prop
    result = _api_post(
        token, f"{_prefix(args)}/sheets/{args.sheet_id}/fields",
        data=data, params={"operatorId": oid},
    )
    print(format_json(result))


def cmd_update_field(config, args):
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)
    data = {}
    if args.name:
        data["name"] = args.name
    prop = _load_json_arg(args.property, "property")
    if prop:
        data["property"] = prop
    if not data:
        print("Error: at least --name or --property is required", file=sys.stderr)
        sys.exit(1)
    result = _api_put(
        token, f"{_prefix(args)}/sheets/{args.sheet_id}/fields/{args.field_id}",
        data=data, params={"operatorId": oid},
    )
    print(format_json(result))


def cmd_delete_field(config, args):
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)
    result = _api_delete(
        token, f"{_prefix(args)}/sheets/{args.sheet_id}/fields/{args.field_id}",
        params={"operatorId": oid},
    )
    print(format_json(result))


# ---------------------------------------------------------------------------
# 记录子命令 (5)
# ---------------------------------------------------------------------------

def cmd_create_records(config, args):
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)
    records = _load_json_required(args.records, getattr(args, "records_file", None), "records")
    if not isinstance(records, list):
        print("Error: records must be a JSON array", file=sys.stderr)
        sys.exit(1)
    result = _api_post(
        token, f"{_prefix(args)}/sheets/{args.sheet_id}/records",
        data={"records": records}, params={"operatorId": oid},
    )
    print(format_json(result))


def cmd_get_record(config, args):
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)
    result = _api_get(
        token, f"{_prefix(args)}/sheets/{args.sheet_id}/records/{args.record_id}",
        {"operatorId": oid},
    )
    print(format_json(result))


def cmd_list_records(config, args):
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)
    data = {}
    if args.max_results:
        data["maxResults"] = args.max_results
    if args.next_token:
        data["nextToken"] = args.next_token
    result = _api_post(
        token, f"{_prefix(args)}/sheets/{args.sheet_id}/records/list",
        data=data, params={"operatorId": oid},
    )

    fmt = getattr(args, "format", "json")
    if fmt == "markdown":
        records = result.get("records", [])
        if records:
            all_keys = []
            seen = set()
            for r in records:
                for k in r.get("fields", {}):
                    if k not in seen:
                        all_keys.append(k)
                        seen.add(k)
            headers = ["id"] + all_keys
            rows = []
            for r in records:
                row = [r.get("id", "")]
                for k in all_keys:
                    v = r.get("fields", {}).get(k, "")
                    row.append(json.dumps(v, ensure_ascii=False) if not isinstance(v, str) else v)
                rows.append(row)
            print(format_markdown_table(headers, rows))
            if result.get("hasMore"):
                print(f"\nhasMore=true, nextToken={result.get('nextToken', '')}")
        else:
            print("No records.")
    else:
        print(format_json(result))


def cmd_update_records(config, args):
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)
    records = _load_json_required(args.records, getattr(args, "records_file", None), "records")
    if not isinstance(records, list):
        print("Error: records must be a JSON array", file=sys.stderr)
        sys.exit(1)
    result = _api_put(
        token, f"{_prefix(args)}/sheets/{args.sheet_id}/records",
        data={"records": records}, params={"operatorId": oid},
    )
    print(format_json(result))


def cmd_delete_records(config, args):
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)
    result = _api_post(
        token, f"{_prefix(args)}/sheets/{args.sheet_id}/records/delete",
        data={"recordIds": args.record_ids}, params={"operatorId": oid},
    )
    print(format_json(result))


# ---------------------------------------------------------------------------
# 附件上传
# ---------------------------------------------------------------------------

def cmd_upload_resource(config, args):
    """三步：获取上传信息 → PUT 上传文件 → 返回 resourceUrl + resourceId"""
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)

    file_path = args.file
    if not os.path.isfile(file_path):
        print(f"Error: file not found: {file_path}", file=sys.stderr)
        sys.exit(1)

    file_size = os.path.getsize(file_path)
    file_name = os.path.basename(file_path)
    media_type = args.media_type or mimetypes.guess_type(file_path)[0] or "application/octet-stream"

    # Step 1: 获取资源上传信息
    upload_path = f"/v1.0/doc/docs/resources/{args.base_id}/uploadInfos/query"
    info = _api_post(token, upload_path, data={
        "resourceName": file_name,
        "size": file_size,
        "mediaType": media_type,
    }, params={"operatorId": oid})

    result = info.get("result", info)
    upload_url = result.get("uploadUrl", "")
    resource_url = result.get("resourceUrl", "")
    resource_id = result.get("resourceId", "")

    if not upload_url:
        print(json.dumps({"error": True, "message": "Failed to get uploadUrl", "detail": info},
                         ensure_ascii=False, indent=2), file=sys.stderr)
        sys.exit(1)

    # Step 2: PUT 上传文件
    with open(file_path, "rb") as f:
        put_resp = requests.put(upload_url, headers={"Content-Type": media_type}, data=f, timeout=120)

    if put_resp.status_code not in (200, 201, 204):
        print(json.dumps({"error": True, "message": f"PUT upload failed: {put_resp.status_code}",
                          "body": put_resp.text[:500]}, ensure_ascii=False, indent=2), file=sys.stderr)
        sys.exit(1)

    # Step 3: 返回结果（供调用方拼装 attachment 字段值）
    print(format_json({
        "filename": file_name,
        "size": file_size,
        "type": media_type,
        "url": resource_url,
        "resourceId": resource_id,
    }))


# ---------------------------------------------------------------------------
# argparse
# ---------------------------------------------------------------------------

def _add_common(p):
    p.add_argument("--base-id", required=True, help="多维表格 ID（baseId / nodeId，从钉钉 URL /nodes/<id> 提取）")
    p.add_argument("--operator-id", help="操作人 unionId（直接指定，最高优先级）")
    p.add_argument("--user-id", help="操作人 userId，从 credentials.yaml 查找 unionId")


def _add_sheet_id(p):
    p.add_argument("--sheet-id", required=True, help="数据表 ID")


def _add_field_id(p):
    p.add_argument("--field-id", required=True, help="字段 ID")


def _add_record_id(p):
    p.add_argument("--record-id", required=True, help="记录 ID")


def _add_format(p):
    p.add_argument("--format", choices=["json", "markdown"], default="json", help="输出格式")


def _add_records_data(p):
    g = p.add_mutually_exclusive_group(required=True)
    g.add_argument("--records", help='记录 JSON 数组（内联），如 \'[{"fields":{"名称":"test"}}]\'')
    g.add_argument("--records-file", help="从 JSON 文件读取记录数据")


def main():
    parser = argparse.ArgumentParser(description="钉钉多维表格（AI 表格 / Notable）读写工具")
    parser.add_argument("--config", help="配置文件路径")
    sub = parser.add_subparsers(dest="command", required=True)

    # --- 数据表 ---
    p = sub.add_parser("list-sheets", help="获取所有数据表")
    _add_common(p)
    _add_format(p)

    p = sub.add_parser("get-sheet", help="获取单个数据表详情")
    _add_common(p)
    _add_sheet_id(p)

    p = sub.add_parser("create-sheet", help="创建数据表")
    _add_common(p)
    p.add_argument("--name", required=True, help="数据表名称")
    p.add_argument("--fields", help='字段定义 JSON 数组，如 \'[{"name":"标题","type":"text"}]\'')

    p = sub.add_parser("update-sheet", help="更新数据表（改名）")
    _add_common(p)
    _add_sheet_id(p)
    p.add_argument("--name", required=True, help="新名称")

    p = sub.add_parser("delete-sheet", help="删除数据表")
    _add_common(p)
    _add_sheet_id(p)

    # --- 字段 ---
    p = sub.add_parser("list-fields", help="获取所有字段")
    _add_common(p)
    _add_sheet_id(p)
    _add_format(p)

    p = sub.add_parser("create-field", help="创建字段")
    _add_common(p)
    _add_sheet_id(p)
    p.add_argument("--name", required=True, help="字段名称")
    p.add_argument("--type", required=True, help="字段类型（text/number/currency/singleSelect/multipleSelect/date/user/department/checkbox/url/attachment/unidirectionalLink/bidirectionalLink）")
    p.add_argument("--property", help="字段属性 JSON（如选项列表、日期格式等）")

    p = sub.add_parser("update-field", help="更新字段")
    _add_common(p)
    _add_sheet_id(p)
    _add_field_id(p)
    p.add_argument("--name", help="新字段名称")
    p.add_argument("--property", help="新字段属性 JSON")

    p = sub.add_parser("delete-field", help="删除字段")
    _add_common(p)
    _add_sheet_id(p)
    _add_field_id(p)

    # --- 记录 ---
    p = sub.add_parser("create-records", help="新增记录（最多 100 条）")
    _add_common(p)
    _add_sheet_id(p)
    _add_records_data(p)

    p = sub.add_parser("get-record", help="获取单条记录")
    _add_common(p)
    _add_sheet_id(p)
    _add_record_id(p)

    p = sub.add_parser("list-records", help="列出多行记录（分页）")
    _add_common(p)
    _add_sheet_id(p)
    p.add_argument("--max-results", type=int, help="每页最多返回条数")
    p.add_argument("--next-token", help="分页令牌（从上次响应的 nextToken 获取）")
    _add_format(p)

    p = sub.add_parser("update-records", help="更新多行记录（最多 100 条）")
    _add_common(p)
    _add_sheet_id(p)
    _add_records_data(p)

    p = sub.add_parser("delete-records", help="删除多行记录（最多 100 条）")
    _add_common(p)
    _add_sheet_id(p)
    p.add_argument("--record-ids", nargs="+", required=True, help="要删除的记录 ID 列表")

    # --- 附件 ---
    p = sub.add_parser("upload-resource", help="上传附件文件（获取上传信息 + PUT 上传 + 返回资源引用）")
    _add_common(p)
    p.add_argument("--file", required=True, help="要上传的文件路径")
    p.add_argument("--media-type", help="文件 MIME 类型（不指定则自动检测）")

    args = parser.parse_args()
    config = load_config(getattr(args, "config", None))

    dispatch = {
        "list-sheets": cmd_list_sheets,
        "get-sheet": cmd_get_sheet,
        "create-sheet": cmd_create_sheet,
        "update-sheet": cmd_update_sheet,
        "delete-sheet": cmd_delete_sheet,
        "list-fields": cmd_list_fields,
        "create-field": cmd_create_field,
        "update-field": cmd_update_field,
        "delete-field": cmd_delete_field,
        "create-records": cmd_create_records,
        "get-record": cmd_get_record,
        "list-records": cmd_list_records,
        "update-records": cmd_update_records,
        "delete-records": cmd_delete_records,
        "upload-resource": cmd_upload_resource,
    }
    dispatch[args.command](config, args)


if __name__ == "__main__":
    main()
