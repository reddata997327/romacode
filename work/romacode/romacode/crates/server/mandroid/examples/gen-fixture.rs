//! Generate `testdata/test-app.apk` — the engine's own test fixture.
//!
//! Everything in the APK is produced by this engine: the manifest is encoded
//! by [`mandroid::axmlenc`], the resource table is written to the exact
//! layout [`mandroid::arsc`] decodes, the DEX is assembled to be fully
//! disassemblable by [`mandroid::smali`], and the container is signed with
//! [`mandroid::sign`] (v1 + v2). Run with:
//!
//! ```text
//! cargo run --example gen-fixture
//! ```
//!
//! The generator doubles as the fixture validator: after signing it decodes,
//! disassembles and verifies the result, failing loudly on any mismatch —
//! a broken fixture can never be checked in silently.

use std::path::Path;

use mandroid::axml::{Attr, AttrValue, Node};
use mandroid::sign::ApkSigner;

const PACKAGE: &str = "org.fdroid.fdroid";
const APP_LABEL: &str = "Test App";
const ANDROID_NS: &str = "http://schemas.android.com/apk/res/android";

fn main() -> anyhow::Result<()> {
    let out_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata");
    std::fs::create_dir_all(&out_dir)?;
    let unsigned = out_dir.join("test-app-unsigned.apk");
    let signed = out_dir.join("test-app.apk");

    // 1. Manifest (AXML via the engine's own encoder) with a round-trip check.
    let axml = mandroid::axmlenc::encode(&build_manifest())?;
    let decoded = mandroid::axml::decode(&axml)?;
    assert_eq!(decoded.name, "manifest");
    assert_eq!(decoded.attr_str("package").as_deref(), Some(PACKAGE));

    // 2. resources.arsc written to the exact layout arsc::decode expects.
    let arsc = build_arsc();

    let table = mandroid::arsc::decode(&arsc)?;
    assert_eq!(table.packages[0].name, PACKAGE);
    assert!(table.global_strings.len() > 1000, "global pool too small");
    let label = mandroid::arsc::app_label(&table).expect("app label must resolve");
    assert_eq!(label, APP_LABEL, "app label mismatch: {label:?}");

    // 3. classes.dex — small, valid, and fully disassemblable.
    let dex = build_dex();
    let classes = mandroid::smali::disassemble(&dex)?;
    assert!(
        classes.len() >= 3,
        "expected >= 3 classes, got {}",
        classes.len()
    );
    assert!(
        classes.iter().any(|c| c.name.contains("org/fdroid/fdroid")),
        "fixture classes must carry the org/fdroid/fdroid package"
    );
    for c in &classes {
        assert!(
            !c.source.contains("# unknown opcode") && !c.source.contains("(undecoded)"),
            "fixture dex must disassemble cleanly:\n{}",
            c.source
        );
    }

    // 4. Assemble the unsigned container (13 entries — all get v1 signatures).
    let entries: Vec<(&str, Vec<u8>)> = vec![
        ("AndroidManifest.xml", axml),
        ("resources.arsc", arsc),
        ("classes.dex", dex),
        ("res/layout/activity_main.xml", small_xml("linear-layout")),
        ("res/xml/backup_rules.xml", small_xml("full-backup-content")),
        (
            "res/xml/data_extraction_rules.xml",
            small_xml("data-extraction-rules"),
        ),
        ("res/values/strings.xml", small_xml("resources")),
        (
            "assets/config.json",
            br#"{"feature_flags":{"sim_roster":true},"version":1}"#.to_vec(),
        ),
        ("assets/readme.txt", b"mandroid test fixture\n".to_vec()),
        (
            "assets/sim/carriers.json",
            br#"{"US":3,"GB":4,"PK":4}"#.to_vec(),
        ),
        ("ok.txt", b"ok\n".to_vec()),
        (
            "META-INF/services/java.util.PropertyResourceBundle",
            b"# fixture\n".to_vec(),
        ),
        ("unknown/opaque.bin", vec![0u8; 256]),
    ];
    write_zip(&unsigned, &entries)?;

    // 5. Sign (v1 + v2) with the engine's signer.
    let signer = ApkSigner::generate("mandroid-test")?;
    let report = mandroid::sign::sign_apk(
        &unsigned,
        &signed,
        &signer,
        &mandroid::sign::SignOptions::default(),
    )?;
    println!(
        "signed: v1={} v2={} entries={} sha256={}",
        report.v1, report.v2, report.entries, report.sha256
    );

    // 6. Verify the shipped artifact end to end.
    let v2 = mandroid::sign::verify_v2(&signed)?;
    assert!(
        v2.found && v2.valid(),
        "v2 signature must verify: {:?}",
        v2.errors
    );
    let v1 = mandroid::sign::verify_v1(&signed)?;
    assert!(
        v1.has_manifest && v1.has_sf && v1.has_block,
        "v1 layout incomplete"
    );
    assert!(v1.signed_entries >= 10, "v1 must cover the payload entries");
    assert!(
        v1.declared_schemes.contains(&2),
        "v1 must declare v2 (downgrade protection), got {:?}",
        v1.declared_schemes
    );
    let info = mandroid::api::info(signed.to_string_lossy().as_ref())?;
    assert_eq!(info["package"], PACKAGE);
    println!(
        "fixture ok: {} entries, package {}, {} bytes -> {}",
        info["entries"],
        info["package"],
        std::fs::metadata(&signed)?.len(),
        signed.display()
    );
    Ok(())
}

// ---------------------------------------------------------------- manifest ---

fn attr(name: &str, value: AttrValue) -> Attr {
    Attr {
        namespace: Some(ANDROID_NS.into()),
        name: name.into(),
        value,
    }
}

fn build_manifest() -> Node {
    let uses_sdk = Node {
        name: "uses-sdk".into(),
        namespace: None,
        attrs: vec![
            attr("minSdkVersion", AttrValue::Int(21)),
            attr("targetSdkVersion", AttrValue::Int(34)),
        ],
        children: vec![],
        line: 0,
    };
    let activity = Node {
        name: "activity".into(),
        namespace: None,
        attrs: vec![
            attr(
                "name",
                AttrValue::Str("org.fdroid.fdroid.MainActivity".into()),
            ),
            attr("exported", AttrValue::Bool(true)),
        ],
        children: vec![Node {
            name: "intent-filter".into(),
            namespace: None,
            attrs: vec![],
            children: vec![
                Node {
                    name: "action".into(),
                    namespace: None,
                    attrs: vec![attr(
                        "name",
                        AttrValue::Str("android.intent.action.MAIN".into()),
                    )],
                    children: vec![],
                    line: 0,
                },
                Node {
                    name: "category".into(),
                    namespace: None,
                    attrs: vec![attr(
                        "name",
                        AttrValue::Str("android.intent.category.LAUNCHER".into()),
                    )],
                    children: vec![],
                    line: 0,
                },
            ],
            line: 0,
        }],
        line: 0,
    };
    let application = Node {
        name: "application".into(),
        namespace: None,
        attrs: vec![
            attr("label", AttrValue::Str(APP_LABEL.into())),
            attr("allowBackup", AttrValue::Bool(true)),
            attr("debuggable", AttrValue::Bool(false)),
        ],
        children: vec![activity],
        line: 0,
    };
    // A realistic spread of components and permissions so the tree is
    // non-trivial for the encoder round trip.
    let permissions: Vec<Node> = [
        "android.permission.INTERNET",
        "android.permission.ACCESS_NETWORK_STATE",
        "android.permission.ACCESS_WIFI_STATE",
        "android.permission.POST_NOTIFICATIONS",
        "android.permission.RECEIVE_BOOT_COMPLETED",
        "android.permission.FOREGROUND_SERVICE",
    ]
    .iter()
    .map(|p| Node {
        name: "uses-permission".into(),
        namespace: None,
        attrs: vec![attr("name", AttrValue::Str((*p).to_string()))],
        children: vec![],
        line: 0,
    })
    .collect();
    let service = Node {
        name: "service".into(),
        namespace: None,
        attrs: vec![attr(
            "name",
            AttrValue::Str("org.fdroid.fdroid.SyncService".into()),
        )],
        children: vec![],
        line: 0,
    };
    let receiver = Node {
        name: "receiver".into(),
        namespace: None,
        attrs: vec![attr(
            "name",
            AttrValue::Str("org.fdroid.fdroid.BootReceiver".into()),
        )],
        children: vec![Node {
            name: "intent-filter".into(),
            namespace: None,
            attrs: vec![],
            children: vec![Node {
                name: "action".into(),
                namespace: None,
                attrs: vec![attr(
                    "name",
                    AttrValue::Str("android.intent.action.BOOT_COMPLETED".into()),
                )],
                children: vec![],
                line: 0,
            }],
            line: 0,
        }],
        line: 0,
    };
    Node {
        name: "manifest".into(),
        namespace: None,
        attrs: vec![
            attr("versionCode", AttrValue::Int(1010)),
            attr("versionName", AttrValue::Str("1.1.0".into())),
            Attr {
                namespace: None,
                name: "package".into(),
                value: AttrValue::Str(PACKAGE.into()),
            },
        ],
        children: [
            vec![uses_sdk],
            permissions,
            vec![application, service, receiver],
        ]
        .concat(),
        line: 0,
    }
}

// --------------------------------------------------------------------- arsc ---

/// Build a minimal, fully valid resources.arsc:
/// table -> global pool -> package { typeStrings, keyStrings, spec, type }.
/// The global pool carries the app strings first (their pool index becomes
/// the `data` of each TYPE_STRING value) plus 1100 filler entries, mirroring
/// the large pools real apps ship.
fn build_arsc() -> Vec<u8> {
    let mut app_strings: Vec<String> = vec![
        APP_LABEL.into(),
        "Hello, MANDROID".into(),
        "SIM roster".into(),
        "Configure the online SIM".into(),
        "spare".into(),
    ];
    for i in 0..1100 {
        app_strings.push(format!("fixture-string-{i:04}"));
    }
    let key_strings: Vec<String> = (0..5)
        .map(|i| {
            [
                "app_name",
                "hello_world",
                "sim_title",
                "sim_hint",
                "unused_key",
            ][i]
                .into()
        })
        .collect();
    let type_strings: Vec<String> = vec!["string".into()];

    let global_pool = write_string_pool_utf8(&app_strings);
    let type_pool = write_string_pool_utf8(&type_strings);
    let key_pool = write_string_pool_utf8(&key_strings);

    // Type spec: one u32 flags word per entry (0 = simple everywhere).
    let entry_count = key_strings.len() as u32;
    let mut spec = Vec::new();
    spec.extend_from_slice(&0x0202u16.to_le_bytes());
    spec.extend_from_slice(&16u16.to_le_bytes());
    spec.extend_from_slice(&(16u32 + entry_count * 4).to_le_bytes());
    spec.push(1); // type id 1 -> typeStrings[0] = "string"
    spec.push(0); // res0
    spec.extend_from_slice(&0u16.to_le_bytes()); // reserved
    spec.extend_from_slice(&entry_count.to_le_bytes());
    for _ in 0..entry_count {
        spec.extend_from_slice(&0u32.to_le_bytes());
    }

    // Type chunk (default config): offsets then 16-byte entries.
    let entry_size = 16u32;
    let header_size = 40u32; // 20 fixed + 20-byte config
    let offsets_size = entry_count * 4;
    let entries_start = header_size + offsets_size;
    let mut tc = Vec::new();
    tc.extend_from_slice(&0x0201u16.to_le_bytes());
    tc.extend_from_slice(&(header_size as u16).to_le_bytes());
    tc.extend_from_slice(&(entries_start + entry_count * entry_size).to_le_bytes());
    tc.push(1); // type id
    tc.push(0); // flags: not sparse, not offset16
    tc.extend_from_slice(&0u16.to_le_bytes()); // reserved
    tc.extend_from_slice(&entry_count.to_le_bytes());
    tc.extend_from_slice(&entries_start.to_le_bytes());
    tc.extend_from_slice(&20u32.to_le_bytes()); // config size
    tc.extend_from_slice(&[0u8; 16]); // default config
    for i in 0..entry_count {
        tc.extend_from_slice(&(i * entry_size).to_le_bytes());
    }
    for (key_idx, _) in key_strings.iter().enumerate() {
        // Res_entry { size=8, flags=0, key_idx } + Res_value { size=8, res0=0, dataType=STRING, data=pool_idx }
        tc.extend_from_slice(&8u16.to_le_bytes());
        tc.extend_from_slice(&0u16.to_le_bytes());
        tc.extend_from_slice(&(key_idx as u32).to_le_bytes());
        tc.extend_from_slice(&8u16.to_le_bytes());
        tc.push(0);
        tc.push(0x03); // TYPE_STRING
        tc.extend_from_slice(&(key_idx as u32).to_le_bytes());
    }
    debug_assert_eq!(tc.len() as u32, entries_start + entry_count * entry_size);

    // Package: id(4) name(256) typeStrings@ lastPublicType keyStrings@ lastPublicKey.
    let type_pool_off = 288u32;
    let key_pool_off = type_pool_off + type_pool.len() as u32;
    let total = key_pool_off + key_pool.len() as u32 + spec.len() as u32 + tc.len() as u32;
    let mut pkg = Vec::new();
    pkg.extend_from_slice(&0x0200u16.to_le_bytes());
    pkg.extend_from_slice(&288u16.to_le_bytes());
    pkg.extend_from_slice(&total.to_le_bytes());
    pkg.extend_from_slice(&0x7fu32.to_le_bytes());
    let mut name = vec![0u8; 256];
    for (i, u) in PACKAGE.encode_utf16().enumerate() {
        name[i * 2..i * 2 + 2].copy_from_slice(&u.to_le_bytes());
    }
    pkg.extend_from_slice(&name);
    pkg.extend_from_slice(&type_pool_off.to_le_bytes());
    pkg.extend_from_slice(&0u32.to_le_bytes());
    pkg.extend_from_slice(&key_pool_off.to_le_bytes());
    pkg.extend_from_slice(&0u32.to_le_bytes());
    // sizeof(ResTable_package) is 288: 4 bytes of struct padding close the
    // header before the pools begin.
    pkg.extend_from_slice(&[0u8; 4]);
    pkg.extend_from_slice(&type_pool);
    pkg.extend_from_slice(&key_pool);
    pkg.extend_from_slice(&spec);
    pkg.extend_from_slice(&tc);

    // Table header.
    let mut out = Vec::new();
    out.extend_from_slice(&0x0002u16.to_le_bytes());
    out.extend_from_slice(&12u16.to_le_bytes());
    out.extend_from_slice(&(12u32 + global_pool.len() as u32 + pkg.len() as u32).to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes()); // packageCount
    out.extend_from_slice(&global_pool);
    out.extend_from_slice(&pkg);
    out
}

/// UTF-8 string pool: header(28) + offsets + data. Layout per the decoder:
/// count@+8, styleCount@+12, flags@+16, stringsStart@+20, stylesStart@+24.
fn write_string_pool_utf8(strings: &[String]) -> Vec<u8> {
    let count = strings.len() as u32;
    let header_size = 28u32;
    let strings_start = header_size + count * 4;

    let mut offsets = Vec::with_capacity(strings.len() * 4);
    let mut data = Vec::new();
    for s in strings {
        offsets.push(data.len() as u32);
        write_uleb128(&mut data, s.chars().count() as u32); // utf16 len
        write_uleb128(&mut data, s.len() as u32); // byte len
        data.extend_from_slice(s.as_bytes());
        data.push(0);
    }
    while data.len() % 4 != 0 {
        data.push(0);
    }

    let mut out = Vec::new();
    out.extend_from_slice(&0x0001u16.to_le_bytes());
    out.extend_from_slice(&28u16.to_le_bytes());
    out.extend_from_slice(&(strings_start + data.len() as u32).to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // styleCount
    out.extend_from_slice(&0x100u32.to_le_bytes()); // UTF8_FLAG
    out.extend_from_slice(&strings_start.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // stylesStart
    for o in &offsets {
        out.extend_from_slice(&o.to_le_bytes());
    }
    out.extend_from_slice(&data);
    out
}

fn write_uleb128(out: &mut Vec<u8>, mut v: u32) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn small_xml(root: &str) -> Vec<u8> {
    format!("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<{root}/>\n").into_bytes()
}

// ---------------------------------------------------------------------- dex ---

/// Assemble a small, valid DEX (035): three classes (`org.fdroid.fdroid.*`)
/// with `()V`-only prototypes (no type lists needed), code bodies covering
/// the const/const-string/invoke/return/arithmetic opcode families, and
/// absolute code offsets in the class_data (per the spec and the
/// disassembler). No map_list, no fields — neither the parser nor the
/// renderer reads them.
///
/// Layout: header | string_ids | type_ids | proto_ids | method_ids |
/// class_defs | string data | class_data (4-aligned) | code items (4-aligned).
/// The class_data blobs encode absolute code offsets, so blob length and code
/// placement are mutually dependent; the builder iterates to a fixpoint.
fn build_dex() -> Vec<u8> {
    // ---- string pool -------------------------------------------------------
    // Indices are baked into every other table, so the order is fixed.
    let strings: Vec<String> = vec![
        "Lorg/fdroid/fdroid/FdroidApp;".into(),    // 0
        "Lorg/fdroid/fdroid/MainActivity;".into(), // 1
        "Lorg/fdroid/fdroid/Util;".into(),         // 2
        "Landroid/app/Activity;".into(),           // 3
        "Landroid/app/Application;".into(),        // 4
        "Ljava/lang/Object;".into(),               // 5
        "V".into(),                                // 6
        "Landroid/util/Log;".into(),               // 7
        "<init>".into(),                           // 8
        "onCreate".into(),                         // 9
        "d".into(),                                // 10
        "sum".into(),                              // 11
        "MANDROID".into(),                         // 12
        "sim roster ready".into(),                 // 13
    ];
    let s_init = 8u32;
    let s_on_create = 9u32;
    let s_d = 10u32;
    let s_sum = 11u32;
    let s_shorty_v = 6u32; // "V" doubles as the ()V shorty
    let s_tag = 12u32;
    let s_msg = 13u32;

    // Types: type_ids[i] is a string index.
    let types: [u32; 8] = [0, 1, 2, 3, 4, 5, 6, 7];
    let (t_app, t_main, t_util, t_activity, t_application, t_object, t_void, t_log) =
        (0u32, 1u32, 2u32, 3u32, 4u32, 5u32, 6u32, 7u32);
    let _ = (t_object, t_void);

    // One proto: ()V -> shorty "V", return V, params_off 0 (no type list).
    let protos: Vec<(u32, u32, u32)> = vec![(s_shorty_v, t_void, 0)];

    // Method ids.
    let methods: Vec<(u32, u32, u32)> = vec![
        (t_app, 0, s_init),         // 0 FdroidApp.<init>()V
        (t_app, 0, s_on_create),    // 1 FdroidApp.onCreate()V
        (t_main, 0, s_init),        // 2 MainActivity.<init>()V
        (t_main, 0, s_on_create),   // 3 MainActivity.onCreate()V
        (t_log, 0, s_d),            // 4 Log.d()V (external)
        (t_util, 0, s_sum),         // 5 Util.sum()V
        (t_application, 0, s_init), // 6 Application.<init>()V (super)
        (t_activity, 0, s_init),    // 7 Activity.<init>()V (super)
    ];

    // Instruction streams (16-bit units; insns_size is in units).
    // 35c: word1 = (A<<12)|op, word2 = meth@, word3 = C|D<<4|E<<8|F<<12.
    let code: Vec<Vec<u16>> = vec![
        // app.<init>: invoke-direct {v0} m@6 (super); return-void
        vec![0x1070, 0x0006, 0x0000, 0x000e],
        // app.onCreate: const-string v1; const-string v2;
        //               invoke-virtual {v1,v2} m@4 (Log.d); return-void
        vec![
            0x011a,
            s_tag as u16,
            0x021a,
            s_msg as u16,
            0x206e,
            0x0004,
            0x0021,
            0x000e,
        ],
        // main.<init>: invoke-direct {v0} m@7 (super); return-void
        vec![0x1070, 0x0007, 0x0000, 0x000e],
        // main.onCreate: two const-strings; invoke-virtual {v1,v2} m@4;
        //                const/4 v0 1; return v0
        vec![
            0x011a,
            s_tag as u16,
            0x021a,
            s_msg as u16,
            0x206e,
            0x0004,
            0x0021,
            0x1012,
            0x000f,
        ],
        // util.sum: const/4 v1 2; const/4 v2 3; add-int v0,v1,v2;
        //           if-eqz v0, +2; const/4 v0 0; return v0
        vec![
            0x2112, 0x3212, 0x0090, 0x0201, 0x0138, 0x0002, 0x0012, 0x000f,
        ],
    ];
    let code_method: [u32; 5] = [0, 1, 2, 3, 5];
    let owner: [u32; 5] = [0, 0, 1, 1, 2];
    let direct: [bool; 5] = [true, false, true, false, true];

    // ---- static layout ------------------------------------------------------
    let header_size = 0x70u32;
    let string_ids_off = header_size;
    let string_ids_size = strings.len() as u32;
    let type_ids_off = string_ids_off + string_ids_size * 4;
    let type_ids_size = types.len() as u32;
    let proto_ids_off = type_ids_off + type_ids_size * 4;
    let proto_ids_size = protos.len() as u32;
    let field_ids_off = proto_ids_off + proto_ids_size * 12;
    let field_ids_size = 0u32;
    let method_ids_off = field_ids_off + field_ids_size * 8;
    let method_ids_size = methods.len() as u32;
    let class_defs_off = method_ids_off + method_ids_size * 8;
    let class_defs_size = 3u32;
    let string_data_base = class_defs_off + class_defs_size * 32;

    let mut string_data: Vec<u8> = Vec::new();
    let mut string_offsets: Vec<u32> = Vec::new();
    for s in &strings {
        string_offsets.push(string_data.len() as u32);
        write_uleb128(&mut string_data, s.chars().count() as u32);
        string_data.extend_from_slice(s.as_bytes());
        string_data.push(0);
    }
    let class_data_base = string_data_base + string_data.len() as u32;

    let class_data_len_for = |code_offsets: &[u32]| -> u32 {
        let mut n = 0u32;
        for ci in 0..3u32 {
            n = (n + 3) & !3; // 4-align each blob
            let own: Vec<usize> = (0..code.len()).filter(|k| owner[*k] == ci).collect();
            let _dcount = own.iter().filter(|k| direct[**k]).count();
            n += 4; // the four leading uleb counts (all < 128 here)
            let mut prev = 0u32;
            for k in own.iter().filter(|k| direct[**k]) {
                let m = code_method[*k];
                n += uleb_len(m - prev) + 1 + uleb_len(code_offsets[*k]);
                prev = m;
            }
            let mut prev = 0u32;
            for k in own.iter().filter(|k| !direct[**k]) {
                let m = code_method[*k];
                n += uleb_len(m - prev) + 1 + uleb_len(code_offsets[*k]);
                prev = m;
            }
        }
        n
    };

    let code_offsets_for = |class_data_len: u32| -> Vec<u32> {
        let mut offsets = Vec::new();
        let mut cursor = class_data_base + class_data_len;
        for body in &code {
            cursor = (cursor + 3) & !3;
            offsets.push(cursor);
            cursor += 16 + body.len() as u32 * 2;
        }
        offsets
    };

    // Fixpoint: seed with zero offsets, iterate — uleb widths only grow.
    let mut code_offsets = code_offsets_for(class_data_len_for(&code_offsets_for(0)));
    let mut class_data_len = class_data_len_for(&code_offsets);
    for _ in 0..8 {
        let next_offsets = code_offsets_for(class_data_len);
        let next_len = class_data_len_for(&next_offsets);
        if next_offsets == code_offsets && next_len == class_data_len {
            break;
        }
        code_offsets = next_offsets;
        class_data_len = next_len;
    }

    // Emit the class_data blobs against the converged code offsets.
    let mut class_data_blob: Vec<u8> = Vec::new();
    let mut class_data_offsets: Vec<u32> = Vec::new();
    for ci in 0..3u32 {
        class_data_blob.resize(class_data_blob.len().div_ceil(4) * 4, 0);
        class_data_offsets.push(class_data_base + class_data_blob.len() as u32);
        let mut blob = Vec::new();
        write_uleb128(&mut blob, 0); // static_fields
        write_uleb128(&mut blob, 0); // instance_fields
        let own: Vec<usize> = (0..code.len()).filter(|k| owner[*k] == ci).collect();
        let dcount = own.iter().filter(|k| direct[**k]).count();
        write_uleb128(&mut blob, dcount as u32);
        write_uleb128(&mut blob, (own.len() - dcount) as u32);
        let mut prev = 0u32;
        for k in own.iter().filter(|k| direct[**k]) {
            let m = code_method[*k];
            write_uleb128(&mut blob, m - prev);
            prev = m;
            write_uleb128(&mut blob, 0x1);
            write_uleb128(&mut blob, code_offsets[*k]);
        }
        let mut prev = 0u32;
        for k in own.iter().filter(|k| !direct[**k]) {
            let m = code_method[*k];
            write_uleb128(&mut blob, m - prev);
            prev = m;
            write_uleb128(&mut blob, 0x1);
            write_uleb128(&mut blob, code_offsets[*k]);
        }
        class_data_blob.extend_from_slice(&blob);
    }

    // ---- emit ---------------------------------------------------------------
    let mut buf: Vec<u8> = Vec::new();
    buf.extend_from_slice(b"dex\n035\0");
    buf.extend_from_slice(&[0u8; 4]); // checksum
    buf.extend_from_slice(&[0u8; 20]); // sha1
    buf.extend_from_slice(&[0u8; 4]); // file_size
    buf.extend_from_slice(&header_size.to_le_bytes());
    buf.extend_from_slice(&0x1234_5678u32.to_le_bytes()); // endian tag
    buf.extend_from_slice(&0u32.to_le_bytes()); // link_size
    buf.extend_from_slice(&0u32.to_le_bytes()); // link_off
    buf.extend_from_slice(&0u32.to_le_bytes()); // map_off (unused here)
    buf.extend_from_slice(&string_ids_size.to_le_bytes());
    buf.extend_from_slice(&string_ids_off.to_le_bytes());
    buf.extend_from_slice(&type_ids_size.to_le_bytes());
    buf.extend_from_slice(&type_ids_off.to_le_bytes());
    buf.extend_from_slice(&proto_ids_size.to_le_bytes());
    buf.extend_from_slice(&proto_ids_off.to_le_bytes());
    buf.extend_from_slice(&field_ids_size.to_le_bytes());
    buf.extend_from_slice(&field_ids_off.to_le_bytes());
    buf.extend_from_slice(&method_ids_size.to_le_bytes());
    buf.extend_from_slice(&method_ids_off.to_le_bytes());
    buf.extend_from_slice(&class_defs_size.to_le_bytes());
    buf.extend_from_slice(&class_defs_off.to_le_bytes());
    buf.extend_from_slice(&[0u8; 4]); // data_size
    buf.extend_from_slice(&[0u8; 4]); // data_off
    debug_assert_eq!(buf.len() as u32, header_size);

    for off in &string_offsets {
        buf.extend_from_slice(&(string_data_base + off).to_le_bytes());
    }
    for t in &types {
        buf.extend_from_slice(&t.to_le_bytes());
    }
    for (shorty, ret, params) in &protos {
        buf.extend_from_slice(&shorty.to_le_bytes());
        buf.extend_from_slice(&ret.to_le_bytes());
        buf.extend_from_slice(&params.to_le_bytes());
    }
    for (cls, proto, name) in &methods {
        buf.extend_from_slice(&(*cls as u16).to_le_bytes());
        buf.extend_from_slice(&(*proto as u16).to_le_bytes());
        buf.extend_from_slice(&name.to_le_bytes());
    }
    let class_types = [t_app, t_main, t_util];
    let supers = [t_application, t_activity, t_object];
    for i in 0..3usize {
        buf.extend_from_slice(&class_types[i].to_le_bytes());
        buf.extend_from_slice(&1u32.to_le_bytes()); // ACC_PUBLIC
        buf.extend_from_slice(&supers[i].to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes()); // interfaces_off
        buf.extend_from_slice(&0xffff_ffffu32.to_le_bytes()); // source_file = NO_INDEX
        buf.extend_from_slice(&0u32.to_le_bytes()); // annotations_off
        buf.extend_from_slice(&class_data_offsets[i].to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes()); // static_values_off
    }
    buf.extend_from_slice(&string_data);
    buf.extend_from_slice(&class_data_blob);
    for body in &code {
        while buf.len() % 4 != 0 {
            buf.push(0);
        }
        buf.extend_from_slice(&3u16.to_le_bytes()); // registers_size
        buf.extend_from_slice(&0u16.to_le_bytes()); // ins_size
        buf.extend_from_slice(&2u16.to_le_bytes()); // outs_size
        buf.extend_from_slice(&0u16.to_le_bytes()); // tries_size
        buf.extend_from_slice(&0u32.to_le_bytes()); // debug_info_off
        buf.extend_from_slice(&(body.len() as u32).to_le_bytes()); // insns_size (units)
        for w in body {
            buf.extend_from_slice(&w.to_le_bytes());
        }
    }

    // fixups: file_size, sha1 over [12..], adler32 over [8..]
    let file_size = buf.len() as u32;
    buf[32..36].copy_from_slice(&file_size.to_le_bytes());
    buf[0x68..0x6c].copy_from_slice(&(file_size - header_size).to_le_bytes()); // data_size
    buf[0x6c..0x70].copy_from_slice(&class_data_base.to_le_bytes()); // data_off
    let digest = {
        use sha1::{Digest, Sha1};
        let mut h = Sha1::new();
        h.update(&buf[12..]);
        h.finalize()
    };
    buf[12..32].copy_from_slice(&digest);
    let sum = adler32(&buf[8..]);
    buf[8..12].copy_from_slice(&sum.to_le_bytes());
    buf
}

fn uleb_len(v: u32) -> u32 {
    let mut n = 1;
    let mut v = v >> 7;
    while v > 0 {
        n += 1;
        v >>= 7;
    }
    n
}

fn adler32(data: &[u8]) -> u32 {
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for &byte in data {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

// ---------------------------------------------------------------------- zip ---

fn write_zip(path: &Path, entries: &[(&str, Vec<u8>)]) -> anyhow::Result<()> {
    let file = std::fs::File::create(path)?;
    let mut zip = zip::ZipWriter::new(file);
    let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, data) in entries {
        zip.start_file(*name, opts)?;
        std::io::Write::write_all(&mut zip, data)?;
    }
    zip.finish()?;
    Ok(())
}
