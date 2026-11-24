//! EIP-2537 BLS12-381 precompiles (0x0b–0x11) + EIP-4844 point evaluation (0x0a),
//! implemented on top of rrx-bls381 and validated against the OFFICIAL EIP-2537
//! test vectors (ethereum/EIPs assets/eip-2537).

use rrx_bls381::curve::{g1_generator, msm_g1, msm_g2, G1Affine, G2Affine};
use rrx_bls381::fp::{r_suborder, Fp};
use rrx_bls381::fp2::Fp2;
use rrx_bls381::fp612::Fp12;
use rrx_bls381::kzg::kzg_sha256::sha256;
use rrx_bls381::pairing::{pairing, kzg_pairing_check};
use num_bigint::BigUint;
use num_traits::Zero;

pub const POINT_EVAL: u8 = 0x0a;
pub const G1ADD: u8 = 0x0b;
pub const G1MSM: u8 = 0x0c;
pub const G2ADD: u8 = 0x0d;
pub const G2MSM: u8 = 0x0e;
pub const PAIRING: u8 = 0x0f;
pub const MAP_FP_TO_G1: u8 = 0x10;
pub const MAP_FP2_TO_G2: u8 = 0x11;

pub fn bls_modulus() -> BigUint { r_suborder() }

// ---------- EIP-2537 gas schedule ----------

/// G1 MSM discount table [k, discount] for k <= 128, max_discount for k > 128.
const G1_DISCOUNT: [(usize, u64); 128] = [
    (1, 1000), (2, 949), (3, 848), (4, 797), (5, 764), (6, 750), (7, 738), (8, 728),
    (9, 719), (10, 712), (11, 705), (12, 698), (13, 692), (14, 687), (15, 682), (16, 677),
    (17, 673), (18, 669), (19, 665), (20, 661), (21, 658), (22, 654), (23, 651), (24, 648),
    (25, 645), (26, 642), (27, 640), (28, 637), (29, 635), (30, 632), (31, 630), (32, 627),
    (33, 625), (34, 623), (35, 621), (36, 619), (37, 617), (38, 615), (39, 613), (40, 611),
    (41, 609), (42, 608), (43, 606), (44, 604), (45, 603), (46, 601), (47, 599), (48, 598),
    (49, 596), (50, 595), (51, 593), (52, 592), (53, 591), (54, 589), (55, 588), (56, 586),
    (57, 585), (58, 584), (59, 582), (60, 581), (61, 580), (62, 579), (63, 577), (64, 576),
    (65, 575), (66, 574), (67, 573), (68, 572), (69, 570), (70, 569), (71, 568), (72, 567),
    (73, 566), (74, 565), (75, 564), (76, 563), (77, 562), (78, 561), (79, 560), (80, 559),
    (81, 558), (82, 557), (83, 556), (84, 555), (85, 554), (86, 553), (87, 552), (88, 551),
    (89, 550), (90, 549), (91, 548), (92, 547), (93, 547), (94, 546), (95, 545), (96, 544),
    (97, 543), (98, 542), (99, 541), (100, 540), (101, 540), (102, 539), (103, 538), (104, 537),
    (105, 536), (106, 536), (107, 535), (108, 534), (109, 533), (110, 532), (111, 532), (112, 531),
    (113, 530), (114, 529), (115, 528), (116, 528), (117, 527), (118, 526), (119, 525), (120, 525),
    (121, 524), (122, 523), (123, 522), (124, 522), (125, 521), (126, 520), (127, 520), (128, 519),
];
const G1_MAX_DISCOUNT: u64 = 519;

const G2_DISCOUNT: [(usize, u64); 128] = [
    (1, 1000), (2, 1000), (3, 923), (4, 884), (5, 855), (6, 832), (7, 812), (8, 796),
    (9, 782), (10, 770), (11, 759), (12, 749), (13, 740), (14, 732), (15, 724), (16, 717),
    (17, 711), (18, 704), (19, 699), (20, 693), (21, 688), (22, 683), (23, 679), (24, 674),
    (25, 670), (26, 666), (27, 663), (28, 659), (29, 655), (30, 652), (31, 649), (32, 646),
    (33, 643), (34, 640), (35, 637), (36, 634), (37, 632), (38, 629), (39, 627), (40, 624),
    (41, 622), (42, 620), (43, 618), (44, 615), (45, 613), (46, 611), (47, 609), (48, 607),
    (49, 606), (50, 604), (51, 602), (52, 600), (53, 598), (54, 597), (55, 595), (56, 593),
    (57, 592), (58, 590), (59, 589), (60, 587), (61, 586), (62, 584), (63, 583), (64, 582),
    (65, 580), (66, 579), (67, 578), (68, 576), (69, 575), (70, 574), (71, 573), (72, 571),
    (73, 570), (74, 569), (75, 568), (76, 567), (77, 566), (78, 565), (79, 563), (80, 562),
    (81, 561), (82, 560), (83, 559), (84, 558), (85, 557), (86, 556), (87, 555), (88, 554),
    (89, 553), (90, 552), (91, 552), (92, 551), (93, 550), (94, 549), (95, 548), (96, 547),
    (97, 546), (98, 545), (99, 545), (100, 544), (101, 543), (102, 542), (103, 541), (104, 541),
    (105, 540), (106, 539), (107, 538), (108, 537), (109, 537), (110, 536), (111, 535), (112, 535),
    (113, 534), (114, 533), (115, 532), (116, 532), (117, 531), (118, 530), (119, 530), (120, 529),
    (121, 528), (122, 528), (123, 527), (124, 526), (125, 526), (126, 525), (127, 524), (128, 524),
];
const G2_MAX_DISCOUNT: u64 = 524;

fn msm_gas(len: usize, per_pair: usize, mult_cost: u64, table: &[(usize, u64); 128], max_discount: u64) -> u128 {
    let k = len / per_pair;
    if k == 0 {
        return 0;
    }
    let discount = if k <= 128 { table[k - 1].1 } else { max_discount };
    (k as u128) * (mult_cost as u128) * (discount as u128) / 1000
}

/// Public gas entry point used by the dispatcher (mirrors gas_cost in lib.rs).
pub fn gas_cost(addr: u8, input: &[u8]) -> u128 {
    match addr {
        POINT_EVAL => {
            if input.len() != 544 {
                return u128::MAX;
            }
            45000
        }
        G1ADD => {
            if input.len() != 256 { u128::MAX } else { 375 }
        }
        G1MSM => msm_gas(input.len(), 160, 12000, &G1_DISCOUNT, G1_MAX_DISCOUNT),
        G2ADD => {
            if input.len() != 512 { u128::MAX } else { 600 }
        }
        G2MSM => msm_gas(input.len(), 288, 22500, &G2_DISCOUNT, G2_MAX_DISCOUNT),
        PAIRING => {
            let k = input.len() / 384;
            if k == 0 { u128::MAX } else { 32600 * k as u128 + 37700 }
        }
        MAP_FP_TO_G1 => {
            if input.len() != 64 { u128::MAX } else { 5500 }
        }
        MAP_FP2_TO_G2 => {
            if input.len() != 128 { u128::MAX } else { 23800 }
        }
        _ => u128::MAX,
    }
}

/// Public run entry point.
pub fn run(addr: u8, input: &[u8]) -> Result<Vec<u8>, String> {
    match addr {
        POINT_EVAL => point_evaluation(input),
        G1ADD => {
            if input.len() != 256 {
                return Err("G1ADD: input must be 256 bytes".into());
            }
            let a = G1Affine::decode128(&input[..128])?;
            let b = G1Affine::decode128(&input[128..])?;
            Ok(a.add(&b).encode128().to_vec())
        }
        G1MSM => {
            if input.is_empty() || input.len() % 160 != 0 {
                return Err("G1MSM: invalid input length".into());
            }
            let k = input.len() / 160;
            let mut pts = Vec::with_capacity(k);
            let mut scs = Vec::with_capacity(k);
            for i in 0..k {
                let chunk = &input[i * 160..(i + 1) * 160];
                let p = G1Affine::decode128(&chunk[..128])?;
                if !p.is_in_subgroup() {
                    return Err("G1MSM: point not in subgroup".into());
                }
                pts.push(p);
                // EIP-2537: scalars >= BLS_MODULUS are invalid input -> all gas consumed
                let s = BigUint::from_bytes_be(&chunk[128..]);
                if s >= r_suborder() {
                    return Err("G1MSM: scalar out of range".into());
                }
                scs.push(s);
            }
            Ok(msm_g1(&pts, &scs).encode128().to_vec())
        }
        G2ADD => {
            if input.len() != 512 {
                return Err("G2ADD: input must be 512 bytes".into());
            }
            let a = G2Affine::decode256(&input[..256])?;
            let b = G2Affine::decode256(&input[256..])?;
            Ok(a.add(&b).encode256().to_vec())
        }
        G2MSM => {
            if input.is_empty() || input.len() % 288 != 0 {
                return Err("G2MSM: invalid input length".into());
            }
            let k = input.len() / 288;
            let mut pts = Vec::with_capacity(k);
            let mut scs = Vec::with_capacity(k);
            for i in 0..k {
                let chunk = &input[i * 288..(i + 1) * 288];
                let p = G2Affine::decode256(&chunk[..256])?;
                if !p.is_in_subgroup() {
                    return Err("G2MSM: point not in subgroup".into());
                }
                pts.push(p);
                // EIP-2537: scalars >= BLS_MODULUS are invalid input -> all gas consumed
                let s = BigUint::from_bytes_be(&chunk[256..]);
                if s >= r_suborder() {
                    return Err("G2MSM: scalar out of range".into());
                }
                scs.push(s);
            }
            Ok(msm_g2(&pts, &scs).encode256().to_vec())
        }
        PAIRING => {
            if input.is_empty() || input.len() % 384 != 0 {
                return Err("PAIRING: invalid input length".into());
            }
            let k = input.len() / 384;
            let mut acc = Fp12::one();
            for i in 0..k {
                let chunk = &input[i * 384..(i + 1) * 384];
                let p = G1Affine::decode128(&chunk[..128])?;
                if !p.is_in_subgroup() {
                    return Err("PAIRING: G1 point not in subgroup".into());
                }
                let q = G2Affine::decode256(&chunk[128..])?;
                if !q.is_in_subgroup() {
                    return Err("PAIRING: G2 point not in subgroup".into());
                }
                acc = acc.mul(&pairing(&p, &q));
            }
            let mut out = vec![0u8; 32];
            if acc == Fp12::one() {
                out[31] = 1;
            }
            Ok(out)
        }
        MAP_FP_TO_G1 => {
            if input.len() != 64 {
                return Err("MAP_FP_TO_G1: input must be 64 bytes".into());
            }
            if input[..16].iter().any(|&b| b != 0) {
                return Err("MAP_FP_TO_G1: top 16 bytes must be zero".into());
            }
            let u = Fp::from_be48(&input[16..])?;
            Ok(map_fp_to_g1(&u).encode128().to_vec())
        }
        MAP_FP2_TO_G2 => {
            if input.len() != 128 {
                return Err("MAP_FP2_TO_G2: input must be 128 bytes".into());
            }
            if input[..16].iter().any(|&b| b != 0) || input[64..80].iter().any(|&b| b != 0) {
                return Err("MAP_FP2_TO_G2: top 16 bytes of each coefficient must be zero".into());
            }
            let u = Fp2::new(Fp::from_be48(&input[16..64])?, Fp::from_be48(&input[80..])?);
            Ok(map_fp2_to_g2(&u).encode256().to_vec())
        }
        _ => Err(format!("unknown BLS precompile 0x{addr:02x}")),
    }
}

// ---------- 0x0a: EIP-4844 point evaluation ----------

pub fn point_evaluation(input: &[u8]) -> Result<Vec<u8>, String> {
    if input.len() != 544 {
        return Err("point evaluation: input must be 544 bytes".into());
    }
    let versioned_hash = &input[..32];
    let z32 = &input[32..64];
    let y32 = &input[64..96];
    let commitment = &input[96..144];
    let proof = &input[144..];
    // versioned hash check: 0x01 || sha256(commitment)[1..]
    let h = sha256(commitment);
    if versioned_hash[0] != 0x01 || versioned_hash[1..] != h[1..] {
        return Err("point evaluation: versioned hash mismatch".into());
    }
    // KZG pairing verification (also enforces z, y < BLS modulus)
    let ok = rrx_bls381::kzg::verify_kzg_proof(commitment, z32, y32, proof)?;
    if !ok {
        return Err("point evaluation: KZG proof verification failed".into());
    }
    // output: FIELD_ELEMENTS_PER_BLOB (4096) || BLS_MODULUS (r)
    let mut out = Vec::with_capacity(64);
    out.extend_from_slice(&4096u64.to_be_bytes());
    out.extend_from_slice(&[0u8; 24]);
    out.extend_from_slice(&r_suborder().to_bytes_be());
    Ok(out)
}

// ---------- simplified SWU + isogeny maps (per EIP-2537 field_to_curve.md) ----------

fn inv0(a: &Fp) -> Fp {
    a.inv().unwrap_or_else(|_| Fp::zero())
}

fn sgn0_fp(u: &Fp) -> bool {
    // m = 1: sign = x mod 2
    u.0.iter_u64_digits().next().unwrap_or(0) & 1 == 1
}

fn sgn0_fp2(u: &Fp2) -> bool {
    // RFC 9380 sgn0 for m = 2: s = x0 mod 2 OR ((x0 == 0) AND (x1 mod 2))
    let s0 = u.c0.0.iter_u64_digits().next().unwrap_or(0) & 1 == 1;
    let z0 = u.c0.is_zero();
    let s1 = u.c1.0.iter_u64_digits().next().unwrap_or(0) & 1 == 1;
    s0 || (z0 && s1)
}

fn fp_hex(s: &str) -> Fp {
    Fp::from_be_bytes(&hex::decode(s).unwrap())
}

// ---- AUTO-GENERATED from EIP-2537 field_to_curve.md + RFC 9380 E.3 (do not hand-edit) ----
const G1_ISO_A: &str = "00144698a3b8e9433d693a02c96d4982b0ea985383ee66a8d8e8981aefd881ac98936f8da0e0f97f5cf428082d584c1d";
const G1_ISO_B: &str = "12e2908d11688030018b12e8753eee3b2016c1f0f24f4070a0b9c14fcef35ef55a23215a316ceaa5d1cc48e98e172be0";
const G1_H_EFF: &str = "d201000000010001";
const G1_K1: [&str; 12] = [
    "11a05f2b1e833340b809101dd99815856b303e88a2d7005ff2627b56cdb4e2c85610c2d5f2e62d6eaeac1662734649b7",
    "17294ed3e943ab2f0588bab22147a81c7c17e75b2f6a8417f565e33c70d1e86b4838f2a6f318c356e834eef1b3cb83bb",
    "0d54005db97678ec1d1048c5d10a9a1bce032473295983e56878e501ec68e25c958c3e3d2a09729fe0179f9dac9edcb0",
    "1778e7166fcc6db74e0609d307e55412d7f5e4656a8dbf25f1b33289f1b330835336e25ce3107193c5b388641d9b6861",
    "0e99726a3199f4436642b4b3e4118e5499db995a1257fb3f086eeb65982fac18985a286f301e77c451154ce9ac8895d9",
    "1630c3250d7313ff01d1201bf7a74ab5db3cb17dd952799b9ed3ab9097e68f90a0870d2dcae73d19cd13c1c66f652983",
    "0d6ed6553fe44d296a3726c38ae652bfb11586264f0f8ce19008e218f9c86b2a8da25128c1052ecaddd7f225a139ed84",
    "17b81e7701abdbe2e8743884d1117e53356de5ab275b4db1a682c62ef0f2753339b7c8f8c8f475af9ccb5618e3f0c88e",
    "080d3cf1f9a78fc47b90b33563be990dc43b756ce79f5574a2c596c928c5d1de4fa295f296b74e956d71986a8497e317",
    "169b1f8e1bcfa7c42e0c37515d138f22dd2ecb803a0c5c99676314baf4bb1b7fa3190b2edc0327797f241067be390c9e",
    "10321da079ce07e272d8ec09d2565b0dfa7dccdde6787f96d50af36003b14866f69b771f8c285decca67df3f1605fb7b",
    "06e08c248e260e70bd1e962381edee3d31d79d7e22c837bc23c0bf1bc24c6b68c24b1b80b64d391fa9c8ba2e8ba2d229",
];
const G1_K2: [&str; 10] = [
    "08ca8d548cff19ae18b2e62f4bd3fa6f01d5ef4ba35b48ba9c9588617fc8ac62b558d681be343df8993cf9fa40d21b1c",
    "12561a5deb559c4348b4711298e536367041e8ca0cf0800c0126c2588c48bf5713daa8846cb026e9e5c8276ec82b3bff",
    "0b2962fe57a3225e8137e629bff2991f6f89416f5a718cd1fca64e00b11aceacd6a3d0967c94fedcfcc239ba5cb83e19",
    "03425581a58ae2fec83aafef7c40eb545b08243f16b1655154cca8abc28d6fd04976d5243eecf5c4130de8938dc62cd8",
    "13a8e162022914a80a6f1d5f43e7a07dffdfc759a12062bb8d6b44e833b306da9bd29ba81f35781d539d395b3532a21e",
    "0e7355f8e4e667b955390f7f0506c6e9395735e9ce9cad4d0a43bcef24b8982f7400d24bc4228f11c02df9a29f6304a5",
    "0772caacf16936190f3e0c63e0596721570f5799af53a1894e2e073062aede9cea73b3538f0de06cec2574496ee84a3a",
    "14a7ac2a9d64a8b230b3f5b074cf01996e7f63c21bca68a81996e1cdf9822c580fa5b9489d11e2d311f7d99bbdcc5a5e",
    "0a10ecf6ada54f825e920b3dafc7a3cce07f8d1d7161366b74100da67f39883503826692abba43704776ec3a79a1d641",
    "095fc13ab9e92ad4476d6e3eb3a56680f682b4ee96f7d03776df533978f31c1593174e4b4b7865002d6384d168ecdd0a",
];
const G1_K3: [&str; 16] = [
    "090d97c81ba24ee0259d1f094980dcfa11ad138e48a869522b52af6c956543d3cd0c7aee9b3ba3c2be9845719707bb33",
    "134996a104ee5811d51036d776fb46831223e96c254f383d0f906343eb67ad34d6c56711962fa8bfe097e75a2e41c696",
    "00cc786baa966e66f4a384c86a3b49942552e2d658a31ce2c344be4b91400da7d26d521628b00523b8dfe240c72de1f6",
    "01f86376e8981c217898751ad8746757d42aa7b90eeb791c09e4a3ec03251cf9de405aba9ec61deca6355c77b0e5f4cb",
    "08cc03fdefe0ff135caf4fe2a21529c4195536fbe3ce50b879833fd221351adc2ee7f8dc099040a841b6daecf2e8fedb",
    "16603fca40634b6a2211e11db8f0a6a074a7d0d4afadb7bd76505c3d3ad5544e203f6326c95a807299b23ab13633a5f0",
    "04ab0b9bcfac1bbcb2c977d027796b3ce75bb8ca2be184cb5231413c4d634f3747a87ac2460f415ec961f8855fe9d6f2",
    "0987c8d5333ab86fde9926bd2ca6c674170a05bfe3bdd81ffd038da6c26c842642f64550fedfe935a15e4ca31870fb29",
    "09fc4018bd96684be88c9e221e4da1bb8f3abd16679dc26c1e8b6e6a1f20cabe69d65201c78607a360370e577bdba587",
    "0e1bba7a1186bdb5223abde7ada14a23c42a0ca7915af6fe06985e7ed1e4d43b9b3f7055dd4eba6f2bafaaebca731c30",
    "19713e47937cd1be0dfd0b8f1d43fb93cd2fcbcb6caf493fd1183e416389e61031bf3a5cce3fbafce813711ad011c132",
    "18b46a908f36f6deb918c143fed2edcc523559b8aaf0c2462e6bfe7f911f643249d9cdf41b44d606ce07c8a4d0074d8e",
    "0b182cac101b9399d155096004f53f447aa7b12a3426b08ec02710e807b4633f06c851c1919211f20d4c04f00b971ef8",
    "0245a394ad1eca9b72fc00ae7be315dc757b3b080d4c158013e6632d3c40659cc6cf90ad1c232a6442d9d3f5db980133",
    "05c129645e44cf1102a159f748c4a3fc5e673d81d7e86568d9ab0f5d396a7ce46ba1049b6579afb7866b1e715475224b",
    "15e6be4e990f03ce4ea50b3b42df2eb5cb181d8f84965a3957add4fa95af01b2b665027efec01c7704b456be69c8b604",
];
const G1_K4: [&str; 15] = [
    "16112c4c3a9c98b252181140fad0eae9601a6de578980be6eec3232b5be72e7a07f3688ef60c206d01479253b03663c1",
    "1962d75c2381201e1a0cbd6c43c348b885c84ff731c4d59ca4a10356f453e01f78a4260763529e3532f6102c2e49a03d",
    "058df3306640da276faaae7d6e8eb15778c4855551ae7f310c35a5dd279cd2eca6757cd636f96f891e2538b53dbf67f2",
    "16b7d288798e5395f20d23bf89edb4d1d115c5dbddbcd30e123da489e726af41727364f2c28297ada8d26d98445f5416",
    "0be0e079545f43e4b00cc912f8228ddcc6d19c9f0f69bbb0542eda0fc9dec916a20b15dc0fd2ededda39142311a5001d",
    "08d9e5297186db2d9fb266eaac783182b70152c65550d881c5ecd87b6f0f5a6449f38db9dfa9cce202c6477faaf9b7ac",
    "166007c08a99db2fc3ba8734ace9824b5eecfdfa8d0cf8ef5dd365bc400a0051d5fa9c01a58b1fb93d1a1399126a775c",
    "16a3ef08be3ea7ea03bcddfabba6ff6ee5a4375efa1f4fd7feb34fd206357132b920f5b00801dee460ee415a15812ed9",
    "1866c8ed336c61231a1be54fd1d74cc4f9fb0ce4c6af5920abc5750c4bf39b4852cfe2f7bb9248836b233d9d55535d4a",
    "167a55cda70a6e1cea820597d94a84903216f763e13d87bb5308592e7ea7d4fbc7385ea3d529b35e346ef48bb8913f55",
    "04d2f259eea405bd48f010a01ad2911d9c6dd039bb61a6290e591b36e636a5c871a5c29f4f83060400f8b49cba8f6aa8",
    "0accbb67481d033ff5852c1e48c50c477f94ff8aefce42d28c0f9a88cea7913516f968986f7ebbea9684b529e2561092",
    "0ad6b9514c767fe3c3613144b45f1496543346d98adf02267d5ceef9a00d9b8693000763e3b90ac11e99b138573345cc",
    "02660400eb2e4f3b628bdd0d53cd76f2bf565b94e72927c1cb748df27942480e420517bd8714cc80d1fadc1326ed06f7",
    "0e0fa1d816ddc03e6b24255e0d7819c171c40f65e273b853324efcd6356caa205ca2f570f13497804415473a1d634b8f",
];
const G2_H_EFF: &str = "bc69f08f2ee75b3584c6a0ea91b352888e2a8e9145ad7689986ff031508ffe1329c2f178731db956d82bf015d1212b02ec0ec69d7477c1ae954cbc06689f6a359894c0adebbf6b4e8020005aaa95551";
const G2_K1: [(&str, &str); 4] = [
    ("05c759507e8e333ebb5b7a9a47d7ed8532c52d39fd3a042a88b58423c50ae15d5c2638e343d9c71c6238aaaaaaaa97d6", "05c759507e8e333ebb5b7a9a47d7ed8532c52d39fd3a042a88b58423c50ae15d5c2638e343d9c71c6238aaaaaaaa97d6"),
    ("000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000", "11560bf17baa99bc32126fced787c88f984f87adf7ae0c7f9a208c6b4f20a4181472aaa9cb8d555526a9ffffffffc71a"),
    ("11560bf17baa99bc32126fced787c88f984f87adf7ae0c7f9a208c6b4f20a4181472aaa9cb8d555526a9ffffffffc71e", "08ab05f8bdd54cde190937e76bc3e447cc27c3d6fbd7063fcd104635a790520c0a395554e5c6aaaa9354ffffffffe38d"),
    ("171d6541fa38ccfaed6dea691f5fb614cb14b4e7f4e810aa22d6108f142b85757098e38d0f671c7188e2aaaaaaaa5ed1", "000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"),
];
const G2_K2: [(&str, &str); 2] = [
    ("000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000", "1a0111ea397fe69a4b1ba7b6434bacd764774b84f38512bf6730d2a0f6b0f6241eabfffeb153ffffb9feffffffffaa63"),
    ("00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000c", "1a0111ea397fe69a4b1ba7b6434bacd764774b84f38512bf6730d2a0f6b0f6241eabfffeb153ffffb9feffffffffaa9f"),
];
const G2_K3: [(&str, &str); 4] = [
    ("1530477c7ab4113b59a4c18b076d11930f7da5d4a07f649bf54439d87d27e500fc8c25ebf8c92f6812cfc71c71c6d706", "1530477c7ab4113b59a4c18b076d11930f7da5d4a07f649bf54439d87d27e500fc8c25ebf8c92f6812cfc71c71c6d706"),
    ("000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000", "05c759507e8e333ebb5b7a9a47d7ed8532c52d39fd3a042a88b58423c50ae15d5c2638e343d9c71c6238aaaaaaaa97be"),
    ("11560bf17baa99bc32126fced787c88f984f87adf7ae0c7f9a208c6b4f20a4181472aaa9cb8d555526a9ffffffffc71c", "08ab05f8bdd54cde190937e76bc3e447cc27c3d6fbd7063fcd104635a790520c0a395554e5c6aaaa9354ffffffffe38f"),
    ("124c9ad43b6cf79bfbf7043de3811ad0761b0f37a1e26286b0e977c69aa274524e79097a56dc4bd9e1b371c71c718b10", "000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"),
];
const G2_K4: [(&str, &str); 3] = [
    ("1a0111ea397fe69a4b1ba7b6434bacd764774b84f38512bf6730d2a0f6b0f6241eabfffeb153ffffb9feffffffffa8fb", "1a0111ea397fe69a4b1ba7b6434bacd764774b84f38512bf6730d2a0f6b0f6241eabfffeb153ffffb9feffffffffa8fb"),
    ("000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000", "1a0111ea397fe69a4b1ba7b6434bacd764774b84f38512bf6730d2a0f6b0f6241eabfffeb153ffffb9feffffffffa9d3"),
    ("000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000012", "1a0111ea397fe69a4b1ba7b6434bacd764774b84f38512bf6730d2a0f6b0f6241eabfffeb153ffffb9feffffffffaa99"),
];

fn poly_eval_fp(coeffs: &[Fp], x: &Fp) -> Fp {
    // Horner, coeffs[0] = constant term ... coeffs[n] = x^n
    let mut acc = Fp::zero();
    for c in coeffs.iter().rev() {
        acc = acc.mul(x).add(c);
    }
    acc
}

pub fn map_fp_to_g1(u: &Fp) -> G1Affine {
    let a = fp_hex(G1_ISO_A);
    let b = fp_hex(G1_ISO_B);
    let z = Fp::from_u64(11);
    // 1. tv1 = inv0(Z^2 u^4 + Z u^2)
    let u2 = u.square();
    let u4 = u2.square();
    let tv1 = inv0(&z.square().mul(&u4).add(&z.mul(&u2)));
    // 2. x1 = (-B/A)(1 + tv1); 3. if tv1 == 0: x1 = B/(Z*A)
    let mut x1 = b.neg().mul(&a.inv().unwrap()).mul(&Fp::one().add(&tv1));
    if tv1.is_zero() {
        x1 = b.mul(&z.mul(&a).inv().unwrap());
    }
    // 4-6. gx1, x2, gx2
    let gx1 = x1.square().mul(&x1).add(&a.mul(&x1)).add(&b);
    let x2 = z.mul(&u2).mul(&x1);
    let gx2 = x2.square().mul(&x2).add(&a.mul(&x2)).add(&b);
    // 7-9. pick square root, adjust sign
    let (xp, mut yp) = if gx1.is_quadratic_residue() {
        (x1, gx1.sqrt().unwrap())
    } else {
        (x2, gx2.sqrt().unwrap())
    };
    if sgn0_fp(u) != sgn0_fp(&yp) {
        yp = yp.neg();
    }
    // iso_map on E'
    let k1: Vec<Fp> = G1_K1.iter().map(|s| fp_hex(s)).collect();
    let k2: Vec<Fp> = G1_K2.iter().map(|s| fp_hex(s)).collect();
    let k3: Vec<Fp> = G1_K3.iter().map(|s| fp_hex(s)).collect();
    let k4: Vec<Fp> = G1_K4.iter().map(|s| fp_hex(s)).collect();
    let x_num = poly_eval_fp(&k1, &xp);
    // x_den = x'^10 + k_(2,9)x'^9 + ... + k_(2,0): constant-first Horner = [k2_0, ..., k2_9, 1]
    let mut xden_coeffs: Vec<Fp> = k2.iter().cloned().collect();
    xden_coeffs.push(Fp::one());
    let x_den = poly_eval_fp(&xden_coeffs, &xp);
    let y_num = poly_eval_fp(&k3, &xp);
    let mut yden_coeffs: Vec<Fp> = k4.iter().cloned().collect();
    yden_coeffs.push(Fp::one());
    let y_den = poly_eval_fp(&yden_coeffs, &xp);
    let x = x_num.mul(&x_den.inv().unwrap());
    let y = yp.mul(&y_num).mul(&y_den.inv().unwrap());
    let p = G1Affine { x, y, infinity: false };
    // clear cofactor
    let h_eff = BigUint::parse_bytes(G1_H_EFF.as_bytes(), 16).unwrap();
    p.mul(&h_eff)
}

// ----- G2: E' : y² = x³ + A'x + B' over Fp2 (3-isogeny to E) -----
fn poly_eval_fp2(coeffs: &[Fp2], x: &Fp2) -> Fp2 {
    let mut acc = Fp2::zero();
    for c in coeffs.iter().rev() {
        acc = acc.mul(x).add(c);
    }
    acc
}

fn fp2_hex_pair(c0: &str, c1: &str) -> Fp2 {
    Fp2::new(fp_hex(c0), fp_hex(c1))
}

// constants expressed as (c0, c1) pairs
pub fn map_fp2_to_g2(u: &Fp2) -> G2Affine {
    let a = Fp2::new(Fp::zero(), Fp::from_u64(240));
    let b = Fp2::new(Fp::from_u64(1012), Fp::from_u64(1012));
    let z = Fp2::new(Fp::from_u64(2).neg(), Fp::one().neg()); // -(2 + I)
    let u2 = u.square();
    let u4 = u2.square();
    let tv1 = (z.square().mul(&u4).add(&z.mul(&u2))).inv().unwrap_or_else(|_| Fp2::zero());
    let mut x1 = b.neg().mul(&a.inv().unwrap()).mul(&Fp2::one().add(&tv1));
    if tv1.is_zero() {
        x1 = b.mul(&z.mul(&a).inv().unwrap());
    }
    let gx1 = x1.square().mul(&x1).add(&a.mul(&x1)).add(&b);
    let x2 = z.mul(&u2).mul(&x1);
    let gx2 = x2.square().mul(&x2).add(&a.mul(&x2)).add(&b);
    // is gx1 a square in Fp2?
    let (xp, mut yp) = if gx1.sqrt().is_ok() {
        (x1.clone(), gx1.sqrt().unwrap())
    } else {
        (x2.clone(), gx2.sqrt().unwrap())
    };
    if sgn0_fp2(u) != sgn0_fp2(&yp) {
        yp = yp.neg();
    }
    // 3-isogeny
    let k1: Vec<Fp2> = G2_K1.iter().map(|(a0, a1)| fp2_hex_pair(a0, a1)).collect();
    let k2: Vec<Fp2> = G2_K2.iter().map(|(a0, a1)| fp2_hex_pair(a0, a1)).collect();
    let k3: Vec<Fp2> = G2_K3.iter().map(|(a0, a1)| fp2_hex_pair(a0, a1)).collect();
    let k4: Vec<Fp2> = G2_K4.iter().map(|(a0, a1)| fp2_hex_pair(a0, a1)).collect();
    // x_num = k1_3 x'^3 + k1_2 x'^2 + k1_1 x' + k1_0 (k1[0] = constant)
    let x_num = poly_eval_fp2(&k1, &xp);
    // x_den = x'^2 + k2_1 x' + k2_0  (constant-first for Horner)
    let xden = vec![k2[0].clone(), k2[1].clone(), Fp2::one()];
    let x_den = poly_eval_fp2(&xden, &xp);
    // y_num = k3_3 x'^3 + k3_2 x'^2 + k3_1 x' + k3_0
    let y_num = poly_eval_fp2(&k3, &xp);
    // y_den = x'^3 + k4_2 x'^2 + k4_1 x' + k4_0  (constant-first for Horner)
    let yden = vec![k4[0].clone(), k4[1].clone(), k4[2].clone(), Fp2::one()];
    let y_den = poly_eval_fp2(&yden, &xp);
    let x = x_num.mul(&x_den.inv().unwrap());
    let y = yp.mul(&y_num).mul(&y_den.inv().unwrap());
    let p = G2Affine { x, y, infinity: false };
    let h_eff = BigUint::parse_bytes(G2_H_EFF.as_bytes(), 16).unwrap();
    p.mul(&h_eff)
}

// silence unused warnings for spec constants referenced in comments

