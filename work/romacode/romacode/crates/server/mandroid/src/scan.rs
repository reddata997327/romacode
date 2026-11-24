//! Static analysis rule engine.
//!
//! The rules table owns the judgement (severity, CWE, wording); this file owns
//! the mechanics. Same split as the dbx taxonomy: knowledge is data, so it can
//! be corrected without a rebuild.

use anyhow::{Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::manifest::{Component, Manifest};

pub const EMBEDDED: &str = include_str!("../data/android-rules.json");

#[derive(Debug, Clone, Deserialize)]
pub struct Rules {
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub secret_patterns: Vec<SecretPattern>,
    #[serde(default)]
    pub rules: Vec<Rule>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SecretPattern {
    pub id: String,
    pub title: String,
    pub severity: String,
    #[serde(default)]
    pub cwe: Option<String>,
    pub regex: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Rule {
    pub id: String,
    pub title: String,
    pub severity: String,
    pub category: String,
    #[serde(default)]
    pub cwe: Option<String>,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub remediation: String,
    pub check: Check,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Check {
    /// An application-level boolean flag must equal `equals`.
    ApplicationFlag {
        flag: String,
        equals: bool,
    },
    /// An attribute must be explicitly false (absent counts as a failure).
    AttributeNotFalse {
        attribute: String,
    },
    AttributeAbsent {
        attribute: String,
    },
    SdkBelow {
        field: String,
        value: i32,
    },
    ExportedComponent {
        component: String,
    },
    ProviderGrantUri,
    ImplicitExportComponent,
    DangerousPermission {
        permissions: Vec<String>,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub id: String,
    pub title: String,
    pub severity: String,
    pub category: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwe: Option<String>,
    /// Component or attribute the finding is about.
    pub subject: String,
    pub detail: String,
    pub remediation: String,
    pub evidence: String,
}

pub fn severity_rank(s: &str) -> u8 {
    match s.to_ascii_lowercase().as_str() {
        "critical" => 4,
        "high" => 3,
        "medium" => 2,
        "low" => 1,
        _ => 0,
    }
}

impl Rules {
    pub fn load() -> Result<Rules> {
        let text = match std::env::var("DROID_RULES") {
            Ok(p) if !p.trim().is_empty() => std::fs::read_to_string(p.trim())
                .with_context(|| format!("reading DROID_RULES {p}"))?,
            _ => EMBEDDED.to_string(),
        };
        serde_json::from_str(&text).context("parsing android-rules.json")
    }
}

fn app_flag(m: &Manifest, flag: &str) -> Option<bool> {
    match flag {
        "debuggable" => Some(m.application.debuggable),
        "allow_backup" => m.application.allow_backup,
        "uses_cleartext_traffic" => m.application.uses_cleartext_traffic,
        "request_legacy_external_storage" => m.application.request_legacy_external_storage,
        "extract_native_libs" => m.application.extract_native_libs,
        _ => None,
    }
}

fn make(r: &Rule, subject: String, evidence: String) -> Finding {
    Finding {
        id: r.id.clone(),
        title: r.title.clone(),
        severity: r.severity.clone(),
        category: r.category.clone(),
        cwe: r.cwe.clone(),
        subject,
        detail: r.detail.clone(),
        remediation: r.remediation.clone(),
        evidence,
    }
}

/// Run every rule against a decoded manifest (and, optionally, DEX strings).
pub fn scan(m: &Manifest, rules: &Rules, dex_strings: &[String]) -> Vec<Finding> {
    let mut out = Vec::new();

    for r in &rules.rules {
        match &r.check {
            Check::ApplicationFlag { flag, equals } => {
                if let Some(v) = app_flag(m, flag) {
                    if v == *equals {
                        out.push(make(r, "application".into(), format!("{flag}={v}")));
                    }
                }
            }
            Check::AttributeNotFalse { attribute } => {
                // Absent or true both fail: the safe state is an explicit false.
                let v = app_flag(m, attribute).unwrap_or(true);
                if v {
                    let shown = match app_flag(m, attribute) {
                        Some(b) => format!("{attribute}={b}"),
                        None => format!("{attribute} not set (defaults to true)"),
                    };
                    out.push(make(r, "application".into(), shown));
                }
            }
            Check::AttributeAbsent { attribute } => {
                let present = match attribute.as_str() {
                    "networkSecurityConfig" => m.application.network_security_config.is_some(),
                    "dataExtractionRules" => false,
                    _ => false,
                };
                if !present {
                    out.push(make(r, "application".into(), format!("{attribute} absent")));
                }
            }
            Check::SdkBelow { field, value } => {
                let got = match field.as_str() {
                    "min_sdk" => m.min_sdk,
                    "target_sdk" => m.target_sdk,
                    _ => None,
                };
                if let Some(v) = got {
                    if v < *value {
                        out.push(make(
                            r,
                            "manifest".into(),
                            format!("{field}={v} (< {value})"),
                        ));
                    }
                }
            }
            Check::ExportedComponent { component } => {
                for c in m.components.iter().filter(|c| &c.kind == component) {
                    if c.effectively_exported && !c.has_permission_guard() {
                        out.push(make(r, c.name.clone(), describe(c)));
                    }
                }
            }
            Check::ProviderGrantUri => {
                for c in m.components.iter().filter(|c| c.is_provider()) {
                    if c.effectively_exported && c.grant_uri_permissions == Some(true) {
                        out.push(make(r, c.name.clone(), describe(c)));
                    }
                }
            }
            Check::ImplicitExportComponent => {
                for c in m.components.iter() {
                    if c.implicitly_exported {
                        out.push(make(
                            r,
                            c.name.clone(),
                            format!("{}: intent filters but no android:exported", c.kind),
                        ));
                    }
                }
            }
            Check::DangerousPermission { permissions } => {
                for p in &m.permissions {
                    if permissions.iter().any(|d| d == p) {
                        out.push(make(r, p.clone(), format!("declared {p}")));
                    }
                }
            }
        }
    }

    // Hardcoded secrets in the DEX string pool.
    for sp in &rules.secret_patterns {
        let Ok(re) = Regex::new(&sp.regex) else {
            continue;
        };
        for s in dex_strings {
            if let Some(mt) = re.find(s) {
                // Never echo a full secret back into a report.
                let hit = mt.as_str();
                let shown = if hit.len() > 12 {
                    format!("{}…({} chars)", &hit[..8], hit.len())
                } else {
                    format!("<{} chars>", hit.len())
                };
                out.push(Finding {
                    id: format!("secret-{}", sp.id),
                    title: sp.title.clone(),
                    severity: sp.severity.clone(),
                    category: "hardcoded-secret".into(),
                    cwe: sp.cwe.clone(),
                    subject: "dex".into(),
                    detail: format!(
                        "A string in the DEX pool matches the {} pattern. Hardcoded credentials in an APK are extractable by anyone who has the file.",
                        sp.title.to_lowercase()
                    ),
                    remediation: "Move the secret server-side, rotate it, and load it at runtime."
                        .into(),
                    evidence: shown,
                });
                break; // one hit per pattern is enough to raise the finding
            }
        }
    }

    out.sort_by(|a, b| {
        severity_rank(&b.severity)
            .cmp(&severity_rank(&a.severity))
            .then_with(|| a.id.cmp(&b.id))
    });
    out.dedup_by(|a, b| a.id == b.id && a.subject == b.subject);
    out
}

fn describe(c: &Component) -> String {
    match (&c.exported, c.permission.as_deref()) {
        (Some(true), _) => format!("{}: exported=true", c.kind),
        (None, _) => format!("{}: exported implied by intent filter", c.kind),
        _ => format!("{}", c.kind),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::axml::{Attr, AttrValue, Node};

    fn n(name: &str, attrs: &[(&str, AttrValue)]) -> Node {
        Node {
            name: name.into(),
            namespace: None,
            attrs: attrs
                .iter()
                .map(|(k, v)| Attr {
                    namespace: Some("android".into()),
                    name: (*k).into(),
                    value: v.clone(),
                })
                .collect(),
            children: vec![],
            line: 0,
        }
    }

    fn manifest_with(children: Vec<Node>) -> Manifest {
        let root = Node {
            name: "manifest".into(),
            namespace: None,
            attrs: vec![Attr {
                namespace: None,
                name: "package".into(),
                value: AttrValue::Str("com.test".into()),
            }],
            children,
            line: 0,
        };
        Manifest::from_node(&root)
    }

    #[test]
    fn embedded_rules_parse_and_are_unique() {
        let r = Rules::load().unwrap();
        assert!(
            r.rules.len() >= 12,
            "expected a real rule set, got {}",
            r.rules.len()
        );
        let mut ids: Vec<&str> = r.rules.iter().map(|x| x.id.as_str()).collect();
        ids.sort();
        let before = ids.len();
        ids.dedup();
        assert_eq!(before, ids.len(), "duplicate rule ids");
        // `info` is a legitimate severity and ranks 0, so validate the
        // vocabulary rather than the rank.
        for rule in &r.rules {
            assert!(
                matches!(
                    rule.severity.as_str(),
                    "critical" | "high" | "medium" | "low" | "info"
                ),
                "{} has an unknown severity {:?}",
                rule.id,
                rule.severity
            );
        }
    }

    #[test]
    fn debuggable_and_cleartext_are_flagged() {
        let app = n(
            "application",
            &[
                ("debuggable", AttrValue::Bool(true)),
                ("usesCleartextTraffic", AttrValue::Bool(true)),
            ],
        );
        let m = manifest_with(vec![
            n(
                "uses-sdk",
                &[
                    ("minSdkVersion", AttrValue::Int(21)),
                    ("targetSdkVersion", AttrValue::Int(30)),
                ],
            ),
            app,
        ]);
        let f = scan(&m, &Rules::load().unwrap(), &[]);
        let ids: Vec<&str> = f.iter().map(|x| x.id.as_str()).collect();
        assert!(ids.contains(&"app-debuggable"));
        assert!(ids.contains(&"cleartext-traffic"));
        assert!(ids.contains(&"min-sdk-too-low"));
        assert!(ids.contains(&"target-sdk-too-low"));
    }

    #[test]
    fn exported_components_without_guards_are_flagged_and_guarded_ones_are_not() {
        let open = n(
            "provider",
            &[
                ("name", AttrValue::Str(".Open".into())),
                ("exported", AttrValue::Bool(true)),
            ],
        );
        let guarded = n(
            "provider",
            &[
                ("name", AttrValue::Str(".Guarded".into())),
                ("exported", AttrValue::Bool(true)),
                ("readPermission", AttrValue::Str("com.test.READ".into())),
            ],
        );
        let m = manifest_with(vec![open, guarded]);
        let f = scan(&m, &Rules::load().unwrap(), &[]);
        let subjects: Vec<&str> = f
            .iter()
            .filter(|x| x.id == "exported-provider-no-permission")
            .map(|x| x.subject.as_str())
            .collect();
        assert!(subjects.contains(&".Open"));
        assert!(
            !subjects.contains(&".Guarded"),
            "a permission guard must suppress the finding"
        );
    }

    #[test]
    fn implicit_export_is_reported_separately() {
        let mut act = n("activity", &[("name", AttrValue::Str(".Implicit".into()))]);
        act.children.push(n("intent-filter", &[]));
        let m = manifest_with(vec![act]);
        let f = scan(&m, &Rules::load().unwrap(), &[]);
        let ids: Vec<&str> = f.iter().map(|x| x.id.as_str()).collect();
        assert!(ids.contains(&"implicit-export"));
        assert!(ids.contains(&"exported-activity-no-permission"));
    }

    #[test]
    fn secrets_are_detected_and_never_echoed_in_full() {
        let m = manifest_with(vec![]);
        let strings = vec![
            "com.example.app".to_string(),
            "AKIAIOSFODNN7EXAMPLE".to_string(),
            "-----BEGIN RSA PRIVATE KEY-----".to_string(),
        ];
        let f = scan(&m, &Rules::load().unwrap(), &strings);
        let sec: Vec<&Finding> = f
            .iter()
            .filter(|x| x.category == "hardcoded-secret")
            .collect();
        assert!(
            sec.len() >= 2,
            "expected aws + private key, got {:?}",
            sec.iter().map(|s| &s.id).collect::<Vec<_>>()
        );
        for s in &sec {
            assert!(
                !s.evidence.contains("AKIAIOSFODNN7EXAMPLE"),
                "full secret leaked: {}",
                s.evidence
            );
            assert!(
                !s.evidence.contains("PRIVATE KEY-----"),
                "full secret leaked: {}",
                s.evidence
            );
        }
    }

    #[test]
    fn findings_sort_worst_first() {
        let app = n("application", &[("debuggable", AttrValue::Bool(true))]);
        let prov = n(
            "provider",
            &[
                ("name", AttrValue::Str(".P".into())),
                ("exported", AttrValue::Bool(true)),
            ],
        );
        let m = manifest_with(vec![app, prov]);
        let f = scan(&m, &Rules::load().unwrap(), &[]);
        let ranks: Vec<u8> = f.iter().map(|x| severity_rank(&x.severity)).collect();
        let mut sorted = ranks.clone();
        sorted.sort_by(|a, b| b.cmp(a));
        assert_eq!(ranks, sorted);
        assert_eq!(
            f[0].severity, "critical",
            "exported provider is the worst finding here"
        );
    }
}
