//! Docker 沙箱 —— 基于 per-workspace 容器 + volume mount 的隔离执行环境。
//!
//! 每个 workspace 一个容器，挂载 `works/{bucket}/{key}` → 容器内 `/user`。
//! 容器常驻（`--restart=unless-stopped`），release 时只杀残留进程。
//! 所有工具操作都通过 docker exec 执行，确保在容器安全边界内。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::process::Command;
use tokio::sync::Mutex;
use tracing::{info, warn};
use tyclaw_types::TyclawError;

use crate::types::*;

/// Docker 容器配置。
#[derive(Debug, Clone)]
pub struct DockerConfig {
    pub image: String,
    pub memory: String,
    pub cpus: String,
    pub network: String,
    pub work_dir: String,
}

impl Default for DockerConfig {
    fn default() -> Self {
        Self {
            image: "tyclaw-sandbox:latest".into(),
            memory: "512m".into(),
            cpus: "1".into(),
            network: "bridge".into(),
            work_dir: "/workspace".into(),
        }
    }
}

/// Docker 沙箱实例：per-user 容器，volume mount 用户 workspace。
/// 所有操作通过 docker exec 在容器内执行。
pub struct DockerSandbox {
    container_id: String,
    container_name: String,
    /// 容器内挂载根目录（如 `/user`，对应整个 `works/{bucket}/{key}`）。
    mount_root: String,
    /// 容器内工作目录（如 `/workspace`）。
    work_dir: String,
    /// workspace key（通常等于 user_id），注入为环境变量供 skill 脚本使用。
    workspace_key: String,
}

impl DockerSandbox {
    /// 将路径解析为容器内绝对路径。
    /// 如果路径已经以 mount_root 开头（如 `/workspace/skills/foo`），直接返回；
    /// 否则拼接 work_dir 前缀（如 `attachments/a.xlsx` → `/workspace/attachments/a.xlsx`）。
    fn resolve(&self, path: &str) -> String {
        if path.starts_with(&self.mount_root) {
            path.to_string()
        } else {
            format!("{}/{}", self.work_dir, path.trim_start_matches('/'))
        }
    }
}

#[async_trait]
impl Sandbox for DockerSandbox {
    async fn exec(&self, cmd: &str, timeout: Duration) -> Result<SandboxExecResult, TyclawError> {
        let tmpdir = format!("{}/work/tmp", self.work_dir);
        let result = tokio::time::timeout(
            timeout,
            Command::new("docker")
                .args([
                    "exec",
                    "-e",
                    &format!("TMPDIR={tmpdir}"),
                    "-e",
                    &format!("TYCLAW_SENDER_STAFF_ID={}", self.workspace_key),
                    "-w",
                    &self.work_dir,
                    &self.container_id,
                    "sh",
                    "-c",
                    cmd,
                ])
                .output(),
        )
        .await;

        match result {
            Err(_) => {
                let _ = Command::new("docker")
                    .args([
                        "exec",
                        &self.container_id,
                        "sh",
                        "-c",
                        "kill -9 -1 2>/dev/null; true",
                    ])
                    .output()
                    .await;
                Ok(SandboxExecResult {
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code: -1,
                    timed_out: true,
                })
            }
            Ok(Err(e)) => Err(TyclawError::Tool {
                tool: "docker_exec".into(),
                message: format!("docker exec failed: {e}"),
            }),
            Ok(Ok(output)) => Ok(SandboxExecResult {
                stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
                exit_code: output.status.code().unwrap_or(-1),
                timed_out: false,
            }),
        }
    }

    async fn stat(&self, path: &str) -> Result<SandboxFileStat, TyclawError> {
        let full_path = self.resolve(path);
        // 用位置参数 $1 传递路径，避免 shell injection
        let output = Command::new("docker")
            .args([
                "exec",
                &self.container_id,
                "sh",
                "-c",
                "if [ -f \"$1\" ]; then printf 'file\\n'; stat -c %s \"$1\" 2>/dev/null || wc -c < \"$1\"; \
                 elif [ -d \"$1\" ]; then printf 'dir\\n'; \
                 elif [ -e \"$1\" ]; then printf 'other\\n'; \
                 else printf 'missing\\n'; fi",
                "_",  // $0 placeholder
                &full_path,
            ])
            .output()
            .await
            .map_err(|e| TyclawError::Tool {
                tool: "docker_stat".into(),
                message: format!("docker exec stat failed: {e}"),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(TyclawError::Tool {
                tool: "docker_stat".into(),
                message: format!("stat failed: {stderr}"),
            });
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut lines = stdout.lines();
        let kind = lines.next().unwrap_or("missing");
        let size = lines.next().and_then(|s| s.trim().parse::<u64>().ok());

        Ok(match kind {
            "file" => SandboxFileStat {
                exists: true,
                is_file: true,
                is_dir: false,
                size,
            },
            "dir" => SandboxFileStat {
                exists: true,
                is_file: false,
                is_dir: true,
                size: None,
            },
            "other" => SandboxFileStat {
                exists: true,
                is_file: false,
                is_dir: false,
                size: None,
            },
            _ => SandboxFileStat {
                exists: false,
                is_file: false,
                is_dir: false,
                size: None,
            },
        })
    }

    async fn read_file(&self, path: &str) -> Result<Vec<u8>, TyclawError> {
        let full_path = self.resolve(path);
        let output = Command::new("docker")
            .args(["exec", &self.container_id, "cat", &full_path])
            .output()
            .await
            .map_err(|e| TyclawError::Tool {
                tool: "docker_read".into(),
                message: format!("docker exec cat failed: {e}"),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(TyclawError::Tool {
                tool: "docker_read".into(),
                message: format!("File read failed: {stderr}"),
            });
        }
        Ok(output.stdout)
    }

    async fn write_file(&self, path: &str, content: &[u8]) -> Result<(), TyclawError> {
        let full_path = self.resolve(path);
        // 先创建父目录
        if let Some(parent) = std::path::Path::new(&full_path).parent() {
            let _ = Command::new("docker")
                .args([
                    "exec",
                    &self.container_id,
                    "mkdir",
                    "-p",
                    &parent.to_string_lossy(),
                ])
                .output()
                .await;
        }
        // 通过 stdin pipe 写入
        let mut child = Command::new("docker")
            .args(["exec", "-i", &self.container_id, "tee", &full_path])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .map_err(|e| TyclawError::Tool {
                tool: "docker_write".into(),
                message: format!("docker exec tee spawn failed: {e}"),
            })?;

        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            stdin
                .write_all(content)
                .await
                .map_err(|e| TyclawError::Tool {
                    tool: "docker_write".into(),
                    message: format!("stdin write failed: {e}"),
                })?;
            drop(stdin);
        }

        let status = child.wait().await.map_err(|e| TyclawError::Tool {
            tool: "docker_write".into(),
            message: format!("docker exec tee wait failed: {e}"),
        })?;

        if !status.success() {
            return Err(TyclawError::Tool {
                tool: "docker_write".into(),
                message: format!("tee exited with: {status}"),
            });
        }
        Ok(())
    }

    async fn create_dir(&self, path: &str) -> Result<(), TyclawError> {
        let full_path = self.resolve(path);
        let output = Command::new("docker")
            .args(["exec", &self.container_id, "mkdir", "-p", &full_path])
            .output()
            .await
            .map_err(|e| TyclawError::Tool {
                tool: "docker_mkdir".into(),
                message: format!("docker exec mkdir failed: {e}"),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(TyclawError::Tool {
                tool: "docker_mkdir".into(),
                message: format!("mkdir failed: {stderr}"),
            });
        }
        Ok(())
    }

    async fn list_dir(&self, path: &str) -> Result<Vec<SandboxDirEntry>, TyclawError> {
        let full_path = if path.is_empty() || path == "." {
            self.work_dir.clone()
        } else {
            self.resolve(path)
        };
        let output = Command::new("docker")
            .args(["exec", &self.container_id, "ls", "-1F", &full_path])
            .output()
            .await
            .map_err(|e| TyclawError::Tool {
                tool: "docker_list_dir".into(),
                message: format!("docker exec ls failed: {e}"),
            })?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let entries = stdout
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| {
                let is_dir = l.ends_with('/');
                let name = l
                    .trim_end_matches('/')
                    .trim_end_matches('*')
                    .trim_end_matches('@')
                    .to_string();
                SandboxDirEntry { name, is_dir }
            })
            .collect();
        Ok(entries)
    }

    async fn walk_dir(
        &self,
        path: &str,
        max_depth: usize,
    ) -> Result<Vec<SandboxWalkEntry>, TyclawError> {
        let output = Command::new("docker")
            .args([
                "exec",
                "-w",
                &self.work_dir,
                &self.container_id,
                "sh",
                "-c",
                "cd \"$1\" || exit 1; find . -mindepth 1 -maxdepth \"$2\" \\( -name .git -o -name node_modules -o -name target -o -name __pycache__ -o -name .venv -o -name .tox -o -name dist -o -name build \\) -prune -o -printf '%y\t%P\t%d\n'",
                "sh",
                path,
                &max_depth.to_string(),
            ])
            .output()
            .await
            .map_err(|e| TyclawError::Tool {
                tool: "docker_walk_dir".into(),
                message: format!("docker exec find failed: {e}"),
            })?;

        if !output.status.success() {
            return Err(TyclawError::Tool {
                tool: "docker_walk_dir".into(),
                message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }

        const MAX_WALK_ENTRIES: usize = 50_000;
        let mut entries = Vec::new();
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            if entries.len() >= MAX_WALK_ENTRIES {
                tracing::warn!(MAX_WALK_ENTRIES, "walk_dir truncated: too many entries");
                break;
            }
            let mut parts = line.splitn(3, '\t');
            let kind = parts.next().unwrap_or_default();
            let rel = parts.next().unwrap_or_default();
            let depth = parts
                .next()
                .and_then(|d| d.parse::<usize>().ok())
                .unwrap_or(1);
            if rel.is_empty() {
                continue;
            }
            entries.push(SandboxWalkEntry {
                path: rel.to_string(),
                is_dir: kind == "d",
                depth,
            });
        }
        Ok(entries)
    }

    async fn grep_search(
        &self,
        request: SandboxGrepRequest,
    ) -> Result<SandboxGrepResponse, TyclawError> {
        let mut cmd = Command::new("docker");
        cmd.args(["exec", "-w", &self.work_dir, &self.container_id, "rg"]);
        cmd.args(["--no-heading", "--line-number", "--color", "never"]);

        match request.output_mode.as_str() {
            "files_only" => {
                cmd.arg("-l");
            }
            "count" => {
                cmd.arg("-c");
            }
            _ => {}
        }

        if request.case_insensitive {
            cmd.arg("-i");
        }
        if let Some(c) = request.context_lines {
            if c > 0 && request.output_mode == "content" {
                cmd.args(["-C", &c.to_string()]);
            }
        }
        if let Some(ref t) = request.file_type {
            cmd.args(["--type", t]);
        }
        if let Some(ref inc) = request.include {
            cmd.args(["--glob", inc]);
        }
        let capped_max = request.max_results.min(10_000);
        cmd.args(["--max-count", &capped_max.to_string()]);
        cmd.arg("--").arg(&request.pattern).arg(&request.path);

        let output = cmd.output().await.map_err(|e| TyclawError::Tool {
            tool: "docker_grep_search".into(),
            message: format!("docker exec rg failed: {e}"),
        })?;

        Ok(SandboxGrepResponse {
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            exit_code: output.status.code().unwrap_or(-1),
        })
    }

    async fn glob_search(
        &self,
        pattern: &str,
        path: &str,
    ) -> Result<Vec<SandboxGlobEntry>, TyclawError> {
        let output = Command::new("docker")
            .args([
                "exec",
                "-w",
                &self.work_dir,
                &self.container_id,
                "bash",
                "-O",
                "globstar",
                "-O",
                "nullglob",
                "-c",
                "cd \"$2\" || exit 1; pattern=\"$1\"; for f in $pattern; do [ -f \"$f\" ] && stat -c '%Y\t%n' \"$f\"; done",
                "_",
                pattern,
                path,
            ])
            .output()
            .await
            .map_err(|e| TyclawError::Tool {
                tool: "docker_glob_search".into(),
                message: format!("docker exec glob failed: {e}"),
            })?;

        if !output.status.success() {
            return Err(TyclawError::Tool {
                tool: "docker_glob_search".into(),
                message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }

        let mut entries = Vec::new();
        for line in String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|l| !l.is_empty())
        {
            let mut parts = line.splitn(2, '\t');
            let modified_unix_secs = parts
                .next()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            let path = parts.next().unwrap_or_default();
            if path.is_empty() {
                continue;
            }
            entries.push(SandboxGlobEntry {
                path: path.to_string(),
                modified_unix_secs,
            });
        }
        Ok(entries)
    }

    async fn file_exists(&self, path: &str) -> bool {
        let full_path = self.resolve(path);
        Command::new("docker")
            .args(["exec", &self.container_id, "test", "-e", &full_path])
            .output()
            .await
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    async fn remove_file(&self, path: &str) -> Result<(), TyclawError> {
        let full_path = self.resolve(path);
        let output = Command::new("docker")
            .args(["exec", &self.container_id, "rm", "-f", &full_path])
            .output()
            .await
            .map_err(|e| TyclawError::Tool {
                tool: "docker_remove".into(),
                message: format!("docker exec rm failed: {e}"),
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(TyclawError::Tool {
                tool: "docker_remove".into(),
                message: format!("rm failed: {stderr}"),
            });
        }
        Ok(())
    }

    async fn copy_from(
        &self,
        container_path: &str,
        host_path: &PathBuf,
    ) -> Result<(), TyclawError> {
        let src = format!("{}:{}", self.container_id, self.resolve(container_path));
        if let Some(parent) = host_path.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
        let output = Command::new("docker")
            .args(["cp", &src, &host_path.to_string_lossy()])
            .output()
            .await
            .map_err(|e| TyclawError::Tool {
                tool: "docker_cp".into(),
                message: format!("docker cp failed: {e}"),
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(TyclawError::Tool {
                tool: "docker_cp".into(),
                message: format!("docker cp failed: {stderr}"),
            });
        }
        Ok(())
    }

    fn workspace_root(&self) -> &str {
        // 返回工作文件根（dispatches/attachments/tmp 所在的容器路径）
        // 固定为 {mount_root}/work，不受 workdir 变化影响
        "/workspace/work"
    }

    fn id(&self) -> &str {
        &self.container_name
    }
}

/// Per-user Docker 容器管理器。
///
/// 每个用户一个容器（按需创建），volume mount 用户 workspace。
/// 容器常驻，release 只杀残留进程。
pub struct DockerPool {
    config: DockerConfig,
    /// workspace_key → WorkspaceContainer
    containers: Mutex<HashMap<String, WorkspaceContainer>>,
    /// 顶层 workspace 根目录（命令行 --workspace）
    root: PathBuf,
}

struct WorkspaceContainer {
    container_id: String,
    container_name: String,
}

/// 计算 workspace 路径（与 tyclaw_control::workspace_path 一致）。
fn workspace_path(root: &Path, workspace_key: &str) -> PathBuf {
    use md5::{Digest, Md5};
    let hash = Md5::digest(workspace_key.as_bytes());
    root.join("works")
        .join(format!("{:02x}", hash[0]))
        .join(workspace_key)
}

impl DockerPool {
    /// 创建 per-workspace 容器管理器。
    pub async fn new(config: DockerConfig, root: PathBuf) -> Result<Arc<Self>, TyclawError> {
        let check = Command::new("docker").args(["info"]).output().await;
        if check.is_err() || !check.unwrap().status.success() {
            return Err(TyclawError::Other("Docker is not available".into()));
        }

        let img_check = Command::new("docker")
            .args(["image", "inspect", &config.image])
            .output()
            .await;
        if img_check.is_err() || !img_check.unwrap().status.success() {
            warn!(image = %config.image, "Docker image not found");
        }

        // Docker volume mount 要求绝对路径
        let root = root
            .canonicalize()
            .unwrap_or_else(|_| std::env::current_dir().unwrap_or_default().join(&root));

        info!(image = %config.image, root = %root.display(), "Docker pool initialized (per-workspace mode)");

        Ok(Arc::new(Self {
            config,
            containers: Mutex::new(HashMap::new()),
            root,
        }))
    }

    /// 为 workspace 创建容器，volume mount workspace 目录 → /user。
    async fn create_workspace_container(&self, workspace_key: &str) -> Result<(String, String), TyclawError> {
        let name = format!("tyclaw-{workspace_key}");

        // 检查是否有同名容器残留
        let inspect = Command::new("docker")
            .args(["inspect", "--format", "{{.State.Running}}", &name])
            .output()
            .await;
        if let Ok(output) = inspect {
            let status = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if status == "true" {
                let cid_output = Command::new("docker")
                    .args(["inspect", "--format", "{{.Id}}", &name])
                    .output()
                    .await
                    .map_err(|e| TyclawError::Other(format!("docker inspect failed: {e}")))?;
                let cid = String::from_utf8_lossy(&cid_output.stdout)
                    .trim()
                    .to_string();
                info!(name = %name, "Reusing existing container for workspace");
                return Ok((cid, name));
            } else if output.status.success() {
                let _ = Command::new("docker")
                    .args(["rm", "-f", &name])
                    .output()
                    .await;
            }
        }

        let ws_root = workspace_path(&self.root, workspace_key);
        let ws_work = ws_root.join("work");
        tokio::fs::create_dir_all(&ws_work).await.ok();
        tokio::fs::create_dir_all(ws_root.join("skills"))
            .await
            .ok();
        let ws_root_abs = std::fs::canonicalize(&ws_root).unwrap_or_else(|_| ws_root.clone());

        let mount_root = user_mount_root(&self.config.work_dir);
        let mount_arg = format!("{}:{}", ws_root_abs.display(), mount_root);

        // 全局 skills 目录只读挂载到容器内 /user/skills（与 PathConfig.global_skills_mount 一致）
        let global_skills = self.root.join("skills");
        let global_skills_abs = std::fs::canonicalize(&global_skills)
            .unwrap_or_else(|_| global_skills.clone());
        let global_skills_mount = format!("{}:{}/skills:ro", global_skills_abs.display(), mount_root);

        info!(
            workspace_key = %workspace_key,
            host_root = %ws_root_abs.display(),
            container_root = %mount_root,
            container_workdir = %self.config.work_dir,
            global_skills = %global_skills_abs.display(),
            "Preparing per-workspace docker mount"
        );

        let tmpdir_env = format!("TMPDIR={}/work/tmp", self.config.work_dir);
        let output = Command::new("docker")
            .args([
                "run",
                "-d",
                "--name",
                &name,
                "--restart",
                "unless-stopped",
                "--memory",
                &self.config.memory,
                "--cpus",
                &self.config.cpus,
                "--network",
                &self.config.network,
                "--pids-limit",
                "128",
                "-e",
                &tmpdir_env,
                "-v",
                &mount_arg,
                "-v",
                &global_skills_mount,
                "-w",
                &self.config.work_dir,
                &self.config.image,
                "sleep",
                "infinity",
            ])
            .output()
            .await
            .map_err(|e| TyclawError::Other(format!("docker run failed: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(TyclawError::Other(format!("docker run failed: {stderr}")));
        }

        let cid = String::from_utf8_lossy(&output.stdout).trim().to_string();

        // 确保容器内 work/tmp 目录存在（TMPDIR 指向此处）
        let _ = Command::new("docker")
            .args(["exec", &cid, "mkdir", "-p", &format!("{}/tmp", self.config.work_dir)])
            .output()
            .await;

        info!(name = %name, workspace_key = %workspace_key, "Created container for workspace");
        Ok((cid, name))
    }
}

#[async_trait]
impl SandboxPool for DockerPool {
    async fn acquire(
        &self,
        task_workspace: &PathBuf,
        _data_mounts: &[PathMount],
    ) -> Result<Arc<dyn Sandbox>, TyclawError> {
        // task_workspace = works/{bucket}/{workspace_key}/work
        // 提取 workspace_key：向上两级取目录名
        let workspace_key = task_workspace
            .parent()                        // works/{bucket}/{workspace_key}
            .and_then(|p| p.file_name())     // {workspace_key}
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_else(|| "default".into());

        let containers = self.containers.lock().await;

        if let Some(entry) = containers.get(&workspace_key) {
            // 验证容器是否仍存活（可能被 reaper 或外部删除）
            let check = Command::new("docker")
                .args(["inspect", "--format", "{{.State.Running}}", &entry.container_name])
                .output()
                .await;
            let alive = check
                .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "true")
                .unwrap_or(false);

            if alive {
                return Ok(Arc::new(DockerSandbox {
                    container_id: entry.container_id.clone(),
                    container_name: entry.container_name.clone(),
                    mount_root: user_mount_root(&self.config.work_dir),
                    work_dir: self.config.work_dir.clone(),
                    workspace_key: workspace_key.clone(),
                }));
            }
            // 容器已不存在，清除缓存，走重建流程
            info!(name = %entry.container_name, "Cached container no longer alive, recreating");
        }
        drop(containers);

        // 清除旧缓存条目
        self.containers.lock().await.remove(&workspace_key);

        let (cid, name) = self.create_workspace_container(&workspace_key).await?;

        let mut containers = self.containers.lock().await;
        let ws_key = workspace_key.clone();
        containers.insert(
            workspace_key,
            WorkspaceContainer {
                container_id: cid.clone(),
                container_name: name.clone(),
            },
        );

        Ok(Arc::new(DockerSandbox {
            container_id: cid,
            container_name: name,
            mount_root: user_mount_root(&self.config.work_dir),
            work_dir: self.config.work_dir.clone(),
            workspace_key: ws_key,
        }))
    }

    async fn release(
        &self,
        sandbox: Arc<dyn Sandbox>,
        _task_workspace: &PathBuf,
    ) -> Result<(), TyclawError> {
        let sandbox_name = sandbox.id().to_string();

        let containers = self.containers.lock().await;
        if let Some(entry) = containers
            .values()
            .find(|e| e.container_name == sandbox_name)
        {
            // 杀掉容器内残留进程，但不移除容器（容器常驻复用）
            let _ = Command::new("docker")
                .args([
                    "exec",
                    &entry.container_id,
                    "sh",
                    "-c",
                    "kill -9 -1 2>/dev/null; true",
                ])
                .output()
                .await;
        }

        Ok(())
    }

    async fn available_count(&self) -> usize {
        let containers = self.containers.lock().await;
        containers.len()
    }

    async fn total_count(&self) -> usize {
        self.containers.lock().await.len()
    }

    async fn is_available(&self) -> bool {
        Command::new("docker")
            .args(["info"])
            .output()
            .await
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

fn user_mount_root(_work_dir: &str) -> String {
    // workspace 根目录始终挂载到 /user，不依赖 workdir 推导
    "/workspace".to_string()
}
