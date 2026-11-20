# download-truth: your download badge counts machines

[![crates.io](https://img.shields.io/crates/v/download-truth.svg)](https://crates.io/crates/download-truth)
[![CI](https://github.com/Mattbusel/download-truth/actions/workflows/ci.yml/badge.svg)](https://github.com/Mattbusel/download-truth/actions/workflows/ci.yml)

Download counts mostly measure CI and publishing, not users. Every `npm ci` on a cold runner, every Docker build, every mirror that fetches a new release and every bot that opens an update PR adds one. None of them is a person choosing your package. download-truth splits a crates.io or npm package's downloads into what machines did and what is left, shows every assumption next to its number, and fails a README downloads badge when most of the count is machines.

Receipts, all from public registry data for the 90 days to 2026-11-19 (full tables in [docs/results.md](docs/results.md)):

- **Our own crate first.** [remoteid](https://crates.io/crates/remoteid) took 835 downloads in its first three days. 88.9% were 0.1.0, a line no published crate asks for: one project's CI pulling a pinned version on every run. The 0.2 line, current at the time, got 11.1%.
- **Venture-backed companies' flagship packages are noisier than the infrastructure under them.** Across 96 flagship packages (npm and crates.io) of venture-backed open-source companies, 58 are mostly noise. The median one of the 72 on npm has 27.1% of its downloads on versions a fresh `npm install` never picks, against 5.5% for the 200 most downloaded npm packages. Only 37.7% of their combined downloads survive as human-rhythm fresh installs.
- **The top 200 crates:** 130 of 200 are mostly noise. The median crate has 40.3% of its downloads on versions a fresh `cargo update` would never pick, and 43.6% of all their downloads survive.
- **The top 200 npm packages:** 38 of 200 are mostly noise; 66.1% of their combined downloads survive.
- Earlier, across 69 crates over 89 days, 80.1% of downloads landed on publish day or the day after ([tensorust.com/articles/crates-io-download-count](https://tensorust.com/articles/crates-io-download-count/), linked in [This Week in Rust](https://github.com/rust-lang/this-week-in-rust/pull/8887)).

A maintainer who publishes often and gets pinned in a few busy pipelines looks more popular than one whose code thousands of people chose. Investors, sponsors and security funds read that number as adoption. It isn't.

## What it measures

```
$ download-truth crate remoteid
remoteid (crates.io): MOSTLY NOISE
  badge number: 1,717 all-time downloads (the crates/d badge)
  window 2026-11-17 to 2026-11-19 (3 days): 835 downloads

     7.4%              62  Lockfile replays (CI, containers)
    88.9%             742  Pinned old line with no published dependent
     0.0%               0  Release-time fetches (mirrors, scanners, update bots)
     0.0%               0  Flat automation (same every day of the week)
     3.7%              31  Human-rhythm fresh resolves (the real part, upper bound)
  ...
  * One old version, 0.1.0, takes 89% of all downloads while the latest is 0.2.3, and 0 published package(s) ask for its line. That is the shape of one project's pinned CI, not of new users.
```

The downloads in the window are split, in this order, into:

| slice | what it is | how it is measured |
|---|---|---|
| Lockfile replays | Versions a fresh resolve would never pick | A fresh `cargo add`/`npm install` takes the newest version of a compatibility line (`1.x`, `0.3.x`). Downloads of an older version in that line can only come from a committed lockfile. A developer machine caches packages, so a repeat download of a pinned version is a cold CI runner or container. crates.io: per version per day, a version is fresh on the days it was newest. npm: per-version counts exist only for the last 7 days; that split is applied to the window. |
| Pinned old line | An old line no published package asks for | The line is not the latest, and no published dependent's requirement matches it. Applied only where every dependent was counted (crates.io reverse dependencies, up to 500 dependents); a zero from deps.dev counts as unknown. Whoever pulls that line never publishes: one app's or one company's CI. |
| Release-time fetches | Mirrors, scanners, Dependabot/Renovate | On a publish day and the day after, downloads above the median of the 7 quiet days before. Skipped when releases land on most days (no quiet baseline). |
| Flat automation | Traffic that does not keep a working week | Model: day = A + H on weekdays, A + k·H on weekends. k is the weekend share of purely human traffic (see Calibration). A, the flat part, is removed. |
| The real part | Human-rhythm fresh resolves | What is left. It still includes push-triggered CI that resolves without a lockfile, so it is an upper bound on people. |

`real users estimate` is the real part on an average weekday: at most that many fresh, human-driven resolves a working day. It is not a count of people; one person can resolve many times.

## Calibration

k is measured, not chosen: it is the 5th percentile of the weekend/weekday ratio, after the first three slices are removed, across the 496 packages in the corpus run on 2026-11-20 (top 200 crates, top 200 npm packages, venture-backed flagships): **k = 0.48**. The packages with the lowest weekend share still contain some automation, so the true human k is at or below this; using it removes less than the truth would. The estimate leans generous. Override with `--human-weekend-ratio`.

## Install

```
cargo install download-truth
```

or download a binary from [Releases](https://github.com/Mattbusel/download-truth/releases).

## Use

```
download-truth crate serde                 # text report
download-truth npm @prisma/client --json   # JSON report
download-truth crate my-crate --fail       # exit 1 when noise > 50%
download-truth readme README.md            # check every downloads badge in a README
download-truth readme --badge-json real.json   # shields.io endpoint JSON with the real number
download-truth batch --top-crates 200 --npm list.txt --out runs.jsonl --cache-dir cache
```

`readme` finds shields.io and badgen.net downloads badges (`crates/d`, `crates/dv`, `crates/dr`, `npm/dm`, `npm/dw`, `npm/dy`, `npm/dt`, `npm/d18m`), analyzes each package and exits 1 when any badge counts mostly machines:

```
$ download-truth readme README.md        # run on storybookjs/storybook's README, Nov 20 2026
README.md:19: dw badge for storybook claims 156,137,568 downloads; 85% of the last 90 days is noise (lockfile replays 84%, pinned old line 0%, release-time 1%, flat automation 0%). FAIL: the badge mostly counts machines.
```

## GitHub Action

```yaml
- uses: Mattbusel/download-truth@v0.1.0
  with:
    readme: README.md
    max-noise: "0.5"
    badge-json: real-downloads.json   # optional: serve it with shields.io/endpoint
```

The step fails when a downloads badge in the README is mostly machines. Put the real number next to it instead: `https://img.shields.io/endpoint?url=<raw url of real-downloads.json>`.

## What it cannot see

- Registries publish counts, not clients. Every slice is inferred from the shape of the counts; each one prints the assumption it rests on.
- A lockfile replay is a real project being built. It is not a new adopter, and a thousand reruns of one pipeline are one user.
- Flat traffic can be real use when the users are machines by design (agent sandboxes, scheduled jobs that are the product). Read the flat slice with that in mind.
- deps.dev lags new releases; a zero for a version released in the last 30 days is treated as unknown, not as zero.
- npm per-version counts cover only the last 7 days.
- PyPI is not covered yet.
- crates.io is queried at one request per second, per its crawler policy.

## License

MIT or Apache-2.0.
