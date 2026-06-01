mod agent_spawn;
mod command_match;
mod config;
mod engine;
mod input;
mod parser;
mod safety;
mod structure;
mod walker;

use config::load_config;
use engine::Engine;
use input::{HookInput, HookOutput};
use std::path::PathBuf;

const VERSION: &str = env!("CARGO_PKG_VERSION");

const USAGE: &str = "\
Usage: claude-hook-guard [OPTIONS]

Options:
  --config <PATH>                Optional. Defaults to ~/.claude/hooks/rules.toml
  --json                         Output as JSON (default).
  --human                        Output as human-readable text.
  --verbose                      Log diagnostic info to stderr.
  --version                      Print version and exit.
  --help                         Print this help and exit.
";

struct Config {
    config_path: Option<PathBuf>,
    json_output: bool,
    verbose: bool,
}

fn parse_args() -> Result<Config, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let mut config_path: Option<PathBuf> = None;
    let mut json_output = true;
    let mut verbose = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                print!("{}", USAGE);
                std::process::exit(0);
            }
            "--version" | "-V" => {
                println!("claude-hook-guard {}", VERSION);
                std::process::exit(0);
            }
            "--verbose" => {
                verbose = true;
            }
            "--json" => {
                json_output = true;
            }
            "--human" => {
                json_output = false;
            }
            "--config" => {
                i += 1;
                let value = args.get(i).ok_or("--config requires a value")?;
                config_path = Some(PathBuf::from(value));
            }
            other => {
                return Err(format!("Unknown argument: '{}'", other));
            }
        }
        i += 1;
    }

    Ok(Config {
        config_path,
        json_output,
        verbose,
    })
}

fn run() -> i32 {
    let config = match parse_args() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("claude-hook-guard: error: {}", e);
            eprintln!("Run with --help for usage.");
            return 2;
        }
    };

    // Read all of stdin.
    let mut stdin_buf = String::new();
    {
        use std::io::Read;
        if std::io::stdin().read_to_string(&mut stdin_buf).is_err() {
            if config.verbose {
                eprintln!("claude-hook-guard: failed to read stdin, passing through");
            }
            return 0;
        }
    }

    if stdin_buf.trim().is_empty() {
        if config.verbose {
            eprintln!("claude-hook-guard: empty stdin, passing through");
        }
        return 0;
    }

    // Deserialize hook input. Fail-open on parse errors.
    let hook_input: HookInput = match serde_json::from_str(&stdin_buf) {
        Ok(v) => v,
        Err(e) => {
            if config.verbose {
                eprintln!(
                    "claude-hook-guard: failed to parse input JSON ({}), passing through",
                    e
                );
            }
            return 0;
        }
    };

    let validated = match load_config(config.config_path.as_deref()) {
        Ok(c) => c,
        Err(e) => {
            output_reason(&format!("BLOCKED: {}", e), config.json_output);
            return 0;
        }
    };

    let engine = Engine { config: &validated };
    if let Some(reason) = engine.evaluate(&hook_input) {
        if config.verbose {
            eprintln!("claude-hook-guard: denied: {}", reason);
        }
        output_reason(&reason, config.json_output);
        return 0;
    }

    if config.verbose {
        eprintln!("claude-hook-guard: all checks passed");
    }

    0
}

fn output_reason(reason: &str, json_output: bool) {
    if json_output {
        println!("{}", HookOutput::deny(reason).to_json());
    } else {
        println!("{}", reason);
    }
}

fn main() {
    std::process::exit(run());
}
