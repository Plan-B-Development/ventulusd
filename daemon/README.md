# Developing ventulusd

This page is for building, testing and running your own build of the daemon. To
install and run the released package, see the [top-level README](../README.md)
and the [User Guide](../docs/USER_GUIDE.md).

## Repository layout

```text
.
├── Cargo.toml        # workspace manifest
├── daemon/           # the ventulusd crate: the service
├── tray/             # the ventulus-tray crate: a KDE tray client that talks to the
│                     #   daemon only over the API; the daemon does not depend on it
├── man/              # scdoc sources for both man pages
├── completions/      # shell completions
├── packaging/        # PKGBUILD, systemd unit, restore script, sleep hook,
│                     #   Super-I/O guard, tray autostart entry, example
│                     #   config and drop-ins
├── docs/             # USER_GUIDE.md and ADRs/
└── CHANGELOG.md
```

## Build

```bash
cargo build --release
```

Run it from the repository root. The workspace has two binaries, and both land in
`target/release/`: `ventulusd` and `ventulus-tray`.

## Checks

The checks are the ones CI runs: the `cargo` steps in `.github/workflows/ci.yml`.
They are not repeated here, because a second copy of a command list drifts. CI
also runs `cargo deny` against `deny.toml`, which allows no LGPL dependency and
accepts serialport's MPL-2.0 (see `NOTICE.md`). `cargo audit` runs by hand at
release time.

## Build the package yourself

```bash
git clone https://github.com/Plan-B-Development/ventulusd.git
cd ventulusd/packaging
makepkg -si
```

`makepkg` builds the tagged release from its source tarball, not your checkout.
The `PKGBUILD`'s `sha256sums` is `SKIP` rather than a pinned hash, so no
`updpkgsums` step is needed. It cannot be a real hash: the tarball GitHub
generates for a tag contains that `PKGBUILD`, so writing a sum into it changes
the archive the sum pins. `makepkg` therefore trusts the HTTPS fetch from this
repository's own tag. For an input that is pinned and verifiable, use the
package attached to the release and check its Sigstore attestation
(`gh attestation verify <package> --repo Plan-B-Development/ventulusd`).

## Running your own build

Only one daemon runs at a time. A second `ventulusd` takes an `flock` on
`{state_dir}/daemon.lock` before it writes anything and never removes a socket
another daemon is serving on, so `cargo run` or a bare `sudo ventulusd` beside
the service exits with *another ventulusd is already running* (status 75). The
reverse is guarded too: a service started while your daemon runs is refused the
same way, leaves your daemon's socket and fans alone, and is retried by systemd
about once a minute until yours exits. Stop the service first all the same.

The supported way to run your own build is to install the package once, then
swap in your binary, so the unit, the hand-back script, the sleep hook, the
Super-I/O guard and `modules-load.d` stay in place:

```bash
sudo systemctl stop ventulusd
cargo build --release
sudo install -m755 target/release/ventulusd /usr/bin/ventulusd
sudo systemctl start ventulusd
journalctl -u ventulusd -f
```

pacman overwrites `/usr/bin/ventulusd` at the next upgrade, and
`pacman -Qkk ventulusd` reports it as modified until then. To go back to the
released binary, reinstall the package: `sudo pacman -S ventulusd`.

`--allow-non-root` skips the root check, for development only. File and socket
permission checks still apply.

## Reference

- Options, environment variables, files and signals: `man ventulusd`
  ([source](../man/ventulusd.1.scd)).
- Configuration keys: [`packaging/daemon.toml.example`](../packaging/daemon.toml.example)
  and the [User Guide § Configuration](../docs/USER_GUIDE.md#configuration).
- Which profile the daemon starts with: [User Guide § Loading a profile](../docs/USER_GUIDE.md#loading-a-profile).
- The HTTP API: the [API contract](https://github.com/Plan-B-Development/ventulus/blob/main/docs/08_API_Integration_Contract.md),
  which lives in the GUI repository.
