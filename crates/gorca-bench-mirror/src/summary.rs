//! Aggregates the CSVs written by `mb1 run` into `summary.md`: medians over the interleaved
//! repeats, plus the min-max range of the headline tail so noise is visible.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

type Row = BTreeMap<String, String>;

fn read_csv(p: &Path) -> Vec<Row> {
    let text = std::fs::read_to_string(p).unwrap_or_default();
    let mut lines = text.lines();
    let Some(h) = lines.next() else {
        return Vec::new();
    };
    let cols: Vec<&str> = h.split(',').collect();
    lines
        .map(|l| {
            cols.iter()
                .map(|c| (*c).to_string())
                .zip(l.split(',').map(str::to_string))
                .collect()
        })
        .collect()
}

fn num(r: &Row, k: &str) -> f64 {
    r.get(k).and_then(|v| v.parse().ok()).unwrap_or(0.0)
}

fn median(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

struct Agg<'a>(Vec<&'a Row>);
impl Agg<'_> {
    fn med(&self, k: &str) -> f64 {
        median(&mut self.0.iter().map(|r| num(r, k)).collect::<Vec<_>>())
    }
    fn range(&self, k: &str) -> (f64, f64) {
        let v: Vec<f64> = self.0.iter().map(|r| num(r, k)).collect();
        (
            v.iter().copied().fold(f64::MAX, f64::min),
            v.iter().copied().fold(0.0, f64::max),
        )
    }
}

fn group<'a>(rows: &'a [Row], keys: &[&str]) -> BTreeMap<Vec<String>, Agg<'a>> {
    let mut m: BTreeMap<Vec<String>, Agg<'a>> = BTreeMap::new();
    for r in rows {
        let k: Vec<String> = keys
            .iter()
            .map(|k| r.get(*k).cloned().unwrap_or_default())
            .collect();
        m.entry(k).or_insert_with(|| Agg(Vec::new())).0.push(r);
    }
    m
}

fn rate_order(r: &str) -> u64 {
    match r {
        "max" => u64::MAX,
        n => n.parse().unwrap_or(0),
    }
}

pub fn write(dir: &Path) -> Result<(), String> {
    let mut md = String::new();
    let _ = writeln!(md, "# MB-1 summary (medians over interleaved repeats)\n");
    let _ = writeln!(
        md,
        "{}\n",
        std::fs::read_to_string(dir.join("meta.txt"))
            .unwrap_or_default()
            .lines()
            .map(|l| format!("    {l}"))
            .collect::<Vec<_>>()
            .join("\n")
    );

    let steady = read_csv(&dir.join("steady.csv"));
    if !steady.is_empty() {
        let _ = writeln!(
            md,
            "## Steady state: reader command = 4 lookups; latency in ns, vis in us\n"
        );
        let g = group(&steady, &["size", "rate", "store"]);
        let mut cells: Vec<(u64, u64)> = g
            .keys()
            .map(|k| (k[0].parse().unwrap_or(0), rate_order(&k[1])))
            .collect();
        cells.sort_unstable();
        cells.dedup();
        for (size, ro) in cells {
            let rate = if ro == u64::MAX {
                "max".to_string()
            } else {
                ro.to_string()
            };
            let _ = writeln!(md, "### size {size}, write rate {rate}/s\n");
            let _ = writeln!(
                md,
                "| store | cmds/s (M) | p50 | p99 | p99 min-max | p999 | max | writes/s | batch p99 (us) | vis p50 (us) | vis p99 (us) | vis p999 (us) | cpu (cores) | writer cpu | n |"
            );
            let _ = writeln!(
                md,
                "|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|"
            );
            for (k, a) in g
                .iter()
                .filter(|(k, _)| k[0] == size.to_string() && k[1] == rate)
            {
                let (lo, hi) = a.range("r_p99");
                let _ = writeln!(
                    md,
                    "| {} | {:.2} | {:.0} | {:.0} | {:.0}-{:.0} | {:.0} | {:.0} | {:.0} | {:.1} | {:.1} | {:.1} | {:.1} | {:.2} | {:.2} | {} |",
                    k[2],
                    a.med("cmds_per_s") / 1e6,
                    a.med("r_p50"),
                    a.med("r_p99"),
                    lo,
                    hi,
                    a.med("r_p999"),
                    a.med("r_max"),
                    a.med("writes_per_s"),
                    a.med("b_p99") / 1e3,
                    a.med("vis_p50") / 1e3,
                    a.med("vis_p99") / 1e3,
                    a.med("vis_p999") / 1e3,
                    a.med("proc_cpu"),
                    a.med("writer_cpu"),
                    a.0.len()
                );
            }
            let _ = writeln!(md);
        }
    }

    let rem = read_csv(&dir.join("removal.csv"));
    if !rem.is_empty() {
        let _ = writeln!(
            md,
            "## 100k-node subtree removal (tree of 200k nodes, readers closed-loop); ns\n"
        );
        let _ = writeln!(
            md,
            "| store | variant | removal (ms) | survivors | cmds/s base (M) | cmds/s during (M) | base p99 | during p50 | during p99 | during p999 | during max | after p99 | n |"
        );
        let _ = writeln!(md, "|---|---|---|---|---|---|---|---|---|---|---|---|---|");
        for (k, a) in group(&rem, &["store", "variant"]) {
            let _ = writeln!(
                md,
                "| {} | {} | {:.1} | {:.0} | {:.2} | {:.2} | {:.0} | {:.0} | {:.0} | {:.0} | {:.0} | {:.0} | {} |",
                k[0],
                k[1],
                a.med("removal_ms"),
                a.range("survivors").1,
                a.med("base_cmds_per_s") / 1e6,
                a.med("during_cmds_per_s") / 1e6,
                a.med("base_p99"),
                a.med("during_p50"),
                a.med("during_p99"),
                a.med("during_p999"),
                a.med("during_max"),
                a.med("after_p99"),
                a.0.len()
            );
        }
        let _ = writeln!(md);
    }

    let rss = read_csv(&dir.join("rss.csv"));
    if !rss.is_empty() {
        let _ = writeln!(
            md,
            "## RSS per node (bytes; child process, growth over build; `floor` = payload only)\n"
        );
        let g = group(&rss, &["size", "store"]);
        let floors: BTreeMap<String, f64> = g
            .iter()
            .filter(|(k, _)| k[1] == "floor")
            .map(|(k, a)| (k[0].clone(), a.med("rss_bytes")))
            .collect();
        let _ = writeln!(
            md,
            "| size | store | total B/node | overhead over payload B/node |"
        );
        let _ = writeln!(md, "|---|---|---|---|");
        let mut keys: Vec<_> = g.keys().cloned().collect();
        keys.sort_by_key(|k| (k[0].parse::<u64>().unwrap_or(0), k[1].clone()));
        for k in keys {
            let a = &g[&k];
            let n: f64 = k[0].parse().unwrap_or(1.0);
            let tot = a.med("rss_bytes");
            let over = tot - floors.get(&k[0]).copied().unwrap_or(0.0);
            let _ = writeln!(
                md,
                "| {} | {} | {:.1} | {:.1} |",
                k[0],
                k[1],
                tot / n,
                over / n
            );
        }
    }
    std::fs::write(dir.join("summary.md"), md).map_err(|e| e.to_string())
}
