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

/// The wrapper captures stdout, watches for the cd sentinel on the last line,
/// re-emits everything else, and cds. `resume` bypasses capture so the chosen
/// harness inherits the terminal; other pickers work because inquire renders
/// on stderr.
pub fn init_script(shell: Shell) -> String {
    let template = match shell {
        Shell::Zsh => format!("{POSIX_WRAPPER}{ZSH_COMPLETIONS}"),
        Shell::Bash => format!("{POSIX_WRAPPER}{BASH_COMPLETIONS}"),
        Shell::Fish => FISH_WRAPPER.to_string(),
    };
    template
        .replace("__BONSAI_WRAPPER_VERSION__", WRAPPER_VERSION)
        .replace("__BONSAI_WRAPPER_SHELL__", shell.name())
}

const POSIX_WRAPPER: &str = r#"bonsai() {
  local out code last arg skip
  skip=0
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
        if [[ "$arg" == "resume" ]]; then
          _BONSAI_WRAPPER_ACTIVE=1 _BONSAI_WRAPPER_VERSION='__BONSAI_WRAPPER_VERSION__' _BONSAI_WRAPPER_SHELL='__BONSAI_WRAPPER_SHELL__' command bonsai "$@"
          return
        fi
        break
        ;;
    esac
  done
  out="$(_BONSAI_WRAPPED=1 _BONSAI_WRAPPER_ACTIVE=1 _BONSAI_WRAPPER_VERSION='__BONSAI_WRAPPER_VERSION__' _BONSAI_WRAPPER_SHELL='__BONSAI_WRAPPER_SHELL__' command bonsai "$@")"
  code=$?
  last="${out##*$'\n'}"
  if [[ "$last" == $'__bonsai_cd\x1f'* ]]; then
    if [[ "$out" == *$'\n'* ]]; then
      printf '%s\n' "${out%$'\n'*}"
    fi
    builtin cd -- "${last#*$'\x1f'}" || return
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
                if test "$arg" = resume
                    _BONSAI_WRAPPER_ACTIVE=1 _BONSAI_WRAPPER_VERSION='__BONSAI_WRAPPER_VERSION__' _BONSAI_WRAPPER_SHELL='__BONSAI_WRAPPER_SHELL__' command bonsai $argv
                    return $status
                end
                break
        end
    end
    set -l sep (printf '\x1f')
    set -l out (_BONSAI_WRAPPED=1 _BONSAI_WRAPPER_ACTIVE=1 _BONSAI_WRAPPER_VERSION='__BONSAI_WRAPPER_VERSION__' _BONSAI_WRAPPER_SHELL='__BONSAI_WRAPPER_SHELL__' command bonsai $argv)
    set -l code $status
    if test (count $out) -gt 0; and string match -q -- "__bonsai_cd$sep*" $out[-1]
        if test (count $out) -gt 1
            printf '%s\n' $out[1..-2]
        end
        cd (string replace -- "__bonsai_cd$sep" '' $out[-1])
    else if test (count $out) -gt 0
        printf '%s\n' $out
    end
    return $code
end
command bonsai completions fish | source
"#;
