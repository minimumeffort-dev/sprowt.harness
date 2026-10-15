use super::*;

#[test]
fn review_preserves_non_utf8_pdf_and_replayable_patch() {
    let data = TestData::new();
    let project = project(&data);
    let before = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\noriginal\n%%EOF\n";
    let after = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\nupdated\n%%EOF\n";
    fs::write(project.join("fixture.pdf"), before).unwrap();
    let root = data.0.join("workspace");
    create(&project, &root).unwrap();
    fs::write(root.join("work/a.txt"), "Readable café text\n").unwrap();
    fs::write(root.join("work/fixture.pdf"), after).unwrap();

    let review = review(&root).unwrap();
    assert_eq!(review.count(), 2);
    assert!(review.patch.contains("Readable café text"));
    assert!(review.patch.contains("fixture.pdf"));
    let patch = fs::read(root.join("review.patch")).unwrap();
    assert!(std::str::from_utf8(&patch).is_err());
    assert!(patch.windows(4).any(|part| part == b"\xe2\xe3\xcf\xd3"));
    assert_eq!(fs::read(root.join("work/fixture.pdf")).unwrap(), after);

    fs::write(root.join("work/fixture.pdf"), b"changed after review").unwrap();
    assert!(review.apply(&project, &root).is_err());
    assert_eq!(fs::read(project.join("fixture.pdf")).unwrap(), before);
    assert_eq!(fs::read(project.join("a.txt")).unwrap(), b"original");
    fs::write(root.join("work/fixture.pdf"), after).unwrap();
    review.apply(&project, &root).unwrap();
    assert_eq!(fs::read(project.join("fixture.pdf")).unwrap(), after);
    assert_eq!(
        fs::read(project.join("a.txt")).unwrap(),
        "Readable café text\n".as_bytes()
    );

    let replay = data.0.join("replay");
    fs::create_dir(&replay).unwrap();
    fs::write(replay.join("fixture.pdf"), before).unwrap();
    fs::write(replay.join("a.txt"), b"original").unwrap();
    checked(
        &trusted_git()
            .arg("-C")
            .arg(&replay)
            .args(["apply", "--binary"])
            .arg(root.join("review.patch"))
            .output()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(fs::read(replay.join("fixture.pdf")).unwrap(), after);
    assert_eq!(
        fs::read(replay.join("a.txt")).unwrap(),
        "Readable café text\n".as_bytes()
    );
}

#[test]
#[ignore = "replays source copies from SPROWT_REVIEW_REPLAY_DIR without changing the originals"]
fn saved_binary_review_exports_exact_source() {
    let saved = PathBuf::from(
        std::env::var_os("SPROWT_REVIEW_REPLAY_DIR").expect("SPROWT_REVIEW_REPLAY_DIR"),
    );
    let before = source_state(&saved.join("before")).unwrap();
    let after = source_state(&saved.join("work")).unwrap();
    let data = TestData::new();
    fs::create_dir(&data.0).unwrap();
    let root = data.0.join("review");
    create_snapshot(&before, &root).unwrap();
    replace_source(&root, &after).unwrap();
    let review = review(&root).unwrap();
    assert_eq!(review.fingerprint, fingerprint(&after).unwrap());
    let patch = fs::read(root.join("review.patch")).unwrap();
    let invalid = std::str::from_utf8(&patch).expect_err("non-UTF-8 regression fixture");

    let destination = data.0.join("destination");
    create_snapshot(&before, &destination).unwrap();
    review.export(&destination.join("work"), &root).unwrap();
    assert_eq!(source_state(&destination.join("work")).unwrap(), after);
    println!(
        "Reviewed and exported {} changed files byte for byte; patch has non-UTF-8 at {}; source {}",
        review.count(),
        invalid.valid_up_to(),
        review.fingerprint
    );
}
