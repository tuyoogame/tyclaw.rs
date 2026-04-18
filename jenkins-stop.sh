#!/bin/bash
# Jenkins 停止入口 —— 委托给 tyc
cd "$(dirname "$0")"
./tyc stop
