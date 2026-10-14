import sys
p=sys.argv[1]
s=open(p).read()
anchor='    out.x("not_consumed", not_consumed as f64, "events");\n'
assert anchor in s
diag='''    if let Ok(dir) = std::env::var("M4_DIAG_DIR") {
        use std::io::Write as _;
        let path = format!("{dir}/{metric}-{}.tsv", emits.first().copied().unwrap_or(0));
        if let Ok(mut fh) = std::fs::File::create(&path) {
            let _ = writeln!(fh, "# lost={lost} n={n}");
            for (i, t) in emits.iter().enumerate() {
                let _ = writeln!(fh, "K\\t{i}\\t{t}\\t{}", if i % 2 == 0 { "prev" } else { "next" });
            }
            for r in &recs {
                match &r.kind {
                    super::mock::Kind::Speak { text, t_verb_ns } => {
                        let _ = writeln!(fh, "S\\t{}\\t{}\\t{}", r.t_ns, t_verb_ns, text.replace('\\t', " "));
                    }
                    super::mock::Kind::Cancel { verb, had_speech } => {
                        let _ = writeln!(fh, "C\\t{}\\t{verb}\\t{had_speech}", r.t_ns);
                    }
                    super::mock::Kind::Connected => {}
                }
            }
        }
    }
'''
s=s.replace(anchor, anchor+diag)
open(p,'w').write(s)
