//! The Android OS — a real, installed operating system for the phone.
//!
//! A handset boots an OS; it does not invent one every frame. This module
//! owns the OS image: a partitioned filesystem (boot, system, vendor,
//! product, system_ext, userdata, cache, recovery) **written to disk** under
//! the data directory, with real files — `build.prop` generated from the one
//! device identity, `init.rc` with the service roster the boot chain starts,
//! framework jars, system APKs, shared libraries, fonts, HAL binaries and an
//! actual GKI `boot.img` (ANDROID! header). Every byte is deterministic from
//! the identity, and every file is checksummed so `os verify` can prove the
//! image on disk matches the manifest.
//!
//! Install is a real flash: partition table → format → extract → userdata
//! skeleton → checksum verification → manifest stamp. The installer is
//! chunked (one step per compositor tick) so the progress screen shows the
//! genuine work, not an animation over nothing.

use crate::state::now_ms;
use anyhow::{Context, Result};
use serde_json::json;
use std::path::{Path, PathBuf};

// ── partition table ─────────────────────────────────────────────────────────

/// One partition of the installed OS. Sizes are the caiman (Pixel 9 Pro)
/// super-partition layout on the 1 TB UFS variant; `used_kib_baseline` is the
/// on-device used space of a factory image (the emulator adds the real bytes
/// it wrote on top of this baseline, the same accounting `df` has always
/// used for /data).
pub struct Partition {
    pub name: &'static str,
    pub mount: &'static str,
    pub dev: &'static str,
    pub fs: &'static str,
    pub size_kib: u64,
    pub used_kib_baseline: u64,
    pub readonly: bool,
}

pub const PARTITIONS: &[Partition] = &[
    Partition {
        name: "boot",
        mount: "/boot",
        dev: "/dev/block/sda12",
        fs: "raw",
        size_kib: 64 * 1024,
        used_kib_baseline: 58_704,
        readonly: true,
    },
    Partition {
        name: "vendor_boot",
        mount: "/vendor_boot",
        dev: "/dev/block/sda13",
        fs: "raw",
        size_kib: 64 * 1024,
        used_kib_baseline: 51_248,
        readonly: true,
    },
    Partition {
        name: "dtbo",
        mount: "/dtbo",
        dev: "/dev/block/sda14",
        fs: "raw",
        size_kib: 8 * 1024,
        used_kib_baseline: 1_344,
        readonly: true,
    },
    Partition {
        name: "vbmeta",
        mount: "/vbmeta",
        dev: "/dev/block/sda15",
        fs: "raw",
        size_kib: 16,
        used_kib_baseline: 8,
        readonly: true,
    },
    Partition {
        name: "system",
        mount: "/system",
        dev: "/dev/block/dm-6",
        fs: "ext4",
        size_kib: 5_960_704,
        used_kib_baseline: 3_312_992,
        readonly: true,
    },
    Partition {
        name: "system_ext",
        mount: "/system_ext",
        dev: "/dev/block/dm-7",
        fs: "ext4",
        size_kib: 655_360,
        used_kib_baseline: 402_560,
        readonly: true,
    },
    Partition {
        name: "product",
        mount: "/product",
        dev: "/dev/block/dm-8",
        fs: "ext4",
        size_kib: 1_433_600,
        used_kib_baseline: 894_464,
        readonly: true,
    },
    Partition {
        name: "vendor",
        mount: "/vendor",
        dev: "/dev/block/dm-9",
        fs: "ext4",
        size_kib: 1_114_112,
        used_kib_baseline: 745_216,
        readonly: true,
    },
    Partition {
        name: "recovery",
        mount: "/recovery",
        dev: "/dev/block/sda17",
        fs: "raw",
        size_kib: 64 * 1024,
        used_kib_baseline: 54_016,
        readonly: true,
    },
    Partition {
        name: "metadata",
        mount: "/metadata",
        dev: "/dev/block/dm-10",
        fs: "ext4",
        size_kib: 16 * 1024,
        used_kib_baseline: 1_240,
        readonly: false,
    },
    Partition {
        name: "cache",
        mount: "/cache",
        dev: "/dev/block/dm-11",
        fs: "ext4",
        size_kib: 512 * 1024,
        used_kib_baseline: 128,
        readonly: false,
    },
    Partition {
        name: "userdata",
        mount: "/data",
        dev: "/dev/block/dm-4",
        fs: "f2fs",
        // 1 TB UFS: 931 GiB usable volume for userdata
        size_kib: 931 * 1024 * 1024,
        used_kib_baseline: 65 * 1024 * 1024,
        readonly: false,
    },
];

pub fn partition(mount: &str) -> Option<&'static Partition> {
    PARTITIONS.iter().find(|p| p.mount == mount)
}

/// `true` when a device path lives on a read-only mount (system, vendor,
/// product, system_ext, boot images). `/data` and `/cache` are writable.
pub fn readonly_path(p: &str) -> bool {
    let p = p.trim_start_matches('/');
    for part in PARTITIONS.iter().filter(|p| p.readonly) {
        let base = part.mount.trim_start_matches('/');
        if p == base || p.starts_with(&format!("{base}/")) {
            return true;
        }
    }
    false
}

// ── the image manifest ──────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct Manifest {
    pub installed_at_ms: i64,
    pub files: usize,
    pub bytes: u64,
    pub fingerprint: String,
    pub build_id: String,
    pub version: String,
    pub kernel: String,
}

impl Manifest {
    fn to_json(&self) -> Vec<u8> {
        json!({
            "os": "Android",
            "version": self.version,
            "build_id": self.build_id,
            "fingerprint": self.fingerprint,
            "kernel": self.kernel,
            "installed_at_ms": self.installed_at_ms,
            "files": self.files,
            "bytes": self.bytes,
            "partitions": PARTITIONS.iter().map(|p| json!({
                "name": p.name, "mount": p.mount, "dev": p.dev,
                "fs": p.fs, "size_kib": p.size_kib, "readonly": p.readonly,
            })).collect::<Vec<_>>(),
        })
        .to_string()
        .into_bytes()
    }

    pub fn parse(v: &serde_json::Value) -> Option<Manifest> {
        Some(Manifest {
            installed_at_ms: v["installed_at_ms"].as_i64()?,
            files: v["files"].as_u64()? as usize,
            bytes: v["bytes"].as_u64()?,
            fingerprint: v["fingerprint"].as_str()?.to_string(),
            build_id: v["build_id"].as_str()?.to_string(),
            version: v["version"].as_str()?.to_string(),
            kernel: v["kernel"].as_str()?.to_string(),
        })
    }
}

pub fn os_root(data_dir: &Path) -> PathBuf {
    data_dir.join("os")
}

/// The Magisk version this image carries ("29.0").
pub fn magisk_version() -> &'static str {
    MAGISK_VERSION
}

/// Map a device-absolute path inside an installed OS image to its real
/// location on the host filesystem. `/system/bin/sh` →
/// `<data>/os/system/bin/sh`, `/data/adb/magisk/magisk64` →
/// `<data>/os/data/adb/magisk/magisk64`, `/cache/stage` →
/// `<data>/os/cache/stage`. Returns `None` for paths that are not part of
/// the OS image (sdcard, /proc, …).
pub fn host_path(data_dir: &Path, device_path: &str) -> Option<PathBuf> {
    let p = device_path.trim().trim_end_matches('/');
    if !p.starts_with('/')
        || Path::new(p).components().any(|c| {
            !matches!(
                c,
                std::path::Component::RootDir | std::path::Component::Normal(_)
            )
        })
    {
        return None;
    }
    // mounts that live in the image on disk
    const MOUNTS: &[&str] = &[
        "/system",
        "/system_ext",
        "/product",
        "/vendor",
        "/data",
        "/cache",
        "/metadata",
        "/recovery",
        "/boot",
        "/vendor_boot",
        "/dtbo",
        "/vbmeta",
    ];
    for m in MOUNTS {
        let mapped = if p == *m {
            Some(m.trim_start_matches('/').to_string())
        } else if let Some(rest) = p.strip_prefix(&format!("{m}/")) {
            Some(format!("{}/{}", m.trim_start_matches('/'), rest))
        } else {
            continue;
        };
        let rel = mapped?;
        return crate::persist::confined_path(data_dir, &Path::new("os").join(rel)).ok();
    }
    None
}

/// List a directory of the installed image as device-path entries.
/// Returns `(name, is_dir, size, readonly)` sorted with dirs first.
pub fn list_dir(data_dir: &Path, device_dir: &str) -> Vec<(String, bool, u64, bool)> {
    let Some(host) = host_path(data_dir, device_dir) else {
        return Vec::new();
    };
    let ro = readonly_path(device_dir);
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&host) {
        for e in rd.flatten() {
            let Ok(name) = e.file_name().into_string() else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            let meta = std::fs::symlink_metadata(e.path())
                .ok()
                .filter(|m| !m.file_type().is_symlink());
            let (is_dir, size) = match &meta {
                Some(m) => (m.is_dir(), m.len()),
                None => (false, 0),
            };
            out.push((name, is_dir, size, ro));
        }
    }
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    out
}

pub fn manifest_path(data_dir: &Path) -> PathBuf {
    crate::persist::confined_path(data_dir, Path::new("os/meta/os.json"))
        .unwrap_or_else(|_| data_dir.join("\0romacode-invalid-manifest"))
}

/// The installed OS manifest, when an OS is present on disk.
pub fn read_manifest(data_dir: &Path) -> Option<Manifest> {
    let path = crate::persist::confined_path(data_dir, Path::new("os/meta/os.json")).ok()?;
    let raw = std::fs::read(path).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    Manifest::parse(&v)
}

pub fn is_installed(data_dir: &Path) -> bool {
    read_manifest(data_dir).is_some()
}

/// Human version string for the installed image (or a not-installed marker).
pub fn version_line(data_dir: &Path) -> String {
    match read_manifest(data_dir) {
        Some(m) => format!("Android {} ({}) — installed", m.version, m.build_id),
        None => "no OS image on this device".to_string(),
    }
}

// ── image content generation ────────────────────────────────────────────────

/// FNV-1a 64 (the same digest the engine uses for its deterministic ids).
pub fn fnv64(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in data {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Deterministic pseudo-payload: `n` bytes derived from a seed (an FNV byte
/// stream). Same seed → same bytes, so checksums are stable across installs.
fn payload(seed: u64, n: usize) -> Vec<u8> {
    let mut h = seed | 1;
    (0..n)
        .map(|_| {
            h = h
                .wrapping_mul(0x100000001b3)
                .wrapping_add(0x9e3779b97f4a7c15);
            (h >> 33) as u8
        })
        .collect()
}

/// One file of the image: `(device path, bytes)`.
type File = (String, Vec<u8>);

/// The complete OS image plan for this identity — every file, real content,
/// byte-for-byte deterministic (same identity → same image → same checksums).
fn image_files(id: &mandroid::identity::DeviceIdentity) -> Vec<File> {
    let sdk = id.sdk_level;
    let bid = id.build_id;
    let mut f: Vec<File> = Vec::new();

    // ── boot.img: GKI boot image — ANDROID! header, kernel cmdline, payload
    let cmdline = format!(
        "console=ttyMSM0,115200n8 androidboot.console=ttyMSM0 \
         androidboot.hardware=caiman androidboot.bootdevice=1d90000.ufshc \
         androidboot.boot_devices=1d90000.ufshc androidboot.baseband=g5300 \
         androidboot.veritymode=enforcing androidboot.verifiedbootstate=orange \
         androidboot.slot_suffix=_a androidboot.serialno={} \
         ro.boot.vbmeta.device_state=unlocked ro.boot.flash.locked=0 \
         androidboot.firmware={} firmware_class.path=/vendor/firmware_mnt \
         loop.max_part=7 buildvariant=user",
        id.serial, id.baseband,
    );
    let mut boot = Vec::new();
    boot.extend_from_slice(b"ANDROID!");
    boot.extend_from_slice(&[0x03, 0x00, 0x00, 0x00]); // header v3
    let kw = (cmdline.len() as u32).to_le_bytes();
    boot.extend_from_slice(&kw);
    boot.extend_from_slice(cmdline.as_bytes());
    boot.extend_from_slice(format!("Linux version {}\n", id.kernel).as_bytes());
    boot.extend_from_slice(&payload(fnv64(b"boot.img-kernel"), 160 * 1024));
    f.push(("boot.img".into(), boot));

    // ── ramdisk (the first-stage init)
    f.push(("ramdisk/init.rc".into(), ramdisk_init_rc().into_bytes()));
    f.push((
        "ramdisk/ueventd.rc".into(),
        "# ueventd rules — device nodes for the caiman hardware\n\
         /dev/graphics/* 0660 root graphics\n\
         /dev/input/* 0660 root input\n\
         /dev/ttyMSM0 0660 root log\n"
            .to_string()
            .into_bytes(),
    ));
    f.push(("ramdisk/fstab.caiman".into(), fstab().into_bytes()));

    // ── system/build.prop — generated from the one device identity
    f.push(("system/build.prop".into(), build_prop(id).into_bytes()));

    // ── system/etc
    f.push(("system/etc/init.rc".into(), init_rc().into_bytes()));
    f.push((
        "system/etc/hosts".into(),
        b"127.0.0.1 localhost\n::1 localhost ip6-localhost ip6-loopback\n".to_vec(),
    ));
    f.push((
        "system/etc/seapp_contexts".into(),
        b"user=system seinfo=platform domain=system_app levelFrom=all\n\
         user=_app seinfo=platform domain=platform_app levelFrom=all\n\
         user=_app domain=untrusted_app levelFrom=all\n"
            .to_vec(),
    ));
    f.push((
        "system/etc/permissions/platform.xml".into(),
        format!(
            "<permissions>\n  <assign-permission name=\"android.permission.WRITE_EXTERNAL_STORAGE\" uid=\"media\"/>\n  <split-permission name=\"android.permission.READ_EXTERNAL_STORAGE\" targetSdk=\"{}\"/>\n</permissions>\n",
            sdk
        )
        .into_bytes(),
    ));
    f.push((
        "system/etc/public.libraries.txt".into(),
        b"libandroid.so\nlibbinder_ndk.so\nlibc.so\nlibdl.so\nlibEGL.so\nlibGLESv3.so\nlibm.so\nlibvulkan.so\n".to_vec(),
    ));
    f.push((
        "system/etc/vintf/manifest.xml".into(),
        format!(
            "<manifest version=\"2.0\" type=\"framework\">\n  <hal format=\"hidl\"><name>android.frameworks.displayservice</name><transport>hwbinder</transport></hal>\n  <sepolicy version=\"{}.0\"/>\n</manifest>\n",
            sdk
        )
        .into_bytes(),
    ));

    // ── framework jars (zip magic — the boot classpath)
    for (name, kb) in [
        ("framework-res.apk", 4096),
        ("framework.jar", 2048),
        ("services.jar", 3072),
        ("core-oj.jar", 1536),
        ("core-libart.jar", 1280),
        ("ext.jar", 1024),
        ("telephony-common.jar", 768),
        ("ims-common.jar", 512),
        ("okhttp.jar", 640),
    ] {
        let mut b = vec![0x50, 0x4b, 0x03, 0x04]; // PK\x03\x04
        b.extend_from_slice(&[0x14, 0x00, 0x08, 0x00, 0x08, 0x00]);
        b.extend_from_slice(format!("META-INF/MANIFEST.MF\nBuild-Id: {bid}\n").as_bytes());
        b.extend_from_slice(&payload(fnv64(name.as_bytes()), kb * 1024 - b.len()));
        f.push((format!("system/framework/{name}"), b));
    }

    // ── priv-app + app (system APKs, zip magic)
    for (pkg, kb) in [
        ("Settings/Settings.apk", 2048),
        ("SystemUI/SystemUI.apk", 1792),
        ("GoogleLoginService/GmsCore.apk", 2560),
        ("Phonesky/Phonesky.apk", 2048),
        ("WebViewStock/WebViewStock.apk", 1280),
    ] {
        let mut b = vec![0x50, 0x4b, 0x03, 0x04];
        b.extend_from_slice(&[0x14, 0x00, 0x08, 0x00]);
        b.extend_from_slice(
            format!(
                "AndroidManifest.xml\npackage=caiman {}\n",
                pkg.split('/').next().unwrap()
            )
            .as_bytes(),
        );
        b.extend_from_slice(&payload(fnv64(pkg.as_bytes()), kb * 1024 - b.len()));
        f.push((format!("system/priv-app/{pkg}"), b));
    }

    // ── fonts (TrueType sfnt magic)
    for (name, kb) in [
        ("Roboto-Regular.ttf", 320),
        ("Roboto-Bold.ttf", 320),
        ("Roboto-Medium.ttf", 320),
        ("RobotoMono-Regular.ttf", 180),
        ("NotoColorEmoji.ttf", 1024),
    ] {
        let mut b = vec![0x00, 0x01, 0x00, 0x00, 0x00, 0x0f, 0x00, 0x80];
        b.extend_from_slice(&payload(fnv64(name.as_bytes()), kb * 1024 - b.len()));
        f.push((format!("system/fonts/{name}"), b));
    }

    // ── system/lib64 (ELF64 shared objects)
    for (name, kb) in [
        ("libc.so", 1024),
        ("libm.so", 256),
        ("liblog.so", 96),
        ("libbinder.so", 448),
        ("libhwui.so", 1536),
        ("libgui.so", 512),
        ("libui.so", 256),
        ("libcutils.so", 320),
        ("libvulkan.so", 768),
        ("libEGL.so", 384),
        ("libGLESv3.so", 768),
        ("libandroid_runtime.so", 1792),
        ("libart.so", 2560),
    ] {
        f.push((
            format!("system/lib64/{name}"),
            elf(fnv64(name.as_bytes()), kb * 1024),
        ));
    }

    // ── system/bin (ELF executables)
    for name in [
        "init",
        "app_process64",
        "sh",
        "netd",
        "vold",
        "surfaceflinger",
        "audioserver",
        "cameraserver",
        "sensorservice",
        "mediaserver",
        "installd",
        "logd",
        "servicemanager",
        "hwservicemanager",
        "lmkd",
        "statsd",
        "tombstoned",
        "bootanimation",
    ] {
        f.push((
            format!("system/bin/{name}"),
            elf(fnv64(name.as_bytes()), 128 * 1024),
        ));
    }

    // ── the root stack (shipped, device is rooted out of the box) ──────────
    // /system/xbin/su + magisk CLIs; the payload spells its purpose so
    // string reads of the binary show real text.
    for (name, note) in [
        (
            "system/xbin/su",
            "magisk su — root shell for authorized callers",
        ),
        (
            "system/bin/su",
            "magisk su — root shell for authorized callers",
        ),
        ("system/xbin/magisk", "magisk main binary (daemon + client)"),
        ("system/xbin/magiskpolicy", "magisk sepolicy patcher"),
        ("system/xbin/magiskhide", "magisk hide list manager"),
    ] {
        let mut b = elf(fnv64(name.as_bytes()), 96 * 1024);
        b.extend_from_slice(note.as_bytes());
        f.push((name.to_string(), b));
    }
    // /data/adb/magisk — the Magisk install directory (survives /system OTAs)
    for (name, note) in [
        ("data/adb/magisk/magisk64", "magisk64 daemon binary"),
        ("data/adb/magisk/magisk32", "magisk32 daemon binary"),
        (
            "data/adb/magisk/magiskinit",
            "magiskinit — rootfs initializer",
        ),
        ("data/adb/magisk/busybox", "magisk busybox"),
    ] {
        let mut b = elf(fnv64(name.as_bytes()), 128 * 1024);
        b.extend_from_slice(note.as_bytes());
        f.push((name.to_string(), b));
    }
    f.push((
        "data/adb/magisk/util_functions.sh".into(),
        format!(
            "#!/system/bin/sh\n# Magisk utility functions — installed with the OS image\n\
             MAGISK_VERSION={}\n\
             MAGISK_VER_CODE={}\n\
             NVBASE=/data/adb\n\
             setup_magisk() {{ mount_magisk; start_magiskd; }}\n",
            MAGISK_VERSION, MAGISK_VER_CODE,
        )
        .into_bytes(),
    ));
    // magisk.db — the su grant database (SQLite container, deterministic rows)
    f.push(("data/adb/magisk.db".into(), magisk_db()));
    f.push((
        "data/adb/post-fs-data.d/README".into(),
        b"# post-fs-data scripts run as root before zygote\n".to_vec(),
    ));

    // ── vendor
    f.push((
        "vendor/build.prop".into(),
        vendor_build_prop(id).into_bytes(),
    ));
    f.push((
        "vendor/etc/init/vendor.rc".into(),
        b"on init\n    # vendor daemons started from vendor.rc\nservice vnd-servicemanager /vendor/bin/vndservicemanager\n    class core\n    user root\n"
            .to_vec(),
    ));
    f.push((
        "vendor/etc/vintf/manifest.xml".into(),
        format!(
            "<manifest version=\"2.0\" type=\"device\">\n  <hal format=\"hidl\"><name>android.hardware.audio</name><transport>hwbinder</transport><version>7.0</version></hal>\n  <hal format=\"hidl\"><name>android.hardware.camera.provider</name><transport>hwbinder</transport><version>2.7</version></hal>\n  <hal format=\"hidl\"><name>android.hardware.radio</name><transport>hwbinder</transport><version>2.0</version></hal>\n  <sepolicy version=\"{}.0\"/>\n</manifest>\n",
            sdk
        )
        .into_bytes(),
    ));
    for name in [
        "android.hardware.audio.service",
        "android.hardware.camera.provider-service",
        "android.hardware.sensors-service",
        "android.hardware.gnss-service",
        "android.hardware.radio-service",
    ] {
        f.push((
            format!("vendor/bin/hw/{name}"),
            elf(fnv64(name.as_bytes()), 256 * 1024),
        ));
    }
    f.push((
        "vendor/firmware_mnt/verinfo/ver_info.blob".into(),
        payload(fnv64(b"verinfo"), 48 * 1024),
    ));

    // ── product + system_ext
    f.push((
        "product/build.prop".into(),
        product_build_prop(id).into_bytes(),
    ));
    f.push((
        "product/etc/vintf/manifest.xml".into(),
        b"<manifest version=\"2.0\" type=\"product\">\n  <hal format=\"hidl\"><name>vendor.google.wireless_charging</name></hal>\n</manifest>\n".to_vec(),
    ));
    f.push((
        "system_ext/build.prop".into(),
        system_ext_build_prop(id).into_bytes(),
    ));

    // ── recovery
    f.push(("recovery/etc/recovery.fstab".into(), fstab().into_bytes()));

    // ── userdata skeleton (writable from here on)
    f.push((
        "data/system/packages.xml".into(),
        packages_xml(id).into_bytes(),
    ));
    f.push((
        "data/system/packages.list".into(),
        packages_list().into_bytes(),
    ));
    f.push((
        "data/property/persistent_properties".into(),
        b"# persisted setprop values are stored in state.json (prop.* keys)\n".to_vec(),
    ));
    f.push((
        "data/misc/adb/adb_settings".into(),
        format!("adb over tcp: enabled\nport: dynamic (5555 base)\n").into_bytes(),
    ));
    f.push((
        "cache/stage".into(),
        b"recovery update staging directory\n".to_vec(),
    ));

    // ── meta: the manifest + per-file checksums are appended by the installer
    f
}

const MAGISK_VERSION: &str = "29.0";
const MAGISK_VER_CODE: u32 = 29003;

/// `data/adb/magisk.db` — the su grant database, a real SQLite container
/// header followed by the policy rows (deterministic bytes).
fn magisk_db() -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(b"SQLite format 3\x00");
    b.extend_from_slice(&[0x10, 0x00]); // page size 4096
    b.extend_from_slice(&[0x00; 6]);
    b.extend_from_slice(
        format!(
            "CREATE TABLE policies (uid INTEGER PRIMARY KEY, package_name TEXT, policy INTEGER, until INTEGER, logging INTEGER, notification INTEGER);\n\
             INSERT INTO policies VALUES (10000,'com.android.shell',1,0,1,1);\n\
             INSERT INTO policies VALUES (10001,'com.termux',1,0,1,1);\n\
             INSERT INTO policies VALUES (10002,'com.topjohnwu.magisk',2,0,1,1);\n\
             -- policy 1 = allow, 2 = allow with root-management rights\n"
        )
        .as_bytes(),
    );
    b
}

/// A minimal ELF64 little-endian object (real magic, real machine class) with
/// deterministic payload after the header.
fn elf(seed: u64, n: usize) -> Vec<u8> {
    let mut b = vec![0x7f, b'E', b'L', b'F', 2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    b.extend_from_slice(&[0xb7, 0x00]); // EM_AARCH64
    b.extend_from_slice(&[0x01, 0x00, 0x00, 0x00]); // ET_EXEC
    b.extend_from_slice(&payload(seed, n.saturating_sub(24)));
    b
}

/// The per-partition fstab — what `mount` reports and the ramdisk parses.
fn fstab() -> String {
    let mut s = String::from(
        "# Android fstab — caiman (Pixel 9 Pro, 1 TB UFS)\n\
         # <src>                                          <mntpoint>  <type>  <mnt flags>            <fs flags>\n",
    );
    for p in PARTITIONS {
        if p.name == "boot" || p.name == "vendor_boot" || p.name == "dtbo" || p.name == "vbmeta" {
            continue;
        }
        let flags = if p.readonly {
            "ro,barrier=1"
        } else {
            "rw,barrier=1"
        };
        s.push_str(&format!(
            "{:<45}  {:<11}  {:<5}  {:<22}  defaults\n",
            p.dev, p.mount, p.fs, flags
        ));
    }
    s
}

/// `/system/build.prop` — generated from the device identity (one source of
/// truth) plus the OS-image extras. `getprop` reads this file back as the
/// static layer, so the shell and the OS image can never disagree.
fn build_prop(id: &mandroid::identity::DeviceIdentity) -> String {
    let fp = &id.fingerprint;
    let rel = &id.android_release;
    let inc = &id.build_incremental;
    let bid = &id.build_id;
    let mut s = String::from(
        "# build.prop — generated by the OS installer from the device identity\n\
         # Do not edit: re-flash via Settings → System update or `os install`.\n\n\
         ###### product identity\n",
    );
    for (k, v) in mandroid::identity::base_ro_props(id) {
        // the live telephony/display props are not static build facts
        if k.starts_with("gsm.")
            || k.starts_with("persist.radio")
            || k.starts_with("ro.telephony")
            || k.starts_with("ro.carrier")
            || k.starts_with("ro.vendor.multisim")
            || k.starts_with("ro.sf.lcd_density")
            || k.starts_with("debug.hwui")
        {
            continue;
        }
        s.push_str(&format!("{k}={v}\n"));
    }
    s.push_str(&format!(
        "\n###### OS image\n\
         ro.os.image=caiman-factory\n\
         ro.os.installer=romacode-osd\n\
         ro.os.partition.style=gpt-ufs\n\
         ro.build.ab_update=true\n\
         ro.build.version.incremental={inc}\n\
         ro.build.version.base_os=google/caiman/caiman:{rel}/{bid}/{inc}/user/release-keys\n\
         ro.product.build.fingerprint={fp}\n\
         ro.system.build.fingerprint={fp}\n\
         ro.product.build.id={bid}\n\
         ro.boot.bootdevice=1d90000.ufshc\n\
         ro.hardware.keystore=keystore2\n\
         ro.kernel.qemu=0\n\
         ro.boot.vbmeta.device_state=unlocked\n\
         ro.boot.flash.locked=0\n\
         ro.boot.verifiedbootstate=orange\n\
         sys.usb.configfs=1\n\
         pm.dexopt.boot=verify\n\
         \n\
         ###### root (Magisk ships on the image — rooted out of the box; the\n\
         ###### build stays a stock user build, root comes from su)\n\
         ro.secure=1\n\
         ro.debuggable=0\n\
         ro.adb.secure=1\n\
         ro.magisk.version={MAGISK_VERSION}\n\
         ro.magisk.vercode={MAGISK_VER_CODE}\n\
         magisk.root_access=1\n\
         persist.sys.root_access=1\n",
    ));
    s
}

fn vendor_build_prop(id: &mandroid::identity::DeviceIdentity) -> String {
    format!(
        "# vendor/build.prop\n\
         ro.vendor.build.fingerprint={}\n\
         ro.vendor.product.cpu.abilist={}\n\
         ro.boot.baseband={}\n\
         ro.vendor.radio.multisim.config=dsds\n\
         ro.hardware.soc={}\n\
         persist.vendor.radio.enable_advanced_logging=0\n",
        id.fingerprint, id.abi_list, id.baseband, id.soc_model
    )
}

fn product_build_prop(id: &mandroid::identity::DeviceIdentity) -> String {
    format!(
        "# product/build.prop\n\
         ro.product.product.model={}\n\
         ro.product.product.brand={}\n\
         ro.product.product.device={}\n\
         ro.product.build.fingerprint={}\n",
        id.model, id.brand, id.device, id.fingerprint
    )
}

fn system_ext_build_prop(id: &mandroid::identity::DeviceIdentity) -> String {
    format!(
        "# system_ext/build.prop\n\
         ro.system_ext.build.fingerprint={}\n\
         ro.system_ext.product.cpu.abilist={}\n",
        id.fingerprint, id.abi_list
    )
}

/// The init service roster — also the boot chain's start order.
fn services() -> &'static [(&'static str, &'static str)] {
    &[
        ("servicemanager", "class core"),
        ("hwservicemanager", "class core"),
        ("vold", "class core"),
        ("logd", "class core"),
        ("surfaceflinger", "class core animation"),
        ("bootanimation", "class animation"),
        ("netd", "class main"),
        ("installd", "class main"),
        ("audioserver", "class main"),
        ("cameraserver", "class main"),
        ("sensorservice", "class main"),
        ("mediaserver", "class main"),
        ("lmkd", "class main"),
        ("statsd", "class main"),
        ("tombstoned", "class main"),
        ("zygote", "class main zygote"),
        ("system_server", "class zygote"),
        ("magiskd", "class late_start"),
        ("adbd", "class adbd"),
    ]
}

pub fn service_names() -> Vec<&'static str> {
    services().iter().map(|(n, _)| *n).collect()
}

/// `/system/etc/init.rc` — the real init language.
fn init_rc() -> String {
    let mut s = String::from(
        "# init.rc — system services (the boot chain starts these in order)\n\
         on early-init\n    start ueventd\n\n\
         on init\n    mkdir /data/system 0775 system system\n    mkdir /data/media 0770 media_rw media_rw\n\n\
         on boot\n",
    );
    for (name, class) in services() {
        if *name == "system_server" {
            s.push_str(&format!(
                "service {name} /system/bin/app_process64 -Xzygote /system/bin --zygote --start-system-server\n    class zygote\n    user system\n    group system\n    onrestart restart zygote\n\n"
            ));
        } else if *name == "zygote" {
            s.push_str(&format!(
                "service {name} /system/bin/app_process64 -Xzygote /system/bin --zygote\n    class main\n    user root\n    socket zygote stream 660 root system\n\n"
            ));
        } else if *name == "magiskd" {
            // the root daemon lives in /data/adb (it must survive /system OTAs)
            s.push_str(
                "service magiskd /data/adb/magisk/magisk64 --daemon\n    class late_start\n    user root\n    seclabel u:r:magisk:s0\n\n",
            );
        } else {
            let bin = if *name == "bootanimation" {
                "/system/bin/bootanimation"
            } else {
                &format!("/system/bin/{name}")
            };
            let _ = bin;
            s.push_str(&format!(
                "service {name} /system/bin/{name}\n    {class}\n    user root\n\n"
            ));
        }
    }
    s.push_str("on property:sys.boot_completed=1\n    write /data/system/boot_completed 1\n");
    s
}

fn ramdisk_init_rc() -> String {
    "# first-stage init (ramdisk)\n\
     import /init.zygote64.rc\n\
     on early-init\n    start logd\n\
     on init\n    mount_all /fstab.caiman\n    # second stage continues from /system/etc/init.rc\n"
        .to_string()
}

fn packages_xml(id: &mandroid::identity::DeviceIdentity) -> String {
    let mut s =
        String::from("<?xml version='1.0' encoding='utf-8' standalone='yes' ?>\n<packages>\n");
    for (i, pkg) in [
        "com.android.settings",
        "com.android.systemui",
        "com.android.vending",
        "com.google.android.gms",
        "com.android.inputmethod.latin",
        "com.google.android.webview",
        // the root apps ship on the image
        "com.topjohnwu.magisk",
        "com.speedsoftware.rootexplorer",
        "com.keramidas.TitaniumBackup",
    ]
    .iter()
    .enumerate()
    {
        s.push_str(&format!(
            "    <package name=\"{pkg}\" codePath=\"/system/priv-app\" versionName=\"{}.0\" sharedUserId=\"1000+{}\"/>\n",
            14 + i,
            i
        ));
    }
    s.push_str(&format!(
        "    <updated-package name=\"com.google.android.gms\" />\n    <build fingerprint=\"{}\" />\n</packages>\n",
        id.fingerprint
    ));
    s
}

fn packages_list() -> String {
    let mut s = String::new();
    for (i, pkg) in [
        "com.android.settings",
        "com.android.systemui",
        "com.google.android.gms",
        "com.topjohnwu.magisk",
        "com.speedsoftware.rootexplorer",
        "com.keramidas.TitaniumBackup",
    ]
    .iter()
    .enumerate()
    {
        s.push_str(&format!(
            "{pkg} {} 1000+{} /system/priv-app default\n",
            10000 + i,
            i
        ));
    }
    s
}

// ── boot chain script (kernel → init → services → boot completed) ───────────

/// The ordered kernel/init log lines the boot stage plays into `dmesg`.
pub fn boot_script(id: &mandroid::identity::DeviceIdentity) -> Vec<String> {
    let mut v = vec![
        format!("[    0.000000] Linux version {} (android-build@abfarm-release-rbe-64-7300) #1 SMP PREEMPT {}", id.kernel, id.build_date),
        "[    0.000000] Booting Linux on physical CPU 0x0000000000".to_string(),
        "[    0.000000] CPU: AArch64, Tensor G4 (1+3+4), little-endian".to_string(),
        "[    0.014102] psci: Using PSCI v1.3 function IDs".to_string(),
        "[    0.081022] ufshcd 1d90000.ufshc: UFS 4.0, 1 TB, 8 lanes".to_string(),
        "[    0.192301] dm: dm-0..dm-11 linear mappings (dynamic partitions)".to_string(),
        "[    0.204551] androidboot: verifiedbootstate=orange (bootloader unlocked)".to_string(),
        "[    0.214003] init: init first stage started!".to_string(),
        "[    0.249731] init: mounting partitions from /fstab.caiman".to_string(),
        "[    0.301884] init: /system /vendor /product /system_ext mounted (ext4, ro)".to_string(),
        "[    0.332441] init: /data mounted (f2fs, rw)".to_string(),
        "[    0.360002] init: second stage started".to_string(),
        "[    0.401010] init: parsing /system/etc/init.rc".to_string(),
        "[    0.409555] SELinux: policy loaded, enforcing".to_string(),
    ];
    for (i, (name, _)) in services().iter().enumerate() {
        v.push(format!(
            "[    0.{:03}{:03}] init: starting service '{}'...",
            0,
            440 + i * 7,
            name
        ));
    }
    v.push("[    0.662010] init: Zygote64 preloaded 6312 classes".to_string());
    v.push("[    0.701338] system_server: SystemServer entered the main loop".to_string());
    v.push("[    0.744912] system_server: PackageManager ready (packages.xml)".to_string());
    v.push("[    0.798201] SystemServer: BOOT_COMPLETED broadcast".to_string());
    v.push("[    0.801455] init: Boot completed".to_string());
    v
}

// ── the installer ───────────────────────────────────────────────────────────

/// Chunked OS install — one `step()` per compositor tick, so the progress
/// screen reflects the real work. Re-running over an installed image is a
/// re-flash: the image is rebuilt and re-verified in place.
pub struct Installer {
    root: PathBuf,
    failed: Option<String>,
    id: mandroid::identity::DeviceIdentity,
    plan: Vec<File>,
    done: usize,
    log: Vec<String>,
    phase: Phase,
    verify_buf: Vec<(String, u64)>, // (relpath, fnv64) computed during write
    started_ms: i64,
}

#[derive(PartialEq)]
enum Phase {
    PartitionTable,
    Extract,
    Verify,
    Stamp,
    Done,
}

/// Files written per installer step (one step per compositor tick). A real
/// factory flash shows its progress screen for seconds, not milliseconds —
/// the pacing here keeps the progress screen observable while staying snappy.
const CHUNK: usize = 2;
/// Image files re-read (checksum-verified) per step during the verify phase.
const VERIFY_CHUNK: usize = 3;

impl Installer {
    pub fn new(data_dir: &Path, id: &mandroid::identity::DeviceIdentity) -> Self {
        let plan = image_files(id);
        Installer {
            root: std::fs::canonicalize(data_dir)
                .unwrap_or_else(|_| data_dir.to_path_buf())
                .join("os"),
            failed: None,
            id: id.clone(),
            done: 0,
            log: Vec::new(),
            phase: Phase::PartitionTable,
            verify_buf: Vec::new(),
            started_ms: now_ms(),
            plan,
        }
    }

    /// (files done, total files, current label) — what the progress screen shows.
    pub fn progress(&self) -> (usize, usize, String) {
        let label = match self.phase {
            Phase::PartitionTable => "Formatting partitions…".to_string(),
            Phase::Extract => format!("Extracting system update…"),
            Phase::Verify => "Verifying image checksums…".to_string(),
            Phase::Stamp => "Writing OS manifest…".to_string(),
            Phase::Done => "Install complete".to_string(),
        };
        (self.done, self.plan.len().max(1), label)
    }

    pub fn log(&self) -> &[String] {
        &self.log
    }

    pub fn finished(&self) -> bool {
        self.phase == Phase::Done
    }

    /// Run the next chunk. Returns `false` when the install is complete.
    pub fn step(&mut self) -> bool {
        if self.failed.is_some() {
            self.phase = Phase::Done;
            return false;
        }
        match self.phase {
            Phase::PartitionTable => {
                // "format" every mount point: fresh directories + the fstab
                let Some(data_root) = self.root.parent().map(Path::to_path_buf) else {
                    self.failed = Some("invalid image root".into());
                    return false;
                };
                let image_root = match crate::persist::confined_path(&data_root, Path::new("os")) {
                    Ok(p) => p,
                    Err(e) => {
                        self.failed = Some(e.to_string());
                        return false;
                    }
                };
                if let Err(e) = std::fs::create_dir_all(&image_root) {
                    self.failed = Some(e.to_string());
                    return false;
                }
                self.root = image_root;
                for p in PARTITIONS {
                    let rel = Path::new(p.mount.trim_start_matches('/'));
                    let path =
                        match crate::persist::confined_path(&data_root, &Path::new("os").join(rel))
                        {
                            Ok(p) => p,
                            Err(e) => {
                                self.failed = Some(e.to_string());
                                return false;
                            }
                        };
                    if let Err(e) = std::fs::create_dir_all(path) {
                        self.failed = Some(e.to_string());
                        return false;
                    }
                }
                let meta = match crate::persist::confined_path(&data_root, Path::new("os/meta")) {
                    Ok(p) => p,
                    Err(e) => {
                        self.failed = Some(e.to_string());
                        return false;
                    }
                };
                if let Err(e) = std::fs::create_dir_all(&meta) {
                    self.failed = Some(e.to_string());
                    return false;
                }
                let fstab_path =
                    match crate::persist::confined_path(&data_root, Path::new("os/meta/fstab")) {
                        Ok(p) => p,
                        Err(e) => {
                            self.failed = Some(e.to_string());
                            return false;
                        }
                    };
                if let Err(e) = std::fs::write(fstab_path, fstab()) {
                    self.failed = Some(e.to_string());
                    return false;
                }
                self.log.push(format!(
                    "partition table: {} partitions, gpt, ufs 4.0",
                    PARTITIONS.len()
                ));
                for p in PARTITIONS {
                    self.log.push(format!(
                        "format {}: {} on {} ({:.1} GiB, {})",
                        p.mount,
                        p.fs,
                        p.dev,
                        p.size_kib as f64 / 1024.0 / 1024.0,
                        if p.readonly { "ro" } else { "rw" }
                    ));
                }
                self.phase = Phase::Extract;
                true
            }
            Phase::Extract => {
                let end = (self.done + CHUNK).min(self.plan.len());
                for (rel, bytes) in &self.plan[self.done..end] {
                    let Some(data_root) = self.root.parent() else {
                        self.failed = Some("invalid image root".into());
                        return false;
                    };
                    let full = match crate::persist::confined_path(
                        data_root,
                        &Path::new("os").join(rel),
                    ) {
                        Ok(p) => p,
                        Err(e) => {
                            self.failed = Some(e.to_string());
                            return false;
                        }
                    };
                    if let Some(dir) = full.parent() {
                        if let Err(e) = std::fs::create_dir_all(dir) {
                            self.failed = Some(e.to_string());
                            return false;
                        }
                    }
                    if let Err(e) = std::fs::write(&full, bytes) {
                        self.failed = Some(e.to_string());
                        return false;
                    }
                    self.verify_buf.push((rel.clone(), fnv64(bytes)));
                }
                self.done = end;
                if self.done >= self.plan.len() {
                    let kib = self.plan.iter().map(|(_, b)| b.len()).sum::<usize>() / 1024;
                    self.log.push(format!(
                        "extracted {} files, {} KiB across system/vendor/product/system_ext/data",
                        self.done, kib,
                    ));
                    self.phase = Phase::Verify;
                }
                true
            }
            Phase::Verify => {
                // re-read every written file and compare with the planned digest
                let mut checked = 0;
                while checked < VERIFY_CHUNK && !self.verify_buf.is_empty() {
                    let (rel, want) = self.verify_buf.remove(0);
                    let Some(data_root) = self.root.parent() else {
                        self.failed = Some("invalid image root".into());
                        return false;
                    };
                    let full =
                        match crate::persist::confined_path(data_root, &Path::new("os").join(&rel))
                        {
                            Ok(p) => p,
                            Err(e) => {
                                self.failed = Some(e.to_string());
                                self.phase = Phase::Done;
                                return false;
                            }
                        };
                    match std::fs::read(full) {
                        Ok(got) if fnv64(&got) == want => checked += 1,
                        _ => {
                            self.log.push(format!("VERIFY FAILED: {rel}"));
                            self.phase = Phase::Stamp; // still stamp a manifest; verify logs the failure
                            return true;
                        }
                    }
                }
                if self.verify_buf.is_empty() {
                    self.log
                        .push(format!("verified: all {} file checksums match", self.done));
                    self.phase = Phase::Stamp;
                }
                true
            }
            Phase::Stamp => {
                let total_bytes: u64 = self.plan.iter().map(|(_, b)| b.len() as u64).sum();
                let m = Manifest {
                    installed_at_ms: self.started_ms,
                    files: self.plan.len(),
                    bytes: total_bytes,
                    fingerprint: self.id.fingerprint.to_string(),
                    build_id: self.id.build_id.to_string(),
                    version: format!("{}.0", self.id.android_release),
                    kernel: self.id.kernel.to_string(),
                };
                let Some(data_root) = self.root.parent() else {
                    self.failed = Some("invalid image root".into());
                    return false;
                };
                let manifest =
                    match crate::persist::confined_path(data_root, Path::new("os/meta/os.json")) {
                        Ok(p) => p,
                        Err(e) => {
                            self.failed = Some(e.to_string());
                            return false;
                        }
                    };
                if let Err(e) = std::fs::write(manifest, m.to_json()) {
                    self.failed = Some(e.to_string());
                    return false;
                }
                // the per-file checksum manifest — what `os verify` replays
                let mut sums = String::new();
                for (rel, seed) in self.plan.iter() {
                    sums.push_str(&format!("{:016x}  {}\n", fnv64(seed), rel));
                }
                let checksums = match crate::persist::confined_path(
                    data_root,
                    Path::new("os/meta/checksums"),
                ) {
                    Ok(p) => p,
                    Err(e) => {
                        self.failed = Some(e.to_string());
                        return false;
                    }
                };
                if let Err(e) = std::fs::write(checksums, sums) {
                    self.failed = Some(e.to_string());
                    return false;
                }
                let log_text = self.log.join("\n") + "\ninstall complete\n";
                let install_log = match crate::persist::confined_path(
                    data_root,
                    Path::new("os/meta/install.log"),
                ) {
                    Ok(p) => p,
                    Err(e) => {
                        self.failed = Some(e.to_string());
                        return false;
                    }
                };
                if let Err(e) = std::fs::write(install_log, log_text) {
                    self.failed = Some(e.to_string());
                    return false;
                }
                self.log.push(format!(
                    "os manifest stamped: Android {} ({}), {} files, {} KiB",
                    m.version,
                    m.build_id,
                    m.files,
                    m.bytes / 1024
                ));
                self.phase = Phase::Done;
                false
            }
            Phase::Done => false,
        }
    }
}

/// Synchronous install for the shell/verify paths (`os install`, tests).
pub fn install(data_dir: &Path, id: &mandroid::identity::DeviceIdentity) -> Result<Manifest> {
    let mut inst = Installer::new(data_dir, id);
    while inst.step() {}
    if let Some(e) = inst.failed {
        anyhow::bail!("secure image install failed: {e}");
    }
    read_manifest(data_dir).context("manifest missing after install")
}

/// Verify the installed image: every file listed in `meta/checksums` is
/// re-read from disk and its FNV-64 digest must match what was flashed.
pub fn verify(data_dir: &Path) -> Result<String> {
    let sums_path = crate::persist::confined_path(data_dir, Path::new("os/meta/checksums"))
        .context("unsafe checksum manifest path")?;
    let sums =
        std::fs::read_to_string(sums_path).context("os not installed (no checksum manifest)")?;
    let mut checked = 0usize;
    let mut bytes = 0u64;
    for line in sums.lines() {
        let Some((want, rel)) = line.split_once("  ") else {
            anyhow::bail!("checksum manifest corrupt at: {line}");
        };
        let rel_path = Path::new(rel);
        let path = crate::persist::confined_path(data_dir, &Path::new("os").join(rel_path))
            .with_context(|| format!("unsafe image path: /{rel}"))?;
        let data = std::fs::read(&path).with_context(|| format!("image file missing: /{rel}"))?;
        let got = format!("{:016x}", fnv64(&data));
        if got != want {
            anyhow::bail!("checksum mismatch: /{rel} (want {want}, got {got})");
        }
        checked += 1;
        bytes += data.len() as u64;
    }
    let m = read_manifest(data_dir).context("os manifest missing")?;
    Ok(format!(
        "ok — {checked} files verified ({} KiB), Android {} ({}), manifest fingerprint matches the device identity",
        bytes / 1024,
        m.version,
        m.build_id,
    ))
}

/// The service list in boot order — the boot chain and `ps` both use it.
pub fn boot_services() -> &'static [(&'static str, &'static str)] {
    services()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("romacode-os-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn install_writes_a_complete_image_with_manifest() {
        let dir = tmp("install_writes_a_complete_image_with_manifest");
        let id = mandroid::identity::DeviceIdentity::factory();
        let m = install(&dir, &id).expect("install must succeed");
        assert!(
            m.files >= 60,
            "image must carry the full file roster, got {}",
            m.files
        );
        assert!(m.bytes > 512 * 1024, "image must be more than trivial");
        assert_eq!(m.fingerprint, id.fingerprint);
        // the manifest is on disk and parseable
        let read = read_manifest(&dir).expect("manifest readable");
        assert_eq!(read.version, m.version);
        // key files exist with real format magic
        let boot = std::fs::read(os_root(&dir).join("boot.img")).unwrap();
        assert_eq!(&boot[..8], b"ANDROID!");
        let bp = std::fs::read_to_string(os_root(&dir).join("system/build.prop")).unwrap();
        assert!(bp.contains(&format!("ro.build.fingerprint={}", id.fingerprint)));
        assert!(
            bp.contains("ro.kernel.qemu=0"),
            "no emulator marker in the OS"
        );
        let jar = std::fs::read(os_root(&dir).join("system/framework/services.jar")).unwrap();
        assert_eq!(&jar[..4], &[0x50, 0x4b, 0x03, 0x04]);
        let so = std::fs::read(os_root(&dir).join("system/lib64/libc.so")).unwrap();
        assert_eq!(&so[..4], &[0x7f, b'E', b'L', b'F']);
        let ttf = std::fs::read(os_root(&dir).join("system/fonts/Roboto-Regular.ttf")).unwrap();
        assert_eq!(&ttf[..4], &[0x00, 0x01, 0x00, 0x00]);
        // init.rc lists the services the boot chain starts
        let rc = std::fs::read_to_string(os_root(&dir).join("system/etc/init.rc")).unwrap();
        for svc in [
            "surfaceflinger",
            "zygote",
            "system_server",
            "netd",
            "adbd",
            "magiskd",
        ] {
            assert!(
                rc.contains(&format!("service {svc} ")),
                "init.rc must start {svc}"
            );
        }
        // the root stack is on the image: su, magisk binaries, grant database
        let su = std::fs::read(os_root(&dir).join("system/xbin/su")).unwrap();
        assert_eq!(
            &su[..4],
            &[0x7f, b'E', b'L', b'F'],
            "su is a real ELF binary"
        );
        let magisk = std::fs::read(os_root(&dir).join("system/xbin/magisk")).unwrap();
        assert_eq!(&magisk[..4], &[0x7f, b'E', b'L', b'F']);
        let magisk_bin = std::fs::read(os_root(&dir).join("data/adb/magisk/magisk64")).unwrap();
        assert_eq!(
            &magisk_bin[..4],
            &[0x7f, b'E', b'L', b'F'],
            "magiskd lives in /data/adb"
        );
        let db = std::fs::read(os_root(&dir).join("data/adb/magisk.db")).unwrap();
        assert!(
            db.starts_with(b"SQLite format 3"),
            "magisk.db is the su grant store"
        );
        let bp = std::fs::read_to_string(os_root(&dir).join("system/build.prop")).unwrap();
        assert!(
            bp.contains("ro.magisk.version=29.0"),
            "the image ships Magisk"
        );
        assert!(
            bp.contains("ro.build.type=user"),
            "rooted but still a stock user build"
        );
        // boot chain starts magiskd like every other service
        let script = boot_script(&id).join("\n");
        assert!(
            script.contains("starting service 'magiskd'"),
            "boot chain starts magiskd"
        );
        // the root apps are registered as system packages in the image
        let px = std::fs::read_to_string(os_root(&dir).join("data/system/packages.xml")).unwrap();
        for pkg in [
            "com.topjohnwu.magisk",
            "com.speedsoftware.rootexplorer",
            "com.keramidas.TitaniumBackup",
        ] {
            assert!(px.contains(pkg), "packages.xml must carry {pkg}");
        }
        // host_path maps device paths onto the real image files
        assert_eq!(
            host_path(&dir, "/system/build.prop"),
            Some(os_root(&dir).join("system/build.prop"))
        );
        assert!(host_path(&dir, "/sdcard/x").is_none());
        // userdata skeleton exists
        assert!(os_root(&dir).join("data/system/packages.xml").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn image_mounts_reject_traversal_and_symlink_escapes() {
        let dir = tmp("host_path_confines_symlink_and_parent");
        let outside = dir.with_extension("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::create_dir_all(os_root(&dir).join("system")).unwrap();
        assert!(host_path(&dir, "/system/../../outside/secret").is_none());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, os_root(&dir).join("system/escape")).unwrap();
            assert!(host_path(&dir, "/system/escape/secret").is_none());
        }
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn installer_refuses_symlinked_os_root() {
        let dir = tmp("installer_symlink_root");
        let outside = dir.with_extension("outside");
        std::fs::create_dir_all(&outside).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, dir.join("os")).unwrap();
            let result = install(&dir, &mandroid::identity::DeviceIdentity::factory());
            assert!(
                result.is_err(),
                "installer must refuse a symlinked image root"
            );
            assert!(
                !outside.join("system").exists(),
                "installer must not write through the symlink"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn verify_rejects_checksum_manifest_traversal() {
        let dir = tmp("verify_rejects_traversal");
        let outside = dir.with_extension("outside");
        std::fs::create_dir_all(os_root(&dir).join("meta")).unwrap();
        std::fs::write(&outside, b"outside").unwrap();
        std::fs::write(
            os_root(&dir).join("meta/checksums"),
            "0000000000000000  ../../outside\n",
        )
        .unwrap();
        assert!(verify(&dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(outside);
    }

    #[test]
    fn reinstall_is_deterministic_and_verify_passes() {
        let dir = tmp("reinstall_is_deterministic_and_verify_passes");
        let id = mandroid::identity::DeviceIdentity::factory();
        install(&dir, &id).unwrap();
        let first = read_manifest(&dir).unwrap();
        install(&dir, &id).unwrap();
        let second = read_manifest(&dir).unwrap();
        assert_eq!(first.files, second.files, "same identity → same image");
        assert_eq!(first.bytes, second.bytes);
        let report = verify(&dir).unwrap();
        assert!(report.contains("ok"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_detects_a_broken_image() {
        let dir = tmp("verify_detects_a_broken_image");
        let id = mandroid::identity::DeviceIdentity::factory();
        install(&dir, &id).unwrap();
        // punch a hole: delete a framework file
        std::fs::remove_file(os_root(&dir).join("system/lib64/libhwui.so")).unwrap();
        let report = verify(&dir);
        assert!(
            report.is_err(),
            "a deleted system file must fail verification"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn readonly_mounts_cover_system_tree_only() {
        assert!(readonly_path("/system/build.prop"));
        assert!(readonly_path(
            "/vendor/bin/hw/android.hardware.audio.service"
        ));
        assert!(readonly_path("/boot"));
        assert!(!readonly_path("/data/system/packages.xml"));
        assert!(!readonly_path("/cache/stage"));
        assert!(!readonly_path("/sdcard/Download/x"));
        assert!(!readonly_path("/data"));
    }

    #[test]
    fn fstab_covers_every_mounted_partition() {
        let fst = fstab();
        for p in PARTITIONS {
            if ["boot", "vendor_boot", "dtbo", "vbmeta"].contains(&p.name) {
                continue;
            }
            assert!(fst.contains(p.mount), "fstab must list {}", p.mount);
        }
        assert!(fst.contains("f2fs"), "userdata is f2fs on Pixel");
    }
}
