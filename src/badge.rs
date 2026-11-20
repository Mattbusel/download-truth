//! Find downloads badges in a README and say which package each one claims.

use crate::model::Ecosystem;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadgeClaim {
    pub line: usize,
    pub ecosystem: Ecosystem,
    pub package: String,
    pub kind: String,
    pub url: String,
}

const HOSTS: [&str; 2] = ["img.shields.io/", "badgen.net/"];
const CRATE_KINDS: [&str; 3] = ["d", "dv", "dr"];
const NPM_KINDS: [&str; 5] = ["dm", "dw", "dy", "dt", "d18m"];

fn clean_segment(s: &str) -> String {
    let s = s
        .split(['?', '#', ')', '"', '\'', ' ', ']', '>'])
        .next()
        .unwrap_or("");
    let s = s.strip_suffix(".svg").unwrap_or(s);
    let s = s.strip_suffix(".png").unwrap_or(s);
    s.replace("%40", "@")
        .replace("%2F", "/")
        .replace("%2f", "/")
}

/// Every downloads badge in `text`, with its 1-based line number.
pub fn find_claims(text: &str) -> Vec<BadgeClaim> {
    let mut out = Vec::new();
    for (ln, line) in text.lines().enumerate() {
        for host in HOSTS {
            let mut rest = line;
            while let Some(pos) = rest.find(host) {
                let after = &rest[pos + host.len()..];
                let url_end = after
                    .find([')', '"', '\'', ' ', ']', '>'])
                    .unwrap_or(after.len());
                let path = &after[..url_end];
                rest = &after[url_end..];
                let segs: Vec<&str> = path.split('/').collect();
                if segs.len() < 3 {
                    continue;
                }
                let (eco, kind) = (segs[0], segs[1]);
                let claim = match eco {
                    "crates" if CRATE_KINDS.contains(&kind) => {
                        let pkg = clean_segment(segs[2]);
                        Some((Ecosystem::Crates, pkg))
                    }
                    "npm" if NPM_KINDS.contains(&kind) => {
                        let first = clean_segment(segs[2]);
                        // `%40scope%2Fname` (and `%252F`, shields' double encoding) is one segment.
                        let first = first.replace("%252F", "/").replace("%252f", "/");
                        let pkg = if first.starts_with('@') && first.contains('/') {
                            first
                        } else if first.starts_with('@') {
                            if segs.len() < 4 {
                                continue;
                            }
                            format!("{}/{}", first, clean_segment(segs[3]))
                        } else {
                            clean_segment(segs[2])
                        };
                        Some((Ecosystem::Npm, pkg))
                    }
                    _ => None,
                };
                if let Some((ecosystem, package)) = claim {
                    if !package.is_empty() {
                        out.push(BadgeClaim {
                            line: ln + 1,
                            ecosystem,
                            package,
                            kind: kind.to_string(),
                            url: format!("https://{host}{path}"),
                        });
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_crates_and_npm_badges() {
        let md = "# x\n[![dl](https://img.shields.io/crates/d/serde.svg)](https://crates.io/crates/serde)\n\
                  ![npm](https://img.shields.io/npm/dm/@prisma/client?style=flat) and <img src=\"https://badgen.net/npm/dw/chalk\">\n\
                  ![v](https://img.shields.io/crates/v/serde.svg) ![r](https://img.shields.io/crates/dr/tokio)";
        let c = find_claims(md);
        assert_eq!(c.len(), 4, "{c:?}");
        assert_eq!(c[0].package, "serde");
        assert_eq!(c[0].line, 2);
        assert_eq!(c[1].package, "@prisma/client");
        assert_eq!(c[1].ecosystem, Ecosystem::Npm);
        assert_eq!(c[2].package, "chalk");
        assert_eq!(c[3].package, "tokio");
        assert_eq!(c[3].kind, "dr");
    }

    #[test]
    fn version_badge_is_not_a_downloads_claim() {
        assert!(find_claims("![v](https://img.shields.io/crates/v/serde.svg)").is_empty());
        assert!(find_claims("![v](https://img.shields.io/npm/v/chalk)").is_empty());
    }

    #[test]
    fn fully_encoded_scope_in_one_segment() {
        let c = find_claims(
            "<img src=\"https://img.shields.io/npm/dw/%40libsql%2Fclient?color=0F624B\" />",
        );
        assert_eq!(c[0].package, "@libsql/client");
        let c = find_claims("[![x](https://img.shields.io/npm/dm/%40mastra%252Fcore)](https://x)");
        assert_eq!(c[0].package, "@mastra/core");
    }

    #[test]
    fn encoded_scope() {
        let c = find_claims("![x](https://img.shields.io/npm/dm/%40scope/pkg.svg)");
        assert_eq!(c[0].package, "@scope/pkg");
    }
}
