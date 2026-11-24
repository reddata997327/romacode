//! Native Android engine.
//!
//! Two halves, no external binaries in either:
//!
//! * **Static** — decode APK internals: binary AndroidManifest (AXML), the
//!   resource table, DEX code, and a data-driven security rule pack.
//! * **Dynamic** — speak the ADB wire protocol straight to `adbd` over TCP:
//!   shell, file push/pull, package install, activity launch.
//!
//! Exposed two ways: the `mandroid` CLI and the native `android_*` tool
//! surface in [`tools`], which the RomaCode agent links directly. All of them
//! call the same functions in [`api`] and [`pipeline`], so they cannot disagree.

pub mod adb;
pub mod api;
pub mod apk;
pub mod arsc;
pub mod axml;
pub mod axmlenc;
pub mod crypto;
pub mod device;
pub mod dex;
pub mod identity;
pub mod key;
pub mod manifest;
pub mod modify;
pub mod net;
pub mod pipeline;
pub mod repack;
pub mod scan;
pub mod screen;
pub mod sign;
pub mod sim;
pub mod smali;
pub mod tools;
pub mod util;
