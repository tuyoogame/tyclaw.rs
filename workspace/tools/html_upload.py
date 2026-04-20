"""上传 HTML 可视化文件到静态存储，获取公开访问 URL。

用法:
  python3 tools/html_upload.py --file /tmp/tyclaw_xxx/viz.html
"""

import argparse
import base64
import json
import os
import sys
import urllib.request

_PROXY_URL = os.environ.get("_TYCLAW_DT_PROXY_URL", "")
_PROXY_TOKEN = os.environ.get("_TYCLAW_DT_PROXY_TOKEN", "")


def upload(filepath: str) -> dict:
    if not _PROXY_URL or not _PROXY_TOKEN:
        return {"error": True, "message": "Proxy env vars not set"}

    with open(filepath, "rb") as f:
        html_bytes = f.read()

    upload_url = _PROXY_URL.replace("/api/dingtalk-proxy", "/api/upload-viz")
    payload = json.dumps({
        "token": _PROXY_TOKEN,
        "data": base64.b64encode(html_bytes).decode("ascii"),
        "filename": os.path.basename(filepath),
    }).encode()

    req = urllib.request.Request(
        upload_url, data=payload,
        headers={"Content-Type": "application/json"},
    )
    resp = urllib.request.urlopen(req, timeout=30)
    return json.loads(resp.read().decode())


def main():
    parser = argparse.ArgumentParser(description="上传 HTML 可视化获取公开 URL")
    parser.add_argument("--file", required=True, help="HTML 文件路径")
    args = parser.parse_args()

    if not os.path.isfile(args.file):
        print(json.dumps({"error": True,
                          "message": f"File not found: {args.file}"}))
        sys.exit(1)

    result = upload(args.file)
    print(json.dumps(result, ensure_ascii=False))
    if result.get("error"):
        sys.exit(1)


if __name__ == "__main__":
    main()
