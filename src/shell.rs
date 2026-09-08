/// Marker emitted as the final stdout line when the shell wrapper should cd.
/// The unit separator (0x1f) cannot appear in a path bonsai produces, so the
/// sentinel never collides with regular output.
pub const CD_SENTINEL: &str = "__bonsai_cd\u{1f}";

/// Set by the wrapper so the binary knows its stdout is being captured.
pub const WRAPPED_ENV: &str = "_BONSAI_WRAPPED";

/// Version and shell stamped into generated wrappers so a newer binary can
/// tell users to refresh shell integration that is still loaded in memory.
const WRAPPER_VERSION_ENV: &str = "_BONSAI_WRAPPER_VERSION";
const WRAPPER_SHELL_ENV: &str = "_BONSAI_WRAPPER_SHELL";
const WRAPPER_ACTIVE_ENV: &str = "_BONSAI_WRAPPER_ACTIVE";
const WRAPPER_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Shell {
    Zsh,
    Bash,
    Fish,
}

impl Shell {
    fn name(self) -> &'static str {
        match self {
            Self::Zsh => "zsh",
            Self::Bash => "bash",
            Self::Fish => "fish",
        }
    }

    fn refresh_command(self) -> &'static str {
        match self {
            Self::Zsh => "eval \"$(bonsai init zsh)\"",
            Self::Bash => "eval \"$(bonsai init bash)\"",
            Self::Fish => "bonsai init fish | source",
        }
    }
}

/// A loaded wrapper is part of the running shell and therefore survives a
/// bonsai upgrade. Warn before command dispatch when its generated version no
/// longer matches this binary. Direct binary calls carry neither integration
/// marker and intentionally remain silent.
pub fn warn_if_stale_integration() {
    let integration_active =
        std::env::var_os(WRAPPED_ENV).is_some() || std::env::var_os(WRAPPER_ACTIVE_ENV).is_some();
    if !integration_active || std::env::var(WRAPPER_VERSION_ENV).as_deref() == Ok(WRAPPER_VERSION) {
        return;
    }

    eprintln!("bonsai: warning: shell integration is out of sync with this bonsai binary");
    if let Some(shell) = wrapper_shell() {
        eprintln!("  run `{}` or restart your shell", shell.refresh_command());
    } else {
        eprintln!("  re-evaluate it for your shell, or restart your shell:");
        for shell in [Shell::Zsh, Shell::Bash, Shell::Fish] {
            eprintln!("    {}: {}", shell.name(), shell.refresh_command());
        }
    }
}

fn wrapper_shell() -> Option<Shell> {
    let value = std::env::var_os(WRAPPER_SHELL_ENV).or_else(|| std::env::var_os("SHELL"))?;
    let name = std::path::Path::new(&value)
        .file_name()
        .unwrap_or(&value)
        .to_string_lossy()
        .to_ascii_lowercase();
    match name.strip_suffix(".exe").unwrap_or(&name) {
        "zsh" => Some(Shell::Zsh),
        "bash" => Some(Shell::Bash),
        "fish" => Some(Shell::Fish),
        _ => None,
    }
}

/// Only directory-changing commands need capture; command substitutions and
/// ordinary output commands preserve the binary's stdout unchanged.
pub fn init_script(shell: Shell) -> String {
    let template = match shell {
        Shell::Zsh => format!("{POSIX_WRAPPER}{ZSH_COMPLETIONS}"),
        Shell::Bash => format!("{POSIX_WRAPPER}{BASH_COMPLETIONS}"),
        Shell::Fish => FISH_WRAPPER.to_string(),
    };
    template
        .replace("__BONSAI_WRAPPER_VERSION__", WRAPPER_VERSION)
        .replace("__BONSAI_WRAPPER_SHELL__", shell.name())
        .replace(
            "__BONSAI_SHELL_SETUP__",
            if shell == Shell::Zsh {
                "emulate -L zsh"
            } else {
                ""
            },
        )
}

const POSIX_WRAPPER: &str = r#"bonsai() {
  __BONSAI_SHELL_SETUP__
  local out code last arg skip capture
  skip=0
  capture=0
  for arg in "$@"; do
    if (( skip )); then
      skip=0
      continue
    fi
    case "$arg" in
      --root|--remote) skip=1 ;;
      --root=*|--remote=*) ;;
      -*) ;;
        *)
        case "$arg" in add|cd|remove|rm|clean|prune) capture=1 ;; esac
        break
        ;;
    esac
  done
  if [[ ! -t 1 ]] || (( ! capture || ${BASH_SUBSHELL:-0} > 0 || ${ZSH_SUBSHELL:-0} > 0 )); then
    _BONSAI_WRAPPER_ACTIVE=1 _BONSAI_WRAPPER_VERSION='__BONSAI_WRAPPER_VERSION__' _BONSAI_WRAPPER_SHELL='__BONSAI_WRAPPER_SHELL__' command bonsai "$@"
    return $?
  fi
  if out="$(_BONSAI_WRAPPED=1 _BONSAI_WRAPPER_ACTIVE=1 _BONSAI_WRAPPER_VERSION='__BONSAI_WRAPPER_VERSION__' _BONSAI_WRAPPER_SHELL='__BONSAI_WRAPPER_SHELL__' command bonsai "$@")"; then
    code=0
  else
    code=$?
  fi
  last="${out##*$'\n'}"
  if [[ "$last" == $'__bonsai_cd\x1f'* ]]; then
    if [[ "$out" == *$'\n'* ]]; then
      printf '%s\n' "${out%$'\n'*}"
    fi
    cd -- "${last#*$'\x1f'}" || return
  elif [[ -n "$out" ]]; then
    printf '%s\n' "$out"
  fi
  return $code
}
"#;

const ZSH_COMPLETIONS: &str = r#"if command -v compdef >/dev/null 2>&1; then
  eval "$(command bonsai completions zsh)"
fi
"#;

const BASH_COMPLETIONS: &str = r#"eval "$(command bonsai completions bash)"
"#;

const FISH_WRAPPER: &str = r#"function bonsai
    set -l skip 0
    set -l capture 0
    for arg in $argv
        if test $skip -eq 1
            set skip 0
            continue
        end
        switch $arg
            case --root --remote
                set skip 1
            case '--root=*' '--remote=*'
            case '-*'
            case '*'
                if contains -- "$arg" add cd remove rm clean prune
                    set capture 1
                end
                break
        end
    end
    if test $capture -eq 0; or status is-command-substitution; or not isatty stdout
        _BONSAI_WRAPPER_ACTIVE=1 _BONSAI_WRAPPER_VERSION='__BONSAI_WRAPPER_VERSION__' _BONSAI_WRAPPER_SHELL='__BONSAI_WRAPPER_SHELL__' command bonsai $argv
        return $status
    end
    set -l sep (printf '\x1f')
    set -l out (_BONSAI_WRAPPED=1 _BONSAI_WRAPPER_ACTIVE=1 _BONSAI_WRAPPER_VERSION='__BONSAI_WRAPPER_VERSION__' _BONSAI_WRAPPER_SHELL='__BONSAI_WRAPPER_SHELL__' command bonsai $argv)
    set -l code $status
    if test (count $out) -gt 0; and string match -q -- "__bonsai_cd$sep*" $out[-1]
        if test (count $out) -gt 1
            printf '%s\n' $out[1..-2]
        end
        cd (string replace -- "__bonsai_cd$sep" '' $out[-1])
        or return $status
    else if test (count $out) -gt 0
        printf '%s\n' $out
    end
    return $code
end
command bonsai completions fish | source
"#;

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        fs,
        io::Read,
        os::{fd::FromRawFd, unix::fs::PermissionsExt},
        process::{Command, Stdio},
    };

    fn run(shell: Shell, body: &str) -> std::process::Output {
        let temp = tempfile::Builder::new()
            .prefix("bonsai shell ")
            .tempdir()
            .unwrap();
        let executable = temp.path().join("bonsai");
        fs::write(&executable, "#!/bin/sh\ncase \"$1\" in\n completions) exit 0;;\n list) printf '%s' direct; exit 0;;\nesac\nif [ -n \"${_BONSAI_WRAPPED:-}\" ]; then printf '__bonsai_cd\\037%s\\n' \"$DESTINATION\"; else printf '%s\\n' \"$DESTINATION\"; fi\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        let mut master = -1;
        let mut slave = -1;
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            0
        );
        let mut master = unsafe { fs::File::from_raw_fd(master) };
        let slave = unsafe { fs::File::from_raw_fd(slave) };
        let reader = std::thread::spawn(move || {
            let mut stdout = Vec::new();
            let mut buffer = [0; 4096];
            loop {
                match master.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => stdout.extend_from_slice(&buffer[..count]),
                    Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
                    Err(error) => panic!("could not read shell PTY: {error}"),
                }
            }
            stdout
        });
        let mut output = Command::new(shell.name())
            .arg("-c")
            .arg(format!("{}\n{body}", init_script(shell)))
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    temp.path().display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .env("DESTINATION", temp.path())
            .stdout(Stdio::from(slave))
            .output()
            .unwrap();
        output.stdout = reader.join().unwrap();
        output
    }

    #[test]
    fn bash_substitution_preserves_path_and_direct_commands_preserve_bytes() {
        let output = run(
            Shell::Bash,
            "set -eu; captured=$(bonsai cd example); test \"$captured\" = \"$DESTINATION\"; bonsai list",
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"direct");
    }

    #[test]
    fn shell_cd_runs_directory_hooks() {
        let output = run(
            Shell::Bash,
            "cd() { printf hooked; builtin cd \"$@\"; }; bonsai cd example; test \"$PWD\" = \"$DESTINATION\"",
        );
        assert!(output.status.success());
        assert_eq!(output.stdout, b"hooked");
    }

    #[test]
    fn redirected_navigation_preserves_path_and_current_directory() {
        let output = run(
            Shell::Bash,
            "set -eu; original=$PWD; bonsai cd example >\"$DESTINATION/captured\"; test \"$PWD\" = \"$original\"; read -r captured <\"$DESTINATION/captured\"; test \"$captured\" = \"$DESTINATION\"",
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty());
    }

    #[test]
    fn bash_pipe_preserves_path_and_failed_cd_fails() {
        let output = run(
            Shell::Bash,
            "set -eu; bonsai cd example | { read -r captured; test \"$captured\" = \"$DESTINATION\"; }; DESTINATION=\"$DESTINATION/missing\"; export DESTINATION; if bonsai --root '/space here' cd example; then exit 23; fi",
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn zsh_isolates_options_and_preserves_substitution() {
        if Command::new("zsh").arg("--version").output().is_err() {
            assert!(
                std::env::var_os("REQUIRE_TEST_SHELLS").is_none(),
                "zsh required in this test environment"
            );
            return;
        }
        let output = run(
            Shell::Zsh,
            "setopt SH_WORD_SPLIT KSH_ARRAYS; set -eu; captured=$(bonsai --root '/space here' cd example); test \"$captured\" = \"$DESTINATION\"; bonsai cd example; test \"$PWD\" = \"$DESTINATION\"; [[ -o SH_WORD_SPLIT && -o KSH_ARRAYS ]]; bonsai list",
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"direct");
    }

    #[test]
    fn fish_preserves_substitution_and_failed_cd_status() {
        if Command::new("fish").arg("--version").output().is_err() {
            assert!(
                std::env::var_os("REQUIRE_TEST_SHELLS").is_none(),
                "fish required in this test environment"
            );
            return;
        }
        let output = run(
            Shell::Fish,
            "set -l captured (bonsai cd example); test \"$captured\" = \"$DESTINATION\"; or exit 21; bonsai cd example | string match -- \"$DESTINATION\" >/dev/null; or exit 24; bonsai cd example; test \"$PWD\" = \"$DESTINATION\"; or exit 22; set -gx DESTINATION \"$DESTINATION/missing\"; if bonsai cd example; exit 23; end; bonsai list",
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"direct");
    }
}
