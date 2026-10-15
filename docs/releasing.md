# Releasing

Releases are built by `.github/workflows/release.yml` when a `v*` tag is pushed.

1. Set `version` in `Cargo.toml` to the new version, run `cargo check` to update `Cargo.lock`, and merge that change into `main`.
2. Tag the merge commit as `v<version>` and push the tag:

   ```sh
   git tag -s v0.1.0 -m "toktop 0.1.0"
   git push origin v0.1.0
   ```

3. The workflow fails unless the tag matches the `Cargo.toml` version. It runs the tests, builds a release binary in the Arch Linux container used by CI, and creates a draft GitHub release with generated notes and these assets:
   - `toktop-<version>-x86_64-linux.tar.gz`: the binary, `LICENSE`, `README.md`, the desktop entries and the icons under `data/`
   - `toktop-<version>-x86_64-linux.tar.gz.sha256`: its SHA-256 checksum
4. Edit the draft's notes and publish it.

The binary links against the system GTK 4, libadwaita, gtk4-layer-shell, SQLite and OpenSSL, so it runs only where those libraries are at least as new as on Arch Linux at build time.
