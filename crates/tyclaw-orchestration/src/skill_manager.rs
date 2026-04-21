//! 技能管理器 —— 全局共享技能和 workspace 私有技能的发现与分类。
//!
//! 技能来源（两层合并）：
//! 1. 全局技能：{root}/skills/{category}/{skill_name}/SKILL.md
//! 2. Workspace 私有技能：works/{bucket}/{key}/skills/{skill_name}/SKILL.md
//!
//! SKILL.md 格式：YAML frontmatter + Markdown 正文

use std::path::{Path, PathBuf};
use parking_lot::Mutex;
use tracing::info;

/// 技能元数据。
#[derive(Debug, Clone)]
pub struct SkillMeta {
    pub key: String,
    pub name: String,
    pub description: String,
    pub category: String,
    pub tags: Vec<String>,
    pub triggers: Vec<String>,
    pub tool: Option<String>,
    pub risk_level: String,
    pub requires_capabilities: Vec<String>,
    pub skill_dir: PathBuf,
    pub content: String,
    pub status: String,
    pub creator: Option<String>,
}

impl SkillMeta {
    /// 返回技能工具脚本的绝对路径（如果 frontmatter 配置了 tool 字段）。
    ///
    /// 对于 `tools/xxx.py` 或 `skills/xxx/tool.py` 这种全局路径，
    /// 通过 skill_dir 的祖先目录定位 workspace root 来拼接；
    /// 其余情况视为 skill 目录内的相对路径。
    pub fn tool_path(&self) -> Option<String> {
        self.tool.as_ref().map(|tool| {
            if tool.starts_with("tools/") || tool.starts_with("skills/") {
                // builtin: skill_dir = {root}/skills/{cat}/{key} → root = parent^3
                // workspace: skill_dir = {ws}/skills/{key} → root = parent^2
                let root = if self.status == "builtin" {
                    self.skill_dir.parent().and_then(|p| p.parent()).and_then(|p| p.parent())
                } else {
                    self.skill_dir.parent().and_then(|p| p.parent())
                };
                match root {
                    Some(r) => r.join(tool).to_string_lossy().to_string(),
                    None => self.skill_dir.join(tool).to_string_lossy().to_string(),
                }
            } else {
                self.skill_dir.join(tool).to_string_lossy().to_string()
            }
        })
    }
}

/// 提供给上下文提示的能力简述（Python 版本的 merged caps 视图）。
#[derive(Debug, Clone)]
pub struct SkillCapability {
    pub key: String,
    pub name: String,
    pub description: String,
    pub category: String,
    pub tags: Vec<String>,
    pub status: String,
    pub creator: Option<String>,
}

/// 解析 SKILL.md 中的 YAML frontmatter。
fn parse_skill_frontmatter(content: &str) -> Option<serde_yaml::Value> {
    let re = regex::Regex::new(r"(?s)^---\s*\n(.*?)\n---\s*\n").unwrap();
    let m = re.captures(content)?;
    let yaml_str = m.get(1)?.as_str();
    serde_yaml::from_str(yaml_str).ok()
}

/// 读取单个技能目录，返回技能元数据。
fn scan_skill_dir(skill_dir: &Path) -> Option<SkillMeta> {
    let skill_md = skill_dir.join("SKILL.md");
    if !skill_md.exists() {
        return None;
    }

    let content = match std::fs::read_to_string(&skill_md) {
        Ok(c) => c,
        Err(_) => return None,
    };

    let dir_name = skill_dir
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    // 尝试解析 frontmatter；没有则用目录名和首行作为 fallback
    let (name, description, category, tags, triggers, tool, risk_level, requires_capabilities) =
        if let Some(meta) = parse_skill_frontmatter(&content) {
            if let Some(mapping) = meta.as_mapping() {
                let get_str = |key: &str| -> String {
                    mapping
                        .get(&serde_yaml::Value::String(key.into()))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string()
                };
                let get_str_vec = |key: &str| -> Vec<String> {
                    mapping
                        .get(&serde_yaml::Value::String(key.into()))
                        .and_then(|v| v.as_sequence())
                        .map(|seq| {
                            seq.iter()
                                .filter_map(|v| v.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default()
                };
                let name = mapping
                    .get(&serde_yaml::Value::String("name".into()))
                    .and_then(|v| v.as_str())
                    .unwrap_or(&dir_name)
                    .to_string();
                let tool_str = get_str("tool");
                let risk = get_str("risk_level");
                (
                    name,
                    get_str("description"),
                    get_str("category"),
                    get_str_vec("tags"),
                    get_str_vec("triggers"),
                    if tool_str.is_empty() { None } else { Some(tool_str) },
                    if risk.is_empty() { "read".into() } else { risk },
                    get_str_vec("requires_capabilities"),
                )
            } else {
                // frontmatter 解析成功但不是 mapping，用 fallback
                let first_line = content.lines().next().unwrap_or("").trim_start_matches('#').trim();
                (dir_name.clone(), first_line.to_string(), String::new(), vec![], vec![], None, "read".into(), vec![])
            }
        } else {
            // 无 frontmatter，用目录名和首行 fallback（兼容子 agent 创建的无 frontmatter SKILL.md）
            let first_line = content.lines().next().unwrap_or("").trim_start_matches('#').trim();
            let tool_file = if skill_dir.join("tool.py").exists() {
                Some("tool.py".to_string())
            } else if skill_dir.join("tool.sh").exists() {
                Some("tool.sh".to_string())
            } else {
                None
            };
            (dir_name.clone(), first_line.to_string(), String::new(), vec![], vec![], tool_file, "read".into(), vec![])
        };

    Some(SkillMeta {
        key: dir_name,
        name,
        description,
        category,
        tags,
        triggers,
        tool,
        risk_level,
        requires_capabilities,
        skill_dir: skill_dir.to_path_buf(),
        content,
        status: String::new(),
        creator: None,
    })
}

/// 技能管理器 —— 管理全局共享技能和 workspace 私有技能的发现和分类。
///
/// 技能来源（合并两层）：
/// 1. 全局技能：`{root}/skills/{category}/{skill_name}/SKILL.md`
/// 2. Workspace 技能：`{root}/works/{bucket}/{key}/skills/{skill_name}/SKILL.md`
pub struct SkillManager {
    builtin_dir: PathBuf,
    /// works 目录，用于计算 workspace skills 路径（默认 {root}/works，可通过 --works-dir 覆盖）
    works_dir: Mutex<PathBuf>,
    builtin_cache: Mutex<Vec<SkillMeta>>,
    builtin_mtime: Mutex<f64>,
}

impl SkillManager {
    pub fn new(builtin_dir: PathBuf, root: PathBuf) -> Self {
        let works_dir = root.join("works");
        Self {
            builtin_dir,
            works_dir: Mutex::new(works_dir),
            builtin_cache: Mutex::new(Vec::new()),
            builtin_mtime: Mutex::new(0.0),
        }
    }

    /// 覆盖 works 目录路径（对应 --works-dir 命令行参数）。
    pub fn set_works_dir(&self, path: PathBuf) {
        *self.works_dir.lock() = path;
    }

    /// 获取内建技能目录下所有 SKILL.md 的最新修改时间。
    fn get_builtin_mtime(&self) -> f64 {
        if !self.builtin_dir.is_dir() {
            return 0.0;
        }
        let mut latest = 0.0f64;
        if let Ok(entries) = glob::glob(&self.builtin_dir.join("**/SKILL.md").to_string_lossy()) {
            for entry in entries.flatten() {
                if let Ok(meta) = entry.metadata() {
                    if let Ok(modified) = meta.modified() {
                        let secs = modified
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs_f64();
                        latest = latest.max(secs);
                    }
                }
            }
        }
        latest
    }

    /// 扫描内建技能目录。
    ///
    /// 目录结构：skills/{category}/{skill_name}/SKILL.md
    /// 通过 mtime 缓存，避免重复扫描。
    pub fn scan_builtin(&self) -> Vec<SkillMeta> {
        let mtime = self.get_builtin_mtime();

        {
            let cached_mtime = self.builtin_mtime.lock();
            if mtime > 0.0 && mtime == *cached_mtime {
                return self
                    .builtin_cache
                    .lock()
                    .clone();
            }
        }

        if !self.builtin_dir.is_dir() {
            return Vec::new();
        }

        let mut skills = Vec::new();
        let mut entries: Vec<_> = std::fs::read_dir(&self.builtin_dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().is_dir())
            .collect();
        entries.sort_by_key(|e| e.file_name());

        for category_entry in entries {
            let category = category_entry.file_name().to_string_lossy().to_string();
            let mut skill_entries: Vec<_> = std::fs::read_dir(category_entry.path())
                .into_iter()
                .flatten()
                .flatten()
                .filter(|e| e.path().is_dir())
                .collect();
            skill_entries.sort_by_key(|e| e.file_name());

            for skill_entry in skill_entries {
                if let Some(mut skill) = scan_skill_dir(&skill_entry.path()) {
                    if skill.category.is_empty() {
                        skill.category = category.clone();
                    }
                    skill.status = "builtin".into();
                    skills.push(skill);
                }
            }
        }

        info!(
            count = skills.len(),
            categories = skills
                .iter()
                .map(|s| s.category.as_str())
                .collect::<std::collections::HashSet<_>>()
                .len(),
            "Scanned builtin skills"
        );

        {
            let mut cache = self.builtin_cache.lock();
            *cache = skills.clone();
            let mut cached_mtime = self.builtin_mtime.lock();
            *cached_mtime = mtime;
        }

        skills
    }

    /// 扫描 workspace 私有技能目录。
    ///
    /// 目录结构：works/{bucket}/{workspace_key}/skills/{skill_name}/SKILL.md
    pub fn scan_workspace_skills(&self, workspace_key: &str) -> Vec<SkillMeta> {
        let works_dir = self.works_dir.lock().clone();
        let ws_root = tyclaw_control::workspace_path_in(&works_dir, workspace_key);
        let mut skills = Vec::new();

        // 标准路径：skills/（新版 skill-creator 创建到此）
        let user_dir = ws_root.join("skills");
        if user_dir.is_dir() {
            let mut entries: Vec<_> = std::fs::read_dir(&user_dir)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|e| e.path().is_dir())
                .collect();
            entries.sort_by_key(|e| e.file_name());
            for entry in entries {
                if let Some(mut skill) = scan_skill_dir(&entry.path()) {
                    skill.status = "workspace".into();
                    skill.creator = Some(workspace_key.to_string());
                    skills.push(skill);
                }
            }
        }

        // 兼容旧版：work/_personal/ 下的 skill（旧版 skill-creator 创建到此路径）
        let personal_dir = ws_root.join("work").join("_personal");
        if personal_dir.is_dir() {
            Self::scan_personal_skills_recursive(&personal_dir, workspace_key, &mut skills);
        }

        skills
    }

    /// 递归扫描 _personal/ 目录下的 skill（兼容旧版数据，目录结构可能嵌套多层）。
    fn scan_personal_skills_recursive(
        dir: &Path,
        workspace_key: &str,
        results: &mut Vec<SkillMeta>,
    ) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.join("SKILL.md").exists() {
                    if let Some(mut skill) = scan_skill_dir(&path) {
                        skill.status = "personal".into();
                        skill.creator = Some(workspace_key.to_string());
                        results.push(skill);
                    }
                } else {
                    Self::scan_personal_skills_recursive(&path, workspace_key, results);
                }
            }
        }
    }

    /// 获取 workspace 可用的能力列表（全局 + workspace 私有合并）。
    pub fn get_caps(&self, workspace_key: &str) -> Vec<SkillCapability> {
        let mut result = Vec::new();

        for skill in self.scan_builtin() {
            result.push(SkillCapability {
                key: skill.key,
                name: skill.name,
                description: skill.description,
                category: skill.category,
                tags: skill.tags,
                status: "builtin".into(),
                creator: None,
            });
        }

        for skill in self.scan_workspace_skills(workspace_key) {
            result.push(SkillCapability {
                key: skill.key,
                name: skill.name,
                description: skill.description,
                category: if skill.category.is_empty() {
                    "workspace".into()
                } else {
                    skill.category
                },
                tags: skill.tags,
                status: "workspace".into(),
                creator: skill.creator,
            });
        }

        result
    }

    /// 获取 workspace 可用技能的完整内容（用于注入 system prompt）。
    pub fn get_skill_contents(&self, workspace_key: &str) -> Vec<SkillMeta> {
        let mut results = self.scan_builtin();
        results.extend(self.scan_workspace_skills(workspace_key));
        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_parse_frontmatter() {
        let content = "---\nname: test-skill\ndescription: A test\ncategory: debug\ntags: [a, b]\n---\n\nBody text.";
        let meta = parse_skill_frontmatter(content).unwrap();
        let mapping = meta.as_mapping().unwrap();
        assert_eq!(
            mapping
                .get(&serde_yaml::Value::String("name".into()))
                .unwrap()
                .as_str()
                .unwrap(),
            "test-skill"
        );
    }

    #[test]
    fn test_scan_skill_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let skill_dir = tmp.path().join("my-skill");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: MySkill\ndescription: does stuff\n---\n\nDetails.",
        )
        .unwrap();

        let skill = scan_skill_dir(&skill_dir).unwrap();
        assert_eq!(skill.name, "MySkill");
        assert_eq!(skill.description, "does stuff");
    }

    #[test]
    fn test_skill_manager_scan_builtin() {
        let tmp = tempfile::TempDir::new().unwrap();
        let builtin = tmp.path().join("skills");
        let cat_dir = builtin.join("debug").join("my-skill");
        fs::create_dir_all(&cat_dir).unwrap();
        fs::write(
            cat_dir.join("SKILL.md"),
            "---\nname: Debug\ndescription: debug tool\n---\n\nBody.",
        )
        .unwrap();

        let mgr = SkillManager::new(builtin, tmp.path().to_path_buf());
        let skills = mgr.scan_builtin();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "Debug");
        assert_eq!(skills[0].category, "debug");
        assert_eq!(skills[0].status, "builtin");
    }

    #[test]
    fn test_scan_real_workspace_skills() {
        let ws_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../workspace");
        let builtin_dir = ws_root.join("skills");
        if !builtin_dir.is_dir() {
            return; // CI 或没有 workspace 目录时跳过
        }

        let mgr = SkillManager::new(builtin_dir, ws_root.clone());
        let skills = mgr.scan_builtin();

        // 验证所有 Skill 被扫描到（数量随迁移变化，至少 25 个）
        assert!(skills.len() >= 25, "expected at least 25 skills, got {}: {:?}",
            skills.len(), skills.iter().map(|s| &s.key).collect::<Vec<_>>());

        // 验证 5 个 category
        let mut cats: Vec<String> = skills.iter().map(|s| s.category.clone()).collect();
        cats.sort();
        cats.dedup();
        assert_eq!(cats, vec!["data", "dingtalk", "meta", "office", "ops"]);

        // 验证所有 skill 都是 builtin 状态
        for s in &skills {
            assert_eq!(s.status, "builtin", "skill {} should be builtin", s.key);
        }

        // 验证 name 不为空
        for s in &skills {
            assert!(!s.name.is_empty(), "skill {} has empty name", s.key);
        }
    }

    #[test]
    fn test_tool_path_global_tools() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        let builtin = root.join("skills");
        let skill_dir = builtin.join("data").join("ga-query");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: GA\ntool: tools/ga_query.py\n---\n",
        )
        .unwrap();

        // 创建 tools/ 目录以验证路径拼接
        let tools_dir = root.join("tools");
        fs::create_dir_all(&tools_dir).unwrap();
        fs::write(tools_dir.join("ga_query.py"), "# stub").unwrap();

        let mgr = SkillManager::new(builtin, root.to_path_buf());
        let skills = mgr.scan_builtin();
        assert_eq!(skills.len(), 1);

        let s = &skills[0];
        assert_eq!(s.tool.as_deref(), Some("tools/ga_query.py"));

        // builtin skill_dir = root/skills/data/ga-query → parent^3 = root
        let tp = s.tool_path().unwrap();
        let expected = root.join("tools/ga_query.py").to_string_lossy().to_string();
        assert_eq!(tp, expected, "tool_path should resolve to global tools/");
    }

    #[test]
    fn test_tool_path_local_tool() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        let builtin = root.join("skills");
        let skill_dir = builtin.join("ops").join("video-analyzer");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: Video\ntool: tool.py\n---\n",
        )
        .unwrap();

        let mgr = SkillManager::new(builtin, root.to_path_buf());
        let skills = mgr.scan_builtin();
        let s = &skills[0];

        // tool.py 是 skill 目录内的脚本，应该用 skill_dir.join()
        let tp = s.tool_path().unwrap();
        let expected = skill_dir.join("tool.py").to_string_lossy().to_string();
        assert_eq!(tp, expected, "tool_path should resolve relative to skill_dir");
    }
}
