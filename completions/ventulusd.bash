# Bash completions for ventulusd
_ventulusd() {
    local cur opts
    COMPREPLY=()
    cur="${COMP_WORDS[COMP_CWORD]}"
    opts="--config --profile --profile-file --allow-non-root --version -h --help"

    if [[ ${cur} == -* ]]; then
        COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
    fi
}
complete -F _ventulusd ventulusd
