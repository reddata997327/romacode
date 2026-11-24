//! The end-to-end workflow: acquire, analyse, modify, rebuild, sign, install, run.
//!
//! Each step is also usable on its own; [`workflow`] chains them and returns a
//! step-by-step report so an agent can narrate exactly what happened and where
//! it stopped.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::adb::{self, AdbDevice, AdbOptions, InstallOptions};
use crate::api;
use crate::crypto;
use crate::key::AdbKey;
use crate::modify::{self, ManifestEdit};
use crate::net::{self, FetchOptions};
use crate::repack;
use crate::sign::{self, ApkSigner, SignOptions};

/// A signing identity that stays stable across runs, so successive builds of
/// the same app share a certificate and can update one another.
pub struct SigningIdentity {
    pub signer: ApkSigner,
    pub key_path: PathBuf,
    pub cert_path: PathBuf,
}

impl SigningIdentity {
    /// Load `<dir>/signing-key.pem` + `signing-cert.der`, generating both on
    /// first use.
    pub fn load_or_create(dir: &Path, cn: &str) -> Result<Self> {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let key_path = dir.join("signing-key.pem");
        let cert_path = dir.join("signing-cert.der");

        if key_path.exists() && cert_path.exists() {
            restrict_private_permissions(&key_path)?;
            let pem = std::fs::read_to_string(&key_path)?;
            let key = crypto::from_pkcs8_pem(&pem)?;
            let der = std::fs::read(&cert_path)?;
            let certificate = rustls::pki_types::CertificateDer::from(der.as_slice());
            let parsed = rustls::server::ParsedCertificate::try_from(&certificate)
                .map_err(|e| anyhow::anyhow!("invalid signing certificate: {e}"))?;
            let key_spki = crypto::spki_der(&key)?;
            if parsed.subject_public_key_info().as_ref() != key_spki {
                bail!(
                    "signing certificate public key does not match {}",
                    key_path.display()
                );
            }
            // The issuer name and serial are derived from the key, so they can
            // be recomputed; only the DER bytes need persisting.
            let mut cert = crypto::self_signed_cert_material(&key, cn, 3650)?;
            cert.der = der;
            return Ok(SigningIdentity {
                signer: ApkSigner { key, cert },
                key_path,
                cert_path,
            });
        }

        let signer = ApkSigner::generate(cn)?;
        write_private_key(&key_path, &crypto::to_pkcs8_pem(&signer.key)?)?;
        std::fs::write(&cert_path, &signer.cert.der)?;
        Ok(SigningIdentity {
            signer,
            key_path,
            cert_path,
        })
    }
}

fn write_private_key(path: &Path, pem: &str) -> Result<()> {
    if path.exists() {
        restrict_private_permissions(path)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("writing {}", path.display()))?;
    file.write_all(pem.as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("flushing {}", path.display()))?;
    restrict_private_permissions(path)?;
    Ok(())
}

#[cfg(unix)]
fn restrict_private_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_mode(0o600);
    std::fs::set_permissions(path, permissions)
        .with_context(|| format!("restricting permissions on {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn restrict_private_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

/// One step of the workflow, as reported back.
#[derive(Debug, Clone)]
pub struct Step {
    pub name: String,
    pub detail: String,
    pub ok: bool,
}

#[derive(Debug, Clone, Default)]
pub struct WorkflowReport {
    pub apk: Option<PathBuf>,
    pub steps: Vec<Step>,
}

impl WorkflowReport {
    fn add(&mut self, name: impl Into<String>, detail: impl Into<String>) {
        self.steps.push(Step {
            name: name.into(),
            detail: detail.into(),
            ok: true,
        });
    }

    fn fail(&mut self, name: impl Into<String>, detail: impl Into<String>) {
        self.steps.push(Step {
            name: name.into(),
            detail: detail.into(),
            ok: false,
        });
    }

    pub fn succeeded(&self) -> bool {
        self.steps.iter().all(|s| s.ok)
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        for step in &self.steps {
            out.push_str(&format!(
                "{} {:<22} {}\n",
                if step.ok { "ok  " } else { "FAIL" },
                step.name,
                step.detail
            ));
        }
        out
    }
}

/// Fetch an APK from a URL, or accept a local path as-is.
pub fn acquire(source: &str, dir: &Path) -> Result<(PathBuf, FetchOptions)> {
    if source.starts_with("http://") || source.starts_with("https://") {
        let name = source
            .rsplit('/')
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or("download.apk");
        let name = name.split('?').next().unwrap_or("download.apk");
        let dest = dir.join(sanitise(name));
        let opts = FetchOptions::default();
        let result = net::fetch(source, &dest, &opts)?;
        if !net::looks_like_apk(&dest)? {
            bail!(
                "{} is not a ZIP/APK (first bytes are not PK\\x03\\x04) — got {} bytes of {:?}",
                result.final_url,
                result.bytes,
                result.content_type
            );
        }
        Ok((dest, opts))
    } else {
        let path = PathBuf::from(source);
        if !path.exists() {
            bail!("{} does not exist", path.display());
        }
        Ok((path, FetchOptions::default()))
    }
}

fn sanitise(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "download.apk".into()
    } else {
        cleaned
    }
}

/// Analysis of an APK: container summary, decoded manifest, static findings.
pub fn analyze(apk: &Path) -> Result<serde_json::Value> {
    let path = apk.to_string_lossy().to_string();
    let info = api::info(&path)?;
    let manifest = api::manifest(&path)?;
    let scan = api::scan(&path, "info")?;
    Ok(serde_json::json!({
        "apk": path,
        "info": info,
        "manifest": manifest,
        "scan": scan,
    }))
}

/// Apply manifest edits and rebuild the APK, leaving it unsigned.
pub fn modify(apk: &Path, output: &Path, edits: &[ManifestEdit]) -> Result<(Vec<String>, u64)> {
    let entries = repack::read_apk(apk)?;
    let manifest_entry = entries
        .iter()
        .find(|e| e.name == "AndroidManifest.xml")
        .ok_or_else(|| anyhow::anyhow!("{} has no AndroidManifest.xml", apk.display()))?;
    let (new_manifest, changes) = modify::modify_manifest(&manifest_entry.data, edits)?;
    sign::repack_apk(
        apk,
        output,
        &[("AndroidManifest.xml".to_string(), new_manifest)],
    )?;
    let size = std::fs::metadata(output)?.len();
    Ok((changes, size))
}

/// Sign an APK.
pub fn sign_with(
    apk: &Path,
    output: &Path,
    identity: &SigningIdentity,
    opts: &SignOptions,
) -> Result<sign::SignReport> {
    sign::sign_apk(apk, output, &identity.signer, opts)
}

/// Verify an APK's signatures the way a device would, structurally.
pub fn verify(apk: &Path) -> Result<serde_json::Value> {
    let v2 = sign::verify_v2(apk)?;
    let v1 = sign::verify_v1(apk)?;
    Ok(serde_json::json!({
        "v1": {
            "present": v1.present,
            "signed_entries": v1.signed_entries,
            "x_android_apk_signed": v1.x_android_apk_signed,
        },
        "v2": {
            "present": v2.found,
            "signers": v2.signers,
            "algorithm": format!("{:#06x}", v2.algorithm),
            "digest_ok": v2.digest_ok,
            "signature_ok": v2.signature_ok,
            "certificate_bytes": v2.certificate_bytes,
            "certificate_sha256": v2.certificate_sha256,
            "errors": v2.errors,
        },
        "valid": v2.valid(),
    }))
}

/// Connect to a device and install an APK.
pub fn install(
    apk: &Path,
    target: &str,
    key: &AdbKey,
    opts: &InstallOptions,
) -> Result<(adb::InstallResult, adb::DeviceInfo)> {
    let verification = sign::verify_v2(apk).context("verifying APK before installation")?;
    if !verification.valid() {
        bail!(
            "refusing to install APK whose v2 signature did not verify: {}",
            verification.errors.join("; ")
        );
    }
    let mut device = AdbDevice::connect(target, key, &AdbOptions::default())?;
    let info = device.info.clone();
    let result = device.install(apk, opts)?;
    Ok((result, info))
}

/// Connect and launch a package (or an explicit component).
pub fn launch(target: &str, key: &AdbKey, component: &str) -> Result<String> {
    let mut device = AdbDevice::connect(target, key, &AdbOptions::default())?;
    device.launch(component)
}

#[derive(Debug, Clone, Default)]
pub struct WorkflowRequest {
    pub source: String,
    pub work_dir: PathBuf,
    pub edits: Vec<ManifestEdit>,
    /// Optional device target; when absent the workflow stops after signing.
    pub device: Option<String>,
    pub launch: bool,
    pub install: bool,
}

/// Run the full workflow and report every step.
pub fn workflow(
    req: &WorkflowRequest,
    identity: &SigningIdentity,
) -> Result<(WorkflowReport, PathBuf)> {
    let mut report = WorkflowReport::default();
    std::fs::create_dir_all(&req.work_dir).ok();

    let (apk, _) = acquire(&req.source, &req.work_dir)?;
    report.add("acquire", format!("{}", apk.display()));

    let analysis = analyze(&apk)?;
    let package = analysis["manifest"]["package"]
        .as_str()
        .unwrap_or("?")
        .to_string();
    let findings = analysis["scan"]["counts"]["total"].as_u64().unwrap_or(0);
    report.add(
        "analyze",
        format!("{package}: {findings} static finding(s)"),
    );

    let unsigned = req.work_dir.join(format!("{package}-unsigned.apk"));
    let (changes, size) = modify(&apk, &unsigned, &req.edits)?;
    report.add(
        "modify",
        format!(
            "{} change(s) -> {} ({} bytes)",
            changes.len(),
            unsigned.display(),
            size
        ),
    );

    let signed = req.work_dir.join(format!("{package}-signed.apk"));
    let sign_report = sign_with(&unsigned, &signed, identity, &SignOptions::default())?;
    report.add(
        "sign",
        format!(
            "v1={} v2={}, digest {}",
            sign_report.v1,
            sign_report.v2,
            &sign_report.content_digest[..16]
        ),
    );

    let verification = verify(&signed)?;
    let valid = verification["valid"].as_bool().unwrap_or(false);
    if valid {
        report.add("verify", "v2 digest and signature check out");
    } else {
        report.fail("verify", format!("{verification}"));
    }

    if let Some(target) = &req.device {
        if !valid {
            if req.install {
                report.fail("install", "refused: APK verification failed");
            }
            if req.launch {
                report.fail("launch", "refused: APK verification failed");
            }
        } else {
            let key = crate::key::load_or_generate(&crate::key::default_dir())?;
            if req.install {
                match install(&signed, target, &key, &InstallOptions::default()) {
                    Ok((result, info)) => {
                        if result.success {
                            report.add(
                                "install",
                                format!("{} on {} (sdk {:?})", package, info.model(), info.sdk()),
                            );
                        } else {
                            report.fail("install", result.output.trim().replace('\n', " | "));
                        }
                    }
                    Err(e) => report.fail("install", format!("{e:#}")),
                }
            }
            if req.launch {
                match launch(target, &key, &package) {
                    Ok(out) => report.add(
                        "launch",
                        out.lines().next().unwrap_or("started").to_string(),
                    ),
                    Err(e) => report.fail("launch", format!("{e:#}")),
                }
            }
        }
    } else {
        report.add("device", "none given — stopping after signing");
    }

    Ok((report, signed))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn real_apk() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/test-app.apk")
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mandroid-flow-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn signing_identity_is_stable_across_loads() {
        let dir = temp_dir("ident");
        let first = SigningIdentity::load_or_create(&dir, "mandroid").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&first.key_path)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600,
                "persisted APK signing key must be owner-only"
            );
        }
        let second = SigningIdentity::load_or_create(&dir, "mandroid").unwrap();
        // The same certificate bytes and the same public modulus must come back.
        assert_eq!(first.signer.cert.der, second.signer.cert.der);
        assert_eq!(
            crypto::public_numbers(&first.signer.key).0,
            crypto::public_numbers(&second.signer.key).0
        );
        assert_eq!(first.signer.subject(), "mandroid");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn signing_identity_rejects_a_certificate_for_a_different_key() {
        let dir = temp_dir("identity-mismatch");
        let identity = SigningIdentity::load_or_create(&dir, "original").unwrap();
        let other = ApkSigner::generate("other").unwrap();
        std::fs::write(&identity.cert_path, other.cert.der).unwrap();
        let error = SigningIdentity::load_or_create(&dir, "original")
            .err()
            .expect("mismatched certificate must be rejected");
        assert!(error.to_string().contains("does not match"), "{error:#}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn acquire_accepts_a_local_path_and_rejects_a_missing_one() {
        let dir = temp_dir("acq");
        assert!(acquire("testdata/test-app.apk", &dir).is_ok());
        let err = acquire("does/not/exist.apk", &dir).unwrap_err().to_string();
        assert!(err.contains("does not exist"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The whole point of this module: take a real, signed APK, change its
    /// declared behaviour, rebuild and re-sign it, and confirm the result is
    /// still a valid, verifiable APK whose manifest really did change.
    #[test]
    fn full_modify_and_resign_of_a_real_apk() {
        let apk = real_apk();
        if !apk.exists() {
            eprintln!("skipping: {} not present", apk.display());
            return;
        }
        let dir = temp_dir("e2e");
        let before = crate::api::manifest(&apk.to_string_lossy()).unwrap();
        assert_ne!(
            before["application"]["debuggable"].as_bool(),
            Some(true),
            "the fixture should not already be debuggable"
        );

        // Make it debuggable without changing the package name — an app whose
        // package changed would install alongside rather than replace.
        let unsigned = dir.join("unsigned.apk");
        let (changes, _) = modify(
            &apk,
            &unsigned,
            &[
                ManifestEdit::SetDebuggable(true),
                ManifestEdit::SetAllowBackup(true),
            ],
        )
        .unwrap();
        assert_eq!(changes.len(), 2);

        let identity = SigningIdentity::load_or_create(&dir, "mandroid e2e").unwrap();
        let signed = dir.join("signed.apk");
        let report = sign_with(&unsigned, &signed, &identity, &SignOptions::default()).unwrap();
        assert!(report.v1 && report.v2);
        assert_eq!(
            report.removed_signatures, 0,
            "the rebuilt APK carried no signatures"
        );

        // Our own verifier must accept it...
        let verification = verify(&signed).unwrap();
        assert_eq!(
            verification["valid"], true,
            "signed APK must verify: {verification}"
        );
        assert_eq!(verification["v2"]["digest_ok"], true);
        assert_eq!(verification["v2"]["signature_ok"], true);
        // ...and so must an independent ZIP reader.
        let archive = zip::ZipArchive::new(std::fs::File::open(&signed).unwrap()).unwrap();
        assert!(archive.len() >= 15, "entries should survive the rebuild");

        // The modification must be visible in the signed APK's manifest.
        let after = crate::api::manifest(&signed.to_string_lossy()).unwrap();
        assert_eq!(after["application"]["debuggable"], true);
        assert_eq!(after["application"]["allow_backup"], true);
        assert_eq!(
            after["package"], before["package"],
            "the package name must not change or the install would be a different app"
        );
        assert_eq!(
            after["components"].as_array().map(|a| a.len()),
            before["components"].as_array().map(|a| a.len()),
            "every component must survive the manifest round-trip"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_preset_drives_the_same_pipeline() {
        let apk = real_apk();
        if !apk.exists() {
            return;
        }
        let dir = temp_dir("preset");
        let edits = modify::preset("debug").unwrap();
        let unsigned = dir.join("u.apk");
        let (changes, _) = modify(&apk, &unsigned, &edits).unwrap();
        assert_eq!(changes.len(), edits.len());

        let identity = SigningIdentity::load_or_create(&dir, "preset").unwrap();
        let signed = dir.join("s.apk");
        sign_with(&unsigned, &signed, &identity, &SignOptions::default()).unwrap();
        assert!(verify(&signed).unwrap()["valid"].as_bool().unwrap());
        assert_eq!(
            crate::api::manifest(&signed.to_string_lossy()).unwrap()["application"]["debuggable"],
            true
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn verify_reports_a_v2_only_or_v1_only_apk_honestly() {
        let dir = temp_dir("unverified");
        let unsigned = dir.join("plain.apk");
        let entries = vec![repack::Entry::new("AndroidManifest.xml", vec![3, 0, 8, 0])];
        let sections = repack::build_sections(&entries).unwrap();
        std::fs::write(
            &unsigned,
            repack::assemble(&sections.content, None, &sections.central_directory, 1),
        )
        .unwrap();

        let report = verify(&unsigned).unwrap();
        assert_eq!(report["v2"]["present"], false);
        assert_eq!(report["v1"]["present"], false);
        assert_eq!(report["valid"], false, "an unsigned APK is not valid");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn direct_install_refuses_an_unverified_apk_before_connecting() {
        let dir = temp_dir("install-unverified");
        let unsigned = dir.join("unsigned.apk");
        let entries = vec![repack::Entry::new("AndroidManifest.xml", vec![3, 0, 8, 0])];
        let sections = repack::build_sections(&entries).unwrap();
        std::fs::write(
            &unsigned,
            repack::assemble(&sections.content, None, &sections.central_directory, 1),
        )
        .unwrap();
        let adb_key = crate::key::generate_at(&dir.join("adb")).unwrap();
        let error = install(
            &unsigned,
            "127.0.0.1:1",
            &adb_key,
            &InstallOptions::default(),
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("refusing to install"),
            "{error:#}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn workflow_stops_after_signing_when_no_device_is_given() {
        let apk = real_apk();
        if !apk.exists() {
            return;
        }
        let dir = temp_dir("wf");
        let identity = SigningIdentity::load_or_create(&dir, "wf").unwrap();
        let req = WorkflowRequest {
            source: apk.to_string_lossy().to_string(),
            work_dir: dir.clone(),
            edits: vec![ManifestEdit::SetDebuggable(true)],
            device: None,
            install: false,
            launch: false,
        };
        let (report, signed) = workflow(&req, &identity).unwrap();
        assert!(report.succeeded(), "{}", report.render());
        let names: Vec<&str> = report.steps.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["acquire", "analyze", "modify", "sign", "verify", "device"]
        );
        assert!(signed.exists());
        assert!(verify(&signed).unwrap()["valid"].as_bool().unwrap());
        std::fs::remove_dir_all(&dir).ok();
    }
}
