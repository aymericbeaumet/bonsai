/// Marker emitted as the final stdout line when the shell wrapper should cd.
/// The unit separator (0x1f) cannot appear in a path bonsai produces, so the
/// sentinel never collides with regular output.
pub const CD_SENTINEL: &str = "__bonsai_cd\u{1f}";

/// Set by the wrapper so the binary knows its stdout is being captured.
pub const WRAPPED_ENV: &str = "_BONSAI_WRAPPED";

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Shell {
    Zsh,
    Bash,
    Fish,
}

/// The wrapper captures stdout, watches for the cd sentinel on the last line,
/// re-emits everything else, and cds. Pickers still work during capture
/// because inquire renders on stderr.
pub fn init_script(shell: Shell) -> String {
    match shell {
        Shell::Zsh => format!("{POSIX_WRAPPER}{ZSH_COMPLETIONS}"),
        Shell::Bash => format!("{POSIX_WRAPPER}{BASH_COMPLETIONS}"),
        Shell::Fish => FISH_WRAPPER.to_string(),
    }
}

const POSIX_WRAPPER: &str = r#"bonsai() {
  local out code last
  out="$(_BONSAI_WRAPPED=1 command bonsai "$@")"
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
    set -l sep (printf '\x1f')
    set -l out (_BONSAI_WRAPPED=1 command bonsai $argv)
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
