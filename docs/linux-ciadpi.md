# ciadpi / ByeDPI on Linux

In accordance with standard Linux packaging practices, Sidestep does **not** vendor precompiled ELF binaries in the git repository.

Instead, Sidestep locates `ciadpi` automatically through:
1. System installation (`/usr/bin/ciadpi`, `/usr/local/bin/ciadpi`, or your `PATH`)
2. User binary directory (`~/.local/bin/ciadpi` or `~/.local/share/sidestep/ciadpi`)
3. Automatic download of the official ByeDPI release matching your host architecture on setup

## Installing ciadpi via Package Manager

```sh
# Arch Linux (AUR)
yay -S byedpi

# Or build from source (any distribution)
git clone https://github.com/hufrea/byedpi
cd byedpi && make
sudo install -m755 ciadpi /usr/local/bin/ciadpi
```

## For Packagers (AUR / Flatpak / Debian)

Declare `byedpi` (or equivalent) as a runtime dependency. The runtime lookup (`find_system_ciadpi()`) locates the system binary automatically without any extra configuration.
