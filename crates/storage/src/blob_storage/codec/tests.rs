use super::*;

#[test]
fn bounded_pack_roundtrip_and_corruption() {
    for data in [
        vec![],
        vec![0; 100_000],
        (0..100_000).map(|i| (i * 37) as u8).collect(),
    ] {
        let p = pack(&data).unwrap();
        assert_eq!(unpack(&p, data.len()).unwrap(), data);
        assert!(unpack(&p, data.len() + 1).is_err());
    }
    let mut p = pack(&vec![7; 100_000]).unwrap();
    let last = p.len() - 1;
    p[last] ^= 1;
    assert!(unpack(&p, 100_000).is_err());
    assert!(unpack(&[], 0).is_err());
}

#[test]
fn dictionary_is_one_hop_and_exact() {
    let base: Vec<u8> = (0..20000)
        .flat_map(|i| format!("paragraph {i}: 内容の検証\n").into_bytes())
        .collect();
    let mut edited = base.clone();
    edited.splice(100..110, b"edited".iter().copied());
    let packed = pack(&base).unwrap();
    let key = sha256_hex(&packed);
    let plans = candidates(&edited, Some((key, base.clone()))).unwrap();
    let plan = plans
        .iter()
        .find(|p| matches!(p.manifest.body, Body::Dictionary { .. }))
        .unwrap();
    if let Body::Dictionary {
        object, base_size, ..
    } = &plan.manifest.body
    {
        assert_eq!(*base_size, base.len());
        assert_eq!(
            dictionary_decode(&plan.objects[object], &base, edited.len()).unwrap(),
            edited
        );
        assert!(dictionary_decode(&plan.objects[object], b"wrong", edited.len()).is_err());
    } else {
        panic!("局所変更は辞書表現になること");
    }
}

#[test]
fn manifest_rejects_unknown_version_path_and_unbounded_sizes() {
    let mut m = Manifest {
        format: 2,
        size: 0,
        body: Body::Full {
            object: "a".repeat(64),
        },
    };
    assert!(m.validate(0).is_err());
    m.format = 1;
    assert!(m.validate(0).is_ok());
    m.body = Body::Full {
        object: "../other-tenant".into(),
    };
    assert!(m.validate(0).is_err());
    m.body = Body::Chunks {
        objects: vec![Chunk {
            object: "a".repeat(64),
            size: 65537,
        }],
    };
    assert!(m.validate(0).is_err());
}
