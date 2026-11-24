# ciadpi / ByeDPI on Linux

Sidestep embeds a precompiled `ciadpi` binary for x86_64 Linux so that most users get a
zero-dependency install. This binary is automatically extracted to your data directory
on first run.

## Why the embedded binary may not work for you

| Situation | Effect |
|---|---|
| ARM64 / Asahi Linux / Raspberry Pi | The x86_64 binary will not execute — use a system package or build from source |
| AUR / Flatpak / Debian packaging | Distro packaging policy forbids vendored precompiled binaries |
| You prefer to audit what runs | Build from source and place the binary on your PATH |

## Using a system-installed ciadpi

If `ciadpi` is on your `PATH` (e.g. installed via your package manager), Sidestep finds
and uses it automatically — the embedded binary is never extracted. No configuration
needed.

```sh
# Arch Linux (AUR)
yay -S byedpi

# or build from source (any distro)
git clone https://github.com/hufrea/byedpi
cd byedpi && make
sudo install -m755 ciadpi /usr/local/bin/ciadpi
```

## For packagers (AUR / Flatpak / Debian)

Do **not** include `vendor/ciadpi` in your package. Instead:

1. Declare `byedpi` (or equivalent) as a runtime dependency.
2. Build Sidestep without the vendored binary by replacing `src/setup/linux.rs`'s
   `PAYLOAD` constant with an empty slice:
   ```rust
   pub const PAYLOAD: [(&str, &[u8]); 0] = [];
   ```
   The runtime path (`find_system_ciadpi()`) will then locate the system binary.
3. The build will still compile cleanly — `include_bytes!` on the ciadpi path only
   runs if `PAYLOAD` references it.

## Building ciadpi from source for non-x86_64

```sh
git clone https://github.com/hufrea/byedpi
cd byedpi
# For ARM64:
make CC=aarch64-linux-gnu-gcc   # or just `make` on native ARM hardware
sudo install -m755 ciadpi /usr/local/bin/ciadpi
```

After installation, `sidestep on` will pick it up without any further setup.
