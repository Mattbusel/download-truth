# Changelog

## 0.1.0 (2026-11-20)

First release. Why it exists: download counts mostly measure CI and publishing, not users, and nothing in the
registry UI says so. This release:

- splits a crates.io crate's or npm package's last 90 days of downloads into lockfile replays, pinned old lines
  with no published dependent, release-time fetches, flat automation and the human-rhythm rest, printing the
  assumption behind each slice;
- `readme` fails (exit 1) when a shields.io/badgen downloads badge in a README counts mostly machines, and can
  write a shields.io endpoint JSON with the real number;
- a composite GitHub Action that runs the README check;
- `batch` with a response cache and `--as-of` for reproducible corpus runs;
- the human weekend ratio default (k) is calibrated on the top 200 crates, top 200 npm packages and
  venture-backed flagships (see README, Calibration).
