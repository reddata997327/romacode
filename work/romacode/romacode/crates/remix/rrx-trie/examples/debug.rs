use rrx_trie::Trie;

fn parse_kv(s: &str) -> Vec<u8> {
    if let Some(h) = s.strip_prefix("0x") {
        rrx_util::from_hex(h).unwrap()
    } else {
        s.as_bytes().to_vec()
    }
}

fn apply(t: &mut Trie, k: &str, v: Option<&str>) {
    let key = parse_kv(k);
    match v {
        Some(v) => t.insert(&key, &parse_kv(v)),
        None => t.remove(&key),
    }
}

fn main() {
    let cases: Vec<(&str, Vec<(&str, Option<&str>)>)> = vec![
        (
            "emptyValues",
            vec![
                ("do", Some("verb")),
                ("ether", Some("wookiedoo")),
                ("horse", Some("stallion")),
                ("shaman", Some("horse")),
                ("doge", Some("coin")),
                ("ether", None),
                ("dog", Some("puppy")),
                ("shaman", None),
            ],
        ),
        (
            "insertMiddle",
            vec![
                ("key1aa", Some("0123456789012345678901234567890123456789xxx")),
                ("key1", Some("0123456789012345678901234567890123456789Very_Long")),
                ("key2bb", Some("aval3")),
                ("key2", Some("short")),
                ("key3cc", Some("aval3")),
                ("key3", Some("1234567890123456789012345678901")),
            ],
        ),
        ("branchValueUpdate", vec![("abc", Some("123")), ("abcd", Some("abcd")), ("abc", Some("abc"))]),
    ];
    for (name, ops) in cases {
        let mut t = Trie::new();
        let mut roots = vec![];
        for (k, v) in ops {
            apply(&mut t, k, v);
            roots.push(rrx_util::to_hex(t.root_hash().as_bytes()));
        }
        println!("{} final: 0x{}", name, roots.last().unwrap());
        println!(
            "{} steps: {}",
            name,
            roots.iter().map(|r| format!("0x{}", r)).collect::<Vec<_>>().join(",")
        );
    }
}
