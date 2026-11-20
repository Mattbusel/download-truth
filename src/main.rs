//! download-truth: download counts mostly measure CI and publishing, not users.

mod badge;
mod fetch;
mod model;

use clap::{Args, Parser, Subcommand};
use model::{analyze, Ecosystem, Params, Report};
use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "download-truth",
    version,
    about = "Download counts mostly measure CI and publishing, not users. This splits them."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Args, Clone)]
struct Common {
    /// Noise share above which the verdict is MOSTLY NOISE (and checks fail).
    #[arg(long, default_value_t = 0.5)]
    max_noise: f64,
    /// Weekend/weekday ratio of purely human traffic (measured default, see README).
    #[arg(long, default_value_t = model::DEFAULT_HUMAN_WEEKEND_RATIO)]
    human_weekend_ratio: f64,
    /// Most compatibility lines fetched one by one per package.
    #[arg(long, default_value_t = 20)]
    max_lines: usize,
    /// Print JSON instead of text.
    #[arg(long)]
    json: bool,
}

impl Common {
    fn params(&self) -> Params {
        Params {
            human_weekend_ratio: self.human_weekend_ratio,
            max_noise: self.max_noise,
            ..Params::default()
        }
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// Analyze a crates.io crate.
    Crate {
        name: String,
        #[command(flatten)]
        common: Common,
        /// Exit 1 when the verdict is MOSTLY NOISE.
        #[arg(long)]
        fail: bool,
        /// Write a shields.io endpoint badge JSON with the real number.
        #[arg(long)]
        badge_json: Option<String>,
    },
    /// Analyze an npm package.
    Npm {
        name: String,
        #[command(flatten)]
        common: Common,
        #[arg(long)]
        fail: bool,
        #[arg(long)]
        badge_json: Option<String>,
    },
    /// Check every downloads badge in a README; exit 1 if any badge's
    /// downloads are mostly noise.
    Readme {
        #[arg(default_value = "README.md")]
        path: String,
        #[command(flatten)]
        common: Common,
        /// Write a shields.io endpoint badge JSON for the first claim.
        #[arg(long)]
        badge_json: Option<String>,
    },
    /// Run many packages and append one JSON report per line (resumable).
    Batch {
        /// Analyze the top N crates by recent downloads.
        #[arg(long, default_value_t = 0)]
        top_crates: usize,
        /// File with one crate name per line.
        #[arg(long)]
        crates: Option<String>,
        /// File with one npm package name per line.
        #[arg(long)]
        npm: Option<String>,
        /// Output JSONL file; packages already in it are skipped.
        #[arg(long)]
        out: String,
        /// Keep every registry response here and reuse it on later runs.
        #[arg(long)]
        cache_dir: Option<String>,
        /// Treat this UTC date (YYYY-MM-DD) as today, to re-analyze a cached run.
        #[arg(long)]
        as_of: Option<chrono::NaiveDate>,
        #[command(flatten)]
        common: Common,
    },
}

fn fmt_n(x: f64) -> String {
    let n = x.round() as i64;
    let s = n.abs().to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    if n < 0 {
        format!("-{out}")
    } else {
        out
    }
}

fn print_text(r: &Report) {
    let eco = match r.ecosystem {
        Ecosystem::Crates => "crates.io",
        Ecosystem::Npm => "npm",
    };
    println!("{} ({eco}): {}", r.name, r.verdict);
    if let (Some(h), Some(what)) = (r.headline, &r.headline_counts) {
        println!("  badge number: {} {what}", fmt_n(h as f64));
    }
    println!(
        "  window {} to {} ({} days): {} downloads",
        r.window_start,
        r.window_end,
        r.window_days,
        fmt_n(r.window_downloads as f64)
    );
    println!();
    for c in &r.components {
        println!(
            "  {:>6.1}%  {:>14}  {}",
            c.share * 100.0,
            fmt_n(c.downloads),
            c.label
        );
    }
    println!();
    println!(
        "  real users estimate: at most {} fresh, human-rhythm resolves per weekday ({:.1}% of downloads)",
        fmt_n(r.real_users_per_weekday),
        r.real_share * 100.0
    );
    if let (Some(ratio), true) = (r.rhythm.weekend_ratio, r.rhythm.enough_data) {
        println!("  weekend/weekday ratio after cleanup: {ratio:.2}");
    }
    if let Some(l) = &r.latest_version {
        println!(
            "  latest: {l}; releases in window: {}",
            r.releases_in_window
        );
    }
    if let Some(d) = r.dependents_total {
        println!("  published dependents: {}", fmt_n(d as f64));
    }
    if !r.top_dependents.is_empty() {
        let s: Vec<String> = r
            .top_dependents
            .iter()
            .map(|(n, d, k)| format!("{n} ({}, {k})", fmt_n(*d as f64)))
            .collect();
        println!("  largest dependents: {}", s.join(", "));
    }
    println!();
    println!("  versions (window share, line, dependents asking for that line):");
    for v in r.versions.iter().take(6) {
        let deps = v
            .line_dependents
            .map(|d| fmt_n(d as f64))
            .unwrap_or_else(|| "?".into());
        println!(
            "    {:>6.1}%  {:<16} {:<8} deps {:<8} {}",
            v.share * 100.0,
            v.version,
            v.line,
            deps,
            v.class
        );
    }
    if !r.findings.is_empty() {
        println!();
        for f in &r.findings {
            println!("  * {f}");
        }
    }
    println!();
    println!("  assumptions:");
    for c in &r.components {
        println!("    [{}] {}", c.key, c.assumption);
    }
    println!(
        "    human weekend ratio {:.2}, noise threshold {:.0}%",
        r.params.human_weekend_ratio,
        r.params.max_noise * 100.0
    );
}

fn badge_json(r: &Report) -> String {
    let color = if r.noise_share > r.params.max_noise {
        "red"
    } else {
        "brightgreen"
    };
    serde_json::json!({
        "schemaVersion": 1,
        "label": "real downloads",
        "message": format!("{}/weekday ({:.0}% of total)", fmt_n(r.real_users_per_weekday), r.real_share * 100.0),
        "color": color,
    })
    .to_string()
}

fn run_one(
    eco: Ecosystem,
    name: &str,
    common: &Common,
    client: &mut fetch::Client,
) -> Result<Report, String> {
    let data = match eco {
        Ecosystem::Crates => fetch::crate_data(client, name, common.max_lines)?,
        Ecosystem::Npm => fetch::npm_data(client, name, common.max_lines)?,
    };
    Ok(analyze(&data, &common.params()))
}

fn single(
    eco: Ecosystem,
    name: &str,
    common: &Common,
    fail: bool,
    badge: &Option<String>,
) -> ExitCode {
    let mut client = fetch::Client::new();
    match run_one(eco, name, common, &mut client) {
        Ok(r) => {
            if common.json {
                println!("{}", serde_json::to_string_pretty(&r).unwrap());
            } else {
                print_text(&r);
            }
            if let Some(p) = badge {
                if let Err(e) = fs::write(p, badge_json(&r)) {
                    eprintln!("error: {p}: {e}");
                    return ExitCode::from(2);
                }
            }
            if fail && r.noise_share > common.max_noise {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}

fn lines_of(path: &str) -> Result<Vec<String>, String> {
    Ok(fs::read_to_string(path)
        .map_err(|e| format!("{path}: {e}"))?
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect())
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Crate {
            name,
            common,
            fail,
            badge_json,
        } => single(Ecosystem::Crates, &name, &common, fail, &badge_json),
        Cmd::Npm {
            name,
            common,
            fail,
            badge_json,
        } => single(Ecosystem::Npm, &name, &common, fail, &badge_json),
        Cmd::Readme {
            path,
            common,
            badge_json: bj,
        } => {
            let text = match fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("error: {path}: {e}");
                    return ExitCode::from(2);
                }
            };
            let claims = badge::find_claims(&text);
            if claims.is_empty() {
                println!("{path}: no downloads badge found; nothing to check.");
                return ExitCode::SUCCESS;
            }
            let mut client = fetch::Client::new();
            let mut failed = false;
            let mut errored = false;
            let mut seen = BTreeSet::new();
            let mut first = true;
            for c in claims {
                if !seen.insert((c.package.clone(), c.ecosystem == Ecosystem::Npm)) {
                    continue;
                }
                match run_one(c.ecosystem, &c.package, &common, &mut client) {
                    Ok(r) => {
                        let bad = r.noise_share > common.max_noise;
                        failed |= bad;
                        println!(
                            "{path}:{}: {} badge for {} claims {} downloads; {:.0}% of the last {} days is noise (lockfile replays {:.0}%, pinned old line {:.0}%, release-time {:.0}%, flat automation {:.0}%). {}",
                            c.line,
                            c.kind,
                            c.package,
                            r.headline.map(|h| fmt_n(h as f64)).unwrap_or_default(),
                            r.noise_share * 100.0,
                            r.window_days,
                            r.components[0].share * 100.0,
                            r.components[1].share * 100.0,
                            r.components[2].share * 100.0,
                            r.components[3].share * 100.0,
                            if bad { "FAIL: the badge mostly counts machines." } else { "ok" }
                        );
                        if common.json {
                            println!("{}", serde_json::to_string(&r).unwrap());
                        }
                        if first {
                            if let Some(p) = &bj {
                                let _ = fs::write(p, badge_json(&r));
                            }
                            first = false;
                        }
                    }
                    Err(e) => {
                        eprintln!("{path}:{}: {}: error: {e}", c.line, c.package);
                        errored = true;
                    }
                }
            }
            if failed {
                ExitCode::from(1)
            } else if errored {
                ExitCode::from(2)
            } else {
                ExitCode::SUCCESS
            }
        }
        Cmd::Batch {
            top_crates,
            crates,
            npm,
            out,
            common,
            cache_dir,
            as_of,
        } => {
            let mut client = fetch::Client::new();
            client.cache_dir = cache_dir.map(std::path::PathBuf::from);
            client.as_of = as_of;
            let mut jobs: Vec<(Ecosystem, String)> = Vec::new();
            if top_crates > 0 {
                match fetch::top_crates(&mut client, top_crates) {
                    Ok(v) => jobs.extend(v.into_iter().map(|n| (Ecosystem::Crates, n))),
                    Err(e) => {
                        eprintln!("error: {e}");
                        return ExitCode::from(2);
                    }
                }
            }
            for (file, eco) in [(crates, Ecosystem::Crates), (npm, Ecosystem::Npm)] {
                if let Some(f) = file {
                    match lines_of(&f) {
                        Ok(v) => jobs.extend(v.into_iter().map(|n| (eco, n))),
                        Err(e) => {
                            eprintln!("error: {e}");
                            return ExitCode::from(2);
                        }
                    }
                }
            }
            let done: BTreeSet<(String, String)> = fs::read_to_string(&out)
                .unwrap_or_default()
                .lines()
                .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                .filter_map(|v| {
                    Some((
                        v["ecosystem"].as_str()?.to_string(),
                        v["name"].as_str()?.to_string(),
                    ))
                })
                .collect();
            let mut f = match fs::OpenOptions::new().create(true).append(true).open(&out) {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("error: {out}: {e}");
                    return ExitCode::from(2);
                }
            };
            let total = jobs.len();
            for (i, (eco, name)) in jobs.into_iter().enumerate() {
                let ek = if eco == Ecosystem::Crates {
                    "crates"
                } else {
                    "npm"
                };
                if done.contains(&(ek.to_string(), name.clone())) {
                    continue;
                }
                match run_one(eco, &name, &common, &mut client) {
                    Ok(r) => {
                        let _ = writeln!(f, "{}", serde_json::to_string(&r).unwrap());
                        eprintln!(
                            "[{}/{total}] {ek} {name}: {} noise {:.0}%",
                            i + 1,
                            r.verdict,
                            r.noise_share * 100.0
                        );
                    }
                    Err(e) => eprintln!("[{}/{total}] {ek} {name}: error {e}", i + 1),
                }
            }
            ExitCode::SUCCESS
        }
    }
}
