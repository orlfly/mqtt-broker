//! Default OpenClaw "soul" workspace.
//!
//! `zeroclaw` 在拼 system prompt 时会按 `agent/prompt.rs` 的固定顺序
//! 注入以下文件：
//!
//! 1. `AGENTS.md`     — 行为规则
//! 2. `SOUL.md`       — 人格、语气、价值观
//! 3. `TOOLS.md`      — 工具使用附加说明
//! 4. `IDENTITY.md`   — 名字、身份
//! 5. `USER.md`       — 用户偏好
//! 6. `HEARTBEAT.md`  — 周期任务（可选）
//! 7. `MEMORY.md`     — 主会话长期记忆（可选）
//!
//! `BOOTSTRAP.md` 不会被 `IdentitySection` 注入；channels 路径只在
//! 文件存在时才注入，所以这里不创建。
//!
//! ## 启动行为
//!
//! [`ensure_workspace`] 会在 broker 启动时：
//! - 创建工作区目录（若不存在）；
//! - 写入所有"核心"文件（AGENTS / SOUL / TOOLS / IDENTITY / USER）
//!   —— **仅当文件不存在时**。用户后续编辑不会被覆盖。
//! - 创建"可选"文件（HEARTBEAT / MEMORY）为空文件 —— 这样
//!   zeroclaw 的 `inject_workspace_file` 会因 `trim().is_empty()`
//!   静默跳过（参见 `zeroclaw::agent::prompt::inject_workspace_file`），
//!   不会污染 prompt。
//!
//! ## 关闭方法
//!
//! 把对应文件写成完全空的内容即可让 zeroclaw 跳过该段；删掉文件
//! 则会让 prompt 里出现 `[File not found: XXX]` 标记。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use zeroclaw::config::IdentityConfig;

/// 默认文件清单：`(文件名, 默认内容)`。
///
/// 注意：`HEARTBEAT.md` 和 `MEMORY.md` 故意写成单个换行而不是空串，
/// 是因为某些编辑器/版本管理工具讨厌 0 字节文件，行为也更可预期。
const DEFAULT_FILES: &[(&str, &str)] = &[
    (
        "SOUL.md",
        include_str!("../soul_defaults/SOUL.md"),
    ),
    (
        "AGENTS.md",
        include_str!("../soul_defaults/AGENTS.md"),
    ),
    (
        "IDENTITY.md",
        include_str!("../soul_defaults/IDENTITY.md"),
    ),
    (
        "TOOLS.md",
        include_str!("../soul_defaults/TOOLS.md"),
    ),
    (
        "USER.md",
        include_str!("../soul_defaults/USER.md"),
    ),
];

/// 可选占位文件 —— 写一个换行，让 zeroclaw 静默跳过。
const OPTIONAL_FILES: &[&str] = &["HEARTBEAT.md", "MEMORY.md"];

/// 首次启动标识：写到工作区根，提示"已成功种子化"。
const SEED_MARKER: &str = ".seeded";

/// 解析工作区路径：相对路径相对 cwd；绝对路径原样返回。
pub fn resolve_workspace_dir(raw: &str) -> PathBuf {
    let p = PathBuf::from(raw);
    if p.is_absolute() {
        p
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(p)
    }
}

/// 把 broker.yaml 里的 identity_format 字符串翻译成 zeroclaw 的
/// `IdentityConfig`。`"openclaw"`（默认）走多文件注入，不需要任何
/// 额外字段；`"aieos"` 需要 `aieos_path` / `aieos_inline`，本函数
/// 暂时只做格式归一化、字段留空（AIEOS 完整配置可由调用方扩展）。
pub fn build_identity_config(format: &str) -> IdentityConfig {
    let normalized = format.trim().to_ascii_lowercase();
    let format = match normalized.as_str() {
        "" | "openclaw" => "openclaw".to_string(),
        "aieos" => "aieos".to_string(),
        other => {
            tracing::warn!(
                "unknown identity format {:?}, falling back to \"openclaw\"",
                other
            );
            "openclaw".to_string()
        }
    };
    IdentityConfig {
        format,
        aieos_path: None,
        aieos_inline: None,
    }
}

/// 确保工作区存在、按需写入默认 SOUL 套件。
///
/// 行为细节：
/// - 目录不存在 → `fs::create_dir_all`；
/// - 核心文件（AGENTS / SOUL / ...）不存在 → 写入默认内容；
///   已存在 → 跳过（用户编辑保留）；
/// - 可选文件（HEARTBEAT / MEMORY）不存在 → 创建为单换行占位文件；
/// - 完成后写一个 `.seeded` 标记文件，方便诊断"是否被种子化过"。
pub fn ensure_workspace(workspace_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(workspace_dir).with_context(|| {
        format!(
            "failed to create soul workspace dir {}",
            workspace_dir.display()
        )
    })?;

    let mut seeded_count = 0usize;
    let mut skipped_count = 0usize;

    for (filename, content) in DEFAULT_FILES {
        let path = workspace_dir.join(filename);
        if path.exists() {
            skipped_count += 1;
            continue;
        }
        std::fs::write(&path, content).with_context(|| {
            format!("failed to seed soul file {}", path.display())
        })?;
        seeded_count += 1;
    }

    for filename in OPTIONAL_FILES {
        let path = workspace_dir.join(filename);
        if !path.exists() {
            std::fs::write(&path, "\n").with_context(|| {
                format!("failed to seed optional soul file {}", path.display())
            })?;
        }
    }

    let marker = workspace_dir.join(SEED_MARKER);
    if !marker.exists() {
        let unix_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let body = format!(
            "seeded_at_unix = {}\nformat = \"openclaw\"\n",
            unix_secs
        );
        std::fs::write(&marker, body).with_context(|| {
            format!("failed to write soul seed marker {}", marker.display())
        })?;
    }

    if seeded_count > 0 {
        tracing::info!(
            "[soul] seeded {} default file(s) in {} (skipped {} existing); \
             edit them freely — they will not be overwritten on next start",
            seeded_count,
            workspace_dir.display(),
            skipped_count,
        );
    } else {
        tracing::info!(
            "[soul] workspace ready at {} ({} existing file(s) preserved)",
            workspace_dir.display(),
            skipped_count,
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_relative_uses_cwd() {
        let resolved = resolve_workspace_dir("config/soul");
        assert!(resolved.is_absolute());
        assert!(resolved.ends_with("config/soul"));
    }

    #[test]
    fn resolve_absolute_passes_through() {
        let resolved = resolve_workspace_dir("/tmp/abs-soul");
        assert_eq!(resolved, PathBuf::from("/tmp/abs-soul"));
    }

    #[test]
    fn identity_config_defaults_to_openclaw() {
        let cfg = build_identity_config("openclaw");
        assert_eq!(cfg.format, "openclaw");
        assert!(cfg.aieos_path.is_none());
    }

    #[test]
    fn identity_config_normalizes_case_and_whitespace() {
        assert_eq!(build_identity_config("OpenClaw").format, "openclaw");
        assert_eq!(build_identity_config("  AIEOS  ").format, "aieos");
        assert_eq!(build_identity_config("").format, "openclaw");
    }

    #[test]
    fn identity_config_unknown_falls_back_to_openclaw() {
        assert_eq!(build_identity_config("soul-md").format, "openclaw");
    }

    #[test]
    fn ensure_workspace_seeds_then_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();

        // 第一次：5 个核心文件都应被创建
        ensure_workspace(ws).unwrap();
        for (name, _) in DEFAULT_FILES {
            assert!(ws.join(name).exists(), "missing default file {name}");
            let body = std::fs::read_to_string(ws.join(name)).unwrap();
            assert!(!body.trim().is_empty(), "default file {name} is empty");
        }
        for name in OPTIONAL_FILES {
            assert!(ws.join(name).exists(), "missing optional file {name}");
        }
        assert!(ws.join(SEED_MARKER).exists());

        // 第二次：用户改了 SOUL.md，再调用应保留用户的版本
        let custom = "# 我的自定义 SOUL\n";
        std::fs::write(ws.join("SOUL.md"), custom).unwrap();
        ensure_workspace(ws).unwrap();
        let after = std::fs::read_to_string(ws.join("SOUL.md")).unwrap();
        assert_eq!(after, custom, "user edits to SOUL.md must be preserved");
    }
}
