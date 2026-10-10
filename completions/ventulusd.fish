# Fish completions for ventulusd
complete -c ventulusd -l config -d 'Path to daemon.toml' -r -F
complete -c ventulusd -l profile -d 'Load a named profile from search paths' -r -x
complete -c ventulusd -l profile-file -d 'Load a profile from an absolute file path' -r -F
complete -c ventulusd -l allow-non-root -d 'Skip root privilege check (dev only)'
complete -c ventulusd -l version -d 'Print the daemon version and exit'
complete -c ventulusd -s h -l help -d 'Print help and exit'
