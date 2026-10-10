# Fish completions for ventulus-tray
complete -c ventulus-tray -l socket -d 'Daemon socket (default: /run/ventulus/ventulus.sock)' -r -F
complete -c ventulus-tray -l version -d 'Print the tray version and exit'
complete -c ventulus-tray -s h -l help -d 'Print help and exit'
