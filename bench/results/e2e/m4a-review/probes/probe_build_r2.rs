
/// Reviewer probe: RR-1 on an unchanged app with a text zone (line rect != node bounds).
#[tokio::test]
async fn probe_r2_rr1_unchanged_text_node_reads_as_changed() {
    let mut s = form(1);
    let mut label = snap(9, Role::Label, "", Some(2), &[], rect(0, 100, 200, 40));
    label.interfaces = Ifaces::TEXT;
    s.nodes.insert(key(9), label);
    if let Some(ChildList::Known(k)) = s.nodes.get_mut(&key(2)).map(|n| &mut n.children) {
        k.push(key(9));
    }
    s.lines.insert(
        key(9),
        vec![
            TextLine { range: 0..5, text: Arc::from("first"), rect: rect(0, 100, 120, 20) },
            TextLine { range: 6..12, text: Arc::from("second"), rect: rect(0, 120, 140, 20) },
        ],
    );
    let f = fake(s);
    let m = build_model(&f.io, spec(None, None)).await.unwrap();
    // Exactly what Reader::maybe_revalidate sends for the line holding "first".
    let line = m.lines.iter().position(|l| &*l.text == "first").unwrap();
    let probe: Vec<ProbeItem> = m
        .line_zones(line)
        .iter()
        .take(8)
        .map(|z| ProbeItem { node: z.node, rect: m.node_rect(z.node), lines: m.texts.get(&z.node).cloned() })
        .collect();
    let clip = m.clip;
    let v = revalidate(&f.io, probe, clip).await;
    eprintln!("PROBE rr1-text: verdict on an unchanged app = {v:?}");
    assert_eq!(v, RrVerdict::Same, "nothing changed in the app");
}
