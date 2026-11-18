# Lumo Desktop: unofficial arm64 Linux builds

> [!WARNING]
> **Unofficial.** Not made, signed or supported by Proton. Lumo, Proton and the Lumo logo are Proton AG's trademarks. Report app bugs to Proton only if they also happen in Proton's own builds.

[Lumo Desktop](https://github.com/ProtonLumo/lumo-desktop) is GPL-3.0, but Proton only publishes x86_64 Linux packages. This fork builds the same source, unchanged, for **arm64 (aarch64) Linux**, such as Asahi Linux on Apple Silicon Macs, and other ARM laptops and boards.

**Downloads:** [Releases](../../releases) (`.deb`, `.rpm`, `.AppImage`, and a `.tar.gz` of the file tree).

## How it works
- **`master`** mirrors [ProtonLumo/lumo-desktop](https://github.com/ProtonLumo/lumo-desktop) exactly. It's force-updated from upstream and never changed here.
- **`arm64-builds`** (this branch) holds only the build workflow, [`.github/workflows/release.yml`](.github/workflows/release.yml).
- Every 4 hours, the workflow checks Proton's `master`. When it points at a commit with no release here, it builds that commit on GitHub's arm64 runner and publishes a release. The release's tag points at that exact upstream commit, so you can see precisely what was built.
- Run it by hand: Actions → *Build arm64 release* → *Run workflow* (tick *force* to rebuild a commit that already has a release).

## What differs from Proton's builds
- **Nothing in the code.** The one build setting is `LUMO_URL=https://lumo.proton.me`. Without it the source defaults to Proton's internal development site, and the app only shows "Unacceptable TLS certificate".
- Upstream's `Cargo.lock` sometimes lags the app's own version number. The build updates only that one entry and fails if any dependency would change.
- **No auto-update:** Proton's updater has no arm64 Linux build to offer, so it does nothing. Install new releases instead.
- **No crash reports to Proton:** their reporting key isn't in the public source.
- Install as a package rather than running the bare binary: browser sign-in returns through `proton-lumo://` links, which only an installed desktop entry handles.

## License
GPL-3.0, the same as upstream ([LICENSE on master](../../blob/master/LICENSE)). Source for every release is the tagged upstream commit plus the workflow on this branch.
