//! Read-only catalog of agent configuration surfaces.
use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::{
    adapter::config as claude,
    core::surface::{Scope, Surface, on_demand_tokens},
};

fn config_root(home: &Path, variable: &str, fallback: &str) -> PathBuf {
    std::env::var_os("HOME")
        .filter(|current| Path::new(current) == home)
        .and_then(|_| std::env::var_os(variable))
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(fallback))
}

/// Read installed configuration without retaining configuration or credential bodies.
pub fn read(agent: &str, home: &Path, projects: &[String]) -> Result<Vec<Value>> {
    let mut rows = Vec::new();
    match agent {
        "claude" => {
            let root = config_root(home, "CLAUDE_CONFIG_DIR", ".claude");
            check_dirs(&[root.join("skills"), root.join("rules"), root.join("agents")])?;
            let scope = Scope::Global;
            let mut surfaces = claude::read_skill_surfaces(&root.join("skills"), &scope);
            surfaces.extend(claude::read_rule_surfaces(&root.join("rules"), &scope));
            surfaces.extend(claude::read_agent_surfaces(&root.join("agents"), &scope));
            if let Some(surface) =
                claude::read_claude_md_surface(&root.join("CLAUDE.md"), "global", &scope)
            {
                surfaces.push(surface);
            }
            rows.extend(surfaces.into_iter().map(|s| surface_row("claude", s)));
            for project in projects {
                let root = PathBuf::from(project);
                if !root.is_dir() {
                    continue;
                }
                check_dirs(&[
                    root.join(".claude/skills"),
                    root.join(".claude/rules"),
                    root.join(".claude/agents"),
                ])?;
                rows.extend(
                    claude::read_project_surfaces(&root, project)
                        .into_iter()
                        .map(|s| surface_row("claude", s)),
                );
            }
        }
        "codex" => {
            let root = config_root(home, "CODEX_HOME", ".codex");
            metadata_row(
                &mut rows,
                "codex",
                "config.toml",
                &root.join("config.toml"),
                None,
            )?;
            add_instruction(&mut rows, "codex", &root.join("AGENTS.md"), "global", None)?;
            add_skills(
                &mut rows,
                "codex",
                &[root.join("skills"), home.join(".agents/skills")],
                "global",
                None,
            )?;
            for project in projects {
                let root = PathBuf::from(project);
                if !root.is_dir() {
                    continue;
                }
                add_instruction(
                    &mut rows,
                    "codex",
                    &root.join("AGENTS.md"),
                    "project",
                    Some(project),
                )?;
                add_skills(
                    &mut rows,
                    "codex",
                    &[root.join(".codex/skills"), root.join(".agents/skills")],
                    "project",
                    Some(project),
                )?;
            }
        }
        "opencode" => {
            let root = config_root(home, "XDG_CONFIG_HOME", ".config").join("opencode");
            for file in ["opencode.json", "opencode.jsonc"] {
                metadata_row(&mut rows, "opencode", file, &root.join(file), None)?;
            }
            add_instruction(
                &mut rows,
                "opencode",
                &root.join("AGENTS.md"),
                "global",
                None,
            )?;
            add_skills(
                &mut rows,
                "opencode",
                &[root.join("skills")],
                "global",
                None,
            )?;
            for project in projects {
                let root = PathBuf::from(project);
                if !root.is_dir() {
                    continue;
                }
                for file in ["opencode.json", "opencode.jsonc"] {
                    metadata_row(&mut rows, "opencode", file, &root.join(file), Some(project))?;
                }
                add_instruction(
                    &mut rows,
                    "opencode",
                    &root.join("AGENTS.md"),
                    "project",
                    Some(project),
                )?;
                add_skills(
                    &mut rows,
                    "opencode",
                    &[root.join(".opencode/skills")],
                    "project",
                    Some(project),
                )?;
            }
        }
        "pi" | "omp" => {
            let root = home.join(format!(".{agent}"));
            metadata_row(
                &mut rows,
                agent,
                "settings.json",
                &root.join("agent/settings.json"),
                None,
            )?;
            add_instruction(
                &mut rows,
                agent,
                &root.join("agent/AGENTS.md"),
                "global",
                None,
            )?;
            add_skills(
                &mut rows,
                agent,
                &[root.join("agent/skills")],
                "global",
                None,
            )?;
            for project in projects {
                let root = PathBuf::from(project);
                if !root.is_dir() {
                    continue;
                }
                metadata_row(
                    &mut rows,
                    agent,
                    "settings.json",
                    &root.join(format!(".{agent}/settings.json")),
                    Some(project),
                )?;
                add_instruction(
                    &mut rows,
                    agent,
                    &root.join("AGENTS.md"),
                    "project",
                    Some(project),
                )?;
                add_skills(
                    &mut rows,
                    agent,
                    &[root.join(format!(".{agent}/skills"))],
                    "project",
                    Some(project),
                )?;
            }
        }
        _ => bail!("unsupported agent: {agent}"),
    }
    Ok(rows)
}

fn surface_row(agent: &str, surface: Surface) -> Value {
    let (scope, project) = match surface.scope {
        Scope::Global => ("global", None),
        Scope::Project(project) => ("project", Some(project)),
    };
    json!({
        "agent": agent, "id": surface.id, "kind": surface.kind,
        "scope": scope, "project": project, "path": surface.config_path,
        "static_tokens": surface.static_tokens, "measurement": "estimated",
        "startup_tokens": surface.startup_tokens,
        "on_demand_tokens": on_demand_tokens(surface.load_mode, surface.static_tokens),
        "load_mode": surface.load_mode.label(),
        "startup_measurement": if surface.startup_tokens.is_some() { "estimated" } else { "unavailable" },
        "usage_status": "unavailable", "uses": null, "loaded": null,
    })
}

fn metadata_row(
    rows: &mut Vec<Value>,
    agent: &str,
    id: &str,
    path: &Path,
    project: Option<&str>,
) -> Result<()> {
    let metadata = match fs::metadata(path) {
        Ok(m) if m.is_file() => m,
        Ok(_) => return Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("inspect {}", path.display())),
    };
    rows.push(json!({
        "agent": agent, "id": id, "kind": "config",
        "scope": if project.is_some() { "project" } else { "global" },
        "project": project, "path": path.display().to_string(),
        "static_tokens": null, "measurement": "measured", "size_bytes": metadata.len(),
        "startup_tokens": null, "on_demand_tokens": null, "load_mode": null,
        "startup_measurement": "unavailable",
        "usage_status": "unavailable", "uses": null, "loaded": null,
    }));
    Ok(())
}

fn add_instruction(
    rows: &mut Vec<Value>,
    agent: &str,
    path: &Path,
    scope: &str,
    project: Option<&str>,
) -> Result<()> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    rows.push(json!({
        "agent": agent, "id": "AGENTS.md", "kind": "claude_md", "scope": scope,
        "project": project, "path": path.display().to_string(),
        "static_tokens": claude::approx_tokens(&content), "measurement": "estimated",
        "startup_tokens": null, "on_demand_tokens": null, "load_mode": null,
        "startup_measurement": "unavailable",
        "usage_status": "unavailable", "uses": null, "loaded": null,
    }));
    Ok(())
}

fn add_skills(
    rows: &mut Vec<Value>,
    agent: &str,
    dirs: &[PathBuf],
    scope: &str,
    project: Option<&str>,
) -> Result<()> {
    for dir in dirs {
        match fs::metadata(dir) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => continue,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e).with_context(|| format!("inspect {}", dir.display())),
        }
        let entries =
            fs::read_dir(dir).with_context(|| format!("read directory {}", dir.display()))?;
        for entry in entries {
            let entry =
                entry.with_context(|| format!("read directory entry in {}", dir.display()))?;
            let candidate = entry.path();
            let candidate_is_dir = match fs::metadata(&candidate) {
                Ok(meta) => meta.is_dir(),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
                Err(e) => {
                    return Err(e).with_context(|| format!("inspect {}", candidate.display()));
                }
            };
            if candidate_is_dir {
                add_skill_file(rows, agent, &candidate, scope, project)?;
                // Support one nested category directory while bounding symlink traversal.
                let children = fs::read_dir(&candidate)
                    .with_context(|| format!("read directory {}", candidate.display()))?;
                for child in children {
                    let child = child.with_context(|| {
                        format!("read directory entry in {}", candidate.display())
                    })?;
                    let child_path = child.path();
                    let child_is_dir = match fs::metadata(&child_path) {
                        Ok(meta) => meta.is_dir(),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
                        Err(e) => {
                            return Err(e)
                                .with_context(|| format!("inspect {}", child_path.display()));
                        }
                    };
                    if child_is_dir {
                        add_skill_file(rows, agent, &child_path, scope, project)?;
                    }
                }
            }
        }
    }
    Ok(())
}

fn add_skill_file(
    rows: &mut Vec<Value>,
    agent: &str,
    dir: &Path,
    scope: &str,
    project: Option<&str>,
) -> Result<()> {
    let path = dir.join("SKILL.md");
    let content = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    let id = dir.file_name().and_then(|s| s.to_str()).unwrap_or("skill");
    rows.push(json!({
        "agent": agent, "id": id, "kind": "skill", "scope": scope,
        "project": project, "path": path.display().to_string(),
        "static_tokens": claude::approx_tokens(&content), "measurement": "estimated",
        "startup_tokens": null, "on_demand_tokens": null, "load_mode": null,
        "startup_measurement": "unavailable",
        "usage_status": "unavailable", "uses": null, "loaded": null,
    }));
    Ok(())
}

fn check_dirs(dirs: &[PathBuf]) -> Result<()> {
    for dir in dirs {
        match fs::metadata(dir) {
            Ok(meta) if meta.is_dir() => {
                fs::read_dir(dir).with_context(|| format!("read directory {}", dir.display()))?;
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("inspect {}", dir.display())),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogs_skill_estimate_without_retaining_file_body_or_claiming_unused() {
        let temp = std::env::temp_dir().join(format!("cclens-config-multi-{}", std::process::id()));
        let skill = temp.join(".codex/skills/review/SKILL.md");
        fs::create_dir_all(skill.parent().unwrap()).unwrap();
        fs::write(&skill, "synthetic private skill body").unwrap();

        let rows = read("codex", &temp, &[]).unwrap();
        let row = rows.iter().find(|r| r["id"] == "review").unwrap();
        assert_eq!(row["kind"], "skill");
        assert_eq!(
            row["static_tokens"],
            claude::approx_tokens("synthetic private skill body")
        );
        assert_eq!(row["measurement"], "estimated");
        assert_eq!(row["usage_status"], "unavailable");
        assert!(row["uses"].is_null());
        assert!(row["loaded"].is_null());
        assert!(
            !rows
                .iter()
                .any(|r| r.to_string().contains("synthetic private skill body"))
        );

        fs::remove_dir_all(temp).unwrap();
    }
}
