//! Large files: chunked upload that only sends what the server lacks, and verified, resumable downloads.

mod common;

use common::{REPO, start};

fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

#[test]
fn a_large_file_uploads_in_chunks_and_downloads_back_identical() {
    let env = start();
    let home = env.dir("home");
    let data = noise(6_500_000, 5);
    let src = env.dir("src").join("big.bin");
    std::fs::create_dir_all(src.parent().unwrap()).unwrap();
    std::fs::write(&src, &data).unwrap();

    let out = env.ok(
        &home,
        "alice",
        &[
            "--repo",
            REPO,
            "commit",
            "Source/big.bin",
            "--file",
            src.to_str().unwrap(),
            "-m",
            "big",
        ],
    );
    assert!(out.contains("is now at revision 1"), "{out}");

    let dest = env.dir("out").join("big.bin");
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    let out = env.ok(
        &home,
        "alice",
        &[
            "--repo",
            REPO,
            "get",
            "Source/big.bin",
            "--output",
            dest.to_str().unwrap(),
        ],
    );
    assert!(out.contains("6500000 bytes"), "{out}");
    assert_eq!(std::fs::read(&dest).unwrap(), data);
}

#[test]
fn an_interrupted_download_continues_from_the_partial_file() {
    let env = start();
    let home = env.dir("home");
    let data = noise(3_000_000, 8);
    let src = env.dir("src").join("big.bin");
    std::fs::create_dir_all(src.parent().unwrap()).unwrap();
    std::fs::write(&src, &data).unwrap();
    env.ok(
        &home,
        "alice",
        &[
            "--repo",
            REPO,
            "commit",
            "Source/big.bin",
            "--file",
            src.to_str().unwrap(),
            "-m",
            "big",
        ],
    );

    let dest = env.dir("out").join("big.bin");
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    std::fs::write(dest.with_file_name("big.bin.pyn-part"), &data[..1_000_000]).unwrap();
    env.ok(
        &home,
        "alice",
        &[
            "--repo",
            REPO,
            "get",
            "Source/big.bin",
            "--output",
            dest.to_str().unwrap(),
        ],
    );
    assert_eq!(std::fs::read(&dest).unwrap(), data);
    assert!(!dest.with_file_name("big.bin.pyn-part").exists());

    std::fs::remove_file(&dest).unwrap();
    let mut wrong = data[..500_000].to_vec();
    wrong[10] ^= 0xff;
    std::fs::write(dest.with_file_name("big.bin.pyn-part"), wrong).unwrap();
    env.ok(
        &home,
        "alice",
        &[
            "--repo",
            REPO,
            "get",
            "Source/big.bin",
            "--output",
            dest.to_str().unwrap(),
        ],
    );
    assert_eq!(
        std::fs::read(&dest).unwrap(),
        data,
        "a bad partial file is discarded"
    );
}
