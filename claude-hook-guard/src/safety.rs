use crate::command_match::matches_prefix;
use crate::config::{SafetyGuard, SafetyGuardKind, ValidatedConfig};

pub fn evaluate_safety(
    config: &ValidatedConfig,
    profile_id: &str,
    command: &[String],
    args: &[String],
) -> Option<String> {
    let profile = config.rules.profiles.get(profile_id)?;

    for (name, guard) in &config.rules.safety {
        if profile
            .safety_overrides
            .iter()
            .any(|override_name| override_name == name)
        {
            continue;
        }

        if !matches_prefix(command, &guard.command.prefix) {
            continue;
        }

        if guard
            .allow_exact
            .iter()
            .any(|allowed| allowed.as_slice() == command)
        {
            continue;
        }

        if let Some(reason) = evaluate_guard(guard, command, args) {
            return Some(reason);
        }
    }

    None
}

fn evaluate_guard(guard: &SafetyGuard, command: &[String], args: &[String]) -> Option<String> {
    match guard.kind {
        SafetyGuardKind::DenyFlags => {
            if args
                .iter()
                .any(|arg| guard.flags.iter().any(|flag| matches_flag(arg, flag)))
            {
                Some(guard.message.clone())
            } else {
                None
            }
        }
        SafetyGuardKind::RequirePositionalPrefix => {
            let positional = args.iter().find(|arg| !arg.starts_with('-'));
            if positional
                .is_some_and(|arg| guard.prefixes.iter().any(|prefix| arg.starts_with(prefix)))
            {
                None
            } else {
                Some(guard.message.clone())
            }
        }
        SafetyGuardKind::RequireCurlUrlPrefix => {
            let url = extract_curl_url(args);
            if url.is_some_and(|arg| {
                guard
                    .prefixes
                    .iter()
                    .any(|prefix| strip_wrapping_quotes(arg).starts_with(prefix))
            }) {
                None
            } else {
                Some(guard.message.clone())
            }
        }
        SafetyGuardKind::AllowCurlHeaders => {
            let headers = extract_curl_flag_values(args, &["-H", "--header"]);
            if headers.iter().all(|header| {
                let header = strip_wrapping_quotes(header);
                guard
                    .prefixes
                    .iter()
                    .any(|prefix| header.starts_with(prefix))
            }) {
                None
            } else {
                Some(guard.message.clone())
            }
        }
        SafetyGuardKind::AllowCurlMethods => {
            let methods = extract_curl_flag_values(args, &["-X", "--request"]);
            if methods.iter().all(|method| {
                let method = strip_wrapping_quotes(method).to_ascii_uppercase();
                guard.prefixes.iter().any(|allowed| method == *allowed)
            }) {
                None
            } else {
                Some(guard.message.clone())
            }
        }
        SafetyGuardKind::AllowCurlForms => {
            let forms = extract_curl_flag_values(args, &["-F", "--form"]);
            if forms.is_empty() {
                None
            } else {
                let url_ok = extract_curl_url(args).is_some_and(|url| {
                    let url = strip_wrapping_quotes(url);
                    guard.targets.iter().any(|target| url.starts_with(target))
                });
                let forms_ok = forms.iter().all(|form| {
                    let form = strip_wrapping_quotes(form);
                    guard.prefixes.iter().any(|prefix| form.starts_with(prefix))
                });

                if url_ok && forms_ok {
                    None
                } else {
                    Some(guard.message.clone())
                }
            }
        }
        SafetyGuardKind::RequireExplicitPathspecs => {
            if args.len() <= 1
                || args[1..]
                    .iter()
                    .any(|arg| guard.forbid.iter().any(|bad| bad == arg))
            {
                Some(guard.message.clone())
            } else {
                None
            }
        }
        SafetyGuardKind::RequireExplicitPushTarget => {
            let positional: Vec<&String> =
                args.iter().filter(|arg| !arg.starts_with('-')).collect();
            if positional.len() < 3 {
                Some(guard.message.clone())
            } else {
                None
            }
        }
        SafetyGuardKind::DenyProtectedBranch => {
            let positional: Vec<&String> =
                args.iter().filter(|arg| !arg.starts_with('-')).collect();
            let branch_args = if positional.len() > 2 {
                &positional[2..]
            } else {
                &[]
            };
            if branch_args.iter().any(|arg| {
                arg.split(':').any(|part| {
                    guard
                        .branches
                        .iter()
                        .any(|branch| branch == part.trim_start_matches('+'))
                })
            }) {
                Some(guard.message.clone())
            } else {
                None
            }
        }
        SafetyGuardKind::RequireBoundedLogs => {
            if args
                .iter()
                .any(|arg| arg == "--tail" || arg.starts_with("--tail=") || arg == "-n")
            {
                None
            } else {
                Some(guard.message.clone())
            }
        }
        SafetyGuardKind::DenyAlways => Some(guard.message.clone()),
        SafetyGuardKind::DenySubcommands => {
            let tokens = command.to_vec();
            if guard
                .subcommands
                .iter()
                .any(|sub| matches_prefix(&tokens[1..], sub))
            {
                Some(guard.message.clone())
            } else {
                None
            }
        }
    }
}

fn short_flag_contains(arg: &str, flag: &str) -> bool {
    if !arg.starts_with('-') || arg.starts_with("--") || !flag.starts_with('-') || flag.len() != 2 {
        return false;
    }
    let ch = flag.chars().nth(1).unwrap();
    arg[1..].chars().any(|c| c == ch)
}

fn matches_flag(arg: &str, flag: &str) -> bool {
    arg == flag
        || arg.strip_prefix(&(flag.to_string() + "=")).is_some()
        || short_flag_contains(arg, flag)
}

fn strip_wrapping_quotes(value: &str) -> &str {
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        if (bytes[0] == b'"' && bytes[value.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[value.len() - 1] == b'\'')
        {
            return &value[1..value.len() - 1];
        }
    }
    value
}

fn extract_curl_url(args: &[String]) -> Option<&str> {
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        if arg == "--" {
            return args.get(index + 1).map(|value| value.as_str());
        }

        if let Some(value) = split_inline_curl_value(arg, &["--url"]) {
            return Some(value);
        }

        if arg == "--url" {
            return args.get(index + 1).map(|value| value.as_str());
        }

        if curl_flag_takes_value(arg) {
            index += 2;
            continue;
        }

        if arg.starts_with('-') {
            index += 1;
            continue;
        }

        return Some(arg);
    }

    None
}

fn extract_curl_flag_values<'a>(args: &'a [String], flags: &[&str]) -> Vec<&'a str> {
    let mut values = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        if arg == "--" {
            break;
        }

        if let Some(value) = split_inline_curl_value(arg, flags) {
            values.push(value);
            index += 1;
            continue;
        }

        if flags.iter().any(|flag| arg == *flag) {
            if let Some(value) = args.get(index + 1) {
                values.push(value.as_str());
            }
            index += 2;
            continue;
        }

        if curl_flag_takes_value(arg) {
            index += 2;
            continue;
        }

        index += 1;
    }

    values
}

fn split_inline_curl_value<'a>(arg: &'a str, flags: &[&str]) -> Option<&'a str> {
    for flag in flags {
        if let Some(value) = arg.strip_prefix(&format!("{flag}=")) {
            return Some(value);
        }
        if flag.len() == 2 && arg.starts_with(flag) && arg.len() > flag.len() {
            return Some(&arg[flag.len()..]);
        }
    }
    None
}

fn curl_flag_takes_value(arg: &str) -> bool {
    matches!(
        arg,
        "-A" | "--user-agent"
            | "-b"
            | "--cookie"
            | "-c"
            | "--cookie-jar"
            | "-d"
            | "--data"
            | "--data-raw"
            | "--data-binary"
            | "--data-urlencode"
            | "-e"
            | "--referer"
            | "-E"
            | "--cert"
            | "--key"
            | "-F"
            | "--form"
            | "--form-string"
            | "-H"
            | "--header"
            | "-K"
            | "--config"
            | "-o"
            | "--output"
            | "-T"
            | "--upload-file"
            | "-u"
            | "--user"
            | "-x"
            | "--proxy"
            | "-X"
            | "--request"
            | "--url"
    ) || split_inline_curl_value(
        arg,
        &[
            "-A",
            "-b",
            "-c",
            "-d",
            "-e",
            "-E",
            "-F",
            "-H",
            "-K",
            "-o",
            "-T",
            "-u",
            "-x",
            "-X",
            "--user-agent",
            "--cookie",
            "--cookie-jar",
            "--data",
            "--data-raw",
            "--data-binary",
            "--data-urlencode",
            "--referer",
            "--cert",
            "--key",
            "--form",
            "--form-string",
            "--header",
            "--config",
            "--output",
            "--upload-file",
            "--user",
            "--proxy",
            "--request",
            "--url",
        ],
    )
    .is_some()
}
