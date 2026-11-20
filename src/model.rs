//! The analysis. Pure functions over downloaded data, no network, so every
//! step can be tested and every assumption is printed next to its number.

use chrono::{Datelike, NaiveDate, Weekday};
use semver::Version;
use serde::Serialize;
use std::collections::BTreeMap;

/// Which registry the numbers came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Ecosystem {
    Crates,
    Npm,
}

/// One published version.
#[derive(Debug, Clone)]
pub struct VersionInfo {
    pub num: String,
    pub published: NaiveDate,
    pub yanked: bool,
}

/// Per-version download data, in whatever shape the registry gives it.
#[derive(Debug, Clone)]
pub enum VersionDownloads {
    /// crates.io: a daily series per version (aligned with `PackageData::days`)
    /// plus the daily remainder for versions that were not fetched one by one,
    /// split into versions that were never newest of their line in the window
    /// (`other_stale`) and a bounded part that might have been (`other_unknown`).
    Daily {
        series: BTreeMap<String, Vec<u64>>,
        other_stale: Vec<u64>,
        other_unknown: Vec<u64>,
    },
    /// npm: per-version counts over the last `period_days` days only.
    Period {
        counts: BTreeMap<String, u64>,
        period_days: i64,
    },
}

/// Everything the analysis needs about one package.
#[derive(Debug, Clone)]
pub struct PackageData {
    pub ecosystem: Ecosystem,
    pub name: String,
    /// Complete UTC days in the window, oldest first, no gaps.
    pub days: Vec<NaiveDate>,
    /// Downloads per day, aligned with `days`.
    pub totals: Vec<u64>,
    pub versions: Vec<VersionInfo>,
    pub by_version: VersionDownloads,
    /// Published dependents that require each compatibility line
    /// (`None` = unknown). Keyed by `line_key`.
    pub line_dependents: BTreeMap<String, Option<u64>>,
    pub dependents_total: Option<u64>,
    /// Largest dependents by their own downloads: (name, downloads, kind).
    pub top_dependents: Vec<(String, u64, String)>,
    /// The number a downloads badge would show, and what it counts.
    pub headline: Option<(u64, String)>,
}

/// Knobs. Every one of them is printed in the report.
#[derive(Debug, Clone, Serialize)]
pub struct Params {
    /// Weekend downloads per weekday download for purely human-driven
    /// traffic. Measured, see README "Calibration".
    pub human_weekend_ratio: f64,
    /// Above this noise share the verdict is MOSTLY NOISE and a badge fails.
    pub max_noise: f64,
    /// Days counted as release-time (publish day + this many after).
    pub release_days_after: i64,
}

impl Default for Params {
    fn default() -> Self {
        Params {
            human_weekend_ratio: DEFAULT_HUMAN_WEEKEND_RATIO,
            max_noise: 0.5,
            release_days_after: 1,
        }
    }
}

/// Measured on 2026-11-20 across the top 200 crates and top 200 npm
/// packages: the 5th percentile of weekend/weekday ratios after lockfile,
/// pinned-line and release-time downloads are removed. See README.
pub const DEFAULT_HUMAN_WEEKEND_RATIO: f64 = 0.48;

/// One slice of the downloads.
#[derive(Debug, Clone, Serialize)]
pub struct Component {
    pub key: &'static str,
    pub label: &'static str,
    pub downloads: f64,
    pub share: f64,
    pub assumption: String,
}

/// One row of the version table.
#[derive(Debug, Clone, Serialize)]
pub struct VersionRow {
    pub version: String,
    pub line: String,
    pub downloads: f64,
    pub share: f64,
    pub class: &'static str,
    pub line_dependents: Option<u64>,
}

/// The weekly rhythm.
#[derive(Debug, Clone, Serialize)]
pub struct Rhythm {
    pub weekday_mean: f64,
    pub weekend_mean: f64,
    pub weekend_ratio: Option<f64>,
    pub flat_per_day: f64,
    pub human_per_weekday: f64,
    pub enough_data: bool,
}

/// The full result for one package.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub ecosystem: Ecosystem,
    pub name: String,
    pub window_start: String,
    pub window_end: String,
    pub window_days: usize,
    pub headline: Option<u64>,
    pub headline_counts: Option<String>,
    pub window_downloads: u64,
    pub components: Vec<Component>,
    pub real_downloads: f64,
    pub real_share: f64,
    pub noise_share: f64,
    /// Human-rhythm fresh downloads on an average weekday: an upper bound on
    /// people resolving this package fresh each working day.
    pub real_users_per_weekday: f64,
    pub verdict: &'static str,
    pub top_version: Option<VersionRow>,
    pub latest_version: Option<String>,
    pub versions: Vec<VersionRow>,
    pub rhythm: Rhythm,
    pub releases_in_window: usize,
    pub dependents_total: Option<u64>,
    pub top_dependents: Vec<(String, u64, String)>,
    pub findings: Vec<String>,
    pub params: Params,
}

/// Cargo/npm caret compatibility line: `1.x`, `0.3.x`, `0.0.7`.
pub fn line_key(v: &Version) -> String {
    if v.major > 0 {
        format!("{}.x", v.major)
    } else if v.minor > 0 {
        format!("0.{}.x", v.minor)
    } else {
        format!("0.0.{}", v.patch)
    }
}

struct Parsed {
    num: String,
    v: Version,
    published: NaiveDate,
    yanked: bool,
}

fn parse_versions(vs: &[VersionInfo]) -> Vec<Parsed> {
    let mut out: Vec<Parsed> = vs
        .iter()
        .filter_map(|x| {
            Version::parse(&x.num).ok().map(|v| Parsed {
                num: x.num.clone(),
                v,
                published: x.published,
                yanked: x.yanked,
            })
        })
        .collect();
    out.sort_by(|a, b| a.v.cmp(&b.v));
    out
}

/// Newest stable, non-yanked version of each line published on or before `day`.
fn newest_by_line(vs: &[Parsed], day: NaiveDate) -> BTreeMap<String, Version> {
    let mut m: BTreeMap<String, Version> = BTreeMap::new();
    for p in vs {
        if p.yanked || !p.v.pre.is_empty() || p.published > day {
            continue;
        }
        let k = line_key(&p.v);
        match m.get(&k) {
            Some(cur) if *cur >= p.v => {}
            _ => {
                m.insert(k, p.v.clone());
            }
        }
    }
    m
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Fresh,
    Stale,
    Orphan,
    Pre,
}

impl Class {
    fn name(self) -> &'static str {
        match self {
            Class::Fresh => "newest in its line",
            Class::Stale => "lockfile replay (a fresh resolve would not pick it)",
            Class::Orphan => "pinned old line, no published dependent asks for it",
            Class::Pre => "prerelease",
        }
    }
}

fn classify(
    p: &Parsed,
    newest: &BTreeMap<String, Version>,
    latest_line: Option<&str>,
    line_dependents: &BTreeMap<String, Option<u64>>,
) -> Class {
    if !p.v.pre.is_empty() {
        return Class::Pre;
    }
    let k = line_key(&p.v);
    let is_newest = newest.get(&k).map(|n| *n == p.v).unwrap_or(true);
    if !is_newest {
        return Class::Stale;
    }
    if Some(k.as_str()) != latest_line && line_dependents.get(&k).copied().flatten() == Some(0) {
        return Class::Orphan;
    }
    Class::Fresh
}

fn median(xs: &mut [f64]) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = xs.len();
    if n % 2 == 1 {
        xs[n / 2]
    } else {
        (xs[n / 2 - 1] + xs[n / 2]) / 2.0
    }
}

fn is_weekend(d: NaiveDate) -> bool {
    matches!(d.weekday(), Weekday::Sat | Weekday::Sun)
}

/// Remove release-time excess from a daily series. Returns (cleaned, excess).
pub fn remove_release_bursts(
    days: &[NaiveDate],
    series: &[f64],
    release_dates: &[NaiveDate],
    days_after: i64,
) -> (Vec<f64>, f64) {
    let n = days.len();
    let mut burst = vec![false; n];
    for r in release_dates {
        for (i, d) in days.iter().enumerate() {
            let off = (*d - *r).num_days();
            if (0..=days_after).contains(&off) {
                burst[i] = true;
            }
        }
    }
    let quiet: Vec<f64> = (0..n).filter(|&i| !burst[i]).map(|i| series[i]).collect();
    let mut cleaned = series.to_vec();
    let mut excess = 0.0;
    // With releases on most days there is no quiet baseline to compare
    // against; removing anything would be a guess, so nothing is removed.
    if quiet.len() * 5 < n * 3 {
        return (cleaned, 0.0);
    }
    for i in 0..n {
        if !burst[i] {
            continue;
        }
        // Baseline: median of the 7 nearest quiet days before this one,
        // falling back to the quiet days after it, then to all quiet days.
        let mut before: Vec<f64> = (0..i)
            .rev()
            .filter(|&j| !burst[j])
            .take(7)
            .map(|j| series[j])
            .collect();
        if before.len() < 3 {
            before.extend(
                (i + 1..n)
                    .filter(|&j| !burst[j])
                    .take(7 - before.len())
                    .map(|j| series[j]),
            );
        }
        let base = if before.len() >= 3 {
            median(&mut before)
        } else {
            median(&mut quiet.clone())
        };
        if series[i] > base {
            excess += series[i] - base;
            cleaned[i] = base;
        }
    }
    (cleaned, excess)
}

/// Split a daily series into flat automation and a human weekly rhythm.
/// Model: day = A + H on weekdays, A + k*H on weekends, k = human weekend ratio.
pub fn rhythm(days: &[NaiveDate], series: &[f64], k: f64) -> Rhythm {
    let (mut wd, mut nwd, mut we, mut nwe) = (0.0, 0usize, 0.0, 0usize);
    for (d, x) in days.iter().zip(series) {
        if is_weekend(*d) {
            we += x;
            nwe += 1;
        } else {
            wd += x;
            nwd += 1;
        }
    }
    let total: f64 = series.iter().sum();
    let enough = nwd >= 10 && nwe >= 4 && total >= 140.0;
    let wd_mean = if nwd > 0 { wd / nwd as f64 } else { 0.0 };
    let we_mean = if nwe > 0 { we / nwe as f64 } else { 0.0 };
    let ratio = if wd_mean > 0.0 {
        Some(we_mean / wd_mean)
    } else {
        None
    };
    if !enough || wd_mean <= 0.0 {
        return Rhythm {
            weekday_mean: wd_mean,
            weekend_mean: we_mean,
            weekend_ratio: ratio,
            flat_per_day: 0.0,
            human_per_weekday: wd_mean,
            enough_data: false,
        };
    }
    let a = ((we_mean - k * wd_mean) / (1.0 - k)).clamp(0.0, wd_mean.min(we_mean));
    Rhythm {
        weekday_mean: wd_mean,
        weekend_mean: we_mean,
        weekend_ratio: ratio,
        flat_per_day: a,
        human_per_weekday: wd_mean - a,
        enough_data: true,
    }
}

fn pct(x: f64) -> String {
    format!("{:.0}%", x * 100.0)
}

/// Run the whole analysis.
pub fn analyze(data: &PackageData, params: &Params) -> Report {
    // Days before the first release carry no information about users.
    let first_pub = data.versions.iter().map(|v| v.published).min();
    if let Some(fp) = first_pub {
        if let Some(cut) = data.days.iter().position(|d| *d >= fp) {
            if cut > 0 {
                let mut trimmed = data.clone();
                trimmed.days = data.days[cut..].to_vec();
                trimmed.totals = data.totals[cut..].to_vec();
                if let VersionDownloads::Daily {
                    series,
                    other_stale,
                    other_unknown,
                } = &mut trimmed.by_version
                {
                    for s in series.values_mut() {
                        *s = s[cut.min(s.len())..].to_vec();
                    }
                    *other_stale = other_stale[cut.min(other_stale.len())..].to_vec();
                    *other_unknown = other_unknown[cut.min(other_unknown.len())..].to_vec();
                }
                return analyze_window(&trimmed, params);
            }
        }
    }
    analyze_window(data, params)
}

fn analyze_window(data: &PackageData, params: &Params) -> Report {
    let n = data.days.len();
    let total: u64 = data.totals.iter().sum();
    let totf = total.max(1) as f64;
    let parsed = parse_versions(&data.versions);
    let end = *data.days.last().unwrap_or(&NaiveDate::MIN);
    let newest_now = newest_by_line(&parsed, end);
    let latest = newest_now.values().max().cloned();
    let latest_line = latest.as_ref().map(line_key);
    let by_num: BTreeMap<&str, &Parsed> = parsed.iter().map(|p| (p.num.as_str(), p)).collect();

    // Per-day split of the series into fresh / stale / orphan.
    let mut fresh = vec![0.0f64; n];
    let mut stale_total = 0.0;
    let mut orphan_total = 0.0;
    let mut vtotals: BTreeMap<String, (f64, Class)> = BTreeMap::new();
    let mut unknown_other = 0.0;
    let stale_assumption = match &data.by_version {
        VersionDownloads::Daily {
            series,
            other_stale,
            other_unknown,
        } => {
            for i in 0..n {
                let day = data.days[i];
                let newest = newest_by_line(&parsed, day);
                let mut day_fresh = 0.0;
                for (num, s) in series {
                    let x = *s.get(i).unwrap_or(&0) as f64;
                    if x == 0.0 {
                        continue;
                    }
                    let class = match by_num.get(num.as_str()) {
                        Some(p) => {
                            classify(p, &newest, latest_line.as_deref(), &data.line_dependents)
                        }
                        None => Class::Fresh,
                    };
                    let e = vtotals.entry(num.clone()).or_insert((0.0, class));
                    e.0 += x;
                    e.1 = class;
                    match class {
                        Class::Stale => stale_total += x,
                        Class::Orphan => orphan_total += x,
                        _ => day_fresh += x,
                    }
                }
                stale_total += *other_stale.get(i).unwrap_or(&0) as f64;
                let u = *other_unknown.get(i).unwrap_or(&0) as f64;
                unknown_other += u;
                day_fresh += u;
                fresh[i] = day_fresh;
            }
            // Per-version sums can differ from the daily totals by a few;
            // scale fresh so the slices add up to the headline total.
            let parts: f64 = fresh.iter().sum::<f64>() + stale_total + orphan_total;
            if parts > 0.0 {
                let f = totf / parts;
                for x in fresh.iter_mut() {
                    *x *= f;
                }
                stale_total *= f;
                orphan_total *= f;
            }
            "A fresh `cargo add`/`cargo update` resolves the newest version of a compatibility line. Downloads of any older version in the same line come from a committed lockfile being replayed, and a developer machine caches crates, so a repeat download of a pinned version is a CI runner or container starting cold. Counted per day: a version counts as fresh on the days it was the newest of its line.".to_string()
        }
        VersionDownloads::Period {
            counts,
            period_days,
        } => {
            let period_total: f64 = counts.values().map(|x| *x as f64).sum::<f64>().max(1.0);
            let grace_from = end - chrono::Duration::days(*period_days);
            let (mut s, mut o) = (0.0, 0.0);
            for (num, c) in counts {
                let x = *c as f64;
                if x == 0.0 {
                    continue;
                }
                let mut class = match by_num.get(num.as_str()) {
                    Some(p) => classify(
                        p,
                        &newest_now,
                        latest_line.as_deref(),
                        &data.line_dependents,
                    ),
                    None => Class::Fresh,
                };
                // Superseded during the period: it was newest for part of it.
                if class == Class::Stale {
                    if let Some(p) = by_num.get(num.as_str()) {
                        let k = line_key(&p.v);
                        if let Some(nv) = newest_now.get(&k) {
                            if let Some(np) = parsed.iter().find(|q| q.v == *nv) {
                                if np.published > grace_from {
                                    class = Class::Fresh;
                                }
                            }
                        }
                    }
                }
                vtotals.insert(num.clone(), (x / period_total * totf, class));
                match class {
                    Class::Stale => s += x,
                    Class::Orphan => o += x,
                    _ => {}
                }
            }
            let (ss, os) = (s / period_total, o / period_total);
            stale_total = ss * totf;
            orphan_total = os * totf;
            for i in 0..n {
                fresh[i] = data.totals[i] as f64 * (1.0 - ss - os);
            }
            format!("A fresh `npm install` resolves the newest version of a compatibility line. Downloads of an older version in the same line come from a package-lock being replayed, and a developer machine caches packages, so a repeat download of a pinned version is a CI runner or container starting cold. npm only publishes per-version counts for the last {period_days} days; that split is applied to the whole window. A version superseded inside those {period_days} days counts as fresh.")
        }
    };

    // Release-time fetches.
    let release_dates: Vec<NaiveDate> = parsed
        .iter()
        .filter(|p| {
            p.v.pre.is_empty()
                && data
                    .days
                    .first()
                    .map(|d| p.published >= *d)
                    .unwrap_or(false)
                && p.published <= end
        })
        .map(|p| p.published)
        .collect();
    let (cleaned, burst) = remove_release_bursts(
        &data.days,
        &fresh,
        &release_dates,
        params.release_days_after,
    );

    // Flat automation vs human rhythm.
    let rh = rhythm(&data.days, &cleaned, params.human_weekend_ratio);
    let flat_total = rh.flat_per_day * n as f64;
    let real: f64 = (cleaned.iter().sum::<f64>() - flat_total).max(0.0);
    let real_share = real / totf;
    let noise_share = 1.0 - real_share;

    let mut components = vec![
        Component {
            key: "lockfile",
            label: "Lockfile replays (CI, containers)",
            downloads: stale_total,
            share: stale_total / totf,
            assumption: stale_assumption,
        },
        Component {
            key: "pinned_line",
            label: "Pinned old line with no published dependent",
            downloads: orphan_total,
            share: orphan_total / totf,
            assumption: "An older compatibility line that no published package requires is being pulled by projects that never publish: one app's or one company's CI. It is a handful of consumers, not a crowd. Only applied when the dependents for that line were counted.".into(),
        },
        Component {
            key: "release",
            label: "Release-time fetches (mirrors, scanners, update bots)",
            downloads: burst,
            share: burst / totf,
            assumption: format!("On a publish day and the {} day(s) after, downloads above the median of the 7 quiet days before are mirrors, registries, scanners and Dependabot/Renovate fetching the new version, not new users. {} release(s) in the window. When release days cover more than 40% of the window there is no quiet baseline and nothing is removed here.", params.release_days_after, release_dates.len()),
        },
        Component {
            key: "flat",
            label: "Flat automation (same every day of the week)",
            downloads: flat_total,
            share: flat_total / totf,
            assumption: if rh.enough_data {
                format!("People work weekdays. Human-driven traffic on a weekend day is {:.2}x a weekday (measured, see README). Whatever does not follow that rhythm is cron jobs, scheduled CI and bots: {:.0} a day here. Push-triggered CI follows the human week and is NOT removed by this step.", params.human_weekend_ratio, rh.flat_per_day)
            } else {
                "Too few downloads or days to read a weekly rhythm; nothing removed for flat automation.".into()
            },
        },
    ];
    components.push(Component {
        key: "real",
        label: "Human-rhythm fresh resolves (the real part, upper bound)",
        downloads: real,
        share: real_share,
        assumption: "What is left: newest-in-line downloads on quiet days that follow the working week. It still includes push-triggered CI that resolves without a lockfile, so it is an upper bound on people.".into(),
    });
    if unknown_other > 0.0 {
        components[4].assumption.push_str(&format!(
            " It also keeps {} downloads ({:.1}%) from rarely used old lines that were not fetched one by one, which makes it generous, not harsh.",
            unknown_other.round(),
            unknown_other / totf * 100.0
        ));
    }

    // Version table.
    let mut versions: Vec<VersionRow> = vtotals
        .iter()
        .map(|(num, (d, c))| {
            let line = Version::parse(num)
                .map(|v| line_key(&v))
                .unwrap_or_default();
            VersionRow {
                version: num.clone(),
                line_dependents: data.line_dependents.get(&line).copied().flatten(),
                line,
                downloads: *d,
                share: d / totf,
                class: c.name(),
            }
        })
        .collect();
    versions.sort_by(|a, b| b.downloads.partial_cmp(&a.downloads).unwrap());
    let top_version = versions.first().cloned();
    versions.truncate(10);

    // Plain-language findings.
    let mut findings = Vec::new();
    if let (Some(top), Some(l)) = (&top_version, &latest) {
        if top.share >= 0.5
            && top.version != l.to_string()
            && top.line_dependents.map(|d| d <= 1).unwrap_or(false)
        {
            findings.push(format!(
                "One old version, {}, takes {} of all downloads while the latest is {}, and {} published package(s) ask for its line. That is the shape of one project's pinned CI, not of new users.",
                top.version,
                pct(top.share),
                l,
                top.line_dependents.unwrap_or(0)
            ));
        }
    }
    for c in &components {
        if c.key == "pinned_line" && c.share >= 0.2 {
            findings.push(format!(
                "{} of downloads go to an old line that no published package requires.",
                pct(c.share)
            ));
        }
        if c.key == "lockfile" && c.share >= 0.5 {
            findings.push(format!(
                "{} of downloads are versions a fresh resolve would never pick: lockfile replays.",
                pct(c.share)
            ));
        }
        if c.key == "release" && c.share >= 0.3 {
            findings.push(format!(
                "{} of downloads land in the publish day and the day after.",
                pct(c.share)
            ));
        }
    }
    if let Some(r) = rh.weekend_ratio {
        if rh.enough_data && r >= 0.9 {
            findings.push(format!(
                "Weekends download {:.0}% as much as weekdays. People take weekends off; machines do not.",
                r * 100.0
            ));
        }
    }
    if let Some(dt) = data.dependents_total {
        if dt == 0 && total > 0 {
            findings.push("No published package depends on it. Every download comes from an app, a CI job or a person, never from another library.".into());
        }
    }

    let verdict = if noise_share > params.max_noise {
        "MOSTLY NOISE"
    } else {
        "MOSTLY REAL"
    };
    Report {
        ecosystem: data.ecosystem,
        name: data.name.clone(),
        window_start: data.days.first().map(|d| d.to_string()).unwrap_or_default(),
        window_end: end.to_string(),
        window_days: n,
        headline: data.headline.as_ref().map(|h| h.0),
        headline_counts: data.headline.as_ref().map(|h| h.1.clone()),
        window_downloads: total,
        components,
        real_downloads: real,
        real_share,
        noise_share,
        real_users_per_weekday: rh.human_per_weekday.max(0.0),
        verdict,
        top_version,
        latest_version: parsed
            .iter()
            .filter(|p| p.v.pre.is_empty() && !p.yanked)
            .map(|p| p.v.clone())
            .max()
            .or(latest)
            .map(|v| v.to_string()),
        versions,
        rhythm: rh,
        releases_in_window: release_dates.len(),
        dependents_total: data.dependents_total,
        top_dependents: data.top_dependents.clone(),
        findings,
        params: params.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn window(start: &str, n: usize) -> Vec<NaiveDate> {
        let s = d(start);
        (0..n)
            .map(|i| s + chrono::Duration::days(i as i64))
            .collect()
    }

    fn vi(num: &str, date: &str) -> VersionInfo {
        VersionInfo {
            num: num.into(),
            published: d(date),
            yanked: false,
        }
    }

    #[test]
    fn line_keys_follow_caret_rules() {
        assert_eq!(line_key(&Version::parse("1.4.2").unwrap()), "1.x");
        assert_eq!(line_key(&Version::parse("0.3.9").unwrap()), "0.3.x");
        assert_eq!(line_key(&Version::parse("0.0.7").unwrap()), "0.0.7");
    }

    #[test]
    fn flat_series_is_all_automation() {
        let days = window("2026-08-03", 28);
        let s = vec![100.0; 28];
        let r = rhythm(&days, &s, 0.3);
        assert!(r.enough_data);
        assert!((r.flat_per_day - 100.0).abs() < 1e-9);
        assert!(r.human_per_weekday.abs() < 1e-9);
    }

    #[test]
    fn human_week_is_all_human() {
        let days = window("2026-08-03", 28);
        let s: Vec<f64> = days
            .iter()
            .map(|d| if is_weekend(*d) { 30.0 } else { 100.0 })
            .collect();
        let r = rhythm(&days, &s, 0.3);
        assert!(r.flat_per_day.abs() < 1e-9);
        assert!((r.human_per_weekday - 100.0).abs() < 1e-9);
    }

    #[test]
    fn mixed_week_splits() {
        // 50 automation every day + 100 human weekdays / 30 weekends.
        let days = window("2026-08-03", 28);
        let s: Vec<f64> = days
            .iter()
            .map(|d| if is_weekend(*d) { 80.0 } else { 150.0 })
            .collect();
        let r = rhythm(&days, &s, 0.3);
        assert!((r.flat_per_day - 50.0).abs() < 1e-6, "{}", r.flat_per_day);
        assert!((r.human_per_weekday - 100.0).abs() < 1e-6);
    }

    #[test]
    fn release_burst_removed_against_quiet_baseline() {
        let days = window("2026-08-03", 14);
        let mut s = vec![10.0; 14];
        s[7] = 60.0;
        s[8] = 25.0;
        let (c, ex) = remove_release_bursts(&days, &s, &[days[7]], 1);
        assert!((ex - 65.0).abs() < 1e-9);
        assert_eq!(c[7], 10.0);
        assert_eq!(c[8], 10.0);
    }

    /// The remoteid case: 0.1.0 (old line, no published dependent) takes
    /// almost everything while 0.2.x/0.3.x get a trickle.
    #[test]
    fn remoteid_shape_is_mostly_noise() {
        let days = window("2026-08-03", 28);
        let n = days.len();
        let mut series = BTreeMap::new();
        series.insert("0.1.0".to_string(), vec![400u64; n]);
        let mut latest = vec![0u64; n];
        for (i, d) in days.iter().enumerate() {
            if i >= 14 {
                latest[i] = if is_weekend(*d) { 3 } else { 10 };
            }
        }
        series.insert("0.2.0".to_string(), latest.clone());
        let totals: Vec<u64> = (0..n).map(|i| 400 + latest[i]).collect();
        let mut line_dependents = BTreeMap::new();
        line_dependents.insert("0.1.x".to_string(), Some(0));
        line_dependents.insert("0.2.x".to_string(), Some(1));
        let data = PackageData {
            ecosystem: Ecosystem::Crates,
            name: "remoteid".into(),
            days: days.clone(),
            totals,
            versions: vec![vi("0.1.0", "2026-07-01"), vi("0.2.0", "2026-08-17")],
            by_version: VersionDownloads::Daily {
                series,
                other_stale: vec![0; n],
                other_unknown: vec![0; n],
            },
            line_dependents,
            dependents_total: Some(1),
            top_dependents: vec![],
            headline: None,
        };
        let r = analyze(&data, &Params::default());
        assert_eq!(r.verdict, "MOSTLY NOISE");
        let pinned = r
            .components
            .iter()
            .find(|c| c.key == "pinned_line")
            .unwrap();
        assert!(pinned.share > 0.9, "{}", pinned.share);
        assert!(r.findings.iter().any(|f| f.contains("0.1.0")));
    }

    #[test]
    fn stale_patch_counts_as_lockfile_only_after_supersede() {
        let days = window("2026-08-03", 20);
        let n = days.len();
        let mut series = BTreeMap::new();
        series.insert("1.0.0".to_string(), vec![10u64; n]);
        series.insert("1.0.1".to_string(), vec![0u64; n]);
        let data = PackageData {
            ecosystem: Ecosystem::Crates,
            name: "x".into(),
            days: days.clone(),
            totals: vec![10; n],
            versions: vec![vi("1.0.0", "2026-01-01"), vi("1.0.1", "2026-08-13")],
            by_version: VersionDownloads::Daily {
                series,
                other_stale: vec![0; n],
                other_unknown: vec![0; n],
            },
            line_dependents: BTreeMap::new(),
            dependents_total: None,
            top_dependents: vec![],
            headline: None,
        };
        let r = analyze(&data, &Params::default());
        let lock = r.components.iter().find(|c| c.key == "lockfile").unwrap();
        // 1.0.1 published on day index 10, so 10 of 20 days are stale.
        assert!((lock.downloads - 100.0).abs() < 1e-6, "{}", lock.downloads);
    }

    #[test]
    fn npm_period_split_applies_to_window() {
        let days = window("2026-08-03", 28);
        let n = days.len();
        let mut counts = BTreeMap::new();
        counts.insert("2.0.0".to_string(), 30u64);
        counts.insert("2.1.0".to_string(), 70u64);
        let data = PackageData {
            ecosystem: Ecosystem::Npm,
            name: "y".into(),
            days: days.clone(),
            totals: days
                .iter()
                .map(|d| if is_weekend(*d) { 30 } else { 100 })
                .collect(),
            versions: vec![vi("2.0.0", "2026-01-01"), vi("2.1.0", "2026-03-01")],
            by_version: VersionDownloads::Period {
                counts,
                period_days: 7,
            },
            line_dependents: BTreeMap::new(),
            dependents_total: None,
            top_dependents: vec![],
            headline: None,
        };
        let r = analyze(&data, &Params::default());
        let lock = r.components.iter().find(|c| c.key == "lockfile").unwrap();
        assert!((lock.share - 0.3).abs() < 1e-9);
        assert!((r.real_share - 0.7).abs() < 1e-6, "{}", r.real_share);
        let _ = n;
    }

    #[test]
    fn window_starts_at_first_release() {
        let days = window("2026-08-03", 28);
        let n = days.len();
        let mut series = BTreeMap::new();
        let s: Vec<u64> = (0..n).map(|i| if i >= 20 { 50 } else { 0 }).collect();
        series.insert("0.1.0".to_string(), s.clone());
        let data = PackageData {
            ecosystem: Ecosystem::Crates,
            name: "new".into(),
            days: days.clone(),
            totals: s,
            versions: vec![vi("0.1.0", "2026-08-23")],
            by_version: VersionDownloads::Daily {
                series,
                other_stale: vec![0; n],
                other_unknown: vec![0; n],
            },
            line_dependents: BTreeMap::new(),
            dependents_total: None,
            top_dependents: vec![],
            headline: None,
        };
        let r = analyze(&data, &Params::default());
        assert_eq!(r.window_days, 8);
        assert_eq!(r.window_start, "2026-08-23");
        assert_eq!(r.window_downloads, 400);
    }

    #[test]
    fn near_daily_releases_remove_nothing() {
        let days = window("2026-08-03", 20);
        let s: Vec<f64> = (0..20).map(|i| 10.0 + i as f64).collect();
        let rel: Vec<NaiveDate> = days.iter().step_by(2).cloned().collect();
        let (c, ex) = remove_release_bursts(&days, &s, &rel, 1);
        assert_eq!(ex, 0.0);
        assert_eq!(c, s);
    }
}
