//! 线协议与文件操作（docs/protocol.md）：hello 形状、未知/畸形操作、每个文件操作、句柄上限、帧长度限制。

mod common;

use common::{Daemon, ERROR, REQUEST, RESULT, TOKEN, TempDir, assert_error, error, hello};
use serde_json::json;
use std::time::Duration;

#[test]
fn handshake_then_hello_reports_the_machine() {
    let mut daemon = Daemon::start(&[]);
    assert_eq!(daemon.handshake(), format!("KIANA-ENV {TOKEN}\n"));
    let info = hello(&mut daemon);
    assert_eq!(info["protocol"], json!(1));
    assert_eq!(info["version"], json!(env!("KIANA_ENV_VERSION")));
    assert_eq!(info["os"], json!(std::env::consts::OS));
    assert_eq!(info["arch"], json!(std::env::consts::ARCH));
    assert_eq!(
        info["separator"],
        json!(std::path::MAIN_SEPARATOR.to_string())
    );
    assert!(info["home"].is_string());
    assert!(info["tmpdir"].is_string());
    assert!(info["cwd"].is_string());
    assert!(info["driveCwds"].is_object());
    assert!(info["pid"].as_u64().unwrap() > 0);
    assert_eq!(info["features"], json!(["listFiles"]));
}

#[test]
fn rejects_unknown_and_malformed_operations() {
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    assert_error(
        &daemon.call_error(1, "info", json!({})),
        "EINVAL",
        "unknown operation info",
    );
    assert_error(
        &daemon.call_error(2, "bogus", json!({})),
        "EINVAL",
        "unknown operation bogus",
    );
    // A request without `op` names the empty operation.
    daemon.send_json(REQUEST, 3, &json!({ "path": "/" }));
    assert_error(&error(daemon.reply(3)), "EINVAL", "unknown operation ");
    // JSON that does not parse arrives as null and gets the invalid-JSON error.
    let body = b"not json";
    let mut raw = Vec::new();
    raw.extend_from_slice(&((9 + body.len()) as u32).to_be_bytes());
    raw.push(REQUEST);
    raw.extend_from_slice(&4u32.to_be_bytes());
    raw.extend_from_slice(&(body.len() as u32).to_be_bytes());
    raw.extend_from_slice(body);
    daemon.send_raw(&raw);
    assert_error(&error(daemon.reply(4)), "EINVAL", "invalid JSON");
}

#[test]
fn a_frame_with_no_json_is_the_same_as_an_empty_object() {
    // jsonLength 0 carries Value::Object(empty); with no op it fails as the empty operation, not as invalid JSON.
    let mut raw = Vec::new();
    raw.extend_from_slice(&9u32.to_be_bytes());
    raw.push(REQUEST);
    raw.extend_from_slice(&7u32.to_be_bytes());
    raw.extend_from_slice(&0u32.to_be_bytes());
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    daemon.send_raw(&raw);
    assert_error(&error(daemon.reply(7)), "EINVAL", "unknown operation ");
}

#[test]
fn lstat_reports_info_and_missing_paths() {
    let dir = TempDir::new("lstat");
    dir.write("file.txt", b"hello");
    dir.mkdir("sub");
    #[cfg(unix)]
    std::os::unix::fs::symlink("file.txt", dir.path().join("link")).unwrap();
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();

    let info = daemon.call(1, "lstat", json!({ "path": dir.join("file.txt") }));
    assert_eq!(info["name"], json!("file.txt"));
    assert_eq!(info["kind"], json!("file"));
    assert_eq!(info["size"], json!(5));
    assert!(info["mtimeSec"].as_i64().unwrap() > 0);
    assert!(info["mtimeNsec"].is_i64());
    assert!(info["dev"].is_u64());
    assert!(info["ino"].is_u64());

    let info = daemon.call(2, "lstat", json!({ "path": dir.join("sub") }));
    assert_eq!(info["kind"], json!("directory"));

    #[cfg(unix)]
    {
        let info = daemon.call(3, "lstat", json!({ "path": dir.join("link") }));
        assert_eq!(info["kind"], json!("symlink"));
    }

    let missing = dir.join("missing");
    assert_error(
        &daemon.call_error(4, "lstat", json!({ "path": missing })),
        "ENOENT",
        &format!("ENOENT: no such file or directory, lstat '{missing}'"),
    );
    assert_error(
        &daemon.call_error(5, "lstat", json!({})),
        "EINVAL",
        "missing field path",
    );
}

#[test]
fn realpath_resolves_links() {
    let dir = TempDir::new("realpath");
    dir.write("file.txt", b"");
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    let result = daemon.call(1, "realpath", json!({ "path": dir.join("file.txt") }));
    let resolved = std::fs::canonicalize(dir.join("file.txt")).unwrap();
    assert_eq!(result["path"], json!(resolved.to_string_lossy()));
    let missing = dir.join("missing");
    let failure = daemon.call_error(2, "realpath", json!({ "path": missing }));
    assert_eq!(failure["code"], json!("ENOENT"));
    assert_eq!(failure["syscall"], json!("realpath"));
}

#[test]
fn write_creates_parents_and_can_keep_a_handle() {
    let dir = TempDir::new("write");
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    let path = dir.join("a/b/c.txt");
    daemon.call_payload(1, "write", json!({ "path": path }), b"one");
    assert_eq!(std::fs::read(&path).unwrap(), b"one");
    daemon.call_payload(2, "write", json!({ "path": path, "append": true }), b"two");
    assert_eq!(std::fs::read(&path).unwrap(), b"onetwo");
    daemon.call_payload(3, "write", json!({ "path": path }), b"x");
    assert_eq!(std::fs::read(&path).unwrap(), b"x");
    // `keep` returns the open handle for `writeChunk`.
    let kept = daemon.call_payload(4, "write", json!({ "path": path, "keep": true }), b"1");
    let handle = kept["handle"].as_u64().expect("handle");
    daemon.call_payload(5, "writeChunk", json!({ "handle": handle }), b"2");
    daemon.call_payload(6, "writeChunk", json!({ "handle": handle }), b"3");
    assert_eq!(std::fs::read(&path).unwrap(), b"123");
    daemon.call(7, "close", json!({ "handle": handle }));
    // Without `parents`, a missing parent fails.
    let missing_parent = dir.join("nope/x.txt");
    let failure = daemon.call_error(
        8,
        "write",
        json!({ "path": missing_parent, "parents": false }),
    );
    assert_eq!(failure["code"], json!("ENOENT"));
    assert_eq!(failure["syscall"], json!("open"));
}

#[cfg(target_os = "linux")]
#[test]
fn a_failed_chunk_write_poisons_the_handle() {
    // `/dev/full` accepts the open and fails every write with ENOSPC; later chunks then fail with EBADF.
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    let kept = daemon.call_payload(
        1,
        "write",
        json!({ "path": "/dev/full", "keep": true }),
        b"",
    );
    let handle = kept["handle"].as_u64().expect("handle");
    assert_error(
        &daemon.call_payload_error(2, "writeChunk", json!({ "handle": handle }), b"x"),
        "ENOSPC",
        "ENOSPC: no space left on device, write '/dev/full'",
    );
    assert_error(
        &daemon.call_payload_error(3, "writeChunk", json!({ "handle": handle }), b"y"),
        "EBADF",
        "an earlier write failed",
    );
}

#[test]
fn truncate_fsync_and_rename_behave_like_node() {
    let dir = TempDir::new("misc");
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    let path = dir.join("a.txt");
    daemon.call_payload(1, "write", json!({ "path": path }), b"abcdef");
    daemon.call(2, "truncate", json!({ "path": path, "size": 3 }));
    assert_eq!(std::fs::read(&path).unwrap(), b"abc");
    assert_error(
        &daemon.call_error(
            3,
            "truncate",
            json!({ "path": dir.join("missing"), "size": 0 }),
        ),
        "ENOENT",
        &format!(
            "ENOENT: no such file or directory, open '{}'",
            dir.join("missing")
        ),
    );
    assert_error(
        &daemon.call_error(4, "truncate", json!({ "path": path })),
        "EINVAL",
        "missing or invalid field size",
    );
    daemon.call(5, "fsync", json!({ "path": path }));
    assert_error(
        &daemon.call_error(6, "fsync", json!({ "path": dir.str() })),
        "EISDIR",
        &format!(
            "EISDIR: illegal operation on a directory, open '{}'",
            dir.str()
        ),
    );
    let to = dir.join("b.txt");
    daemon.call(7, "rename", json!({ "path": path, "to": to }));
    assert_eq!(std::fs::read(&to).unwrap(), b"abc");
    let from = dir.join("missing");
    let failure = daemon.call_error(8, "rename", json!({ "path": from, "to": to }));
    assert_eq!(failure["code"], json!("ENOENT"));
    assert_eq!(failure["syscall"], json!("rename"));
    assert_eq!(
        failure["message"],
        json!(format!(
            "ENOENT: no such file or directory, rename '{from}' -> '{to}'"
        ))
    );
}

#[test]
fn mkdir_recursive_and_rm_follow_node() {
    let dir = TempDir::new("mkrm");
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    let made = dir.join("made");
    daemon.call(1, "mkdir", json!({ "path": made }));
    assert_error(
        &daemon.call_error(2, "mkdir", json!({ "path": made })),
        "EEXIST",
        &format!("EEXIST: file already exists, mkdir '{made}'"),
    );
    let deep = dir.join("x/y/z");
    daemon.call(3, "mkdir", json!({ "path": deep, "recursive": true }));
    assert!(std::path::Path::new(&deep).is_dir());
    dir.write("file", b"");
    let through_file = dir.join("file/x");
    assert_error(
        &daemon.call_error(
            4,
            "mkdir",
            json!({ "path": through_file, "recursive": true }),
        ),
        "ENOTDIR",
        &format!("ENOTDIR: not a directory, mkdir '{through_file}'"),
    );
    dir.mkdir("tree/inner");
    let tree = dir.join("tree");
    let failure = daemon.call_error(5, "rm", json!({ "path": tree }));
    assert_eq!(failure["code"], json!("ERR_FS_EISDIR"));
    assert_eq!(
        failure["message"],
        json!(format!(
            "Path is a directory: rm returned EISDIR (is a directory) {tree}"
        ))
    );
    daemon.call(6, "rm", json!({ "path": tree, "recursive": true }));
    assert!(!std::path::Path::new(&tree).exists());
    let missing = dir.join("missing");
    assert_error(
        &daemon.call_error(7, "rm", json!({ "path": missing })),
        "ENOENT",
        &format!("ENOENT: no such file or directory, lstat '{missing}'"),
    );
    daemon.call(8, "rm", json!({ "path": missing, "force": true }));
}

#[test]
fn mkdtemp_creates_a_random_directory() {
    let dir = TempDir::new("mkdtemp");
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    let prefix = dir.join("pre-");
    let result = daemon.call(1, "mkdtemp", json!({ "path": prefix }));
    let created = result["path"].as_str().expect("path").to_string();
    assert!(created.starts_with(&prefix));
    assert_eq!(created.len(), prefix.len() + 6);
    assert!(std::path::Path::new(&created).is_dir());
    let missing = dir.join("missing/pre-");
    let failure = daemon.call_error(2, "mkdtemp", json!({ "path": missing }));
    assert_eq!(failure["code"], json!("ENOENT"));
    assert_eq!(failure["path"], json!(format!("{missing}XXXXXX")));
    assert_eq!(failure["syscall"], json!("mkdtemp"));
}

#[test]
fn open_reads_regular_files_and_refuses_others() {
    let dir = TempDir::new("open");
    dir.write("a.txt", b"hello");
    dir.mkdir("sub");
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    let opened = daemon.call(1, "open", json!({ "path": dir.join("a.txt") }));
    let handle = opened["handle"].as_u64().unwrap();
    assert_eq!(opened["info"]["kind"], json!("file"));
    assert_eq!(opened["info"]["size"], json!(5));
    let fstat = daemon.call(2, "fstat", json!({ "handle": handle }));
    assert_eq!(fstat["kind"], json!("file"));
    daemon.request(
        3,
        "pread",
        json!({ "handle": handle, "offset": 1, "length": 3 }),
    );
    let frame = daemon.reply(3);
    assert_eq!(frame.payload, b"ell");
    // Without `offset`, one read at the current position.
    daemon.request(4, "pread", json!({ "handle": handle, "length": 10 }));
    let frame = daemon.reply(4);
    assert_eq!(frame.payload, b"hello");
    daemon.call(5, "close", json!({ "handle": handle }));
    assert_error(
        &daemon.call_error(6, "fstat", json!({ "handle": handle })),
        "EBADF",
        "unknown handle",
    );
    assert_error(
        &daemon.call_error(7, "open", json!({ "path": dir.join("sub") })),
        "EISDIR",
        "EISDIR: illegal operation on a directory, read",
    );
    let missing = dir.join("missing");
    assert_error(
        &daemon.call_error(8, "open", json!({ "path": missing })),
        "ENOENT",
        &format!("ENOENT: no such file or directory, open '{missing}'"),
    );
}

#[cfg(unix)]
#[test]
fn open_refuses_a_final_symbolic_link_with_no_follow() {
    let dir = TempDir::new("nofollow");
    dir.write("a.txt", b"hello");
    std::os::unix::fs::symlink(dir.join("a.txt"), dir.path().join("link")).unwrap();
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    let failure = daemon.call_error(
        1,
        "open",
        json!({ "path": dir.join("link"), "noFollow": true }),
    );
    assert_error(&failure, "SYMLINK", "Refusing to follow a symbolic link");
    assert_eq!(failure["path"], json!(dir.join("link")));
    // Following is the default.
    let opened = daemon.call(2, "open", json!({ "path": dir.join("link") }));
    assert_eq!(opened["info"]["size"], json!(5));
}

#[cfg(unix)]
#[test]
fn open_in_read_mode_opens_any_file_like_node() {
    let dir = TempDir::new("readmode");
    dir.mkdir("sub");
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    // Node's `open(path, "r")` opens a directory too; reading it later fails.
    let opened = daemon.call(
        1,
        "open",
        json!({ "path": dir.join("sub"), "mode": "read" }),
    );
    assert_eq!(opened["stat"]["kind"], json!("directory"));
    assert!(opened["handle"].as_u64().is_some());
}

#[test]
fn opendir_and_readdir_page_entries() {
    let dir = TempDir::new("readdir");
    dir.write("b.txt", b"");
    dir.write("a.txt", b"");
    dir.mkdir("sub");
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    let opened = daemon.call(1, "opendir", json!({ "path": dir.str() }));
    let handle = opened["handle"].as_u64().unwrap();
    let first = daemon.call(2, "readdir", json!({ "handle": handle, "max": 1 }));
    assert_eq!(first["entries"].as_array().unwrap().len(), 1);
    assert_eq!(first["done"], json!(false));
    let mut names = vec![first["entries"][0]["name"].as_str().unwrap().to_string()];
    loop {
        let page = daemon.call(3, "readdir", json!({ "handle": handle, "max": 10 }));
        for entry in page["entries"].as_array().unwrap() {
            names.push(entry["name"].as_str().unwrap().to_string());
        }
        if page["done"] == json!(true) {
            break;
        }
    }
    names.sort();
    assert_eq!(names, ["a.txt", "b.txt", "sub"]);
    // `max` is clamped to at least one.
    let page = daemon.call(4, "readdir", json!({ "handle": handle, "max": 0 }));
    assert_eq!(page["entries"], json!([]));
    assert_eq!(page["done"], json!(true));
    assert_error(
        &daemon.call_error(5, "readdir", json!({ "handle": 999, "max": 1 })),
        "EBADF",
        "not a directory handle",
    );
    assert_error(
        &daemon.call_error(6, "opendir", json!({ "path": dir.join("a.txt") })),
        "ENOTDIR",
        &format!("ENOTDIR: not a directory, opendir '{}'", dir.join("a.txt")),
    );
    let missing = dir.join("missing");
    let failure = daemon.call_error(7, "opendir", json!({ "path": missing }));
    assert_eq!(failure["code"], json!("ENOENT"));
    assert_eq!(failure["syscall"], json!("opendir"));
}

#[cfg(unix)]
#[test]
fn readdir_reports_names_that_are_not_utf8_like_node() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let dir = TempDir::new("rawnames");
    // A name with a byte that is not valid UTF-8, and one that is.
    let bad = dir.path().join(OsStr::from_bytes(b"bad\xff"));
    std::fs::write(&bad, b"").unwrap();
    dir.write("good", b"");
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    let opened = daemon.call(1, "opendir", json!({ "path": dir.str() }));
    let handle = opened["handle"].as_u64().unwrap();
    let page = daemon.call(2, "readdir", json!({ "handle": handle, "max": 10 }));
    let entries = page["entries"].as_array().unwrap();
    let broken = entries
        .iter()
        .find(|entry| entry["name"] == json!("bad\u{fffd}"))
        .expect("entry with a replacement character");
    // lstat runs under the decoded name, so the entry reports ENOENT as Node does.
    assert_eq!(broken["error"]["code"], json!("ENOENT"));
    assert_eq!(broken["raw"], json!([98, 97, 100, 255]));
}

#[test]
fn scan_lines_measures_the_selection() {
    let dir = TempDir::new("scanlines");
    let content: &[u8] = b"one\ntwo\nthree\n";
    dir.write("a.txt", content);
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    let opened = daemon.call(1, "open", json!({ "path": dir.join("a.txt") }));
    let handle = opened["handle"].as_u64().unwrap();
    let all = daemon.call(2, "scanLines", json!({ "handle": handle, "startLine": 0 }));
    assert_eq!(all["newlines"], json!(3));
    assert_eq!(all["start"], json!(0));
    assert_eq!(all["end"], json!(14));
    assert_eq!(all["selectedBytes"], json!(14));
    assert_eq!(all["firstLineEnd"], json!(3));
    assert_eq!(all["firstLineBytes"], json!(3));
    let middle = daemon.call(
        3,
        "scanLines",
        json!({ "handle": handle, "startLine": 1, "endLine": 2 }),
    );
    assert_eq!(middle["start"], json!(4));
    assert_eq!(middle["end"], json!(7));
    assert_eq!(middle["selectedBytes"], json!(3));
    assert_eq!(middle["lastLineStart"], json!(4));
    let past = daemon.call(4, "scanLines", json!({ "handle": handle, "startLine": 9 }));
    assert_eq!(past["start"], json!(14));
    assert_eq!(past["end"], json!(14));
    assert_eq!(past["selectedBytes"], json!(0));
    assert_error(
        &daemon.call_error(
            5,
            "scanLines",
            json!({ "handle": handle, "startLine": 2, "endLine": 2 }),
        ),
        "EINVAL",
        "Invalid line range",
    );
    // `endLine` that is not a u64 counts as absent.
    let open_ended = daemon.call(
        6,
        "scanLines",
        json!({ "handle": handle, "startLine": 2, "endLine": "x" }),
    );
    assert_eq!(open_ended["end"], json!(14));
}

#[test]
fn list_files_lists_the_tree() {
    let dir = TempDir::new("listfiles");
    dir.write("a.txt", b"");
    dir.write("sub/b.txt", b"");
    dir.mkdir("empty");
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    daemon.request(1, "listFiles", json!({ "path": dir.str() }));
    let frame = daemon.reply(1);
    assert_eq!(frame.json["count"], json!(4));
    assert_eq!(frame.json["truncated"], json!(false));
    let records = frame.payload.split(|byte| *byte == 0);
    let mut entries = Vec::new();
    for record in records {
        if record.is_empty() {
            break;
        }
        entries.push(format!(
            "{} {}",
            record[0] as char,
            String::from_utf8_lossy(&record[1..])
        ));
    }
    assert_eq!(entries, ["f a.txt", "d empty", "d sub", "f sub/b.txt"]);
    // The path is required, unlike the other options.
    assert_error(
        &daemon.call_error(2, "listFiles", json!({})),
        "EINVAL",
        "missing field path",
    );
    let missing = dir.join("missing");
    assert_error(
        &daemon.call_error(3, "listFiles", json!({ "path": missing })),
        "ENOENT",
        &format!("ENOENT: no such file or directory, scandir '{missing}'"),
    );
    let file = dir.join("a.txt");
    let failure = daemon.call_error(4, "listFiles", json!({ "path": file }));
    assert_eq!(failure["code"], json!("ENOTDIR"));
    assert_eq!(
        failure["message"],
        json!(format!("ENOTDIR: not a directory, scandir '{file}'"))
    );
    assert_error(
        &daemon.call_error(
            5,
            "listFiles",
            json!({ "path": dir.str(), "maxEntries": "x" }),
        ),
        "EINVAL",
        "invalid field maxEntries",
    );
}

#[test]
fn close_of_an_unknown_handle_succeeds() {
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    assert_eq!(daemon.call(1, "close", json!({ "handle": 999 })), json!({}));
    assert_error(
        &daemon.call_error(2, "close", json!({})),
        "EINVAL",
        "missing or invalid field handle",
    );
}

#[test]
fn handles_of_the_kind_mismatch_report_ebadf() {
    let dir = TempDir::new("handles");
    dir.write("a.txt", b"x");
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    let opened = daemon.call(1, "opendir", json!({ "path": dir.str() }));
    let dir_handle = opened["handle"].as_u64().unwrap();
    assert_error(
        &daemon.call_error(2, "pread", json!({ "handle": dir_handle, "length": 1 })),
        "EBADF",
        "not a file handle",
    );
    let opened = daemon.call(3, "open", json!({ "path": dir.join("a.txt") }));
    let file_handle = opened["handle"].as_u64().unwrap();
    assert_error(
        &daemon.call_error(4, "readdir", json!({ "handle": file_handle, "max": 1 })),
        "EBADF",
        "not a directory handle",
    );
}

#[test]
fn refuses_more_than_4096_open_handles() {
    let dir = TempDir::new("emfile");
    dir.write("a.txt", b"x");
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    let path = dir.join("a.txt");
    for id in 0..4096u32 {
        let opened = daemon.call(id + 1, "open", json!({ "path": path }));
        assert!(opened["handle"].as_u64().is_some(), "handle {id}");
    }
    assert_error(
        &daemon.call_error(5000, "open", json!({ "path": path })),
        "EMFILE",
        "EMFILE: too many open files",
    );
}

#[test]
fn cancelling_a_scan_lines_request_stops_it() {
    let dir = TempDir::new("cancel-scan");
    let mut content = Vec::new();
    for index in 0..200_000 {
        content.extend_from_slice(format!("line {index}\n").as_bytes());
    }
    dir.write("big.txt", &content);
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    let opened = daemon.call(1, "open", json!({ "path": dir.join("big.txt") }));
    let handle = opened["handle"].as_u64().unwrap();
    daemon.request(2, "scanLines", json!({ "handle": handle, "startLine": 0 }));
    daemon.cancel(2, false);
    let failure = error(daemon.reply(2));
    assert_eq!(failure["code"], json!("aborted"));
}

#[test]
fn cancelling_a_list_files_request_stops_it() {
    let dir = TempDir::new("cancel-list");
    for index in 0..50 {
        dir.write(&format!("d{index}/f"), b"");
    }
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    daemon.request(1, "listFiles", json!({ "path": dir.str() }));
    daemon.cancel(1, false);
    let failure = error(daemon.reply(1));
    assert_eq!(failure["code"], json!("aborted"));
}

#[test]
fn frame_length_limits_end_the_connection() {
    // A length below the header size is a framing error; the daemon stops.
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    daemon.send_raw(&8u32.to_be_bytes());
    let status = daemon.wait(Duration::from_secs(10));
    assert_eq!(status.code(), Some(1));
    assert!(
        daemon.stderr_text().contains("frame length out of range"),
        "stderr reports the framing error"
    );
}

#[test]
fn json_length_beyond_the_frame_ends_the_connection() {
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    let mut raw = Vec::new();
    raw.extend_from_slice(&9u32.to_be_bytes());
    raw.push(REQUEST);
    raw.extend_from_slice(&1u32.to_be_bytes());
    raw.extend_from_slice(&5u32.to_be_bytes());
    daemon.send_raw(&raw);
    let status = daemon.wait(Duration::from_secs(10));
    assert_eq!(status.code(), Some(1));
    assert!(
        daemon.stderr_text().contains("JSON length out of range"),
        "stderr reports the JSON length error"
    );
}

#[test]
fn answers_requests_concurrently_and_in_any_order() {
    let dir = TempDir::new("concurrent");
    for index in 0..20 {
        dir.write(&format!("f{index}"), b"x");
    }
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    for id in 0..20u32 {
        daemon.request(
            id + 1,
            "lstat",
            json!({ "path": dir.join(&format!("f{id}")) }),
        );
    }
    let mut seen = Vec::new();
    while seen.len() < 20 {
        let frame = daemon.next_frame();
        assert_eq!(frame.kind, RESULT);
        seen.push(frame.id);
    }
    seen.sort();
    assert_eq!(seen, (1..=20).collect::<Vec<u32>>());
}

#[test]
fn a_result_with_a_large_payload_keeps_its_bytes() {
    let dir = TempDir::new("bigpread");
    let content: Vec<u8> = (0..1_000_000u32).map(|index| (index % 251) as u8).collect();
    dir.write("big.bin", &content);
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    let opened = daemon.call(1, "open", json!({ "path": dir.join("big.bin") }));
    let handle = opened["handle"].as_u64().unwrap();
    daemon.request(
        2,
        "pread",
        json!({ "handle": handle, "offset": 0, "length": 1_000_000 }),
    );
    let frame = daemon.reply(2);
    assert_eq!(frame.payload, content);
}

#[test]
fn ignores_unsolicited_cancels_and_other_frame_types() {
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    daemon.send_json(common::CANCEL, 42, &json!({}));
    daemon.send_json(RESULT, 43, &json!({}));
    daemon.send_json(ERROR, 44, &json!({}));
    daemon.send_json(common::PING, 45, &json!({}));
    daemon.send_json(common::EVENT, 46, &json!({}));
    // Still answering afterwards.
    assert_eq!(hello(&mut daemon)["protocol"], json!(1));
}

#[test]
fn exits_after_thirty_seconds_without_stdin_bytes() {
    // The daemon sends a ping every 5 s and exits with 0 after 30 s without receiving any bytes,
    // killing the process groups it started. Started processes are stopped before the check.
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    let started = std::time::Instant::now();
    let deadline = started + Duration::from_secs(60);
    let mut pings = 0;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        match daemon.recv(remaining) {
            Some(frame) if frame.kind == common::PING => pings += 1,
            Some(_) => {}
            None => break,
        }
    }
    assert_eq!(daemon.wait(Duration::from_secs(10)).code(), Some(0));
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_secs(29),
        "exited after {elapsed:?}"
    );
    // Generous upper bound: the ping thread sleeps, so a loaded machine can overshoot 30 s.
    assert!(
        elapsed < Duration::from_secs(45),
        "exited after {elapsed:?}"
    );
    assert!(pings >= 5, "pings {pings}");
}

#[test]
fn a_ping_frame_is_sent_every_five_seconds() {
    let mut daemon = Daemon::start(&[]);
    daemon.handshake();
    let started = std::time::Instant::now();
    match daemon.recv(Duration::from_secs(10)) {
        Some(frame) => {
            assert_eq!(frame.kind, common::PING);
            assert_eq!(frame.id, 0);
            assert_eq!(frame.json, json!({}));
            assert!(started.elapsed() >= Duration::from_secs(4));
        }
        None => panic!("no ping within ten seconds"),
    }
}
