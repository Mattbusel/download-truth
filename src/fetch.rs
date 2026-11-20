//! Registry clients: crates.io, npm and deps.dev. Blocking, one request at a
//! time; crates.io gets at most one request per second (its crawler policy).

use crate::model::{line_key, Ecosystem, PackageData, VersionDownloads, VersionInfo};
use chrono::{Duration, NaiveDate, Utc};
use semver::{Version, VersionReq};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::thread::sleep;
use std::time::{Duration as StdDuration, Instant};

pub const USER_AGENT: &str = concat!(
    "download-truth/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/Mattbusel/download-truth)"
);

pub struct Client {
    agent: ureq::Agent,
    last_crates: Option<Instant>,
    /// Optional directory where every response is kept, so a corpus can be
    /// re-analyzed with other parameters without hitting the registries again.
    pub cache_dir: Option<std::path::PathBuf>,
    /// Treat this UTC date as today (re-analysis of a cached run).
    pub as_of: Option<NaiveDate>,
}

fn cache_name(url: &str) -> String {
    // FNV-1a, stable across runs and platforms.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in url.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}.json")
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

impl Client {
    pub fn new() -> Self {
        Client {
            agent: ureq::AgentBuilder::new()
                .user_agent(USER_AGENT)
                .timeout(StdDuration::from_secs(90))
                .build(),
            last_crates: None,
            cache_dir: None,
            as_of: None,
        }
    }

    fn today(&self) -> NaiveDate {
        self.as_of.unwrap_or_else(|| Utc::now().date_naive())
    }

    fn get(&mut self, url: &str) -> Result<Option<Value>, String> {
        if let Some(dir) = self.cache_dir.clone() {
            let p = dir.join(cache_name(url));
            if let Ok(t) = std::fs::read_to_string(&p) {
                let v: Value =
                    serde_json::from_str(&t).map_err(|e| format!("{}: {e}", p.display()))?;
                return Ok(if v.is_null() { None } else { Some(v) });
            }
            let r = self.get_net(url)?;
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(
                &p,
                r.as_ref()
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "null".into()),
            );
            return Ok(r);
        }
        self.get_net(url)
    }

    fn get_net(&mut self, url: &str) -> Result<Option<Value>, String> {
        if url.starts_with("https://crates.io/") {
            if let Some(t) = self.last_crates {
                let gap = StdDuration::from_millis(1000);
                let el = t.elapsed();
                if el < gap {
                    sleep(gap - el);
                }
            }
            self.last_crates = Some(Instant::now());
        }
        let mut tries = 0;
        loop {
            tries += 1;
            match self.agent.get(url).call() {
                Ok(r) => {
                    let v: Value = r.into_json().map_err(|e| format!("{url}: {e}"))?;
                    return Ok(Some(v));
                }
                Err(ureq::Error::Status(404, _)) => return Ok(None),
                Err(ureq::Error::Status(code, r)) if (code == 429 || code >= 500) && tries < 7 => {
                    let wait = r
                        .header("retry-after")
                        .and_then(|h| h.trim().parse::<u64>().ok())
                        .unwrap_or(15 * tries as u64)
                        .min(300);
                    sleep(StdDuration::from_secs(wait));
                }
                Err(ureq::Error::Transport(_)) if tries < 5 => {
                    sleep(StdDuration::from_secs(3 * tries as u64))
                }
                Err(e) => return Err(format!("{url}: {e}")),
            }
        }
    }
}

fn date_of(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.get(..10)?, "%Y-%m-%d").ok()
}

fn enc(name: &str) -> String {
    name.replace('/', "%2F")
}

/// Newest stable version of each line, by line.
fn line_newest(versions: &[VersionInfo]) -> BTreeMap<String, Version> {
    let mut m: BTreeMap<String, Version> = BTreeMap::new();
    for x in versions {
        if x.yanked {
            continue;
        }
        if let Ok(v) = Version::parse(&x.num) {
            if !v.pre.is_empty() {
                continue;
            }
            let k = line_key(&v);
            if m.get(&k).map(|c| *c < v).unwrap_or(true) {
                m.insert(k, v);
            }
        }
    }
    m
}

/// deps.dev direct dependents of one exact version.
fn depsdev_direct(
    c: &mut Client,
    system: &str,
    name: &str,
    version: &str,
) -> Result<Option<u64>, String> {
    let url = format!(
        "https://api.deps.dev/v3alpha/systems/{system}/packages/{}/versions/{}:dependents",
        enc(name),
        version
    );
    Ok(c.get(&url)?
        .and_then(|v| v["directDependentCount"].as_u64()))
}

/// deps.dev counts dependents of an exact version and lags or misses some
/// (it reports 0 for tokenizers 0.21.4 while crates.io lists crates requiring
/// `^0.21`). A zero from it is never evidence of zero, only "unknown"; the
/// pinned-line slice is applied only where dependents were counted in full.
fn reliable_zero(n: Option<u64>) -> Option<u64> {
    match n {
        Some(0) => None,
        other => other,
    }
}

/// Fetch a crates.io crate.
pub fn crate_data(c: &mut Client, name: &str, max_lines: usize) -> Result<PackageData, String> {
    let info = c
        .get(&format!(
            "https://crates.io/api/v1/crates/{name}?include=versions"
        ))?
        .ok_or_else(|| format!("crate {name} not found on crates.io"))?;
    let all_time = info["crate"]["downloads"].as_u64();
    let mut versions = Vec::new();
    let mut alltime_by_num: BTreeMap<String, u64> = BTreeMap::new();
    for v in info["versions"].as_array().cloned().unwrap_or_default() {
        let num = v["num"].as_str().unwrap_or("").to_string();
        let published = v["created_at"]
            .as_str()
            .and_then(date_of)
            .unwrap_or(NaiveDate::MIN);
        alltime_by_num.insert(num.clone(), v["downloads"].as_u64().unwrap_or(0));
        versions.push(VersionInfo {
            num,
            published,
            yanked: v["yanked"].as_bool().unwrap_or(false),
        });
    }
    let id_to_num: BTreeMap<u64, String> = info["versions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|v| Some((v["id"].as_u64()?, v["num"].as_str()?.to_string())))
        .collect();

    let dl = c
        .get(&format!("https://crates.io/api/v1/crates/{name}/downloads"))?
        .ok_or("downloads not found")?;
    let today = c.today();
    // Window: the last 90 complete UTC days (crates.io includes today, partial).
    let end = today - Duration::days(1);
    let start = end - Duration::days(89);
    let days: Vec<NaiveDate> = (0..90).map(|i| start + Duration::days(i)).collect();
    let idx = |d: NaiveDate| -> Option<usize> {
        if d < start || d > end {
            None
        } else {
            Some((d - start).num_days() as usize)
        }
    };
    let mut series: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    for r in dl["version_downloads"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        let Some(num) = r["version"]
            .as_u64()
            .and_then(|id| id_to_num.get(&id))
            .cloned()
        else {
            continue;
        };
        let Some(i) = r["date"].as_str().and_then(date_of).and_then(idx) else {
            continue;
        };
        series.entry(num).or_insert_with(|| vec![0; 90])[i] += r["downloads"].as_u64().unwrap_or(0);
    }
    let mut extra = vec![0u64; 90];
    for r in dl["meta"]["extra_downloads"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        if let Some(i) = r["date"].as_str().and_then(date_of).and_then(idx) {
            extra[i] += r["downloads"].as_u64().unwrap_or(0);
        }
    }
    let top5: BTreeSet<String> = series.keys().cloned().collect();

    // Newest version of every line, fetched one by one when not in the top 5.
    let newest = line_newest(&versions);
    let mut line_rank: Vec<(String, Version, u64)> = newest
        .iter()
        .map(|(k, v)| {
            let lt: u64 = versions
                .iter()
                .filter(|x| {
                    Version::parse(&x.num)
                        .map(|p| line_key(&p) == *k)
                        .unwrap_or(false)
                })
                .map(|x| alltime_by_num.get(&x.num).copied().unwrap_or(0))
                .sum();
            (k.clone(), v.clone(), lt)
        })
        .collect();
    line_rank.sort_by_key(|x| std::cmp::Reverse(x.2));
    // Every version that was the newest of its line on some day of the
    // window is fetched one by one: a fresh resolve could have picked it.
    let mut unfetched_alltime: u64 = 0;
    let mut to_fetch: Vec<String> = Vec::new();
    for (rank, (k, _, lt)) in line_rank.iter().enumerate() {
        if *lt == 0 {
            continue;
        }
        let mut line_vs: Vec<(Version, NaiveDate)> = versions
            .iter()
            .filter(|x| !x.yanked)
            .filter_map(|x| Version::parse(&x.num).ok().map(|v| (v, x.published)))
            .filter(|(v, _)| v.pre.is_empty() && line_key(v) == *k)
            .collect();
        line_vs.sort();
        let at_start = line_vs
            .iter()
            .filter(|(_, p)| *p <= start)
            .map(|(v, _)| v.clone())
            .max();
        let mut cands: Vec<Version> = line_vs
            .iter()
            .filter(|(v, p)| {
                *p > start && *p <= end && at_start.as_ref().map(|s| v > s).unwrap_or(true)
            })
            .map(|(v, _)| v.clone())
            .collect();
        cands.extend(at_start);
        for v in cands {
            let num = v.to_string();
            if top5.contains(&num) {
                continue;
            }
            if rank < max_lines {
                to_fetch.push(num);
            } else {
                unfetched_alltime += alltime_by_num.get(&num).copied().unwrap_or(0);
            }
        }
    }
    for num in to_fetch {
        if let Some(vd) = c.get(&format!(
            "https://crates.io/api/v1/crates/{name}/{num}/downloads"
        ))? {
            let mut s = vec![0u64; 90];
            for r in vd["version_downloads"]
                .as_array()
                .cloned()
                .unwrap_or_default()
            {
                if let Some(i) = r["date"].as_str().and_then(date_of).and_then(idx) {
                    s[i] += r["downloads"].as_u64().unwrap_or(0);
                }
            }
            for i in 0..90 {
                extra[i] = extra[i].saturating_sub(s[i]);
            }
            series.insert(num, s);
        }
    }
    // What is left are versions that were never newest of their line in the
    // window, except for lines skipped above. Bound those by their all-time
    // downloads and keep that part as real (generous, not harsh).
    let other_total: u64 = extra.iter().sum();
    let unknown_share = if other_total > 0 {
        (unfetched_alltime.min(other_total)) as f64 / other_total as f64
    } else {
        0.0
    };
    let other_unknown: Vec<u64> = extra
        .iter()
        .map(|x| (*x as f64 * unknown_share).round() as u64)
        .collect();
    let other_stale: Vec<u64> = extra
        .iter()
        .zip(&other_unknown)
        .map(|(x, u)| x.saturating_sub(*u))
        .collect();
    let totals: Vec<u64> = (0..90)
        .map(|i| series.values().map(|s| s[i]).sum::<u64>() + extra[i])
        .collect();

    // Reverse dependencies.
    let rd = c
        .get(&format!(
            "https://crates.io/api/v1/crates/{name}/reverse_dependencies?per_page=100&page=1"
        ))?
        .unwrap_or(Value::Null);
    let dependents_total = rd["meta"]["total"].as_u64();
    let mut pages = vec![rd];
    let total_deps = dependents_total.unwrap_or(0);
    if total_deps > 100 && total_deps <= 500 {
        for p in 2..=total_deps.div_ceil(100) {
            if let Some(v) = c.get(&format!(
                "https://crates.io/api/v1/crates/{name}/reverse_dependencies?per_page=100&page={p}"
            ))? {
                pages.push(v);
            }
        }
    }
    let mut top_dependents: Vec<(String, u64, String)> = Vec::new();
    let mut reqs: Vec<(VersionReq, String)> = Vec::new();
    for pg in &pages {
        let vmap: BTreeMap<u64, (String, u64)> = pg["versions"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|v| {
                Some((
                    v["id"].as_u64()?,
                    (
                        v["crate"].as_str()?.to_string(),
                        v["downloads"].as_u64().unwrap_or(0),
                    ),
                ))
            })
            .collect();
        for d in pg["dependencies"].as_array().cloned().unwrap_or_default() {
            let kind = d["kind"].as_str().unwrap_or("normal").to_string();
            if let Some((cn, cd)) = d["version_id"].as_u64().and_then(|id| vmap.get(&id)) {
                top_dependents.push((cn.clone(), *cd, kind.clone()));
            }
            if kind != "dev" {
                if let Some(rq) = d["req"].as_str().and_then(|r| VersionReq::parse(r).ok()) {
                    reqs.push((rq, kind));
                }
            }
        }
    }
    top_dependents.sort_by_key(|x| std::cmp::Reverse(x.1));
    top_dependents.truncate(5);
    let mut line_dependents: BTreeMap<String, Option<u64>> = BTreeMap::new();
    let complete = total_deps <= 500;
    for (k, v) in &newest {
        if complete {
            let n = reqs.iter().filter(|(r, _)| r.matches(v)).count() as u64;
            line_dependents.insert(k.clone(), Some(n));
        } else if reqs.iter().any(|(r, _)| r.matches(v)) {
            // First page only: a match proves dependents exist, no match proves nothing.
            let n = reqs.iter().filter(|(r, _)| r.matches(v)).count() as u64;
            let dd = depsdev_direct(c, "cargo", name, &v.to_string()).unwrap_or(None);
            line_dependents.insert(k.clone(), Some(dd.unwrap_or(0).max(n)));
        } else {
            let n = depsdev_direct(c, "cargo", name, &v.to_string()).unwrap_or(None);
            line_dependents.insert(k.clone(), reliable_zero(n));
        }
    }

    Ok(PackageData {
        ecosystem: Ecosystem::Crates,
        name: name.to_string(),
        days,
        totals,
        versions,
        by_version: VersionDownloads::Daily {
            series,
            other_stale,
            other_unknown,
        },
        line_dependents,
        dependents_total,
        top_dependents,
        headline: all_time.map(|d| (d, "all-time downloads (the crates/d badge)".to_string())),
    })
}

/// Fetch an npm package.
pub fn npm_data(c: &mut Client, name: &str, max_lines: usize) -> Result<PackageData, String> {
    let doc = c
        .get(&format!("https://registry.npmjs.org/{}", enc(name)))?
        .ok_or_else(|| format!("package {name} not found on npm"))?;
    let mut versions = Vec::new();
    if let Some(times) = doc["time"].as_object() {
        for (num, t) in times {
            if num == "created" || num == "modified" {
                continue;
            }
            let Some(published) = t.as_str().and_then(date_of) else {
                continue;
            };
            let exists = doc["versions"].get(num).is_some();
            versions.push(VersionInfo {
                num: num.clone(),
                published,
                yanked: !exists,
            });
        }
    }
    let end = c.today() - Duration::days(1);
    let start = end - Duration::days(89);
    let rng = c
        .get(&format!(
            "https://api.npmjs.org/downloads/range/{start}:{end}/{name}"
        ))?
        .ok_or("npm downloads not found")?;
    let days: Vec<NaiveDate> = (0..90).map(|i| start + Duration::days(i)).collect();
    let mut totals = vec![0u64; 90];
    for r in rng["downloads"].as_array().cloned().unwrap_or_default() {
        if let Some(d) = r["day"].as_str().and_then(date_of) {
            if d >= start && d <= end {
                totals[(d - start).num_days() as usize] += r["downloads"].as_u64().unwrap_or(0);
            }
        }
    }
    let vw = c
        .get(&format!(
            "https://api.npmjs.org/versions/{}/last-week",
            enc(name)
        ))?
        .unwrap_or(Value::Null);
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    if let Some(m) = vw["downloads"].as_object() {
        for (k, v) in m {
            let x = v.as_u64().unwrap_or(0);
            if x > 0 {
                counts.insert(k.clone(), x);
            }
        }
    }
    let last30: u64 = totals[60..].iter().sum();

    // Dependents per line from deps.dev, for the most downloaded lines.
    let newest = line_newest(&versions);
    let mut line_dl: BTreeMap<String, u64> = BTreeMap::new();
    for (k, v) in &counts {
        if let Ok(p) = Version::parse(k) {
            *line_dl.entry(line_key(&p)).or_default() += v;
        }
    }
    let mut ranked: Vec<(String, u64)> = line_dl.into_iter().collect();
    ranked.sort_by_key(|x| std::cmp::Reverse(x.1));
    let latest_line = newest.values().max().map(line_key);
    let mut line_dependents: BTreeMap<String, Option<u64>> = BTreeMap::new();
    let mut latest_found = None;
    if let Some(ll) = &latest_line {
        if let Some(v) = newest.get(ll) {
            latest_found = depsdev_direct(c, "npm", name, &v.to_string()).unwrap_or(None);
            line_dependents.insert(ll.clone(), latest_found);
        }
    }
    for (k, _) in ranked.iter().take(max_lines) {
        if Some(k) == latest_line.as_ref() {
            continue;
        }
        if let Some(v) = newest.get(k) {
            let n = depsdev_direct(c, "npm", name, &v.to_string()).unwrap_or(None);
            // "not found" means zero only when deps.dev indexes the package.
            let n = if n.is_none() && latest_found.is_some() {
                Some(0)
            } else {
                n
            };
            line_dependents.insert(k.clone(), reliable_zero(n));
        }
    }
    let dependents_total = latest_found.map(|l| {
        l + line_dependents
            .iter()
            .filter(|(k, _)| Some(*k) != latest_line.as_ref())
            .filter_map(|(_, v)| *v)
            .sum::<u64>()
    });

    Ok(PackageData {
        ecosystem: Ecosystem::Npm,
        name: name.to_string(),
        days,
        totals,
        versions,
        by_version: VersionDownloads::Period {
            counts,
            period_days: 7,
        },
        line_dependents,
        dependents_total,
        top_dependents: vec![],
        headline: Some((
            last30,
            "downloads in the last 30 days (the npm/dm badge)".into(),
        )),
    })
}

/// Top crates.io crates by recent (90-day) downloads.
pub fn top_crates(c: &mut Client, n: usize) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut page = 1;
    while out.len() < n {
        let v = c
            .get(&format!(
                "https://crates.io/api/v1/crates?sort=recent-downloads&per_page=100&page={page}"
            ))?
            .ok_or("listing failed")?;
        let arr = v["crates"].as_array().cloned().unwrap_or_default();
        if arr.is_empty() {
            break;
        }
        for x in arr {
            if let Some(id) = x["id"].as_str() {
                out.push(id.to_string());
            }
        }
        page += 1;
    }
    out.truncate(n);
    Ok(out)
}
