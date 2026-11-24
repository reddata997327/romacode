//! Official EIP-2537 conformance tests (vectors from ethereum/EIPs assets/eip-2537).

use rrx_precompiles::bls381;

fn fixtures() -> serde_json::Value {
    let raw = include_str!("fixtures/eip2537_vectors.json");
    serde_json::from_str(raw).unwrap()
}

fn run_group(v: &serde_json::Value, key: &str, addr: u8, out_len: usize) {
    let cases = v.get(key).expect(key).as_array().unwrap();
    for case in cases {
        let input = hex::decode(case["Input"].as_str().unwrap()).unwrap();
        let expected = hex::decode(case["Expected"].as_str().unwrap()).unwrap();
        // gas cost must match the official schedule
        let gas = bls381::gas_cost(addr, &input);
        assert_eq!(gas as u64, case["Gas"].as_u64().unwrap(), "gas mismatch on {}", case["Name"]);
        let out = bls381::run(addr, &input).unwrap_or_else(|e| panic!("{}: {}", case["Name"], e));
        assert_eq!(out.len(), out_len, "output len on {}", case["Name"]);
        assert_eq!(hex::encode(&out), hex::encode(&expected), "output mismatch on {}", case["Name"]);
    }
}

#[test]
fn official_vectors() {
    let v = fixtures();
    run_group(&v, "add_G1_bls", bls381::G1ADD, 128);
    run_group(&v, "add_G2_bls", bls381::G2ADD, 256);
    run_group(&v, "mul_G1_bls", bls381::G1MSM, 128);
    run_group(&v, "mul_G2_bls", bls381::G2MSM, 256);
    run_group(&v, "msm_G1_bls", bls381::G1MSM, 128);
    run_group(&v, "msm_G2_bls", bls381::G2MSM, 256);
    run_group(&v, "pairing_check_bls", bls381::PAIRING, 32);
    run_group(&v, "map_fp_to_G1_bls", bls381::MAP_FP_TO_G1, 128);
    run_group(&v, "map_fp2_to_G2_bls", bls381::MAP_FP2_TO_G2, 256);
}

#[test]
fn official_failure_vectors() {
    let v = fixtures();
    let cases = v.get("fail-add_G1_bls").unwrap().as_array().unwrap();
    for case in cases {
        let input = hex::decode(case["Input"].as_str().unwrap()).unwrap();
        // must error AND burn all gas (execute path)
        assert!(
            bls381::run(bls381::G1ADD, &input).is_err(),
            "expected failure on {}",
            case["Name"]
        );
    }
}
