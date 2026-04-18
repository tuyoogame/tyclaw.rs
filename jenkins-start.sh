#!/bin/bash
# Jenkins 启动入口 —— 委托给 tyc
cd "$(dirname "$0")"
./tyc deploy --works-dir /home/tuyoo/tyclaw/works "$@"
