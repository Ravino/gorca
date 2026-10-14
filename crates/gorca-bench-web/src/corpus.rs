//! Corpus loader: reads `tests/web/b6/corpus.tsv` and renders each entry to HTML bytes.
//!
//! `gen_large` entries are generated here with the same templates as
//! `tests/web/pages/gen_large.py` (so the numbers are comparable with the earlier reports and no
//! Python is needed at run time).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Kind {
    GenLarge(u32),
    /// File under `tests/web/b6/pages/`, optionally with a query string.
    Page(String),
    /// File under `tests/web/pages/`.
    PagesRaw(String),
    /// Built SPA app: URL path on the SPA server.
    Spa(String),
    /// Generated bundle under `tests/web/b6/virt/dist/` (needs `npm install && node build.mjs`).
    Dist(String),
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub kind: Kind,
    /// Substring of the document title, used to find the right `document web` object.
    pub title: String,
    pub approx_nodes: u32,
    pub purpose: String,
}

#[derive(Debug)]
pub struct Corpus {
    pub root: PathBuf,
    pub entries: Vec<Entry>,
}

impl Corpus {
    /// `root` is the `tests/web` directory.
    pub fn load(root: &Path) -> Result<Self> {
        let manifest = root.join("b6/corpus.tsv");
        let text = std::fs::read_to_string(&manifest)
            .with_context(|| format!("reading {}", manifest.display()))?;
        let mut entries = Vec::new();
        for (ln, line) in text.lines().enumerate() {
            let line = line.trim_end();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() < 5 {
                bail!(
                    "{}:{}: expected at least 5 tab-separated columns",
                    manifest.display(),
                    ln + 1
                );
            }
            let kind = match f[1] {
                "gen_large" => Kind::GenLarge(f[2].parse().context("gen_large budget")?),
                "page" => Kind::Page(f[2].to_owned()),
                "pages_raw" => Kind::PagesRaw(f[2].to_owned()),
                "spa" => Kind::Spa(f[2].to_owned()),
                "dist" => Kind::Dist(f[2].to_owned()),
                other => bail!("{}:{}: unknown kind {other}", manifest.display(), ln + 1),
            };
            entries.push(Entry {
                name: f[0].to_owned(),
                kind,
                title: f[3].to_owned(),
                approx_nodes: f[4].parse().unwrap_or(0),
                purpose: f.get(5).copied().unwrap_or("").to_owned(),
            });
        }
        Ok(Self {
            root: root.to_owned(),
            entries,
        })
    }

    pub fn get(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.name == name)
    }

    /// The URL path (below the page server) and body for an entry. SPA entries have no body:
    /// they are served by the SPA server and return `None`.
    pub fn render(&self, e: &Entry) -> Result<Option<(String, Vec<u8>)>> {
        match &e.kind {
            Kind::GenLarge(n) => Ok(Some((
                format!("/pages/{}.html", e.name),
                gen_large(*n).into_bytes(),
            ))),
            Kind::Page(f) => {
                let (file, query) = f
                    .split_once('?')
                    .map_or((f.as_str(), None), |(a, b)| (a, Some(b)));
                let body = std::fs::read(self.root.join("b6/pages").join(file))
                    .with_context(|| format!("page {file}"))?;
                let path = match query {
                    Some(q) => format!("/pages/{file}?{q}"),
                    None => format!("/pages/{file}"),
                };
                Ok(Some((path, body)))
            }
            Kind::PagesRaw(f) => {
                let body = std::fs::read(self.root.join("pages").join(f))
                    .with_context(|| format!("page {f}"))?;
                Ok(Some((format!("/pages/{f}"), body)))
            }
            Kind::Dist(f) => {
                let file = f.split('?').next().unwrap_or(f);
                let query = f.split_once('?').map(|(_, q)| q);
                let body = std::fs::read(self.root.join("b6/virt/dist").join(file))
                    .with_context(|| format!("dist bundle {file} (run `npm install && node build.mjs` in tests/web/b6/virt)"))?;
                let path = match query {
                    Some(q) => format!("/pages/{file}?{q}"),
                    None => format!("/pages/{file}"),
                };
                Ok(Some((path, body)))
            }
            Kind::Spa(_) => Ok(None),
        }
    }
}

fn card(i: u32) -> String {
    format!(
        "<article aria-label=\"card {i}\"><h3>Item {i} heading</h3>\n\
<p>Paragraph one of card {i} with a <a href=\"#c{i}\">link {i}</a> and <strong>strong text</strong>.</p>\n\
<p>Second paragraph, plain text for card {i}.</p>\n\
<ul><li>alpha {i}</li><li>beta {i}</li><li>gamma {i}</li></ul>\n\
<button type=\"button\">Action {i}</button><img alt=\"image {i}\" src=\"data:image/gif;base64,R0lGODlhAQABAAAAACw=\"></article>\n"
    )
}

fn table(i: u32) -> String {
    let mut rows = String::new();
    for r in 0..10 {
        let _ = write!(
            rows,
            "<tr><th scope=row>r{r}</th><td>{i}.{r}.a</td><td>{i}.{r}.b</td></tr>"
        );
    }
    format!(
        "<table><caption>Table {i}</caption><thead><tr><th>h</th><th>a</th><th>b</th></tr></thead><tbody>{rows}</tbody></table>\n"
    )
}

fn form(i: u32) -> String {
    format!(
        "<form aria-label=\"form {i}\"><label for=\"t{i}\">Name {i}</label><input id=\"t{i}\" type=\"text\">\
<label><input type=\"checkbox\"> check {i}</label><select aria-label=\"sel {i}\"><option>one</option><option>two</option></select>\
<div role=\"combobox\" aria-expanded=\"false\" aria-label=\"combo {i}\" tabindex=\"0\"></div></form>\n"
    )
}

/// Same shape as `tests/web/pages/gen_large.py N`.
pub fn gen_large(n: u32) -> String {
    let cards = (n / 12).max(1);
    let mut s = String::with_capacity(n as usize * 60);
    let _ = write!(
        s,
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>large {n}</title></head><body>\n\
<header><nav aria-label=\"main\"><a href=\"#a\">A</a><a href=\"#b\">B</a></nav></header><main id=\"main\"><h1>Large page</h1>\n\
<div id=\"live\" role=\"status\" aria-live=\"polite\"></div>\n\
<div id=\"shadowhost\"></div><script>document.getElementById(\"shadowhost\").attachShadow({{mode:\"open\"}}).innerHTML=\"<h2>in shadow</h2><button>shadow button</button>\";</script>\n"
    );
    for i in 0..cards {
        s.push_str(&card(i));
        if i % 200 == 199 {
            s.push_str(&table(i));
            s.push_str(&form(i));
        }
    }
    s.push_str(
        "</main><footer><p>end</p></footer>\n<script>\n\
window.mut = {\n\
  addCards(k){const m=document.getElementById(\"main\");const f=document.createDocumentFragment();for(let i=0;i<k;i++){const a=document.createElement(\"article\");a.innerHTML=\"<h3>new \"+i+\"</h3><p>added paragraph</p>\";f.appendChild(a);}m.appendChild(f);},\n\
  rename(k){const bs=document.querySelectorAll(\"button\");for(let i=0;i<k&&i<bs.length;i++)bs[i].textContent=\"renamed \"+i;},\n\
  live(k){const l=document.getElementById(\"live\");for(let i=0;i<k;i++){setTimeout(()=>{l.textContent=\"msg \"+i;},i);}},\n\
  removeCards(k){const a=document.querySelectorAll(\"article\");for(let i=0;i<k&&i<a.length;i++)a[i].remove();},\n\
  route(){document.getElementById(\"main\").innerHTML=\"<h1>Route 2</h1><p>new view</p>\";history.pushState({},\"\",\"#route2\");document.title=\"route 2\";}\n\
};\n</script></body></html>",
    );
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_in_the_repo_parses() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/web");
        let c = Corpus::load(&root).unwrap();
        assert!(c.get("large10k").is_some());
        assert!(c.entries.len() >= 5);
        for e in &c.entries {
            if !matches!(e.kind, Kind::Spa(_)) {
                // Pages that are not written yet are reported by `b6 corpus`, not here.
                let _ = c.render(e);
            }
        }
    }

    #[test]
    fn generated_page_has_the_expected_shape() {
        let h = gen_large(1000);
        assert!(h.contains("<title>large 1000</title>"));
        assert_eq!(h.matches("<article").count(), 83);
        assert!(h.contains("window.mut"));
    }
}
