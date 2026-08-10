//! TEMPORARY empirical probe — deleted before commit.

#[test]
fn probe_order_preservation() {
    // Is toml::Table insertion-ordered (indexmap) or sorted (BTreeMap)?
    let mut t = toml::Table::new();
    t.insert("zebra".into(), toml::Value::Integer(1));
    t.insert("alpha".into(), toml::Value::Integer(2));
    t.insert("middle".into(), toml::Value::Integer(3));
    let keys: Vec<&str> = t.keys().map(String::as_str).collect();
    println!("P1_ITER_ORDER: {keys:?}  (insertion=zebra,alpha,middle)");
    println!("P1_RENDER>>>\n{}<<<", toml::to_string_pretty(&t).unwrap());
}

#[test]
fn probe_scalar_after_table_deep() {
    // The load-bearing question: can a merge produce a body where a bare
    // key = value follows a [header] and thus re-parses WRONG?
    // Build worst case: nested tables, then insert scalars AFTER them.
    let mut inner = toml::Table::new();
    inner.insert("t_sub".into(), {
        let mut s = toml::Table::new();
        s.insert("deep".into(), toml::Value::Integer(1));
        toml::Value::Table(s)
    });
    // scalar inserted AFTER the sub-table at this level
    inner.insert("zz_scalar_after".into(), toml::Value::Integer(2));

    let mut root = toml::Table::new();
    root.insert("a_table".into(), toml::Value::Table(inner));
    root.insert("zz_root_scalar".into(), toml::Value::Integer(3));
    root.insert(
        "aot".into(),
        toml::Value::Array(vec![toml::Value::Table({
            let mut m = toml::Table::new();
            m.insert("x".into(), toml::Value::Integer(1));
            m
        })]),
    );
    root.insert("zz_after_aot".into(), toml::Value::Integer(4));

    let body = toml::to_string_pretty(&root).expect("ser");
    println!("P2_RENDER>>>\n{body}<<<");
    let round: toml::Table = body.parse().expect("P2 MUST re-parse");
    println!("P2_ROUNDTRIP_EQUAL: {}", round == root);
    assert_eq!(round, root, "serializer must emit a re-parseable document");
}

#[test]
fn probe_empty_table_and_edge() {
    // An empty sub-table, and a table whose value is an empty AoT.
    let mut root = toml::Table::new();
    root.insert("empty_tbl".into(), toml::Value::Table(toml::Table::new()));
    root.insert("empty_arr".into(), toml::Value::Array(vec![]));
    root.insert("z_scalar".into(), toml::Value::Integer(1));
    let body = toml::to_string_pretty(&root).expect("ser");
    println!("P3_RENDER>>>\n{body}<<<");
    match body.parse::<toml::Table>() {
        Ok(r) => println!("P3_REPARSE OK equal={}", r == root),
        Err(e) => println!("P3_REPARSE ERR {e}"),
    }
}
