"""
视频处理工具：场景检测、关键帧提取、场景切片、静音检测
作为 builtin Skill 的基础设施，不含评分/分类逻辑
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
from datetime import datetime
from pathlib import Path


# ---------------------------------------------------------------------------
# URL 识别
# ---------------------------------------------------------------------------

_URL_PATTERNS = [
    re.compile(r"https?://(?:www\.)?youtube\.com/watch\?v=[\w-]+"),
    re.compile(r"https?://youtu\.be/[\w-]+"),
    re.compile(r"https?://(?:www\.)?bilibili\.com/video/[\w]+"),
    re.compile(r"https?://b23\.tv/[\w]+"),
    re.compile(r"https?://(?:www\.)?douyin\.com/video/\d+"),
    re.compile(r"https?://v\.douyin\.com/[\w]+"),
    re.compile(r"https?://(?:www\.)?tiktok\.com/@[\w.]+/video/\d+"),
]


def is_url(s: str) -> bool:
    return any(p.match(s) for p in _URL_PATTERNS) or s.startswith(("http://", "https://"))


# ---------------------------------------------------------------------------
# ffprobe 元数据
# ---------------------------------------------------------------------------

def get_video_metadata(video_path: str) -> dict:
    """用 ffprobe 获取视频元数据"""
    cmd = [
        "ffprobe", "-v", "quiet",
        "-print_format", "json",
        "-show_format", "-show_streams",
        video_path,
    ]
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=30)
        if r.returncode != 0:
            return {}
        data = json.loads(r.stdout)
    except Exception:
        return {}

    video_stream = next(
        (s for s in data.get("streams", []) if s.get("codec_type") == "video"),
        {},
    )
    audio_stream = next(
        (s for s in data.get("streams", []) if s.get("codec_type") == "audio"),
        {},
    )
    fmt = data.get("format", {})

    duration = float(fmt.get("duration", 0) or video_stream.get("duration", 0) or 0)
    width = int(video_stream.get("width", 0))
    height = int(video_stream.get("height", 0))

    fps_str = video_stream.get("r_frame_rate", "0/1")
    try:
        num, den = fps_str.split("/")
        fps = round(int(num) / int(den), 2) if int(den) else 0
    except (ValueError, ZeroDivisionError):
        fps = 0

    meta = {
        "duration": round(duration, 2),
        "resolution": f"{width}x{height}" if width and height else "",
        "fps": fps,
        "title": fmt.get("tags", {}).get("title", ""),
    }

    if audio_stream:
        meta["audio"] = {
            "codec": audio_stream.get("codec_name", ""),
            "sample_rate": int(audio_stream.get("sample_rate", 0) or 0),
            "channels": int(audio_stream.get("channels", 0) or 0),
            "bitrate": audio_stream.get("bit_rate", ""),
        }

    return meta


# ---------------------------------------------------------------------------
# yt-dlp 下载
# ---------------------------------------------------------------------------

def download_video(url: str, output_path: str) -> bool:
    """通过 yt-dlp 下载视频"""
    cmd = [
        "yt-dlp",
        "-f", "bestvideo[ext=mp4]+bestaudio[ext=m4a]/best[ext=mp4]/best",
        "--merge-output-format", "mp4",
        "-o", output_path,
        "--no-playlist",
        url,
    ]
    print(f"Downloading: {url}", file=sys.stderr)
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=300)
        if r.returncode != 0:
            print(f"yt-dlp error: {r.stderr[:500]}", file=sys.stderr)
            return False
        return Path(output_path).exists()
    except FileNotFoundError:
        print("Error: yt-dlp not found, please install: pip install yt-dlp",
              file=sys.stderr)
        return False
    except subprocess.TimeoutExpired:
        print("Error: download timed out (300s)", file=sys.stderr)
        return False


# ---------------------------------------------------------------------------
# 场景检测 (scenedetect Python API)
# ---------------------------------------------------------------------------

def detect_scenes(video_path: str, threshold: float = 27.0,
                  max_scenes: int = 30) -> list[dict]:
    """
    用 scenedetect ContentDetector 检测场景边界。
    如果场景数超过 max_scenes，自动提高阈值重新检测。
    返回 [{start_time, end_time, duration}, ...]
    """
    try:
        from scenedetect import open_video, SceneManager
        from scenedetect.detectors import ContentDetector
    except ImportError:
        print("Error: scenedetect not installed, please install: "
              "pip install scenedetect[opencv]", file=sys.stderr)
        return []

    current_threshold = threshold
    max_attempts = 5

    for attempt in range(max_attempts):
        video = open_video(video_path)
        scene_manager = SceneManager()
        scene_manager.add_detector(ContentDetector(threshold=current_threshold))
        scene_manager.detect_scenes(video)
        scene_list = scene_manager.get_scene_list()

        if len(scene_list) <= max_scenes or attempt == max_attempts - 1:
            break

        current_threshold = min(current_threshold + 5, 80)
        print(f"Too many scenes ({len(scene_list)}), "
              f"retrying with threshold={current_threshold}",
              file=sys.stderr)

    if not scene_list:
        meta = get_video_metadata(video_path)
        dur = meta.get("duration", 0)
        if dur > 0:
            return [{
                "start_time": 0.0,
                "end_time": dur,
                "duration": dur,
            }]
        return []

    scenes = []
    for start, end in scene_list:
        s = start.get_seconds()
        e = end.get_seconds()
        scenes.append({
            "start_time": round(s, 3),
            "end_time": round(e, 3),
            "duration": round(e - s, 3),
        })

    return scenes


# ---------------------------------------------------------------------------
# 关键帧提取
# ---------------------------------------------------------------------------

def _format_ts(seconds: float) -> str:
    """秒数 → HH:MM:SS.mmm"""
    h = int(seconds // 3600)
    m = int((seconds % 3600) // 60)
    s = seconds % 60
    return f"{h:02d}:{m:02d}:{s:06.3f}"


def extract_frames(video_path: str, scenes: list[dict],
                   frames_dir: str, frame_width: int = 960) -> list[str]:
    """从每个场景起始位置提取关键帧，返回帧文件路径列表"""
    Path(frames_dir).mkdir(parents=True, exist_ok=True)
    paths = []

    scale_filter = f"scale={frame_width}:-2" if frame_width > 0 else ""

    for i, scene in enumerate(scenes, 1):
        out = str(Path(frames_dir) / f"scene_{i:03d}.jpg")
        cmd = [
            "ffmpeg", "-y",
            "-ss", _format_ts(scene["start_time"]),
            "-i", video_path,
            "-frames:v", "1",
        ]
        if scale_filter:
            cmd.extend(["-vf", scale_filter])
        cmd.extend(["-q:v", "2", out])

        r = subprocess.run(cmd, capture_output=True, timeout=30)
        if r.returncode == 0 and Path(out).exists():
            paths.append(out)
        else:
            print(f"Warning: failed to extract frame for scene {i}",
                  file=sys.stderr)

    return paths


# ---------------------------------------------------------------------------
# 场景切片
# ---------------------------------------------------------------------------

def split_scenes(video_path: str, scenes: list[dict],
                 scenes_dir: str) -> list[str]:
    """将视频按场景边界切片（stream copy，快速无重编码）"""
    Path(scenes_dir).mkdir(parents=True, exist_ok=True)
    paths = []

    for i, scene in enumerate(scenes, 1):
        out = str(Path(scenes_dir) / f"scene_{i:03d}.mp4")
        cmd = [
            "ffmpeg", "-y",
            "-i", video_path,
            "-ss", _format_ts(scene["start_time"]),
            "-to", _format_ts(scene["end_time"]),
            "-c", "copy",
            "-avoid_negative_ts", "make_zero",
            out,
        ]
        r = subprocess.run(cmd, capture_output=True, timeout=60)
        if r.returncode == 0 and Path(out).exists():
            paths.append(out)
        else:
            print(f"Warning: failed to split scene {i}", file=sys.stderr)

    return paths


# ---------------------------------------------------------------------------
# 静音检测
# ---------------------------------------------------------------------------

def detect_silence(video_path: str, noise_db: float = -30,
                   min_duration: float = 0.5) -> list[dict]:
    """用 ffmpeg silencedetect 检测静音段"""
    cmd = [
        "ffmpeg", "-i", video_path,
        "-af", f"silencedetect=noise={noise_db}dB:d={min_duration}",
        "-f", "null", "-",
    ]
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=120)
    except subprocess.TimeoutExpired:
        return []

    output = r.stderr
    silence_segments = []
    starts = re.findall(r"silence_start:\s*([\d.]+)", output)
    ends = re.findall(r"silence_end:\s*([\d.]+)\s*\|\s*silence_duration:\s*([\d.]+)", output)

    for j, start_s in enumerate(starts):
        start = float(start_s)
        if j < len(ends):
            end = float(ends[j][0])
            dur = float(ends[j][1])
        else:
            end = start
            dur = 0
        silence_segments.append({
            "start": round(start, 3),
            "end": round(end, 3),
            "duration": round(dur, 3),
        })

    return silence_segments


# ---------------------------------------------------------------------------
# 主流程
# ---------------------------------------------------------------------------

def run(video_input: str, output_dir: str, threshold: float = 27.0,
        max_scenes: int = 30, frame_width: int = 960) -> dict:
    """完整处理流程，返回 scenes.json 内容"""
    output_path = Path(output_dir)
    output_path.mkdir(parents=True, exist_ok=True)

    video_path = str(output_path / "video.mp4")

    # 1. 获取视频文件
    if is_url(video_input):
        print(f"Step 1/5: Downloading video from URL ...", file=sys.stderr)
        if not download_video(video_input, video_path):
            print("Error: failed to download video", file=sys.stderr)
            sys.exit(1)
    else:
        src = Path(video_input)
        if not src.exists():
            print(f"Error: file not found: {video_input}", file=sys.stderr)
            sys.exit(1)
        print(f"Step 1/5: Copying video file ...", file=sys.stderr)
        shutil.copy2(str(src), video_path)

    # 2. 元数据
    print("Step 2/5: Extracting video metadata ...", file=sys.stderr)
    metadata = get_video_metadata(video_path)
    metadata["path"] = "video.mp4"

    # 3. 场景检测
    print(f"Step 3/5: Detecting scenes (threshold={threshold}) ...",
          file=sys.stderr)
    scenes = detect_scenes(video_path, threshold=threshold,
                           max_scenes=max_scenes)
    print(f"  Found {len(scenes)} scene(s)", file=sys.stderr)

    # 4. 关键帧 + 切片
    frames_dir = str(output_path / "frames")
    scenes_dir = str(output_path / "scenes")

    print(f"Step 4/5: Extracting frames and splitting clips ...",
          file=sys.stderr)
    frame_paths = extract_frames(video_path, scenes, frames_dir,
                                 frame_width=frame_width)
    clip_paths = split_scenes(video_path, scenes, scenes_dir)

    # 5. 静音检测
    print("Step 5/5: Detecting silence ...", file=sys.stderr)
    silence = detect_silence(video_path)

    # 组装 scenes.json
    scene_entries = []
    for i, scene in enumerate(scenes):
        entry = {
            "id": i + 1,
            "start": _format_ts(scene["start_time"]),
            "end": _format_ts(scene["end_time"]),
            "duration": scene["duration"],
        }
        if i < len(frame_paths):
            entry["frame"] = str(Path(frame_paths[i]).relative_to(output_path))
        if i < len(clip_paths):
            entry["clip"] = str(Path(clip_paths[i]).relative_to(output_path))
        scene_entries.append(entry)

    result = {
        "video": metadata,
        "scenes": scene_entries,
        "silence": silence,
        "detection_threshold": threshold,
        "scene_count": len(scenes),
    }

    json_path = output_path / "scenes.json"
    json_path.write_text(
        json.dumps(result, ensure_ascii=False, indent=2),
        encoding="utf-8",
    )

    print(f"\nDone. Output: {output_dir}", file=sys.stderr)
    print(f"  scenes.json: {len(scenes)} scene(s), "
          f"{len(silence)} silence segment(s)", file=sys.stderr)
    print(f"  frames/: {len(frame_paths)} frame(s)", file=sys.stderr)
    print(f"  scenes/: {len(clip_paths)} clip(s)", file=sys.stderr)

    # stdout 输出 JSON 供 AI 读取
    print(json.dumps(result, ensure_ascii=False, indent=2))
    return result


def main():
    parser = argparse.ArgumentParser(
        description="Video scene detection, frame extraction, and silence analysis")
    parser.add_argument("video", help="Video file path or URL")
    parser.add_argument("--threshold", type=float, default=27.0,
                        help="Scene detection threshold (default: 27.0, "
                             "lower = more sensitive)")
    parser.add_argument("--max-scenes", type=int, default=30,
                        help="Max scenes before auto-raising threshold "
                             "(default: 30)")
    parser.add_argument("--frame-width", type=int, default=960,
                        help="Max frame width in pixels (default: 960, "
                             "0 = original)")
    parser.add_argument("--output-dir",
                        help="Override output directory")

    args = parser.parse_args()

    staff_id = os.environ.get("TYCLAW_SENDER_STAFF_ID", "unknown")
    ts = datetime.now().strftime("%Y%m%d_%H%M%S")
    output_dir = args.output_dir or f"/tmp/tyclaw_{staff_id}_{ts}_video-analyzer"

    run(args.video, output_dir,
        threshold=args.threshold,
        max_scenes=args.max_scenes,
        frame_width=args.frame_width)


if __name__ == "__main__":
    main()
