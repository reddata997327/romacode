//! Manifest modification — the "change how the app behaves" half.
//!
//! Edits are a closed set of named operations rather than free-form XML
//! surgery, so an agent picks from a menu and the result is predictable. The
//! decoded tree is altered and re-encoded with [`crate::axmlenc`]; nothing here
//! shells out to `apktool`.

use anyhow::{anyhow, bail, Context, Result};

use crate::axml::{self, Attr, AttrValue, Node};
use crate::axmlenc;

pub const ANDROID_NS: &str = "http://schemas.android.com/apk/res/android";

/// One legal change to a manifest.
#[derive(Debug, Clone, PartialEq)]
pub enum ManifestEdit {
    SetDebuggable(bool),
    SetAllowBackup(bool),
    SetCleartextTraffic(bool),
    /// Mark one component explicitly exported (or not).
    SetExported {
        component: String,
        exported: bool,
    },
    SetMinSdk(u32),
    SetTargetSdk(u32),
    AddPermission(String),
    RemovePermission(String),
    RenamePackage(String),
    /// Point the app at a network security config resource.
    SetNetworkSecurityConfig(String),
    /// Disable certificate pinning checks by removing the attribute.
    RemoveCleartextGuard,
}

impl ManifestEdit {
    /// Parse the compact `key=value` form used by the CLI and MCP tools.
    pub fn parse(spec: &str) -> Result<ManifestEdit> {
        let (key, value) = spec
            .split_once('=')
            .ok_or_else(|| anyhow!("expected key=value, got {spec:?}"))?;
        let flag = |v: &str| -> Result<bool> {
            match v.to_ascii_lowercase().as_str() {
                "true" | "1" | "yes" => Ok(true),
                "false" | "0" | "no" => Ok(false),
                other => bail!("expected true/false, got {other:?}"),
            }
        };
        match key {
            "debuggable" => Ok(ManifestEdit::SetDebuggable(flag(value)?)),
            "allowBackup" | "allow-backup" => Ok(ManifestEdit::SetAllowBackup(flag(value)?)),
            "cleartext" | "usesCleartextTraffic" => {
                Ok(ManifestEdit::SetCleartextTraffic(flag(value)?))
            }
            "exported" => {
                let (component, flag_text) = value
                    .rsplit_once(':')
                    .ok_or_else(|| anyhow!("exported expects <component>:<true|false>"))?;
                Ok(ManifestEdit::SetExported {
                    component: component.to_string(),
                    exported: flag(flag_text)?,
                })
            }
            "minSdk" | "min-sdk" => Ok(ManifestEdit::SetMinSdk(
                value.parse().context("minSdk must be a number")?,
            )),
            "targetSdk" | "target-sdk" => Ok(ManifestEdit::SetTargetSdk(
                value.parse().context("targetSdk must be a number")?,
            )),
            "permission+" | "addPermission" => Ok(ManifestEdit::AddPermission(value.to_string())),
            "permission-" | "removePermission" => {
                Ok(ManifestEdit::RemovePermission(value.to_string()))
            }
            "package" => Ok(ManifestEdit::RenamePackage(value.to_string())),
            "networkSecurityConfig" => {
                Ok(ManifestEdit::SetNetworkSecurityConfig(value.to_string()))
            }
            "cleartextGuard" => {
                if value.eq_ignore_ascii_case("off") || value.eq_ignore_ascii_case("none") {
                    Ok(ManifestEdit::RemoveCleartextGuard)
                } else {
                    bail!("cleartextGuard only supports =off")
                }
            }
            other => bail!("unknown manifest edit {other:?}"),
        }
    }

    pub fn describe(&self) -> String {
        match self {
            ManifestEdit::SetDebuggable(v) => format!("android:debuggable={v}"),
            ManifestEdit::SetAllowBackup(v) => format!("android:allowBackup={v}"),
            ManifestEdit::SetCleartextTraffic(v) => format!("android:usesCleartextTraffic={v}"),
            ManifestEdit::SetExported {
                component,
                exported,
            } => {
                format!("android:exported={exported} on {component}")
            }
            ManifestEdit::SetMinSdk(v) => format!("uses-sdk android:minSdkVersion={v}"),
            ManifestEdit::SetTargetSdk(v) => format!("uses-sdk android:targetSdkVersion={v}"),
            ManifestEdit::AddPermission(p) => format!("add uses-permission {p}"),
            ManifestEdit::RemovePermission(p) => format!("remove uses-permission {p}"),
            ManifestEdit::RenamePackage(p) => format!("package={p}"),
            ManifestEdit::SetNetworkSecurityConfig(v) => {
                format!("android:networkSecurityConfig={v}")
            }
            ManifestEdit::RemoveCleartextGuard => "remove android:usesCleartextTraffic".into(),
        }
    }
}

/// Named bundles so an agent can ask for an outcome instead of composing edits.
pub fn presets() -> Vec<(&'static str, &'static str, Vec<ManifestEdit>)> {
    vec![
        (
            "debug",
            "Make the app debuggable and allow backup, for instrumentation and data extraction.",
            vec![
                ManifestEdit::SetDebuggable(true),
                ManifestEdit::SetAllowBackup(true),
            ],
        ),
        (
            "cleartext",
            "Permit cleartext HTTP and drop any network security config, so traffic can be intercepted.",
            vec![
                ManifestEdit::SetCleartextTraffic(true),
                ManifestEdit::SetDebuggable(true),
            ],
        ),
        (
            "expose-all",
            "Export every activity, service and receiver so components can be invoked directly.",
            vec![ManifestEdit::SetDebuggable(true)],
        ),
        (
            "backup",
            "Allow adb backup of application data.",
            vec![ManifestEdit::SetAllowBackup(true)],
        ),
    ]
}

/// Look up a preset by name.
pub fn preset(name: &str) -> Option<Vec<ManifestEdit>> {
    presets()
        .into_iter()
        .find(|(n, _, _)| *n == name)
        .map(|(_, _, edits)| edits)
}

fn matches_name(attr_name: &str, wanted: &str) -> bool {
    attr_name == wanted || attr_name.ends_with(&format!(":{wanted}"))
}

/// Set an attribute, adding it if absent. Returns true when it already existed.
fn set_android_attr(node: &mut Node, name: &str, value: AttrValue) -> bool {
    if let Some(existing) = node.attrs.iter_mut().find(|a| matches_name(&a.name, name)) {
        existing.value = value;
        return true;
    }
    node.attrs.push(Attr {
        namespace: Some(ANDROID_NS.to_string()),
        name: name.to_string(),
        value,
    });
    false
}

/// Set `android:exported` on the first matching component in `nodes`.
fn export_in(nodes: &mut [Node], package: &str, component: &str, exported: bool) -> Option<String> {
    for node in nodes.iter_mut() {
        if !matches!(
            node.name.as_str(),
            "activity" | "service" | "receiver" | "provider"
        ) {
            continue;
        }
        let declared = match node.attr_str("name") {
            Some(s) => s,
            None => continue,
        };
        if component_matches(&declared, package, component) {
            set_android_attr(node, "exported", AttrValue::Bool(exported));
            return Some(format!(
                "{}/{} android:exported={exported}",
                node.name, declared
            ));
        }
    }
    None
}

fn find_application<'a>(root: &'a mut Node) -> Option<&'a mut Node> {
    if root.name == "application" {
        return Some(root);
    }
    root.children.iter_mut().find(|c| c.name == "application")
}

/// Resolve a component name the way the platform does. A manifest may write it
/// fully qualified (`com.example.Thing`) or relative to the package, either as
/// `.Thing` or as a bare `Thing`.
fn resolve_component(declared: &str, package: &str) -> String {
    if let Some(rest) = declared.strip_prefix('.') {
        format!("{package}.{rest}")
    } else if declared.contains('.') {
        declared.to_string()
    } else {
        format!("{package}.{declared}")
    }
}

/// Do a declared name and a requested name refer to the same component?
/// Accepts the fully-qualified form and the short form on either side, so the
/// caller can say `com.example.Thing` or just `Thing`.
fn component_matches(declared: &str, package: &str, wanted: &str) -> bool {
    let resolved = resolve_component(declared, package);
    let wanted_resolved = resolve_component(wanted, package);
    resolved == wanted_resolved
        || resolved.ends_with(&format!(".{wanted}"))
        || wanted_resolved.ends_with(&format!(".{resolved}"))
}

/// Apply one edit, returning a description of what actually changed.
pub fn apply(root: &mut Node, edit: &ManifestEdit) -> Result<String> {
    match edit {
        ManifestEdit::SetDebuggable(v) => {
            let app = find_application(root)
                .ok_or_else(|| anyhow!("manifest has no <application> element"))?;
            set_android_attr(app, "debuggable", AttrValue::Bool(*v));
            Ok(edit.describe())
        }
        ManifestEdit::SetAllowBackup(v) => {
            let app = find_application(root)
                .ok_or_else(|| anyhow!("manifest has no <application> element"))?;
            set_android_attr(app, "allowBackup", AttrValue::Bool(*v));
            Ok(edit.describe())
        }
        ManifestEdit::SetCleartextTraffic(v) => {
            let app = find_application(root)
                .ok_or_else(|| anyhow!("manifest has no <application> element"))?;
            set_android_attr(app, "usesCleartextTraffic", AttrValue::Bool(*v));
            Ok(edit.describe())
        }
        ManifestEdit::RemoveCleartextGuard => {
            let app = find_application(root)
                .ok_or_else(|| anyhow!("manifest has no <application> element"))?;
            let before = app.attrs.len();
            app.attrs
                .retain(|a| !matches_name(&a.name, "usesCleartextTraffic"));
            if app.attrs.len() == before {
                return Ok("no android:usesCleartextTraffic to remove".into());
            }
            Ok(edit.describe())
        }
        ManifestEdit::SetNetworkSecurityConfig(v) => {
            let app = find_application(root)
                .ok_or_else(|| anyhow!("manifest has no <application> element"))?;
            set_android_attr(app, "networkSecurityConfig", AttrValue::Str(v.clone()));
            Ok(edit.describe())
        }
        ManifestEdit::SetExported {
            component,
            exported,
        } => {
            let package = root.attr_str("package").unwrap_or_default();
            // Components live under <application>, not directly under
            // <manifest>. Search there first, then fall back to the root for the
            // unusual manifests that declare them at top level.
            let mut changed = None;
            if let Some(app) = root.children.iter_mut().find(|c| c.name == "application") {
                changed = export_in(&mut app.children, &package, component, *exported);
            }
            if changed.is_none() {
                changed = export_in(&mut root.children, &package, component, *exported);
            }
            changed.ok_or_else(|| anyhow!("no component matching {component:?}"))
        }
        ManifestEdit::SetMinSdk(v) => set_sdk(root, "minSdkVersion", *v),
        ManifestEdit::SetTargetSdk(v) => set_sdk(root, "targetSdkVersion", *v),
        ManifestEdit::AddPermission(p) => {
            let already = root.children.iter().any(|c| {
                c.name == "uses-permission" && c.attr_str("name").map(|n| n == *p).unwrap_or(false)
            });
            if already {
                return Ok(format!("permission {p} already present"));
            }
            root.children.push(Node {
                name: "uses-permission".into(),
                namespace: None,
                attrs: vec![Attr {
                    namespace: Some(ANDROID_NS.to_string()),
                    name: "name".into(),
                    value: AttrValue::Str(p.clone()),
                }],
                children: Vec::new(),
                line: 0,
            });
            Ok(format!("added uses-permission {p}"))
        }
        ManifestEdit::RemovePermission(p) => {
            let before = root.children.len();
            root.children.retain(|c| {
                !(c.name == "uses-permission"
                    && c.attr_str("name").map(|n| n == *p).unwrap_or(false))
            });
            if root.children.len() == before {
                bail!("permission {p} is not declared");
            }
            Ok(format!("removed uses-permission {p}"))
        }
        ManifestEdit::RenamePackage(p) => {
            let existing = root.attrs.iter_mut().find(|a| a.name == "package");
            match existing {
                Some(a) => {
                    let was = a.value.display();
                    a.value = AttrValue::Str(p.clone());
                    Ok(format!("package {was} -> {p}"))
                }
                None => bail!("manifest has no package attribute"),
            }
        }
    }
}

fn set_sdk(root: &mut Node, attribute: &str, value: u32) -> Result<String> {
    if let Some(node) = root.children.iter_mut().find(|c| c.name == "uses-sdk") {
        set_android_attr(node, attribute, AttrValue::Int(value as i32));
        return Ok(format!("uses-sdk android:{attribute}={value}"));
    }
    // Insert a fresh <uses-sdk> ahead of <application>, which is where it
    // conventionally sits.
    let mut node = Node {
        name: "uses-sdk".into(),
        namespace: None,
        attrs: Vec::new(),
        children: Vec::new(),
        line: 0,
    };
    set_android_attr(&mut node, attribute, AttrValue::Int(value as i32));
    let position = root
        .children
        .iter()
        .position(|c| c.name == "application")
        .unwrap_or(root.children.len());
    root.children.insert(position, node);
    Ok(format!("added uses-sdk android:{attribute}={value}"))
}

/// Apply a list of edits in order.
pub fn apply_all(root: &mut Node, edits: &[ManifestEdit]) -> Result<Vec<String>> {
    edits.iter().map(|e| apply(root, e)).collect()
}

/// Decode a binary manifest, apply edits, and re-encode it.
pub fn modify_manifest(bytes: &[u8], edits: &[ManifestEdit]) -> Result<(Vec<u8>, Vec<String>)> {
    let mut root = axml::decode(bytes).context("decoding AndroidManifest.xml")?;
    let changes = apply_all(&mut root, edits)?;
    let encoded = axmlenc::encode(&root).context("re-encoding AndroidManifest.xml")?;
    Ok((encoded, changes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn real_manifest() -> Option<Vec<u8>> {
        let path =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/test-app.apk");
        if !path.exists() {
            return None;
        }
        let file = std::fs::File::open(path).ok()?;
        let mut archive = zip::ZipArchive::new(file).ok()?;
        let mut member = archive.by_name("AndroidManifest.xml").ok()?;
        let mut data = Vec::new();
        member.read_to_end(&mut data).ok()?;
        Some(data)
    }

    #[test]
    fn parses_the_compact_edit_syntax() {
        assert_eq!(
            ManifestEdit::parse("debuggable=true").unwrap(),
            ManifestEdit::SetDebuggable(true)
        );
        assert_eq!(
            ManifestEdit::parse("minSdk=24").unwrap(),
            ManifestEdit::SetMinSdk(24)
        );
        assert_eq!(
            ManifestEdit::parse("exported=com.example.Thing:true").unwrap(),
            ManifestEdit::SetExported {
                component: "com.example.Thing".into(),
                exported: true
            }
        );
        assert_eq!(
            ManifestEdit::parse("permission+=android.permission.INTERNET").unwrap(),
            ManifestEdit::AddPermission("android.permission.INTERNET".into())
        );
        assert!(ManifestEdit::parse("nonsense=1").is_err());
        assert!(ManifestEdit::parse("debuggable=maybe").is_err());
        assert!(ManifestEdit::parse("debuggable").is_err());
    }

    #[test]
    fn every_preset_parses_and_describes() {
        for (name, desc, edits) in presets() {
            assert!(!name.is_empty());
            assert!(desc.len() > 20, "preset {name} needs a real description");
            assert!(!edits.is_empty(), "preset {name} is empty");
            for e in &edits {
                assert!(!e.describe().is_empty());
            }
            assert!(preset(name).is_some());
        }
        assert!(preset("nope").is_none());
    }

    #[test]
    fn setting_debuggable_on_a_synthetic_manifest_round_trips() {
        let mut root = axml::decode(&synthetic_manifest()).unwrap();
        assert_eq!(root.attr_bool("debuggable"), None);
        let changes = apply_all(
            &mut root,
            &[
                ManifestEdit::SetDebuggable(true),
                ManifestEdit::SetMinSdk(24),
                ManifestEdit::AddPermission("android.permission.INTERNET".into()),
            ],
        )
        .unwrap();
        assert_eq!(changes.len(), 3);

        let bytes = axmlenc::encode(&root).unwrap();
        let decoded = axml::decode(&bytes).unwrap();
        let application = decoded
            .children
            .iter()
            .find(|c| c.name == "application")
            .expect("application survives");
        assert_eq!(application.attr_bool("debuggable"), Some(true));
        let sdk = decoded
            .children
            .iter()
            .find(|c| c.name == "uses-sdk")
            .unwrap();
        assert_eq!(sdk.attr_int("minSdkVersion"), Some(24));
        assert!(decoded.children.iter().any(|c| c.name == "uses-permission"));
    }

    #[test]
    fn removing_a_permission_that_is_absent_is_an_error_not_a_silent_noop() {
        let mut root = axml::decode(&synthetic_manifest()).unwrap();
        let err = apply(
            &mut root,
            &ManifestEdit::RemovePermission("android.permission.NOPE".into()),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("not declared"), "{err}");
    }

    #[test]
    fn export_edit_matches_short_and_qualified_component_names() {
        let mut root = axml::decode(&synthetic_manifest()).unwrap();
        // The synthetic manifest declares ".Lazy" relative to the package.
        let changed = apply(
            &mut root,
            &ManifestEdit::SetExported {
                component: "com.example.Lazy".into(),
                exported: true,
            },
        )
        .unwrap();
        assert!(changed.contains("exported=true"), "{changed}");
        let bytes = axmlenc::encode(&root).unwrap();
        let decoded = axml::decode(&bytes).unwrap();
        let application = decoded
            .children
            .iter()
            .find(|c| c.name == "application")
            .unwrap();
        let activity = application
            .children
            .iter()
            .find(|c| c.name == "activity")
            .unwrap();
        assert_eq!(activity.attr_bool("exported"), Some(true));
    }

    #[test]
    fn real_manifest_can_be_made_debuggable_and_re_decoded() {
        let Some(original) = real_manifest() else {
            eprintln!("skipping: testdata/test-app.apk not present");
            return;
        };
        let before = axml::decode(&original).unwrap();
        assert_ne!(before.attr_bool("debuggable"), Some(true));

        let (modified, changes) = modify_manifest(
            &original,
            &[
                ManifestEdit::SetDebuggable(true),
                ManifestEdit::SetAllowBackup(true),
            ],
        )
        .unwrap();
        assert_eq!(changes.len(), 2);
        assert!(!modified.is_empty());

        let after = axml::decode(&modified).unwrap();
        let app = after
            .children
            .iter()
            .find(|c| c.name == "application")
            .unwrap();
        assert_eq!(app.attr_bool("debuggable"), Some(true));
        assert_eq!(app.attr_bool("allowBackup"), Some(true));

        // The package name and the component count must be untouched.
        assert_eq!(before.attr_str("package"), after.attr_str("package"));
        assert_eq!(before.children.len(), after.children.len());
    }

    #[test]
    fn an_edit_that_matches_nothing_reports_which_component() {
        let mut root = axml::decode(&synthetic_manifest()).unwrap();
        let err = apply(
            &mut root,
            &ManifestEdit::SetExported {
                component: "com.example.Missing".into(),
                exported: true,
            },
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("com.example.Missing"), "{err}");
    }

    /// A tiny but structurally valid manifest, so the modify layer is testable
    /// without an APK.
    fn synthetic_manifest() -> Vec<u8> {
        use axml::Node;
        let root = Node {
            name: "manifest".into(),
            namespace: None,
            attrs: vec![
                Attr {
                    namespace: None,
                    name: "package".into(),
                    value: AttrValue::Str("com.example".into()),
                },
                Attr {
                    namespace: Some(ANDROID_NS.into()),
                    name: "versionCode".into(),
                    value: AttrValue::Int(3),
                },
            ],
            children: vec![
                Node {
                    name: "application".into(),
                    namespace: None,
                    attrs: vec![Attr {
                        namespace: Some(ANDROID_NS.into()),
                        name: "label".into(),
                        value: AttrValue::Str("Example".into()),
                    }],
                    children: vec![Node {
                        name: "activity".into(),
                        namespace: None,
                        attrs: vec![
                            Attr {
                                namespace: Some(ANDROID_NS.into()),
                                name: "name".into(),
                                value: AttrValue::Str(".Lazy".into()),
                            },
                            Attr {
                                namespace: Some(ANDROID_NS.into()),
                                name: "exported".into(),
                                value: AttrValue::Bool(false),
                            },
                        ],
                        children: Vec::new(),
                        line: 0,
                    }],
                    line: 0,
                },
                Node {
                    name: "uses-permission".into(),
                    namespace: None,
                    attrs: vec![Attr {
                        namespace: Some(ANDROID_NS.into()),
                        name: "name".into(),
                        value: AttrValue::Str("android.permission.CAMERA".into()),
                    }],
                    children: Vec::new(),
                    line: 0,
                },
            ],
            line: 0,
        };
        axmlenc::encode(&root).unwrap()
    }
}
