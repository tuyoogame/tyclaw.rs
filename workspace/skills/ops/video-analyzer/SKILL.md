---
name: 视频处理
description: 分析视频场景结构，提取关键帧和场景切片，检测静音段
triggers:
  - 分析视频
  - 视频分析
  - 分析这个视频
  - 处理视频
  - 视频截帧
  - 视频场景
tool: skills/video-analyzer/tool.py
default: false
---
# 视频处理

基础视频处理工具：场景检测、关键帧提取、场景切片、静音检测。

## 工具用法

```bash
# 处理本地视频文件
python skills/video-analyzer/tool.py /path/to/video.mp4

# 处理 URL（YouTube/Bilibili/抖音）
python skills/video-analyzer/tool.py "https://www.bilibili.com/video/BVxxxxx"

# 调整场景灵敏度（越小越敏感，默认 27.0）
python skills/video-analyzer/tool.py video.mp4 --threshold 20

# 限制最大场景数（默认 30，超出自动降低灵敏度）
python skills/video-analyzer/tool.py video.mp4 --max-scenes 15

# 调整帧图宽度（默认 960px，0 = 原始分辨率）
python skills/video-analyzer/tool.py video.mp4 --frame-width 720
```

### 参数

| 参数 | 说明 | 默认值 |
|------|------|--------|
| `video` | 视频文件路径或 URL | 必填 |
| `--threshold` | 场景检测灵敏度（越小越敏感） | 27.0 |
| `--max-scenes` | 最大场景数（超出自动提高阈值） | 30 |
| `--frame-width` | 关键帧最大宽度像素（0=原始） | 960 |
| `--output-dir` | 覆盖输出目录 | 自动生成 |

## 输出

输出到 `/tmp/tyclaw_{staff_id}_{timestamp}_video-analyzer/`：

```
frames/          关键帧图片（scene_001.jpg, scene_002.jpg, ...）
scenes/          场景切片视频（scene_001.mp4, scene_002.mp4, ...）
scenes.json      完整元数据（见下方结构）
video.mp4        源视频文件
```

### scenes.json 结构

```json
{
  "video": {
    "path": "video.mp4",
    "duration": 120.5,
    "resolution": "1920x1080",
    "fps": 30,
    "title": "视频标题",
    "audio": {"codec": "aac", "sample_rate": 44100, "channels": 2}
  },
  "scenes": [
    {
      "id": 1,
      "start": "00:00:00.000",
      "end": "00:00:05.200",
      "duration": 5.2,
      "frame": "frames/scene_001.jpg",
      "clip": "scenes/scene_001.mp4"
    }
  ],
  "silence": [
    {"start": 10.5, "end": 12.3, "duration": 1.8}
  ],
  "scene_count": 12,
  "detection_threshold": 27.0
}
```

## AI 工作流

1. 运行 tool.py 处理视频，获得 scenes.json 和关键帧
2. 读取 scenes.json 了解视频结构（场景数、时长分布、静音段）
3. 查看 frames/ 中的关键帧图片（每次 3-5 张），理解各场景画面内容
4. 根据用户需求回答问题或生成分析报告

工具负责处理和提取，AI 负责理解和分析。如果用户需要特定的评分体系或分类规则，建议用户创建个人 Skill 来定义自己的分析标准。

## 安全约束

1. 输出路径必须在 `/tmp/tyclaw_{staff_id}_` 前缀下
2. 完成后直接输出分析结果
3. 如需发送场景切片或帧图给用户，在 `## 附件文件` 段落中列出完整路径
