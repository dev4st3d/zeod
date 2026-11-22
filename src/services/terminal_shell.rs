//! Default terminal shell preference and local shell discovery.
//!
//! Discovery reads only local executable paths without spawning subprocesses,
//! so opening Settings stays fast and deterministic.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use gpui::{App, Global};
use serde::{Deserialize, Serialize};

const MAX_PROGRAM_LEN: usize = 512;

static DEFAULT_SHELL: LazyLock<TerminalShell> = LazyLock::new(TerminalShell::default);

/// Chosen shell executable for newly created terminals.
/// `None` uses the operating system default (`ComSpec` on Windows, `SHELL` elsewhere).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TerminalShell {
    pub(crate) program: Option<PathBuf>,
}

impl Global for TerminalShell {}

impl TerminalShell {
    /// The active preference, or the system default before preferences have loaded.
    pub(crate) fn global(cx: &App) -> &Self {
        cx.try_global::<Self>().unwrap_or(&DEFAULT_SHELL)
    }

    #[cfg(test)]
    pub(crate) fn with_program(program: Option<PathBuf>) -> Self {
        Self {
            program: program.map(|path| super::paths::without_windows_verbatim_prefix(&path)),
        }
    }

    pub(crate) fn is_default(&self) -> bool {
        self.program.is_none()
    }

    pub(crate) fn same(&self, other: &Self) -> bool {
        match (&self.program, &other.program) {
            (None, None) => true,
            (Some(left), Some(right)) => same_executable(left, right),
            _ => false,
        }
    }

    pub(crate) fn label(&self) -> String {
        match &self.program {
            None => "System default".to_owned(),
            Some(path) => shell_label(path),
        }
    }

    pub(crate) fn prompt_preview(&self) -> &'static str {
        let name = self
            .program
            .as_deref()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .map(str::to_ascii_lowercase);
        match name.as_deref() {
            Some("pwsh" | "pwsh.exe" | "powershell" | "powershell.exe") => "PS C:\\project> ",
            Some("bash" | "bash.exe" | "zsh" | "fish" | "sh") => "~/project $ ",
            Some("cmd" | "cmd.exe") => "C:\\project> ",
            _ => {
                #[cfg(windows)]
                {
                    "C:\\project> "
                }
                #[cfg(not(windows))]
                {
                    "~/project $ "
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ShellChoice {
    pub(crate) shell: TerminalShell,
    pub(crate) label: String,
    pub(crate) detail: String,
}

impl ShellChoice {
    pub(crate) fn system_default() -> Self {
        Self {
            shell: TerminalShell::default(),
            label: "System default".to_owned(),
            detail: system_default_detail(),
        }
    }
}

pub(crate) fn shell_args(program: &Path) -> &'static [&'static str] {
    let name = program
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_ascii_lowercase);
    match name.as_deref() {
        Some("pwsh" | "pwsh.exe" | "powershell" | "powershell.exe") => &["-NoLogo"],
        Some("bash" | "bash.exe") => &["--login", "-i"],
        _ => &[],
    }
}

pub(crate) fn shell_label(program: &Path) -> String {
    let name = program
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("Shell");
    match name.to_ascii_lowercase().as_str() {
        "pwsh" | "pwsh.exe" => "PowerShell 7".to_owned(),
        "powershell" | "powershell.exe" => "Windows PowerShell".to_owned(),
        "cmd" | "cmd.exe" => "Command Prompt".to_owned(),
        "bash" | "bash.exe" => {
            if is_git_bash(program) {
                "Git Bash".to_owned()
            } else {
                "Bash".to_owned()
            }
        }
        "zsh" => "Zsh".to_owned(),
        "fish" => "Fish".to_owned(),
        "sh" => "sh".to_owned(),
        _ => name.to_owned(),
    }
}

pub(crate) fn is_available(shell: &TerminalShell) -> bool {
    match &shell.program {
        None => true,
        Some(path) => path.is_file(),
    }
}

/// Discover installed local shells without running any external processes.
pub(crate) fn discover_shells() -> Vec<ShellChoice> {
    let mut choices = vec![ShellChoice::system_default()];
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut push_path = |path: PathBuf| {
        if !path.is_file() {
            return;
        }
        let clean = super::paths::without_windows_verbatim_prefix(&path);
        if seen
            .iter()
            .any(|existing| same_executable(existing, &clean))
        {
            return;
        }
        let label = shell_label(&clean);
        let detail = clean
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("")
            .to_owned();
        seen.push(clean.clone());
        choices.push(ShellChoice {
            shell: TerminalShell {
                program: Some(clean),
            },
            label,
            detail,
        });
    };

    #[cfg(windows)]
    {
        for path in windows_shell_candidates() {
            push_path(path);
        }
    }

    #[cfg(not(windows))]
    {
        for path in unix_shell_candidates() {
            push_path(path);
        }
    }

    choices
}

#[cfg(windows)]
fn windows_shell_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let path_dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|value| std::env::split_paths(&value).collect())
        .unwrap_or_default();

    // PowerShell 7 (`pwsh.exe`)
    for dir in &path_dirs {
        candidates.push(dir.join("pwsh.exe"));
    }
    for env_key in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(base) = std::env::var_os(env_key) {
            candidates.push(PathBuf::from(base).join(r"PowerShell\7\pwsh.exe"));
        }
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        candidates.push(PathBuf::from(local).join(r"Microsoft\WindowsApps\pwsh.exe"));
    }

    // Windows PowerShell (`powershell.exe`)
    if let Some(root) = std::env::var_os("SystemRoot") {
        candidates
            .push(PathBuf::from(root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe"));
    }
    for dir in &path_dirs {
        candidates.push(dir.join("powershell.exe"));
    }

    // Git Bash (`bash.exe` under a Git installation)
    for env_key in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(base) = std::env::var_os(env_key) {
            candidates.push(PathBuf::from(&base).join(r"Git\bin\bash.exe"));
            candidates.push(PathBuf::from(base).join(r"Git\usr\bin\bash.exe"));
        }
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        candidates.push(PathBuf::from(&local).join(r"Programs\Git\bin\bash.exe"));
        candidates.push(PathBuf::from(local).join(r"Programs\Git\usr\bin\bash.exe"));
    }
    for dir in &path_dirs {
        if dir.join("git.exe").is_file()
            && let Some(parent) = dir.parent()
        {
            candidates.push(parent.join(r"bin\bash.exe"));
            candidates.push(parent.join(r"usr\bin\bash.exe"));
        }
    }

    // Command Prompt (`cmd.exe`)
    if let Some(comspec) = std::env::var_os("ComSpec") {
        candidates.push(PathBuf::from(comspec));
    }
    if let Some(root) = std::env::var_os("SystemRoot") {
        candidates.push(PathBuf::from(root).join(r"System32\cmd.exe"));
    }

    candidates
}

#[cfg(not(windows))]
fn unix_shell_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(shell) = std::env::var_os("SHELL") {
        candidates.push(PathBuf::from(shell));
    }
    for path in [
        "/bin/zsh",
        "/usr/bin/zsh",
        "/bin/bash",
        "/usr/bin/bash",
        "/usr/bin/fish",
        "/usr/local/bin/fish",
        "/bin/sh",
    ] {
        candidates.push(PathBuf::from(path));
    }
    candidates
}

fn is_git_bash(program: &Path) -> bool {
    program
        .to_string_lossy()
        .to_ascii_lowercase()
        .contains("git")
}

fn system_default_detail() -> String {
    #[cfg(windows)]
    let raw = std::env::var_os("ComSpec")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("cmd.exe"));

    #[cfg(not(windows))]
    let raw = std::env::var_os("SHELL")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/bin/sh"));

    raw.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("shell")
        .to_owned()
}

fn same_executable(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        super::paths::without_windows_verbatim_prefix(left)
            .as_os_str()
            .as_encoded_bytes()
            .eq_ignore_ascii_case(
                super::paths::without_windows_verbatim_prefix(right)
                    .as_os_str()
                    .as_encoded_bytes(),
            )
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

#[derive(Deserialize, Serialize)]
struct StoredShell {
    version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    program: Option<String>,
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

pub(crate) fn load(path: &Path) -> io::Result<TerminalShell> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(TerminalShell::default());
        }
        Err(error) => return Err(error),
    };
    let stored: StoredShell = serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if stored.version != 1 {
        return Err(invalid("Unsupported terminal shell version"));
    }
    let program = match stored.program {
        None => None,
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty()
                || trimmed.chars().count() > MAX_PROGRAM_LEN
                || trimmed.chars().any(char::is_control)
            {
                return Err(invalid("Invalid terminal shell path"));
            }
            Some(super::paths::without_windows_verbatim_prefix(Path::new(
                trimmed,
            )))
        }
    };
    Ok(TerminalShell { program })
}

pub(crate) fn save(path: &Path, shell: &TerminalShell) -> io::Result<()> {
    let stored = StoredShell {
        version: 1,
        program: shell
            .program
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned()),
    };
    let mut bytes = serde_json::to_vec_pretty(&stored)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    bytes.push(b'\n');
    super::atomic_file::write(path, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST: AtomicU64 = AtomicU64::new(1);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "zeod-terminal-shell-{}-{}",
                std::process::id(),
                NEXT_TEST.fetch_add(1, Ordering::Relaxed),
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn preferences_roundtrip_and_missing_file_means_system_default() {
        let root = TestDirectory::new();
        let path = root.0.join("terminal-shell.json");
        assert_eq!(load(&path).unwrap(), TerminalShell::default());
        assert!(load(&path).unwrap().is_default());

        let pwsh = TerminalShell::with_program(Some(PathBuf::from(
            r"C:\Program Files\PowerShell\7\pwsh.exe",
        )));
        save(&path, &pwsh).unwrap();
        assert_eq!(load(&path).unwrap(), pwsh);
        assert!(!pwsh.is_default());

        save(&path, &TerminalShell::default()).unwrap();
        assert_eq!(load(&path).unwrap(), TerminalShell::default());
        assert_eq!(fs::read_dir(&root.0).unwrap().count(), 1);
    }

    #[test]
    fn corrupt_empty_and_future_preferences_are_preserved() {
        let root = TestDirectory::new();
        let path = root.0.join("terminal-shell.json");
        for bytes in [
            b"invalid json".as_slice(),
            br#"{"version":2,"program":"cmd.exe"}"#.as_slice(),
            br#"{"version":1,"program":""}"#.as_slice(),
            br#"{"version":1,"program":"   "}"#.as_slice(),
            br#"{"version":1,"program":"pwsh\u0007.exe"}"#.as_slice(),
        ] {
            fs::write(&path, bytes).unwrap();
            assert_eq!(load(&path).unwrap_err().kind(), io::ErrorKind::InvalidData);
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
    }

    #[test]
    fn shell_labels_args_and_discovery_are_deterministic() {
        assert_eq!(
            shell_label(Path::new(r"C:\Program Files\PowerShell\7\pwsh.exe")),
            "PowerShell 7"
        );
        assert_eq!(
            shell_label(Path::new(
                r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"
            )),
            "Windows PowerShell"
        );
        assert_eq!(
            shell_label(Path::new(r"C:\Windows\System32\cmd.exe")),
            "Command Prompt"
        );
        assert_eq!(
            shell_label(Path::new(r"C:\Program Files\Git\bin\bash.exe")),
            "Git Bash"
        );
        assert_eq!(shell_args(Path::new("pwsh.exe")), &["-NoLogo"]);
        assert_eq!(shell_args(Path::new("bash.exe")), &["--login", "-i"]);
        assert!(shell_args(Path::new("cmd.exe")).is_empty());

        let discovered = discover_shells();
        assert!(!discovered.is_empty());
        assert!(discovered[0].shell.is_default());
    }
}
