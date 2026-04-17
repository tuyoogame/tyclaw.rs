FROM python:3.11-slim

# 安装系统工具 + 开发环境 + OpenCV 运行时依赖
RUN apt-get update && apt-get install -y --no-install-recommends \
    git \
    curl \
    wget \
    jq \
    zip \
    unzip \
    build-essential \
    pkg-config \
    ca-certificates \
    gnupg \
    ripgrep \
    ffmpeg \
    tesseract-ocr \
    tesseract-ocr-chi-sim \
    tesseract-ocr-eng \
    libxcb1 \
    libglib2.0-0 \
    libsm6 \
    libxext6 \
    libxrender1 \
    libgl1 \
    && rm -rf /var/lib/apt/lists/*

# Node.js 22 LTS + TypeScript
RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
    && apt-get install -y --no-install-recommends nodejs \
    && rm -rf /var/lib/apt/lists/* \
    && npm install -g typescript ts-node

# Rust (stable, minimal profile)
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal \
    && . /root/.cargo/env \
    && rustup component add clippy rustfmt
ENV PATH="/root/.cargo/bin:${PATH}"

# Python 常用包
RUN pip install --no-cache-dir \
    pandas \
    numpy \
    openpyxl \
    requests \
    opencv-python-headless \
    av \
    pytesseract \
    faster-whisper \
    "scenedetect[opencv]" \
    yt-dlp

# 浏览器自动化环境（Xvfb + Chromium + Playwright）
RUN apt-get update && apt-get install -y --no-install-recommends \
    xvfb \
    xauth \
    fonts-noto-cjk \
    && pip install --no-cache-dir playwright playwright-stealth \
    && playwright install --with-deps chromium \
    && rm -rf /var/lib/apt/lists/*

ENV DISPLAY=:99
ENV LANG=C.UTF-8
ENV LC_ALL=C.UTF-8

# 工作目录
WORKDIR /work

# 启动 Xvfb 虚拟屏幕 + sleep infinity 等待 docker exec
COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh
ENTRYPOINT ["/entrypoint.sh"]
