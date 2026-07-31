use std::path::Path;

#[derive(Debug, Clone)]
pub struct Command {
    pub name: String,
    pub description: String,
    pub template: String,
}

/// Built-in commands compiled into the binary.
pub fn builtin_commands() -> Vec<Command> {
    vec![
        Command {
            name: "research".into(),
            description: "Research the codebase for a topic".into(),
            template: "Research the codebase to understand and document: {args}\n\n\
                Use read, grep, and find tools to explore. Return findings with file:line references. \
                Do not modify any files."
                .into(),
        },
        Command {
            name: "plan".into(),
            description: "Create an implementation plan".into(),
            template: "Create an implementation plan for: {args}\n\n\
                Analyze the existing codebase to understand the current architecture. \
                Identify all files that need to change. Write the plan to IMPLEMENTATION_PLAN.md \
                with clear tasks marked [TODO]."
                .into(),
        },
        Command {
            name: "implement".into(),
            description: "Implement the next task from the plan".into(),
            template: "Read IMPLEMENTATION_PLAN.md. Pick the next [TODO] or [CURRENT] task. \
                Implement it fully. Run validation commands if configured. \
                Update the task status to [DONE] in the plan. \
                Commit the changes with a descriptive message."
                .into(),
        },
        Command {
            name: "validate".into(),
            description: "Run validation commands".into(),
            template: "Run the project's validation commands to check for errors. \
                If validation commands are configured in RHO.md, run those. \
                Otherwise run: cargo test && cargo clippy -- -D warnings\n\n\
                Report any failures and suggest fixes."
                .into(),
        },
        Command {
            name: "commit".into(),
            description: "Stage and commit changes".into(),
            template: "Review the current git status and diff. Stage the relevant changed files. \
                Generate a concise, descriptive commit message that explains what changed and why. \
                Create the commit."
                .into(),
        },
    ]
}

/// Discover user command overrides from .rho/commands/.
pub fn discover_commands(cwd: &Path) -> Vec<Command> {
    let mut commands = Vec::new();

    let dirs = [
        cwd.join(".rho/commands"),
        dirs::home_dir()
            .map(|h| h.join(".rho/commands"))
            .unwrap_or_default(),
    ];

    for dir in &dirs {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("md") {
                    continue;
                }
                let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                let Ok(content) = std::fs::read_to_string(&path) else {
                    continue;
                };

                if let Some(cmd) = parse_command_file(stem, &content) {
                    commands.push(cmd);
                }
            }
        }
    }

    commands
}

fn parse_command_file(name: &str, content: &str) -> Option<Command> {
    let trimmed = content.trim_start();

    // Try to parse frontmatter for description
    if let Some(after_first) = trimmed.strip_prefix("---") {
        if let Some(end) = after_first.find("\n---") {
            let frontmatter = &after_first[..end];
            let body_start = 3 + end + 4;
            let body = trimmed[body_start..].trim().to_string();

            let mut description = name.to_string();
            for line in frontmatter.lines() {
                if let Some(val) = line.strip_prefix("description:") {
                    description = val.trim().to_string();
                }
            }

            return Some(Command {
                name: name.to_string(),
                description,
                template: body,
            });
        }
    }

    // No frontmatter — entire file is template
    Some(Command {
        name: name.to_string(),
        description: format!("Custom command: {name}"),
        template: content.to_string(),
    })
}

/// Resolve a command by name, expanding {args} in the template.
/// User commands override built-ins by name.
pub fn resolve_command(name: &str, args: &str, cwd: &Path) -> Option<String> {
    // Check user commands first (override built-ins)
    let user_cmds = discover_commands(cwd);
    if let Some(cmd) = user_cmds.iter().find(|c| c.name == name) {
        return Some(cmd.template.replace("{args}", args));
    }

    // Fall back to built-ins
    let builtins = builtin_commands();
    builtins
        .iter()
        .find(|c| c.name == name)
        .map(|cmd| cmd.template.replace("{args}", args))
}

/// List all available commands (builtins + user, user overrides by name).
pub fn all_commands(cwd: &Path) -> Vec<Command> {
    let mut commands = builtin_commands();
    let user_cmds = discover_commands(cwd);

    for user_cmd in user_cmds {
        if let Some(existing) = commands.iter_mut().find(|c| c.name == user_cmd.name) {
            *existing = user_cmd;
        } else {
            commands.push(user_cmd);
        }
    }

    commands
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_commands_exist() {
        let cmds = builtin_commands();
        assert!(cmds.iter().any(|c| c.name == "research"));
        assert!(cmds.iter().any(|c| c.name == "plan"));
        assert!(cmds.iter().any(|c| c.name == "implement"));
        assert!(cmds.iter().any(|c| c.name == "validate"));
        assert!(cmds.iter().any(|c| c.name == "commit"));
    }

    #[test]
    fn resolve_builtin_command() {
        let tmp = tempfile::tempdir().unwrap();
        let result = resolve_command("research", "auth module", tmp.path());
        assert!(result.is_some());
        let expanded = result.unwrap();
        assert!(expanded.contains("auth module"));
        assert!(!expanded.contains("{args}"));
    }

    #[test]
    fn resolve_unknown_command() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(resolve_command("nonexistent", "", tmp.path()).is_none());
    }

    #[test]
    fn parse_command_file_with_frontmatter() {
        let content = "---\ndescription: My custom command\n---\nDo {args} now.";
        let cmd = parse_command_file("custom", content).unwrap();
        assert_eq!(cmd.name, "custom");
        assert_eq!(cmd.description, "My custom command");
        assert_eq!(cmd.template, "Do {args} now.");
    }

    #[test]
    fn parse_command_file_no_frontmatter() {
        let content = "Just do the thing with {args}.";
        let cmd = parse_command_file("simple", content).unwrap();
        assert_eq!(cmd.name, "simple");
        assert_eq!(cmd.template, content);
    }

    #[test]
    fn user_commands_override_builtins() {
        let tmp = tempfile::tempdir().unwrap();
        let cmd_dir = tmp.path().join(".rho/commands");
        std::fs::create_dir_all(&cmd_dir).unwrap();
        std::fs::write(
            cmd_dir.join("research.md"),
            "---\ndescription: Custom research\n---\nMy custom research for {args}.",
        )
        .unwrap();

        let result = resolve_command("research", "auth", tmp.path()).unwrap();
        assert!(result.contains("My custom research"));
    }
}
