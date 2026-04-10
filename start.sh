#!/bin/bash
# TyClaw.rs 启动脚本
# 用法：
#   ./start.sh              直接启动（保留上次状态）
#   ./start.sh --clean      清理运行时数据后启动
#   ./start.sh --build-docker 重新构建 sandbox Docker 镜像
#   ./start.sh --dingtalk   启动并连接钉钉

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

RUN_DIR="$SCRIPT_DIR/workspace"
CLEAN=false
BUILD_DOCKER=false
EXTRA_ARGS=""

for arg in "$@"; do
    case "$arg" in
        --clean) CLEAN=true ;;
        --build-docker) BUILD_DOCKER=true ;;
        *) EXTRA_ARGS="$EXTRA_ARGS $arg" ;;
    esac
done

# =========================================================================
# --clean: 清理运行时数据（保留 memory 和 skills）
# =========================================================================
if [ "$CLEAN" = true ]; then
    echo "=== TyClaw Clean ==="

    killall -9 tyclaw 2>/dev/null

    # 清空日志
    mkdir -p "$RUN_DIR/logs"
    > "$RUN_DIR/logs/tyclaw.log"
    echo "[ok] 日志已清空"

    # 清空 works 下所有 workspace 的临时数据（保留 memory，清空 skills/cases/work/_personal/skills）
    if [ -d "$RUN_DIR/works" ]; then
        for bucket_dir in "$RUN_DIR"/works/*/; do
            [ -d "$bucket_dir" ] || continue
            for ws_dir in "$bucket_dir"*/; do
                [ -d "$ws_dir" ] || continue
                rm -f "$ws_dir/history.jsonl"
                rm -f "$ws_dir/timer_jobs.json"
                rm -rf "$ws_dir/skills/"*
                rm -rf "$ws_dir/cases/"*
                rm -rf "$ws_dir/_personal/skills/"*
                rm -rf "$ws_dir/work/"*
            done
        done
        echo "[ok] works 已清空（memory 已保留）"
    fi

    # 清空审计日志和案例
    rm -rf "$RUN_DIR/audit/"*
    rm -rf "$RUN_DIR/cases/"*
    echo "[ok] 审计日志和案例已清空"

    # 清空根目录临时文件
    rm -rf "$RUN_DIR/tmp/" "$RUN_DIR/dispatches/" "$RUN_DIR"/.dispatch-*
    rm -f "$RUN_DIR/.active_tasks.json"
    echo "[ok] 临时文件已清空"

    # 停止并删除 sandbox 容器
    docker ps -a --filter "name=tyclaw-" --format "{{.Names}}" 2>/dev/null | while read name; do
        docker rm -f "$name" 2>/dev/null
    done
    echo "[ok] Docker 容器已清理"

    # 清空 /tmp 下的 tyclaw 临时文件
    rm -rf /tmp/tyclaw_*
    rm -f /tmp/_tyclaw_inline_*

    # 为 CLI 测试用户准备 demo 数据
    workspace_path() {
        local key="$1"
        local bucket
        bucket=$(printf "%s" "$key" | md5 2>/dev/null || printf "%s" "$key" | md5sum | cut -c1-32)
        echo "$RUN_DIR/works/${bucket:0:2}/$key"
    }
    CLI_WS="$(workspace_path "cli_user")"
    mkdir -p "$CLI_WS/work/attachments"
    cp demo/*.xlsx demo/*.mp4 demo/*.txt "$CLI_WS/work/attachments/" 2>/dev/null
    echo "[ok] demo 数据已复制到 cli_user"

    echo "=== Clean done ==="
    echo ""
fi

# =========================================================================
# --build-docker: 重新构建 sandbox 镜像并回收旧容器
# =========================================================================
if [ "$BUILD_DOCKER" = true ]; then
    echo "=== Build sandbox Docker image ==="
    docker build --no-cache -t tyclaw-sandbox "$SCRIPT_DIR/docker/sandbox"
    echo "[ok] sandbox 镜像已重建"

    # 回收使用旧镜像的容器
    OLD_CONTAINERS=$(docker ps -a --filter "name=tyclaw-" --format "{{.Names}}" 2>/dev/null)
    if [ -n "$OLD_CONTAINERS" ]; then
        echo "$OLD_CONTAINERS" | while read name; do
            docker rm -f "$name" 2>/dev/null
        done
        echo "[ok] 旧 sandbox 容器已回收"
    fi
    echo ""
fi

# =========================================================================
# 启动
# =========================================================================
echo "Starting TyClaw.rs ..."
cargo run -p tyclaw-app -- --run-dir "$RUN_DIR" $EXTRA_ARGS
