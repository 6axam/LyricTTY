# Contributing

Run formatting, Clippy with `-D warnings`, tests and a release build before opening a pull request. Keep providers and playback backends behind interfaces so the TUI remains testable without YouTube or a user account. The MPRIS tests create their own D-Bus daemon; do not point them at personal players. Install `mpv`, `dbus`, `python3`, `fontconfig` and a sans font to run every local integration test.

Add explicit fixtures for timing bugs, property-event ordering and hit-testing regressions. Do not synthesize word timings from line-only lyrics. Document external API failures as external failures, and never claim a live check solely because a mock passed. Keep cookies, tokens, private metadata, machine-specific paths and recordings of unrelated windows out of commits.

Demo artwork is original and reproducible with `cargo run --example generate_demo_cover`. Demo lyrics are authored fixtures, not transcribed song lyrics. Use those assets for public screenshots.
