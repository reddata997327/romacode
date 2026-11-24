//! Typed AndroidManifest model.
//!
//! Turns the decoded AXML tree into the things a security review actually asks
//! about: which components exist, which are reachable by other apps, and what
//! the application-level flags allow.

use serde::Serialize;

use crate::axml::Node;

#[derive(Debug, Clone, Serialize, Default)]
pub struct Application {
    pub label: Option<String>,
    /// `android:debuggable="true"` — ships a debuggable production build.
    pub debuggable: bool,
    /// `android:allowBackup` (default true unless explicitly false).
    pub allow_backup: Option<bool>,
    /// `android:usesCleartextTraffic`.
    pub uses_cleartext_traffic: Option<bool>,
    /// `android:networkSecurityConfig` — the modern way to scope cleartext.
    pub network_security_config: Option<String>,
    pub request_legacy_external_storage: Option<bool>,
    pub extract_native_libs: Option<bool>,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Component {
    /// activity | activity-alias | service | receiver | provider
    pub kind: String,
    pub name: String,
    /// The literal `android:exported` value, if declared.
    pub exported: Option<bool>,
    /// What the platform will actually do. Before API 31 an intent filter made a
    /// component implicitly exported; API 31+ requires the attribute. We report
    /// both so the finding can distinguish "declared" from "implicit".
    pub effectively_exported: bool,
    pub implicitly_exported: bool,
    pub permission: Option<String>,
    pub read_permission: Option<String>,
    pub write_permission: Option<String>,
    pub intent_filter_count: usize,
    pub authorities: Option<String>,
    pub grant_uri_permissions: Option<bool>,
}

impl Component {
    pub fn is_provider(&self) -> bool {
        self.kind == "provider"
    }
    /// A component is "guarded" when some permission gates access to it.
    pub fn has_permission_guard(&self) -> bool {
        self.permission.is_some()
            || self.read_permission.is_some()
            || self.write_permission.is_some()
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Manifest {
    pub package: String,
    pub version_code: Option<i32>,
    pub version_name: Option<String>,
    pub min_sdk: Option<i32>,
    pub target_sdk: Option<i32>,
    pub compile_sdk: Option<i32>,
    pub permissions: Vec<String>,
    pub declared_permissions: Vec<String>,
    pub application: Application,
    pub components: Vec<Component>,
    pub has_network_security_config: bool,
}

impl Manifest {
    pub fn from_node(root: &Node) -> Manifest {
        let mut m = Manifest {
            package: root.attr_str("package").unwrap_or_default(),
            version_code: root.attr_int("versionCode"),
            version_name: root.attr_str("versionName"),
            compile_sdk: root.attr_int("compileSdkVersion"),
            ..Default::default()
        };

        let mut nodes = Vec::new();
        root.walk(&mut nodes);

        for n in &nodes {
            match n.name.as_str() {
                "uses-sdk" => {
                    m.min_sdk = n.attr_int("minSdkVersion");
                    m.target_sdk = n.attr_int("targetSdkVersion");
                }
                "uses-permission" | "uses-permission-sdk-23" => {
                    if let Some(p) = n.attr_str("name") {
                        m.permissions.push(p);
                    }
                }
                "permission" => {
                    if let Some(p) = n.attr_str("name") {
                        m.declared_permissions.push(p);
                    }
                }
                "application" => {
                    m.application = Application {
                        label: n.attr_str("label"),
                        debuggable: n.attr_bool("debuggable").unwrap_or(false),
                        allow_backup: n.attr_bool("allowBackup"),
                        uses_cleartext_traffic: n.attr_bool("usesCleartextTraffic"),
                        network_security_config: n.attr_str("networkSecurityConfig"),
                        request_legacy_external_storage: n
                            .attr_bool("requestLegacyExternalStorage"),
                        extract_native_libs: n.attr_bool("extractNativeLibs"),
                        name: n.attr_str("name"),
                    };
                    m.has_network_security_config = m.application.network_security_config.is_some();
                }
                k @ ("activity" | "activity-alias" | "service" | "receiver" | "provider") => {
                    let intent_filters = n
                        .children
                        .iter()
                        .filter(|c| c.name == "intent-filter")
                        .count();
                    let exported = n.attr_bool("exported");
                    // Pre-API-31 rule: an intent filter implies exported.
                    let implicit = intent_filters > 0 && exported.is_none();
                    m.components.push(Component {
                        kind: k.to_string(),
                        name: n.attr_str("name").unwrap_or_default(),
                        exported,
                        effectively_exported: exported.unwrap_or(implicit),
                        implicitly_exported: implicit,
                        permission: n.attr_str("permission"),
                        read_permission: n.attr_str("readPermission"),
                        write_permission: n.attr_str("writePermission"),
                        intent_filter_count: intent_filters,
                        authorities: n.attr_str("authorities"),
                        grant_uri_permissions: n.attr_bool("grantUriPermissions"),
                    });
                }
                _ => {}
            }
        }

        // targetSdk may only live in uses-sdk; keep min/target sane if absent.
        if m.min_sdk.is_none() {
            m.min_sdk = Some(1);
        }
        if m.target_sdk.is_none() {
            m.target_sdk = m.min_sdk;
        }
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::axml::{Attr, AttrValue};

    fn node(name: &str, attrs: &[(&str, AttrValue)]) -> Node {
        Node {
            name: name.into(),
            namespace: None,
            attrs: attrs
                .iter()
                .map(|(n, v)| Attr {
                    namespace: Some("android".into()),
                    name: (*n).into(),
                    value: v.clone(),
                })
                .collect(),
            children: vec![],
            line: 0,
        }
    }

    #[test]
    fn implicit_export_from_intent_filter_is_detected() {
        let mut act = node("activity", &[("name", AttrValue::Str(".Main".into()))]);
        act.children.push(node("intent-filter", &[]));
        let app = node("application", &[]);
        let root = Node {
            name: "manifest".into(),
            namespace: None,
            attrs: vec![Attr {
                namespace: None,
                name: "package".into(),
                value: AttrValue::Str("com.example".into()),
            }],
            children: vec![app, act],
            line: 0,
        };
        let m = Manifest::from_node(&root);
        assert_eq!(m.package, "com.example");
        assert_eq!(m.components.len(), 1);
        let c = &m.components[0];
        assert_eq!(c.exported, None, "attribute was not declared");
        assert!(c.implicitly_exported);
        assert!(c.effectively_exported);
        assert!(!c.has_permission_guard());
    }

    #[test]
    fn explicit_exported_false_wins_over_intent_filter() {
        let mut act = node(
            "activity",
            &[
                ("name", AttrValue::Str(".Main".into())),
                ("exported", AttrValue::Bool(false)),
            ],
        );
        act.children.push(node("intent-filter", &[]));
        let root = Node {
            name: "manifest".into(),
            namespace: None,
            attrs: vec![],
            children: vec![act],
            line: 0,
        };
        let m = Manifest::from_node(&root);
        let c = &m.components[0];
        assert_eq!(c.exported, Some(false));
        assert!(!c.effectively_exported);
        assert!(
            !c.implicitly_exported,
            "declaring exported=false is not implicit export"
        );
    }

    #[test]
    fn application_flags_and_permissions_are_collected() {
        let app = node(
            "application",
            &[
                ("debuggable", AttrValue::Bool(true)),
                ("allowBackup", AttrValue::Bool(true)),
                ("usesCleartextTraffic", AttrValue::Bool(true)),
                (
                    "networkSecurityConfig",
                    AttrValue::Str("@0x7f100000".into()),
                ),
            ],
        );
        let perm = node(
            "uses-permission",
            &[("name", AttrValue::Str("android.permission.INTERNET".into()))],
        );
        let sdk = node(
            "uses-sdk",
            &[
                ("minSdkVersion", AttrValue::Int(21)),
                ("targetSdkVersion", AttrValue::Int(30)),
            ],
        );
        let root = Node {
            name: "manifest".into(),
            namespace: None,
            attrs: vec![],
            children: vec![sdk, perm, app],
            line: 0,
        };
        let m = Manifest::from_node(&root);
        assert!(m.application.debuggable);
        assert_eq!(m.application.allow_backup, Some(true));
        assert_eq!(m.application.uses_cleartext_traffic, Some(true));
        assert!(m.has_network_security_config);
        assert_eq!(m.min_sdk, Some(21));
        assert_eq!(m.target_sdk, Some(30));
        assert_eq!(m.permissions, vec!["android.permission.INTERNET"]);
    }

    #[test]
    fn provider_permissions_guard_correctly() {
        let p = node(
            "provider",
            &[
                ("name", AttrValue::Str(".P".into())),
                ("readPermission", AttrValue::Str("com.example.READ".into())),
                ("grantUriPermissions", AttrValue::Bool(true)),
            ],
        );
        let root = Node {
            name: "manifest".into(),
            namespace: None,
            attrs: vec![],
            children: vec![p],
            line: 0,
        };
        let m = Manifest::from_node(&root);
        let c = &m.components[0];
        assert!(c.is_provider());
        assert!(c.has_permission_guard(), "readPermission is a guard");
        assert_eq!(c.grant_uri_permissions, Some(true));
    }
}
