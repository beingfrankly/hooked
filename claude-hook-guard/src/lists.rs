/// Commands that write to the filesystem (blocked in read-only mode)
pub const WRITE_COMMANDS: &[&str] = &[
    "rm", "rmdir", "mv", "cp", "mkdir", "touch", "chmod", "chown", "ln", "install",
    "nano", "vi", "vim",
];

/// Two-word command patterns that write (command, subcommand)
/// Blocked in read-only mode
pub const WRITE_SUBCOMMANDS: &[(&str, &str)] = &[
    ("git", "add"),
    ("git", "commit"),
    ("git", "push"),
    ("git", "checkout"),
    ("git", "reset"),
    ("git", "clean"),
    ("git", "rebase"),
    ("git", "merge"),
    ("git", "stash"),
    ("npm", "install"),
    ("pip", "install"),
    ("cargo", "install"),
    ("brew", "install"),
];

/// Commands that use -i flag for in-place editing (blocked in read-only when -i present)
pub const INPLACE_EDIT_COMMANDS: &[&str] = &["sed", "awk"];

/// Allowed pipe targets in read-only mode (right-hand side of |)
pub const ALLOWED_PIPE_TARGETS: &[&str] = &[
    "head", "tail", "grep", "egrep", "fgrep", "rg",
    "sort", "uniq", "wc", "tr", "cut", "awk", "sed",
    "less", "jq", "column", "cat",
];

/// Default protected branches for safety mode
pub const DEFAULT_PROTECTED_BRANCHES: &[&str] = &["main", "master", "develop"];

/// Commands that have native Claude Code tool equivalents (blocked in native-tools mode).
/// Each entry is (command_name, deny_message).
pub const NATIVE_TOOL_COMMANDS: &[(&str, &str)] = &[
    ("find", "BLOCKED: Use the Glob tool for file discovery (e.g. pattern: `**/*.ts`). Glob supports glob patterns and returns files sorted by modification time."),
    ("tree", "BLOCKED: Use the Glob tool for file discovery, or Bash(`eza --tree`) for tree views."),
    ("grep", "BLOCKED: Use the Grep tool which provides ripgrep-powered content search with regex, glob filters, and context lines."),
    ("rg", "BLOCKED: Use the Grep tool which provides ripgrep-powered content search with regex, glob filters, and context lines."),
    ("cat", "BLOCKED: Use the Read tool to read files. It supports offset and limit parameters for partial reads."),
    ("head", "BLOCKED: Use the Read tool to read files. It supports offset and limit parameters for partial reads."),
    ("tail", "BLOCKED: Use the Read tool to read files. It supports offset and limit parameters for partial reads."),
    ("sed", "BLOCKED: Use the Edit tool for targeted string replacements in files."),
    ("awk", "BLOCKED: Use the Grep tool for pattern extraction, or the Edit tool for file modifications."),
    ("ls", "BLOCKED: Use the Glob tool for file listing (e.g. pattern: `*`), or Bash(`eza`) for detailed listings."),
];

/// Build/test commands that must be delegated to the build-runner agent.
/// Each entry is (command_name, allowed_subcommands).
/// Only blocked when the first argument matches one of the listed subcommands.
pub const BUILD_COMMANDS_WITH_SUBCOMMANDS: &[(&str, &[&str])] = &[
    ("pnpm", &["run", "build", "test", "lint", "compile", "exec", "dlx", "start", "ci"]),
    ("npm", &["run", "build", "test", "exec", "ci", "start"]),
    ("mvn", &["package", "compile", "test", "install", "verify", "clean"]),
    ("./mvnw", &["package", "compile", "test", "install", "verify", "clean"]),
    ("gradle", &["build", "test", "compile", "clean", "assemble"]),
    ("./gradlew", &["build", "test", "compile", "clean", "assemble"]),
    ("go", &["build", "test", "run"]),
];

/// Build/test/docker commands blocked entirely in delegation mode (any subcommand).
pub const BUILD_COMMANDS_ANY: &[&str] = &[
    "npx", "jest", "vitest", "node",
    "docker", "docker-compose",
];

/// SQL destructive patterns (case-insensitive substring match)
pub const DESTRUCTIVE_SQL_PATTERNS: &[&str] = &[
    "DROP TABLE",
    "DROP DATABASE",
    "TRUNCATE TABLE",
    "DELETE FROM",
];
