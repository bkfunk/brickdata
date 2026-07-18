use std::io::Write;

use brickdata::extract::{ExtractError, gunzip_file, gunzip_to_vec, unzip_to};

fn write_gz(path: &std::path::Path, content: &[u8]) {
    let file = std::fs::File::create(path).unwrap();
    let mut encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    encoder.write_all(content).unwrap();
    encoder.finish().unwrap();
}

fn write_zip(path: &std::path::Path, entries: &[(&str, &[u8])]) {
    let file = std::fs::File::create(path).unwrap();
    let mut writer = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default();
    for (name, content) in entries {
        writer.start_file(*name, options).unwrap();
        writer.write_all(content).unwrap();
    }
    writer.finish().unwrap();
}

#[test]
fn gunzip_roundtrips() {
    let dir = tempfile::tempdir().unwrap();
    let gz = dir.path().join("table.csv.gz");
    write_gz(&gz, b"id,name\n3001,Brick 2 x 4\n");

    assert_eq!(gunzip_to_vec(&gz).unwrap(), b"id,name\n3001,Brick 2 x 4\n");

    let out = dir.path().join("table.csv");
    gunzip_file(&gz, &out).unwrap();
    assert_eq!(std::fs::read(&out).unwrap(), b"id,name\n3001,Brick 2 x 4\n");
}

#[test]
fn gunzip_of_non_gzip_errors() {
    let dir = tempfile::tempdir().unwrap();
    let fake = dir.path().join("not.gz");
    std::fs::write(&fake, b"plain text, no gzip magic").unwrap();
    assert!(matches!(gunzip_to_vec(&fake), Err(ExtractError::Io { .. })));
}

#[test]
fn unzip_extracts_nested_tree() {
    let dir = tempfile::tempdir().unwrap();
    let zip_path = dir.path().join("tree.zip");
    write_zip(
        &zip_path,
        &[
            ("parts/3001.dat", b"0 Brick 2 x 4".as_slice()),
            ("p/stud.dat", b"0 Stud".as_slice()),
            ("LICENSE.txt", b"CC BY".as_slice()),
        ],
    );

    let dest = dir.path().join("tree");
    unzip_to(&zip_path, &dest).unwrap();
    assert_eq!(
        std::fs::read(dest.join("parts/3001.dat")).unwrap(),
        b"0 Brick 2 x 4"
    );
    assert_eq!(std::fs::read(dest.join("p/stud.dat")).unwrap(), b"0 Stud");
    assert_eq!(std::fs::read(dest.join("LICENSE.txt")).unwrap(), b"CC BY");
}

#[test]
fn unzip_rejects_path_traversal() {
    let dir = tempfile::tempdir().unwrap();
    let zip_path = dir.path().join("evil.zip");
    write_zip(&zip_path, &[("../evil.txt", b"escaped".as_slice())]);

    let dest = dir.path().join("safe");
    let err = unzip_to(&zip_path, &dest).unwrap_err();
    assert!(matches!(err, ExtractError::UnsafeZipPath { .. }));
    assert!(!dir.path().join("evil.txt").exists());
}

#[test]
fn unzip_of_garbage_errors() {
    let dir = tempfile::tempdir().unwrap();
    let fake = dir.path().join("not.zip");
    std::fs::write(&fake, b"definitely not a zip").unwrap();
    assert!(matches!(
        unzip_to(&fake, dir.path().join("out")),
        Err(ExtractError::Zip { .. })
    ));
}
