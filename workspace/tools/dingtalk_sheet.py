"""
钉钉在线表格读写工具
通过钉钉开放平台 API 操作在线表格（Workbook），支持工作表管理、行列操作（含显隐）、
单元格区域读写与样式设置、合并单元格、下拉列表、条件格式、合并区域探测等

用法示例:
  python tools/dingtalk_sheet.py list-sheets --workbook-id <id>
  python tools/dingtalk_sheet.py get-range --workbook-id <id> --sheet-id <id> --range "A1:Z10"
  python tools/dingtalk_sheet.py update-range --workbook-id <id> --sheet-id <id> --range "A1:C1" --data '[["a","b","c"]]'
  python tools/dingtalk_sheet.py update-sheet --workbook-id <id> --sheet-id <id> --name "新名称" --frozen-rows 1
  python tools/dingtalk_sheet.py set-columns-width --workbook-id <id> --sheet-id <id> --column 0 --count 5 --width 120
  python tools/dingtalk_sheet.py set-rows-height --workbook-id <id> --sheet-id <id> --row 0 --count 3 --height 40
  python tools/dingtalk_sheet.py merge-cells --workbook-id <id> --sheet-id <id> --range "A1:B2"
  python tools/dingtalk_sheet.py insert-dropdown --workbook-id <id> --sheet-id <id> --range "C1:C10" --options '[{"value":"是","color":"#00ff00"},{"value":"否","color":"#ff0000"}]'
  python tools/dingtalk_sheet.py delete-dropdown --workbook-id <id> --sheet-id <id> --range "C1:C10"
  python tools/dingtalk_sheet.py find-all --workbook-id <id> --sheet-id <id> --text "关键词" --select a1Notation
  python tools/dingtalk_sheet.py create-conditional-format --workbook-id <id> --sheet-id <id> --ranges '["A1:A100"]' --number-op greater --value1 90 --bg-color "#00ff00"
"""

import argparse
import json
import os
import sys
import urllib.parse

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
    """统一处理 API 响应，非 2xx 时输出错误信息并退出"""
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


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _get_operator_id(config, args):
    return require_operator_id(config, args, scope_label="钉钉表格")


def _encode_range(r):
    return urllib.parse.quote(r, safe="")


def _load_data(args, required=True):
    """从 --data 或 --data-file 加载二维数组"""
    raw = None
    if getattr(args, "data_file", None):
        with open(args.data_file, "r", encoding="utf-8") as f:
            raw = f.read()
    elif getattr(args, "data", None):
        raw = args.data
    if raw is None:
        if required:
            print("Error: --data or --data-file is required", file=sys.stderr)
            sys.exit(1)
        return None
    try:
        values = json.loads(raw)
    except json.JSONDecodeError as e:
        print(f"Error: invalid JSON data: {e}", file=sys.stderr)
        sys.exit(1)
    if not isinstance(values, list) or (values and not isinstance(values[0], list)):
        print("Error: data must be a 2D array, e.g. [[\"a\",\"b\"],[\"c\",\"d\"]]", file=sys.stderr)
        sys.exit(1)
    return values


def _load_json_arg(raw):
    """解析 JSON 字符串参数，返回解析后的对象"""
    if raw is None:
        return None
    try:
        return json.loads(raw)
    except json.JSONDecodeError as e:
        print(f"Error: invalid JSON: {e}", file=sys.stderr)
        sys.exit(1)


# ---------------------------------------------------------------------------
# 工作表子命令
# ---------------------------------------------------------------------------

def cmd_list_sheets(config, args):
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)
    result = _api_get(token, f"/v1.0/doc/workbooks/{args.workbook_id}/sheets", {"operatorId": oid})

    fmt = getattr(args, "format", "json")
    sheets = result.get("value", [])
    if fmt == "markdown":
        headers = ["id", "name", "rowCount", "columnCount"]
        rows = [[str(s.get(h, "")) for h in headers] for s in sheets]
        print(format_markdown_table(headers, rows))
    else:
        print(format_json(result))


def cmd_get_sheet(config, args):
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)
    result = _api_get(
        token,
        f"/v1.0/doc/workbooks/{args.workbook_id}/sheets/{args.sheet_id}",
        {"operatorId": oid},
    )
    print(format_json(result))


def cmd_create_sheet(config, args):
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)
    result = _api_post(
        token,
        f"/v1.0/doc/workbooks/{args.workbook_id}/sheets",
        data={"name": args.name},
        params={"operatorId": oid},
    )
    print(format_json(result))


# ---------------------------------------------------------------------------
# 行列子命令
# ---------------------------------------------------------------------------

def cmd_insert_rows(config, args):
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)
    result = _api_post(
        token,
        f"/v1.0/doc/workbooks/{args.workbook_id}/sheets/{args.sheet_id}/insertRowsBefore",
        data={"row": args.row, "rowCount": args.count},
        params={"operatorId": oid},
    )
    print(format_json(result))


def cmd_insert_columns(config, args):
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)
    result = _api_post(
        token,
        f"/v1.0/doc/workbooks/{args.workbook_id}/sheets/{args.sheet_id}/insertColumnsBefore",
        data={"column": args.column, "columnCount": args.count},
        params={"operatorId": oid},
    )
    print(format_json(result))


def cmd_delete_rows(config, args):
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)
    result = _api_post(
        token,
        f"/v1.0/doc/workbooks/{args.workbook_id}/sheets/{args.sheet_id}/deleteRows",
        data={"row": args.row, "rowCount": args.count},
        params={"operatorId": oid},
    )
    print(format_json(result))


def cmd_delete_columns(config, args):
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)
    result = _api_post(
        token,
        f"/v1.0/doc/workbooks/{args.workbook_id}/sheets/{args.sheet_id}/deleteColumns",
        data={"column": args.column, "columnCount": args.count},
        params={"operatorId": oid},
    )
    print(format_json(result))


def cmd_set_rows_visibility(config, args):
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)
    result = _api_post(
        token,
        f"/v1.0/doc/workbooks/{args.workbook_id}/sheets/{args.sheet_id}/setRowsVisibility",
        data={"row": args.row, "rowCount": args.count, "visibility": args.visibility},
        params={"operatorId": oid},
    )
    print(format_json(result))


def cmd_set_columns_visibility(config, args):
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)
    result = _api_post(
        token,
        f"/v1.0/doc/workbooks/{args.workbook_id}/sheets/{args.sheet_id}/setColumnsVisibility",
        data={"column": args.column, "columnCount": args.count, "visibility": args.visibility},
        params={"operatorId": oid},
    )
    print(format_json(result))


# ---------------------------------------------------------------------------
# 单元格区域子命令
# ---------------------------------------------------------------------------

def cmd_get_range(config, args):
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)
    encoded = _encode_range(args.range)
    params = {"operatorId": oid}
    select = getattr(args, "select", None)
    if select:
        params["select"] = select
    result = _api_get(
        token,
        f"/v1.0/doc/workbooks/{args.workbook_id}/sheets/{args.sheet_id}/ranges/{encoded}",
        params,
    )

    fmt = getattr(args, "format", "json")
    if fmt == "markdown":
        display = result.get("displayValues", result.get("values", []))
        if display:
            headers = [str(v) for v in display[0]]
            rows = display[1:]
            print(format_markdown_table(headers, rows))
        else:
            print("No data.")
    else:
        print(format_json(result))


def cmd_update_range(config, args):
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)
    encoded = _encode_range(args.range)

    data = {}
    values = _load_data(args, required=False)
    if values is not None:
        data["values"] = values

    bg = _load_json_arg(getattr(args, "background_colors", None))
    if bg is not None:
        data["backgroundColors"] = bg

    fs = _load_json_arg(getattr(args, "font_sizes", None))
    if fs is not None:
        data["fontSizes"] = fs

    fw = _load_json_arg(getattr(args, "font_weights", None))
    if fw is not None:
        data["fontWeights"] = fw

    ha = _load_json_arg(getattr(args, "h_aligns", None))
    if ha is not None:
        data["horizontalAlignments"] = ha

    va = _load_json_arg(getattr(args, "v_aligns", None))
    if va is not None:
        data["verticalAlignments"] = va

    hl = _load_json_arg(getattr(args, "hyperlinks", None))
    if hl is not None:
        data["hyperlinks"] = hl

    nf = getattr(args, "number_format", None)
    if nf is not None:
        data["numberFormat"] = nf

    if not data:
        print("Error: at least one of --data, --background-colors, --font-sizes, "
              "--font-weights, --h-aligns, --v-aligns, --hyperlinks, --number-format "
              "is required", file=sys.stderr)
        sys.exit(1)

    result = _api_put(
        token,
        f"/v1.0/doc/workbooks/{args.workbook_id}/sheets/{args.sheet_id}/ranges/{encoded}",
        data=data,
        params={"operatorId": oid},
    )
    print(format_json(result))


def cmd_append_rows(config, args):
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)
    values = _load_data(args)
    result = _api_post(
        token,
        f"/v1.0/doc/workbooks/{args.workbook_id}/sheets/{args.sheet_id}/appendRows",
        data={"values": values, "operatorId": oid},
    )
    print(format_json(result))


def cmd_clear_data(config, args):
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)
    encoded = _encode_range(args.range)
    result = _api_post(
        token,
        f"/v1.0/doc/workbooks/{args.workbook_id}/sheets/{args.sheet_id}/ranges/{encoded}/clearData",
        params={"operatorId": oid},
    )
    print(format_json(result))


def cmd_autofit_rows(config, args):
    """根据字体大小自动调整行高"""
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)
    result = _api_post(
        token,
        f"/v1.0/doc/workbooks/{args.workbook_id}/sheets/{args.sheet_id}/autofitRows",
        data={"row": args.row, "rowCount": args.count, "fontWidth": args.font_size},
        params={"operatorId": oid},
    )
    print(format_json(result))


def cmd_find_next(config, args):
    """从指定位置查找下一个匹配的单元格"""
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)
    encoded = _encode_range(args.range)

    body = {"text": args.text}
    opts = {}
    if args.match_case:
        opts["matchCase"] = True
    if args.match_entire_cell:
        opts["matchEntireCell"] = True
    if args.use_regexp:
        opts["useRegExp"] = True
    if args.match_formula:
        opts["matchFormulaText"] = True
    if args.include_hidden:
        opts["includeHidden"] = True
    scope = getattr(args, "scope", None)
    if scope:
        opts["scope"] = scope
    if opts:
        body["findOptions"] = opts

    result = _api_post(
        token,
        f"/v1.0/doc/workbooks/{args.workbook_id}/sheets/{args.sheet_id}/ranges/{encoded}/findNext",
        data=body,
        params={"operatorId": oid},
    )
    print(format_json(result))


def cmd_clear_all(config, args):
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)
    encoded = _encode_range(args.range)
    result = _api_post(
        token,
        f"/v1.0/doc/workbooks/{args.workbook_id}/sheets/{args.sheet_id}/ranges/{encoded}/clear",
        params={"operatorId": oid},
    )
    print(format_json(result))


# ---------------------------------------------------------------------------
# 工作表属性 / 布局子命令
# ---------------------------------------------------------------------------

def cmd_update_sheet(config, args):
    """更新工作表属性：重命名、冻结行列、隐藏/显示"""
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)
    data = {}
    if args.name is not None:
        data["name"] = args.name
    if args.frozen_rows is not None:
        data["frozenRowCount"] = args.frozen_rows
    if args.frozen_cols is not None:
        data["frozenColumnCount"] = args.frozen_cols
    if args.visibility is not None:
        data["visibility"] = args.visibility
    if not data:
        print("Error: at least one of --name, --frozen-rows, --frozen-cols, --visibility is required",
              file=sys.stderr)
        sys.exit(1)
    result = _api_put(
        token,
        f"/v1.0/doc/workbooks/{args.workbook_id}/sheets/{args.sheet_id}",
        data=data,
        params={"operatorId": oid},
    )
    print(format_json(result))


def cmd_set_columns_width(config, args):
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)
    result = _api_post(
        token,
        f"/v1.0/doc/workbooks/{args.workbook_id}/sheets/{args.sheet_id}/setColumnsWidth",
        data={"column": args.column, "columnCount": args.count, "width": args.width},
        params={"operatorId": oid},
    )
    print(format_json(result))


def cmd_set_rows_height(config, args):
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)
    result = _api_post(
        token,
        f"/v1.0/doc/workbooks/{args.workbook_id}/sheets/{args.sheet_id}/setRowsHeight",
        data={"row": args.row, "rowCount": args.count, "height": args.height},
        params={"operatorId": oid},
    )
    print(format_json(result))


# ---------------------------------------------------------------------------
# 合并单元格 / 下拉列表
# ---------------------------------------------------------------------------

def cmd_merge_cells(config, args):
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)
    encoded = _encode_range(args.range)
    data = {}
    if args.merge_type:
        data["mergeType"] = args.merge_type
    result = _api_post(
        token,
        f"/v1.0/doc/workbooks/{args.workbook_id}/sheets/{args.sheet_id}/ranges/{encoded}/merge",
        data=data or None,
        params={"operatorId": oid},
    )
    print(format_json(result))


def cmd_insert_dropdown(config, args):
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)
    encoded = _encode_range(args.range)
    options = json.loads(args.options)
    result = _api_post(
        token,
        f"/v1.0/doc/workbooks/{args.workbook_id}/sheets/{args.sheet_id}/ranges/{encoded}/insertDropdownLists",
        data={"options": options},
        params={"operatorId": oid},
    )
    print(format_json(result))


def cmd_delete_dropdown(config, args):
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)
    encoded = _encode_range(args.range)
    result = _api_post(
        token,
        f"/v1.0/doc/workbooks/{args.workbook_id}/sheets/{args.sheet_id}/ranges/{encoded}/deleteDropdownLists",
        params={"operatorId": oid},
    )
    print(format_json(result))


# ---------------------------------------------------------------------------
# 查找 / 条件格式
# ---------------------------------------------------------------------------

def cmd_find_all(config, args):
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)
    body = {"text": args.text}
    opts = {"unionCells": not args.no_union}
    if args.match_case:
        opts["matchCase"] = True
    if args.match_entire_cell:
        opts["matchEntireCell"] = True
    if args.use_regexp:
        opts["useRegExp"] = True
    if args.match_formula:
        opts["matchFormulaText"] = True
    if args.include_hidden:
        opts["includeHidden"] = True
    scope = getattr(args, "scope", None)
    if scope:
        opts["scope"] = scope
    body["findOptions"] = opts

    params = {"operatorId": oid}
    select = getattr(args, "select", None)
    if select:
        params["select"] = select

    result = _api_post(
        token,
        f"/v1.0/doc/workbooks/{args.workbook_id}/sheets/{args.sheet_id}/findAll",
        data=body,
        params=params,
    )
    print(format_json(result))


def cmd_create_conditional_format(config, args):
    token = get_dingtalk_token(config)
    oid = _get_operator_id(config, args)
    ranges = json.loads(args.ranges)
    data = {"ranges": ranges}

    if args.duplicate:
        data["duplicateCondition"] = {"operator": "duplicate"}
    if args.number_op:
        nc = {"operator": args.number_op, "value1": args.value1}
        if args.value2 is not None:
            nc["value2"] = args.value2
        data["numberCondition"] = nc

    style = {}
    if args.bg_color:
        style["backgroundColor"] = args.bg_color
    if args.font_color:
        style["fontColor"] = args.font_color
    if style:
        data["cellStyle"] = style

    result = _api_post(
        token,
        f"/v1.0/doc/workbooks/{args.workbook_id}/sheets/{args.sheet_id}/conditionalFormattingRules",
        data=data,
        params={"operatorId": oid},
    )
    print(format_json(result))


# ---------------------------------------------------------------------------
# argparse
# ---------------------------------------------------------------------------

def _add_common(p):
    p.add_argument("--workbook-id", required=True, help="表格 ID（dentryUuid）")
    p.add_argument("--operator-id", help="操作人 unionId（直接指定，最高优先级）")
    p.add_argument("--user-id", help="操作人 userId，从 credentials.yaml 查找 unionId")


def _add_sheet_id(p):
    p.add_argument("--sheet-id", required=True, help="工作表 ID")


def _add_range(p):
    p.add_argument("--range", required=True, help="单元格区域，如 A1:C10")


def _add_data(p):
    g = p.add_mutually_exclusive_group(required=True)
    g.add_argument("--data", help="二维 JSON 数组（内联）")
    g.add_argument("--data-file", help="从 JSON 文件读取数据")


def _add_format(p):
    p.add_argument("--format", choices=["json", "markdown"], default="json", help="输出格式")


def _add_row_params(p):
    _add_sheet_id(p)
    p.add_argument("--row", type=int, required=True, help="行号（0-based）")
    p.add_argument("--count", type=int, required=True, help="行数")


def _add_column_params(p):
    _add_sheet_id(p)
    p.add_argument("--column", type=int, required=True, help="列号（0-based）")
    p.add_argument("--count", type=int, required=True, help="列数")


def main():
    parser = argparse.ArgumentParser(description="钉钉在线表格读写工具")
    parser.add_argument("--config", help="配置文件路径")
    sub = parser.add_subparsers(dest="command", required=True)

    p = sub.add_parser("list-sheets", help="获取所有工作表")
    _add_common(p)
    _add_format(p)

    p = sub.add_parser("get-sheet", help="获取单个工作表详情")
    _add_common(p)
    _add_sheet_id(p)

    p = sub.add_parser("create-sheet", help="创建工作表")
    _add_common(p)
    p.add_argument("--name", required=True, help="工作表名称")

    p = sub.add_parser("insert-rows", help="在指定行上方插入若干行")
    _add_common(p)
    _add_row_params(p)

    p = sub.add_parser("insert-columns", help="在指定列左侧插入若干列")
    _add_common(p)
    _add_column_params(p)

    p = sub.add_parser("delete-rows", help="删除行")
    _add_common(p)
    _add_row_params(p)

    p = sub.add_parser("delete-columns", help="删除列")
    _add_common(p)
    _add_column_params(p)

    p = sub.add_parser("set-rows-visibility", help="设置行隐藏或显示")
    _add_common(p)
    _add_row_params(p)
    p.add_argument("--visibility", required=True, choices=["visible", "hidden"], help="visible 或 hidden")

    p = sub.add_parser("set-columns-visibility", help="设置列隐藏或显示")
    _add_common(p)
    _add_column_params(p)
    p.add_argument("--visibility", required=True, choices=["visible", "hidden"], help="visible 或 hidden")

    p = sub.add_parser("get-range", help="获取单元格区域数据")
    _add_common(p)
    _add_sheet_id(p)
    _add_range(p)
    _add_format(p)
    p.add_argument("--select", help="筛选返回字段，逗号分隔（如 values,backgroundColors,fontSizes）")

    p = sub.add_parser("update-range", help="更新单元格区域（值+样式）")
    _add_common(p)
    _add_sheet_id(p)
    _add_range(p)
    g = p.add_mutually_exclusive_group()
    g.add_argument("--data", help="values 二维 JSON 数组（内联）")
    g.add_argument("--data-file", help="从 JSON 文件读取 values 数据")
    p.add_argument("--background-colors", help='背景色二维数组 JSON，如 [["#ff0000","#00ff00"]]')
    p.add_argument("--font-sizes", help="字号二维数组 JSON，如 [[10,14,20]]")
    p.add_argument("--font-weights", help='加粗二维数组 JSON，如 [["bold","normal"]]')
    p.add_argument("--h-aligns", help='水平对齐二维数组 JSON，如 [["left","center","right"]]')
    p.add_argument("--v-aligns", help='垂直对齐二维数组 JSON，如 [["top","middle","bottom"]]')
    p.add_argument("--hyperlinks", help='超链接二维数组 JSON，type: path/range/sheet')
    p.add_argument("--number-format", help='数字格式串，如 "#,##0.00" / "@" / "0%%" 等')

    p = sub.add_parser("autofit-rows", help="根据字体大小自动调整行高")
    _add_common(p)
    _add_sheet_id(p)
    p.add_argument("--row", type=int, required=True, help="起始行（0-based）")
    p.add_argument("--count", type=int, required=True, help="调整行数")
    p.add_argument("--font-size", type=int, required=True, help="字号大小")

    p = sub.add_parser("find-next", help="查找下一个匹配的单元格")
    _add_common(p)
    _add_sheet_id(p)
    _add_range(p)
    p.add_argument("--text", required=True, help="查找文本")
    p.add_argument("--scope", help="搜索范围（A1 表示法，如 A1:E10）")
    p.add_argument("--match-case", action="store_true", help="区分大小写")
    p.add_argument("--match-entire-cell", action="store_true", help="全单元格匹配")
    p.add_argument("--use-regexp", action="store_true", help="正则匹配")
    p.add_argument("--match-formula", action="store_true", help="搜索公式文本")
    p.add_argument("--include-hidden", action="store_true", help="包含隐藏单元格")

    p = sub.add_parser("append-rows", help="追加行到工作表末尾")
    _add_common(p)
    _add_sheet_id(p)
    _add_data(p)

    p = sub.add_parser("clear-data", help="清除区域数据（保留格式）")
    _add_common(p)
    _add_sheet_id(p)
    _add_range(p)

    p = sub.add_parser("clear-all", help="清除区域所有内容（含格式）")
    _add_common(p)
    _add_sheet_id(p)
    _add_range(p)

    # -- 工作表属性 / 布局 --
    p = sub.add_parser("update-sheet", help="更新工作表属性（重命名/冻结/隐藏）")
    _add_common(p)
    _add_sheet_id(p)
    p.add_argument("--name", help="新的工作表名")
    p.add_argument("--frozen-rows", type=int, help="冻结至第 N 行（0=不冻结）")
    p.add_argument("--frozen-cols", type=int, help="冻结至第 N 列（0=不冻结）")
    p.add_argument("--visibility", choices=["visible", "hidden"], help="显示或隐藏")

    p = sub.add_parser("set-columns-width", help="批量设置列宽")
    _add_common(p)
    _add_column_params(p)
    p.add_argument("--width", type=int, required=True, help="列宽（像素）")

    p = sub.add_parser("set-rows-height", help="批量设置行高")
    _add_common(p)
    _add_row_params(p)
    p.add_argument("--height", type=int, required=True, help="行高（像素）")

    # -- 合并单元格 / 下拉列表 --
    p = sub.add_parser("merge-cells", help="合并单元格")
    _add_common(p)
    _add_sheet_id(p)
    _add_range(p)
    p.add_argument("--merge-type", choices=["mergeAll", "mergeRows", "mergeColumns"],
                   default=None, help="合并方式（默认 mergeAll）")

    p = sub.add_parser("insert-dropdown", help="插入下拉列表")
    _add_common(p)
    _add_sheet_id(p)
    _add_range(p)
    p.add_argument("--options", required=True,
                   help='JSON 数组，如 [{"value":"选项1","color":"#ff0000"}]')

    p = sub.add_parser("delete-dropdown", help="删除下拉列表")
    _add_common(p)
    _add_sheet_id(p)
    _add_range(p)

    # -- 查找 / 条件格式 --
    p = sub.add_parser("find-all", help="查找所有匹配的单元格")
    _add_common(p)
    _add_sheet_id(p)
    p.add_argument("--text", required=True, help="查找文本（支持正则）")
    p.add_argument("--select", help="筛选返回字段，如 a1Notation,values")
    p.add_argument("--scope", help="搜索范围（A1 表示法）")
    p.add_argument("--match-case", action="store_true")
    p.add_argument("--match-entire-cell", action="store_true")
    p.add_argument("--use-regexp", action="store_true")
    p.add_argument("--match-formula", action="store_true")
    p.add_argument("--include-hidden", action="store_true")
    p.add_argument("--no-union", action="store_true", help="不聚合单元格地址")

    p = sub.add_parser("create-conditional-format", help="创建条件格式规则")
    _add_common(p)
    _add_sheet_id(p)
    p.add_argument("--ranges", required=True, help='JSON 数组，如 ["A1:B10"]')
    p.add_argument("--duplicate", action="store_true", help="重复值规则")
    p.add_argument("--number-op", choices=["equal", "not-equal", "greater", "greater-equal",
                                           "less", "less-equal", "between", "not-between"],
                   help="数字比较运算符")
    p.add_argument("--value1", help="比较值1")
    p.add_argument("--value2", help="比较值2（between/not-between 时需要）")
    p.add_argument("--bg-color", help="背景色，如 #ff0000")
    p.add_argument("--font-color", help="字体颜色，如 #ff0000")

    args = parser.parse_args()
    config = load_config(getattr(args, "config", None))

    dispatch = {
        "list-sheets": cmd_list_sheets,
        "get-sheet": cmd_get_sheet,
        "create-sheet": cmd_create_sheet,
        "insert-rows": cmd_insert_rows,
        "insert-columns": cmd_insert_columns,
        "delete-rows": cmd_delete_rows,
        "delete-columns": cmd_delete_columns,
        "set-rows-visibility": cmd_set_rows_visibility,
        "set-columns-visibility": cmd_set_columns_visibility,
        "get-range": cmd_get_range,
        "update-range": cmd_update_range,
        "autofit-rows": cmd_autofit_rows,
        "find-next": cmd_find_next,
        "append-rows": cmd_append_rows,
        "clear-data": cmd_clear_data,
        "clear-all": cmd_clear_all,
        "update-sheet": cmd_update_sheet,
        "set-columns-width": cmd_set_columns_width,
        "set-rows-height": cmd_set_rows_height,
        "merge-cells": cmd_merge_cells,
        "insert-dropdown": cmd_insert_dropdown,
        "delete-dropdown": cmd_delete_dropdown,
        "find-all": cmd_find_all,
        "create-conditional-format": cmd_create_conditional_format,
    }
    dispatch[args.command](config, args)


if __name__ == "__main__":
    main()
