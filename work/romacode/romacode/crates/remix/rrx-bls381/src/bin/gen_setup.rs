//! One-shot tool: converts the official c-kzg-4844 trusted_setup.txt into the
//! compact binary blob embedded by rrx-bls381 (src/trusted_setup.bin).
//!
//! Every point is decompressed, checked on-curve AND in the r-subgroup, and
//! sanity-checked against the known G2 generator. Run once:
//!   cargo run --release -p rrx-bls381 --bin gen-setup

use rrx_bls381::curve::{g2_generator, G1Affine, G2Affine};
use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let txt = std::fs::read_to_string(manifest.join("assets/trusted_setup.txt")).expect("trusted_setup.txt");
    let lines: Vec<&str> = txt.lines().map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
    assert!(lines.len() >= 2, "bad setup file");
    let g1n: usize = lines[0].parse().unwrap();
    let g2n: usize = lines[1].parse().unwrap();
    assert_eq!(g1n, 4096);
    assert_eq!(g2n, 65);
    // c-kzg 2.x file layout: [g1_lagrange 4096][g2_monomial 65][g1_monomial 4096]
    // (the trailing monomial G1 block is an EIP-7594 extension; skip it, but
    //  verify its first entry is the canonical G1 generator as a sanity check)
    assert!(lines.len() == 2 + g1n + g2n || lines.len() == 2 + 2 * g1n + g2n, "unexpected setup layout");

    let g1_gen = rrx_bls381::curve::g1_generator();
    if lines.len() == 2 + 2 * g1n + g2n {
        let mono0 = G1Affine::decompress(&hex::decode(lines[2 + g1n + g2n]).unwrap()).unwrap();
        assert_eq!(mono0, g1_gen, "g1_monomial[0] != G1 generator — layout mismatch");
    }

    let t0 = std::time::Instant::now();
    let mut g1_out: Vec<u8> = Vec::with_capacity(g1n * 96);
    let mut checked_subgroup = 0;
    for (i, line) in lines[2..2 + g1n].iter().enumerate() {
        let raw = hex::decode(line).unwrap();
        assert_eq!(raw.len(), 48, "g1 line {i} not 48 bytes");
        let p = G1Affine::decompress(&raw).unwrap_or_else(|e| panic!("g1[{i}]: {e}"));
        assert!(p.is_on_curve(), "g1[{i}] not on curve");
        assert!(p.is_in_subgroup(), "g1[{i}] not in subgroup — flag convention wrong?");
        checked_subgroup += 1;
        g1_out.extend_from_slice(&p.x.to_be48());
        g1_out.extend_from_slice(&p.y.to_be48());
    }

    let mut g2_out: Vec<u8> = Vec::with_capacity(g2n * 192);
    for (i, line) in lines[2 + g1n..2 + g1n + g2n].iter().enumerate() {
        let raw = hex::decode(line).unwrap();
        assert_eq!(raw.len(), 96, "g2 line {i} not 96 bytes");
        let p = G2Affine::decompress(&raw).unwrap_or_else(|e| panic!("g2[{i}]: {e}"));
        assert!(p.is_on_curve(), "g2[{i}] not on curve");
        assert!(p.is_in_subgroup(), "g2[{i}] not in subgroup");
        if i == 0 {
            // [1]_2 must be the canonical G2 generator
            assert_eq!(p, g2_generator(), "setup g2[0] != canonical G2 generator");
        }
        g2_out.extend_from_slice(&p.x.c0.to_be48());
        g2_out.extend_from_slice(&p.x.c1.to_be48());
        g2_out.extend_from_slice(&p.y.c0.to_be48());
        g2_out.extend_from_slice(&p.y.c1.to_be48());
    }

    // ---- verkle Pedersen basis (256 points) ----
    let g1 = rrx_bls381::curve::g1_generator();
    let mut basis_bin = Vec::with_capacity(256 * 96);
    for i in 0..256usize {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"rrx-verkle-leaf-v1");
        buf.extend_from_slice(&(i as u32).to_be_bytes());
        let h = rrx_util::keccak256(&buf);
        let s = rrx_bls381::fp::Fr::from_be_bytes_reduced(&h);
        let pt = g1.mul(&s.0);
        assert!(!pt.infinity && pt.is_on_curve() && pt.is_in_subgroup());
        basis_bin.extend_from_slice(&pt.x.to_be48());
        basis_bin.extend_from_slice(&pt.y.to_be48());
    }
    let manifest_b = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let basis_out = manifest_b.parent().unwrap().join("rrx-trie/src/verkle_basis.bin");
    std::fs::write(&basis_out, &basis_bin).unwrap();
    println!("verkle_basis.bin written: {} bytes", basis_bin.len());

    let mut bin = Vec::new();
    bin.extend_from_slice(b"RRXKZG1");
    bin.extend_from_slice(&(g1n as u32).to_be_bytes());
    bin.extend_from_slice(&(g2n as u32).to_be_bytes());
    bin.extend_from_slice(&g1_out);
    bin.extend_from_slice(&g2_out);
    let out = manifest.join("src/trusted_setup.bin");
    std::fs::write(&out, &bin).unwrap();
    println!(
        "trusted_setup.bin written: {} bytes ({} G1 + {} G2, all on-curve; {} subgroup checks; {:?})",
        bin.len(),
        g1n,
        g2n,
        checked_subgroup,
        t0.elapsed()
    );
}
