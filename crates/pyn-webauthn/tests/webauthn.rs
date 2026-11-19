mod common;

use common::*;
use pyn_webauthn::{
    ALG_EDDSA, ALG_ES256, AuthenticatorData, CounterVerdict, Error, Expected, PublicKey,
    verify_assertion, verify_registration,
};

const ORIGINS: &[&str] = &[ORIGIN, "https://alt.pyn.example"];

fn expected(uv: bool) -> Expected<'static> {
    Expected {
        challenge: CHALLENGE,
        rp_id: RP_ID,
        origins: ORIGINS,
        require_user_verification: uv,
    }
}

fn auths() -> Vec<Authenticator> {
    vec![Authenticator::es256(7), Authenticator::ed(9)]
}

fn register(a: &Authenticator, flags: u8, count: u32) -> (Vec<u8>, Vec<u8>) {
    let cd = client_data("webauthn.create", CHALLENGE, ORIGIN);
    (cd, none_object(&a.reg_auth_data(RP_ID, flags, count)))
}

fn reg_err(cd: &[u8], obj: &[u8], uv: bool) -> Error {
    verify_registration(cd, obj, &[], &expected(uv)).unwrap_err()
}

struct Get {
    cd: Vec<u8>,
    ad: Vec<u8>,
    sig: Vec<u8>,
}

fn get(a: &Authenticator, flags: u8, count: u32) -> Get {
    let cd = client_data("webauthn.get", CHALLENGE, ORIGIN);
    let ad = plain_auth_data(RP_ID, flags, count);
    let sig = assertion(a, &ad, &cd);
    Get { cd, ad, sig }
}

fn assert_err(g: &Get, key: &[u8], stored: u32, uv: bool) -> Error {
    verify_assertion(&g.cd, &g.ad, &g.sig, key, stored, &expected(uv)).unwrap_err()
}

#[test]
fn registration_succeeds() {
    for (a, alg) in auths().iter().zip([ALG_ES256, ALG_EDDSA]) {
        let (cd, obj) = register(a, UP | UV | BE | BS, 5);
        let r = verify_registration(&cd, &obj, &["internal", "hybrid"], &expected(true)).unwrap();
        assert_eq!(r.credential_id, a.cred_id);
        assert_eq!(r.cose_key, a.cose_key());
        assert_eq!(r.alg, alg);
        assert_eq!(r.aaguid, [0xA5; 16]);
        assert_eq!(r.sign_count, 5);
        assert!(r.user_verified && r.backup_eligible && r.backup_state);
        assert_eq!(r.transports, ["internal", "hybrid"]);
        PublicKey::from_cose(&r.cose_key).unwrap();
    }
}

#[test]
fn registration_without_uv_when_not_required() {
    let a = Authenticator::es256(1);
    let (cd, obj) = register(&a, UP, 0);
    let r = verify_registration(&cd, &obj, &[], &expected(false)).unwrap();
    assert!(!r.user_verified && !r.backup_eligible);
}

#[test]
fn registration_origin_list() {
    let a = Authenticator::es256(1);
    let obj = none_object(&a.reg_auth_data(RP_ID, UP, 0));
    let cd = client_data("webauthn.create", CHALLENGE, "https://alt.pyn.example");
    verify_registration(&cd, &obj, &[], &expected(false)).unwrap();
}

#[test]
fn registration_rejections() {
    let a = Authenticator::es256(1);
    let (cd, obj) = register(&a, UP | UV, 0);

    let bad = client_data("webauthn.get", CHALLENGE, ORIGIN);
    assert_eq!(reg_err(&bad, &obj, false), Error::WrongType);
    let bad = client_data("webauthn.create", b"other challenge", ORIGIN);
    assert_eq!(reg_err(&bad, &obj, false), Error::ChallengeMismatch);
    let mut same_len = CHALLENGE.to_vec();
    same_len[31] ^= 1;
    let bad = client_data("webauthn.create", &same_len, ORIGIN);
    assert_eq!(reg_err(&bad, &obj, false), Error::ChallengeMismatch);
    let bad = client_data("webauthn.create", &CHALLENGE[..10], ORIGIN);
    assert_eq!(reg_err(&bad, &obj, false), Error::ChallengeMismatch);
    let bad = client_data("webauthn.create", CHALLENGE, "https://evil.example");
    assert_eq!(reg_err(&bad, &obj, false), Error::OriginNotAllowed);
    let bad = client_data("webauthn.create", CHALLENGE, "https://pyn.example/");
    assert_eq!(reg_err(&bad, &obj, false), Error::OriginNotAllowed);

    let wrong_rp = none_object(&a.reg_auth_data("evil.example", UP, 0));
    assert_eq!(reg_err(&cd, &wrong_rp, false), Error::RpIdHashMismatch);
    let no_up = none_object(&a.reg_auth_data(RP_ID, UV, 0));
    assert_eq!(reg_err(&cd, &no_up, false), Error::UserNotPresent);
    let no_uv = none_object(&a.reg_auth_data(RP_ID, UP, 0));
    assert_eq!(reg_err(&cd, &no_uv, true), Error::UserNotVerified);
    let bs_only = none_object(&a.reg_auth_data(RP_ID, UP | BS, 0));
    assert_eq!(reg_err(&cd, &bs_only, false), Error::InvalidFlags);
}

#[test]
fn registration_cross_origin_and_client_data_shape() {
    let a = Authenticator::es256(1);
    let (_, obj) = register(&a, UP, 0);
    let b64 = base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, CHALLENGE);
    let cross = format!(
        r#"{{"type":"webauthn.create","challenge":"{b64}","origin":"{ORIGIN}","crossOrigin":true}}"#
    );
    assert_eq!(reg_err(cross.as_bytes(), &obj, false), Error::CrossOrigin);
    for junk in [&b"not json"[..], b"[]", b"{}", b"", b"{\"type\":1}"] {
        assert_eq!(reg_err(junk, &obj, false), Error::MalformedClientData);
    }
    let padded =
        format!(r#"{{"type":"webauthn.create","challenge":"{b64}=","origin":"{ORIGIN}"}}"#);
    assert_eq!(
        reg_err(padded.as_bytes(), &obj, false),
        Error::MalformedClientData
    );
    let mut trailing = client_data("webauthn.create", CHALLENGE, ORIGIN);
    trailing.extend(b" x");
    assert_eq!(reg_err(&trailing, &obj, false), Error::MalformedClientData);
    let dup = format!(
        r#"{{"type":"webauthn.create","type":"webauthn.create","challenge":"{b64}","origin":"{ORIGIN}"}}"#
    );
    assert_eq!(
        reg_err(dup.as_bytes(), &obj, false),
        Error::MalformedClientData
    );
}

#[test]
fn registration_attestation_shape() {
    let a = Authenticator::es256(1);
    let cd = client_data("webauthn.create", CHALLENGE, ORIGIN);
    let ad = a.reg_auth_data(RP_ID, UP, 0);

    for fmt in [
        "packed",
        "fido-u2f",
        "apple",
        "tpm",
        "android-key",
        "android-safetynet",
        "None",
        "",
    ] {
        let obj = attestation_object(fmt, map(&[]), &ad);
        assert_eq!(
            reg_err(&cd, &obj, false),
            Error::UnsupportedAttestationFormat(fmt.into())
        );
    }
    let stmt = map(&[(text("alg"), int(-7)), (text("sig"), bstr(b"x"))]);
    assert_eq!(
        reg_err(&cd, &attestation_object("none", stmt, &ad), false),
        Error::MalformedAttestation
    );

    let mut obj = none_object(&ad);
    obj.push(0);
    assert_eq!(reg_err(&cd, &obj, false), Error::TrailingData);

    let dup = map(&[(text("fmt"), text("none")), (text("fmt"), text("none"))]);
    assert_eq!(reg_err(&cd, &dup, false), Error::DuplicateKey);

    let missing = map(&[(text("fmt"), text("none")), (text("attStmt"), map(&[]))]);
    assert_eq!(reg_err(&cd, &missing, false), Error::MalformedAttestation);
    let wrong = map(&[
        (text("fmt"), int(1)),
        (text("attStmt"), map(&[])),
        (text("authData"), bstr(&ad)),
    ]);
    assert_eq!(reg_err(&cd, &wrong, false), Error::MalformedAttestation);
    assert_eq!(reg_err(&cd, &[0x80], false), Error::MalformedCbor);

    let plain = none_object(&plain_auth_data(RP_ID, UP, 0));
    assert_eq!(reg_err(&cd, &plain, false), Error::MissingCredentialData);
}

#[test]
fn registration_key_rejections() {
    let a = Authenticator::es256(1);
    let cd = client_data("webauthn.create", CHALLENGE, ORIGIN);
    let with_key = |key: Vec<u8>| {
        let mut v = plain_auth_data(RP_ID, UP | AT, 0);
        v.extend([0u8; 16]);
        v.extend((a.cred_id.len() as u16).to_be_bytes());
        v.extend(&a.cred_id);
        v.extend(key);
        none_object(&v)
    };
    let rs256 = map(&[
        (int(1), int(3)),
        (int(3), int(-257)),
        (int(-1), bstr(&[1; 256])),
        (int(-2), bstr(&[1, 0, 1])),
    ]);
    assert_eq!(
        reg_err(&cd, &with_key(rs256), false),
        Error::UnsupportedAlgorithm(-257)
    );
    let es384 = map(&[(int(1), int(2)), (int(3), int(-35)), (int(-1), int(2))]);
    assert_eq!(
        reg_err(&cd, &with_key(es384), false),
        Error::UnsupportedAlgorithm(-35)
    );

    let off_curve = map(&[
        (int(1), int(2)),
        (int(3), int(-7)),
        (int(-1), int(1)),
        (int(-2), bstr(&[1; 32])),
        (int(-3), bstr(&[1; 32])),
    ]);
    assert_eq!(
        reg_err(&cd, &with_key(off_curve), false),
        Error::MalformedPublicKey
    );
    let short = map(&[
        (int(1), int(2)),
        (int(3), int(-7)),
        (int(-1), int(1)),
        (int(-2), bstr(&[1; 31])),
        (int(-3), bstr(&[1; 32])),
    ]);
    assert_eq!(
        reg_err(&cd, &with_key(short), false),
        Error::MalformedPublicKey
    );
    let wrong_curve = map(&[
        (int(1), int(1)),
        (int(3), int(-8)),
        (int(-1), int(7)),
        (int(-2), bstr(&[1; 32])),
    ]);
    assert_eq!(
        reg_err(&cd, &with_key(wrong_curve), false),
        Error::MalformedPublicKey
    );
    let ok = Authenticator::es256(2);
    let pt = match &ok.key {
        Key::Es256(k) => k.verifying_key().to_encoded_point(false),
        _ => unreachable!(),
    };
    for (kty, crv) in [(1, 1), (2, 2)] {
        let k = map(&[
            (int(1), int(kty)),
            (int(3), int(-7)),
            (int(-1), int(crv)),
            (int(-2), bstr(pt.x().unwrap())),
            (int(-3), bstr(pt.y().unwrap())),
        ]);
        assert_eq!(reg_err(&cd, &with_key(k), false), Error::MalformedPublicKey);
    }
    let no_alg = map(&[(int(1), int(1))]);
    assert_eq!(
        reg_err(&cd, &with_key(no_alg), false),
        Error::MalformedPublicKey
    );
    let dup = map(&[(int(1), int(2)), (int(1), int(2)), (int(3), int(-7))]);
    assert_eq!(reg_err(&cd, &with_key(dup), false), Error::DuplicateKey);
    assert_eq!(
        reg_err(&cd, &with_key(vec![0xFF]), false),
        Error::MalformedCbor
    );
}

#[test]
fn auth_data_structure() {
    let a = Authenticator::es256(1);
    let full = a.reg_auth_data(RP_ID, UP | UV, 3);
    AuthenticatorData::parse(&full).unwrap();

    for n in 0..full.len() {
        assert!(AuthenticatorData::parse(&full[..n]).is_err(), "prefix {n}");
    }
    let mut extra = full.clone();
    extra.push(0);
    assert_eq!(AuthenticatorData::parse(&extra), Err(Error::TrailingData));
    let mut plain = plain_auth_data(RP_ID, UP, 0);
    plain.push(0);
    assert_eq!(AuthenticatorData::parse(&plain), Err(Error::TrailingData));

    let mut zero_len = plain_auth_data(RP_ID, UP | AT, 0);
    zero_len.extend([0u8; 16]);
    zero_len.extend([0, 0]);
    assert_eq!(
        AuthenticatorData::parse(&zero_len),
        Err(Error::BadCredentialIdLength)
    );
    let mut too_long = plain_auth_data(RP_ID, UP | AT, 0);
    too_long.extend([0u8; 16]);
    too_long.extend(1024u16.to_be_bytes());
    too_long.extend(vec![0u8; 1024]);
    too_long.extend(a.cose_key());
    assert_eq!(
        AuthenticatorData::parse(&too_long),
        Err(Error::BadCredentialIdLength)
    );

    let mut ext = a.reg_auth_data(RP_ID, UP | ED, 1);
    let ext_map = map(&[(text("credProtect"), int(2))]);
    ext.extend(&ext_map);
    let parsed = AuthenticatorData::parse(&ext).unwrap();
    assert_eq!(parsed.extensions.as_deref(), Some(&ext_map[..]));
    let mut bad_ext = plain_auth_data(RP_ID, UP | ED, 1);
    bad_ext.extend(int(1));
    assert_eq!(
        AuthenticatorData::parse(&bad_ext),
        Err(Error::MalformedCbor)
    );
    assert!(AuthenticatorData::parse(&plain_auth_data(RP_ID, UP | ED, 1)).is_err());
}

#[test]
fn auth_data_flags() {
    for (flags, up, uv, be, bs) in [
        (UP, true, false, false, false),
        (UV, false, true, false, false),
        (UP | UV | BE, true, true, true, false),
        (BE | BS, false, false, true, true),
    ] {
        let d = AuthenticatorData::parse(&plain_auth_data(RP_ID, flags, 0x01020304)).unwrap();
        assert_eq!(
            (
                d.user_present,
                d.user_verified,
                d.backup_eligible,
                d.backup_state
            ),
            (up, uv, be, bs)
        );
        assert_eq!(d.sign_count, 0x01020304);
        assert!(d.attested.is_none() && d.extensions.is_none());
    }
}

#[test]
fn assertion_succeeds() {
    for a in auths() {
        let g = get(&a, UP | UV | BE, 10);
        let r = verify_assertion(&g.cd, &g.ad, &g.sig, &a.cose_key(), 4, &expected(true)).unwrap();
        assert_eq!(r.sign_count, 10);
        assert_eq!(r.counter, CounterVerdict::Advanced);
        assert!(r.user_verified && r.backup_eligible && !r.backup_state);
    }
}

#[test]
fn assertion_counter_verdicts() {
    let a = Authenticator::es256(3);
    let key = a.cose_key();
    let verdict = |new: u32, stored: u32| {
        let g = get(&a, UP, new);
        verify_assertion(&g.cd, &g.ad, &g.sig, &key, stored, &expected(false))
            .unwrap()
            .counter
    };
    assert_eq!(verdict(0, 0), CounterVerdict::NotUsed);
    assert_eq!(verdict(1, 0), CounterVerdict::Advanced);
    assert_eq!(verdict(6, 5), CounterVerdict::Advanced);
    assert_eq!(verdict(5, 5), CounterVerdict::Regressed);
    assert_eq!(verdict(4, 5), CounterVerdict::Regressed);
    assert_eq!(verdict(0, 5), CounterVerdict::Regressed);
}

#[test]
fn assertion_rejections() {
    for a in auths() {
        let key = a.cose_key();
        let ok = get(&a, UP | UV, 2);
        let rewrap = |cd: Vec<u8>| Get {
            sig: assertion(&a, &ok.ad, &cd),
            cd,
            ad: ok.ad.clone(),
        };

        let g = rewrap(client_data("webauthn.create", CHALLENGE, ORIGIN));
        assert_eq!(assert_err(&g, &key, 0, false), Error::WrongType);
        let g = rewrap(client_data("webauthn.get", b"stale", ORIGIN));
        assert_eq!(assert_err(&g, &key, 0, false), Error::ChallengeMismatch);
        let g = rewrap(client_data(
            "webauthn.get",
            CHALLENGE,
            "https://evil.example",
        ));
        assert_eq!(assert_err(&g, &key, 0, false), Error::OriginNotAllowed);

        let ad = plain_auth_data("evil.example", UP, 2);
        let g = Get {
            sig: assertion(&a, &ad, &ok.cd),
            ad,
            cd: ok.cd.clone(),
        };
        assert_eq!(assert_err(&g, &key, 0, false), Error::RpIdHashMismatch);
        let ad = plain_auth_data(RP_ID, UV, 2);
        let g = Get {
            sig: assertion(&a, &ad, &ok.cd),
            ad,
            cd: ok.cd.clone(),
        };
        assert_eq!(assert_err(&g, &key, 0, false), Error::UserNotPresent);
        let ad = plain_auth_data(RP_ID, UP, 2);
        let g = Get {
            sig: assertion(&a, &ad, &ok.cd),
            ad,
            cd: ok.cd.clone(),
        };
        assert_eq!(assert_err(&g, &key, 0, true), Error::UserNotVerified);
    }
}

#[test]
fn assertion_signature_failures() {
    let a = Authenticator::es256(3);
    let other = Authenticator::es256(4);
    let ed = Authenticator::ed(3);
    let key = a.cose_key();
    let g = get(&a, UP, 2);

    let wrong_signer = get(&other, UP, 2);
    assert_eq!(
        assert_err(&wrong_signer, &key, 0, false),
        Error::SignatureInvalid
    );
    assert_eq!(
        assert_err(&g, &ed.cose_key(), 0, false),
        Error::MalformedSignature
    );

    let mut tampered = Get {
        cd: g.cd.clone(),
        ad: g.ad.clone(),
        sig: g.sig.clone(),
    };
    tampered.ad[36] ^= 1;
    assert_eq!(
        assert_err(&tampered, &key, 0, false),
        Error::SignatureInvalid
    );
    let mut tampered = Get {
        cd: g.cd.clone(),
        ad: g.ad.clone(),
        sig: g.sig.clone(),
    };
    let last = tampered.sig.len() - 1;
    tampered.sig[last] ^= 1;
    assert_eq!(
        assert_err(&tampered, &key, 0, false),
        Error::SignatureInvalid
    );
    let mut tampered = Get {
        cd: g.cd.clone(),
        ad: g.ad.clone(),
        sig: g.sig.clone(),
    };
    *tampered.cd.last_mut().unwrap() = b' ';
    assert_eq!(
        assert_err(&tampered, &key, 0, false),
        Error::MalformedClientData
    );
    let mut spaced = g.cd.clone();
    spaced.push(b' ');
    let tampered = Get {
        cd: spaced,
        ad: g.ad.clone(),
        sig: g.sig.clone(),
    };
    assert_eq!(
        assert_err(&tampered, &key, 0, false),
        Error::SignatureInvalid
    );

    for sig in [
        vec![],
        vec![0; 10],
        g.sig[..g.sig.len() - 1].to_vec(),
        [g.sig.clone(), vec![0]].concat(),
    ] {
        let bad = Get {
            cd: g.cd.clone(),
            ad: g.ad.clone(),
            sig,
        };
        assert!(matches!(
            assert_err(&bad, &key, 0, false),
            Error::MalformedSignature | Error::SignatureInvalid
        ));
    }
    let edg = get(&ed, UP, 2);
    for n in [0, 63, 65] {
        let sig = edg.sig.iter().copied().cycle().take(n).collect();
        let bad = Get {
            cd: edg.cd.clone(),
            ad: edg.ad.clone(),
            sig,
        };
        assert_eq!(
            assert_err(&bad, &ed.cose_key(), 0, false),
            Error::MalformedSignature
        );
    }
    let mut flipped = Get {
        cd: edg.cd.clone(),
        ad: edg.ad.clone(),
        sig: edg.sig.clone(),
    };
    flipped.sig[0] ^= 1;
    assert_eq!(
        assert_err(&flipped, &ed.cose_key(), 0, false),
        Error::SignatureInvalid
    );
}

#[test]
fn assertion_rejects_bad_key_and_auth_data() {
    let a = Authenticator::es256(3);
    let g = get(&a, UP, 2);
    assert_eq!(assert_err(&g, &[0xA0], 0, false), Error::MalformedPublicKey);
    let mut k = a.cose_key();
    k.push(0);
    assert_eq!(assert_err(&g, &k, 0, false), Error::TrailingData);
    let mut trailing = Get {
        cd: g.cd.clone(),
        ad: g.ad.clone(),
        sig: vec![],
    };
    trailing.ad.push(0);
    assert_eq!(
        assert_err(&trailing, &a.cose_key(), 0, false),
        Error::TrailingData
    );
    let short = Get {
        cd: g.cd.clone(),
        ad: g.ad[..20].to_vec(),
        sig: vec![],
    };
    assert_eq!(
        assert_err(&short, &a.cose_key(), 0, false),
        Error::MalformedAuthData
    );
}

#[test]
fn malformed_inputs_never_panic() {
    let mut seed = 0x9E3779B97F4A7C15u64;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let a = Authenticator::es256(5);
    let e = Authenticator::ed(5);
    let (cd, obj) = register(&a, UP | UV, 1);
    let g = get(&e, UP | UV, 1);
    let key = e.cose_key();
    for i in 0..4000 {
        let mutate = |src: &[u8], r: &mut dyn FnMut() -> u64| {
            let mut v = src.to_vec();
            match r() % 4 {
                0 if !v.is_empty() => {
                    let n = (r() as usize) % v.len();
                    v.truncate(n);
                }
                1 => v.push(r() as u8),
                _ if !v.is_empty() => {
                    for _ in 0..=(r() % 3) {
                        let n = (r() as usize) % v.len();
                        v[n] ^= 1 << (r() % 8);
                    }
                }
                _ => {}
            }
            v
        };
        let o = mutate(&obj, &mut rnd);
        let c = mutate(&cd, &mut rnd);
        let _ = verify_registration(&c, &o, &[], &expected(i % 2 == 0));
        let ad = mutate(&g.ad, &mut rnd);
        let sg = mutate(&g.sig, &mut rnd);
        let k = mutate(&key, &mut rnd);
        let _ = verify_assertion(&g.cd, &ad, &sg, &k, 0, &expected(false));
        let _ = AuthenticatorData::parse(&ad);
        let _ = PublicKey::from_cose(&k);
    }
}

#[test]
fn hostile_cbor_lengths_and_nesting() {
    let cd = client_data("webauthn.create", CHALLENGE, ORIGIN);
    let cases: Vec<Vec<u8>> = vec![
        vec![0xBB, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF],
        vec![0x5B, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 1],
        vec![0x9A, 0xFF, 0xFF, 0xFF, 0xFF],
        vec![0x7A, 0xFF, 0xFF, 0xFF, 0xFF, b'a'],
        vec![0xBF],
        vec![0x9F; 100_000],
        [vec![0xA1, 0x63, b'f', b'm', b't'], vec![0x81; 50_000]].concat(),
        vec![],
    ];
    for c in &cases {
        let _ = verify_registration(&cd, c, &[], &expected(false));
        let _ = PublicKey::from_cose(c);
        let mut ad = plain_auth_data(RP_ID, UP | AT, 0);
        ad.extend([0u8; 16]);
        ad.extend([0, 1, 1]);
        ad.extend(c);
        let _ = AuthenticatorData::parse(&ad);
    }
}
