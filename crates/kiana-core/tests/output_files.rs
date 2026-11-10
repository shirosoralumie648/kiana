//! `utils/output-files.ts` 的测试（TS 无对应测试文件；覆盖全部公开 API）。

use std::io::Read as _;

use kiana_core::output_files::{create_output_file_stream, write_output_file};
use regex::Regex;

fn file_name_pattern(prefix: &str, extension: &str) -> Regex {
    Regex::new(&format!(r"^{prefix}-[0-9a-f]{{16}}{extension}$")).expect("valid name regex")
}

#[test]
fn write_output_file_creates_a_private_new_file_with_expected_content() {
    let path = write_output_file("kiana-test-output", ".log", b"hello output\n")
        .expect("write output file");
    assert!(path.is_file(), "created file exists: {}", path.display());
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .expect("utf-8 file name");
    assert!(
        file_name_pattern("kiana-test-output", ".log").is_match(name),
        "name: {name}"
    );

    let mut content = Vec::new();
    std::fs::File::open(&path)
        .expect("open created file")
        .read_to_end(&mut content)
        .expect("read created file");
    assert_eq!(content, b"hello output\n");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "output files must be user-only (0o600)");
    }

    std::fs::remove_file(&path).expect("cleanup");
}

#[test]
fn write_output_file_never_reuses_a_path() {
    let first = write_output_file("kiana-test-unique", ".txt", b"a").expect("first file");
    let second = write_output_file("kiana-test-unique", ".txt", b"b").expect("second file");
    assert_ne!(first, second);
    std::fs::remove_file(&first).expect("cleanup first");
    std::fs::remove_file(&second).expect("cleanup second");
}

#[tokio::test]
async fn create_output_file_stream_opens_a_private_new_file() {
    use tokio::io::AsyncWriteExt as _;

    let (path, mut file) =
        create_output_file_stream("kiana-test-stream", ".log").expect("create stream");
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .expect("utf-8 file name");
    assert!(
        file_name_pattern("kiana-test-stream", ".log").is_match(name),
        "name: {name}"
    );

    file.write_all(b"streamed output").await.expect("write");
    file.flush().await.expect("flush");
    drop(file);

    let content = std::fs::read(&path).expect("read streamed file");
    assert_eq!(content, b"streamed output");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    std::fs::remove_file(&path).expect("cleanup");
}

#[test]
fn extension_is_appended_verbatim() {
    let path = write_output_file("kiana-test-ext", ".json", b"{}").expect("write");
    assert!(
        path.file_name()
            .and_then(|name| name.to_str())
            .expect("utf-8")
            .ends_with(".json")
    );
    std::fs::remove_file(&path).expect("cleanup");
}
