use std::{
    env,
    io::{Read, Write},
    process::{Command, Stdio},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use wait_timeout::ChildExt;

const MAX_PATTERN_BYTES: usize = 4 * 1024;
const MAX_TEXT_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug)]
pub struct EcmaRegexWorker {
    executable: std::path::PathBuf,
    timeout: Duration,
}

/// Environment override for the worker executable. The isolated worker is
/// normally the running binary, but when STcli runs embedded in another host
/// (for example an integration-test harness) that binary does not carry the
/// `internal regex-*-worker` subcommands. Pointing this variable at a real
/// `stcli` binary lets the isolated matcher still run.
pub const WORKER_EXECUTABLE_ENV: &str = "STCLI_REGEX_WORKER";

impl EcmaRegexWorker {
    pub fn current(timeout: Duration) -> Result<Self, EcmaRegexError> {
        let executable = match env::var_os(WORKER_EXECUTABLE_ENV) {
            Some(path) => path.into(),
            None => env::current_exe().map_err(EcmaRegexError::Executable)?,
        };
        Ok(Self::new(executable, timeout))
    }

    pub fn new(executable: impl Into<std::path::PathBuf>, timeout: Duration) -> Self {
        Self {
            executable: executable.into(),
            timeout,
        }
    }

    pub fn is_match(&self, pattern: &str, flags: &str, text: &str) -> Result<bool, EcmaRegexError> {
        validate_sizes(pattern, text)?;
        let request = RegexRequest {
            pattern: pattern.to_owned(),
            flags: normalize_flags(flags),
            text: text.to_owned(),
        };
        let output = self.run(["internal", "regex-worker"], &request)?;
        let response =
            serde_json::from_slice::<RegexResponse>(&output).map_err(EcmaRegexError::Decode)?;
        match response {
            RegexResponse::Match { matched } => Ok(matched),
            RegexResponse::Error { message } => Err(EcmaRegexError::Pattern(message)),
        }
    }

    pub fn find_matches(
        &self,
        pattern: &str,
        flags: &str,
        text: &str,
    ) -> Result<Vec<RegexMatch>, EcmaRegexError> {
        validate_sizes(pattern, text)?;
        let request = RegexReplaceRequest {
            pattern: pattern.to_owned(),
            global: flags.contains('g'),
            flags: normalize_flags(flags),
            text: text.to_owned(),
        };
        let output = self.run(["internal", "regex-replace-worker"], &request)?;
        let response = serde_json::from_slice::<RegexReplaceResponse>(&output)
            .map_err(EcmaRegexError::Decode)?;
        match response {
            RegexReplaceResponse::Matches { matches } => Ok(matches),
            RegexReplaceResponse::Error { message } => Err(EcmaRegexError::Pattern(message)),
        }
    }

    fn run<S: serde::Serialize>(
        &self,
        args: [&str; 2],
        request: &S,
    ) -> Result<Vec<u8>, EcmaRegexError> {
        let mut child = Command::new(&self.executable)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(EcmaRegexError::Spawn)?;
        child
            .stdin
            .take()
            .ok_or(EcmaRegexError::MissingPipe)?
            .write_all(&serde_json::to_vec(request).map_err(EcmaRegexError::Encode)?)
            .map_err(EcmaRegexError::Write)?;
        let status = child
            .wait_timeout(self.timeout)
            .map_err(EcmaRegexError::Wait)?;
        if status.is_none() {
            child.kill().map_err(EcmaRegexError::Kill)?;
            child.wait().map_err(EcmaRegexError::Wait)?;
            return Err(EcmaRegexError::Timeout(self.timeout));
        }
        let mut output = Vec::new();
        child
            .stdout
            .take()
            .ok_or(EcmaRegexError::MissingPipe)?
            .read_to_end(&mut output)
            .map_err(EcmaRegexError::Read)?;
        Ok(output)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RegexRequest {
    pub pattern: String,
    pub flags: String,
    pub text: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "result", rename_all = "kebab-case")]
pub enum RegexResponse {
    Match { matched: bool },
    Error { message: String },
}

pub fn run_worker(request: RegexRequest) -> RegexResponse {
    if let Err(error) = validate_sizes(&request.pattern, &request.text) {
        return RegexResponse::Error {
            message: error.to_string(),
        };
    }
    match regress::Regex::with_flags(&request.pattern, request.flags.as_str()) {
        Ok(regex) => RegexResponse::Match {
            matched: regex.find(&request.text).is_some(),
        },
        Err(error) => RegexResponse::Error {
            message: error.to_string(),
        },
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RegexReplaceRequest {
    pub pattern: String,
    pub flags: String,
    pub global: bool,
    pub text: String,
}

/// One regex match with its captured groups, resolved to strings against the
/// original text. `groups[0]` is always the whole match; `groups[n]` is the
/// n-th capturing group, `None` when that group did not participate.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RegexMatch {
    pub start: usize,
    pub end: usize,
    pub groups: Vec<Option<String>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "result", rename_all = "kebab-case")]
pub enum RegexReplaceResponse {
    Matches { matches: Vec<RegexMatch> },
    Error { message: String },
}

pub fn run_replace_worker(request: RegexReplaceRequest) -> RegexReplaceResponse {
    if let Err(error) = validate_sizes(&request.pattern, &request.text) {
        return RegexReplaceResponse::Error {
            message: error.to_string(),
        };
    }
    let regex = match regress::Regex::with_flags(&request.pattern, request.flags.as_str()) {
        Ok(regex) => regex,
        Err(error) => {
            return RegexReplaceResponse::Error {
                message: error.to_string(),
            };
        }
    };
    let mut matches = Vec::new();
    for found in regex.find_iter(&request.text) {
        let mut groups = Vec::with_capacity(found.captures.len() + 1);
        groups.push(Some(request.text[found.range()].to_owned()));
        for capture in &found.captures {
            groups.push(capture.clone().map(|range| request.text[range].to_owned()));
        }
        matches.push(RegexMatch {
            start: found.start(),
            end: found.end(),
            groups,
        });
        if !request.global {
            break;
        }
    }
    RegexReplaceResponse::Matches { matches }
}

fn validate_sizes(pattern: &str, text: &str) -> Result<(), EcmaRegexError> {
    if pattern.len() > MAX_PATTERN_BYTES {
        return Err(EcmaRegexError::PatternTooLarge(pattern.len()));
    }
    if text.len() > MAX_TEXT_BYTES {
        return Err(EcmaRegexError::TextTooLarge(text.len()));
    }
    Ok(())
}

fn normalize_flags(flags: &str) -> String {
    flags
        .chars()
        .filter(|flag| matches!(flag, 'i' | 'm' | 's' | 'u'))
        .collect()
}

#[derive(Debug, Error)]
pub enum EcmaRegexError {
    #[error("failed to resolve regex worker executable: {0}")]
    Executable(std::io::Error),
    #[error("failed to spawn regex worker: {0}")]
    Spawn(std::io::Error),
    #[error("regex worker pipe is unavailable")]
    MissingPipe,
    #[error("failed to write regex worker request: {0}")]
    Write(std::io::Error),
    #[error("failed to wait for regex worker: {0}")]
    Wait(std::io::Error),
    #[error("failed to kill timed-out regex worker: {0}")]
    Kill(std::io::Error),
    #[error("failed to read regex worker response: {0}")]
    Read(std::io::Error),
    #[error("failed to encode regex request: {0}")]
    Encode(serde_json::Error),
    #[error("failed to decode regex response: {0}")]
    Decode(serde_json::Error),
    #[error("ECMAScript regex failed: {0}")]
    Pattern(String),
    #[error("ECMAScript regex exceeded {0:?}")]
    Timeout(Duration),
    #[error("regex pattern is too large: {0} bytes")]
    PatternTooLarge(usize),
    #[error("regex input is too large: {0} bytes")]
    TextTooLarge(usize),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_supports_backreferences_and_lookaround() {
        let backreference = run_worker(RegexRequest {
            pattern: r"(\w)\1".to_owned(),
            flags: String::new(),
            text: "book".to_owned(),
        });
        assert!(matches!(
            backreference,
            RegexResponse::Match { matched: true }
        ));
        let lookaround = run_worker(RegexRequest {
            pattern: r"library(?= door)".to_owned(),
            flags: "i".to_owned(),
            text: "Library door".to_owned(),
        });
        assert!(matches!(lookaround, RegexResponse::Match { matched: true }));
    }

    #[test]
    fn oversized_input_is_rejected_before_matching() {
        let response = run_worker(RegexRequest {
            pattern: "a".to_owned(),
            flags: String::new(),
            text: "a".repeat(MAX_TEXT_BYTES + 1),
        });
        assert!(matches!(response, RegexResponse::Error { .. }));
    }

    fn replace(pattern: &str, flags: &str, global: bool, text: &str) -> Vec<RegexMatch> {
        match run_replace_worker(RegexReplaceRequest {
            pattern: pattern.to_owned(),
            flags: normalize_flags(flags),
            global,
            text: text.to_owned(),
        }) {
            RegexReplaceResponse::Matches { matches } => matches,
            RegexReplaceResponse::Error { message } => panic!("regex error: {message}"),
        }
    }

    #[test]
    fn replace_worker_reports_capture_groups() {
        let matches = replace(r"(\w+)@(\w+)", "", false, "reach me at alice@example");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].groups[0].as_deref(), Some("alice@example"));
        assert_eq!(matches[0].groups[1].as_deref(), Some("alice"));
        assert_eq!(matches[0].groups[2].as_deref(), Some("example"));
    }

    #[test]
    fn replace_worker_honors_global_flag() {
        assert_eq!(replace("a", "", false, "banana").len(), 1);
        assert_eq!(replace("a", "g", true, "banana").len(), 3);
    }

    #[test]
    fn replace_worker_reports_unmatched_optional_group_as_none() {
        let matches = replace(r"(a)?b", "", false, "b");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].groups[0].as_deref(), Some("b"));
        assert_eq!(matches[0].groups[1], None);
    }

    #[test]
    fn replace_worker_returns_no_matches_when_absent() {
        assert!(replace("zzz", "g", true, "banana").is_empty());
    }
}
