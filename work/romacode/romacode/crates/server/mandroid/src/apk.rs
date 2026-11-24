//! APK container access.
//!
//! An APK is a ZIP. We read it directly rather than shelling out to `unzip`,
//! and everything downstream (AXML, DEX) works on the bytes we pull out here.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

use crate::axml;

pub struct Apk {
    pub path: PathBuf,
    zip: zip::ZipArchive<File>,
}

impl Apk {
    pub fn open(path: impl AsRef<Path>) -> Result<Apk> {
        let path = path.as_ref().to_path_buf();
        let file = File::open(&path).with_context(|| format!("opening {}", path.display()))?;
        let zip = zip::ZipArchive::new(file)
            .with_context(|| format!("{} is not a valid ZIP/APK", path.display()))?;
        Ok(Apk { path, zip })
    }

    pub fn entries(&mut self) -> Vec<String> {
        (0..self.zip.len())
            .filter_map(|i| self.zip.by_index(i).ok().map(|f| f.name().to_string()))
            .collect()
    }

    pub fn has(&mut self, name: &str) -> bool {
        self.zip.by_name(name).is_ok()
    }

    pub fn read(&mut self, name: &str) -> Result<Vec<u8>> {
        let mut f = self
            .zip
            .by_name(name)
            .map_err(|_| anyhow!("{name} not present in {}", self.path.display()))?;
        let mut buf = Vec::with_capacity(f.size() as usize);
        f.read_to_end(&mut buf)?;
        Ok(buf)
    }

    /// Read an entry by name if present.
    pub fn read_opt(&mut self, name: &str) -> Option<Vec<u8>> {
        self.read(name).ok()
    }

    /// The compiled `AndroidManifest.xml`.
    pub fn manifest_axml(&mut self) -> Result<Vec<u8>> {
        self.read("AndroidManifest.xml")
            .context("reading AndroidManifest.xml")
    }

    /// The decoded manifest tree.
    pub fn manifest(&mut self) -> Result<axml::Node> {
        let raw = self.manifest_axml()?;
        axml::decode(&raw).context("decoding binary AndroidManifest.xml")
    }

    /// `classes.dex`, `classes2.dex`, ... in load order.
    pub fn dex_names(&mut self) -> Vec<String> {
        let mut names: Vec<String> = self
            .entries()
            .into_iter()
            .filter(|n| n == "classes.dex" || (n.starts_with("classes") && n.ends_with(".dex")))
            .collect();
        names.sort_by_key(|n| {
            n.trim_start_matches("classes")
                .trim_end_matches(".dex")
                .parse::<u32>()
                .unwrap_or(1)
        });
        names
    }

    /// Native libraries, grouped by ABI.
    pub fn native_libs(&mut self) -> Vec<(String, String)> {
        self.entries()
            .into_iter()
            .filter(|n| n.starts_with("lib/") && n.ends_with(".so"))
            .filter_map(|n| {
                let mut it = n.splitn(3, '/');
                let _ = it.next()?;
                let abi = it.next()?.to_string();
                let lib = it.next()?.to_string();
                Some((abi, lib))
            })
            .collect()
    }
}
