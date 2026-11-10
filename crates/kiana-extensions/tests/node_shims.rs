//! Node 内建 shim（设计 §6.1、§6.3、§7.3）：path/fs/child_process/os/url/util/
//! zlib/readline/module/events/net/http，以及 §10.4 的四个 npm 回归。

mod support;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use kiana_extensions::engine::EngineConfig;
use kiana_extensions::loader::js_host;
use kiana_extensions::loader::loader_impl::{ExtensionLoader, LoaderConfig};
use kiana_extensions::loader::resolver::{
    ModuleResolver, ResolutionKind, ResolverOptions, VirtualFs,
};
use rquickjs::{Array, Function};
use support::{Harness, run_local};

/// 建好运行时后装载真 CJS 加载器，并把 Node 内建模块挂成测试脚本的全局名。
///
/// 生产的扩展代码通过 `require`/`import` 拿到它们；测试里直接别名化，
/// 断言失败时脚本可读性更好。
fn node_harness() -> Harness {
    let harness = Harness::new();
    js_host::install_intl(&harness.engine).expect("install intl");
    js_host::install(&harness.engine).expect("install node globals");
    harness
        .engine
        .eval_script::<()>(include_str!("../js/cjs.js"), "<cjs>")
        .expect("install cjs loader");
    harness.eval::<()>(
        r#"
        globalThis.require = __kiana_make_require("/tmp/kiana-node-shims-test.js", "/tmp");
        globalThis.path = require("node:path");
        globalThis.fs = require("node:fs");
        globalThis.os = require("node:os");
        globalThis.url = require("node:url");
        globalThis.util = require("node:util");
        globalThis.zlib = require("node:zlib");
        globalThis.module = require("node:module");
        globalThis.events = require("node:events");
        globalThis.stream = require("node:stream");
        globalThis.readline = require("node:readline");
        globalThis.child_process = require("node:child_process");
        globalThis.net = require("node:net");
        globalThis.http = require("node:http");
        globalThis.https = require("node:https");
        "#,
    );
    harness
}

/// JavaScript 字符串字面量（路径可能含反斜杠/引号）。
fn js_string(value: &str) -> String {
    serde_json::to_string(value).expect("json string")
}

fn eval_json(harness: &Harness, script: &str) -> serde_json::Value {
    match harness.try_eval::<String>(script) {
        Ok(text) => serde_json::from_str(&text).expect("json"),
        Err(error) => panic!(
            "eval failed: {}: {}\n{}",
            error.name,
            error.message,
            error.stack.unwrap_or_default()
        ),
    }
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/node")
}

static UNIQUE: AtomicU64 = AtomicU64::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let unique = UNIQUE.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "kiana-node-shims-{}-{tag}-{unique}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

// ---------------------------------------------------------------------------
// path
// ---------------------------------------------------------------------------

#[test]
fn path_posix_semantics_match_node() {
    let harness = node_harness();
    assert_eq!(
        harness.eval::<String>("path.join('/foo', 'bar/', 'baz')"),
        "/foo/bar/baz"
    );
    assert_eq!(
        harness.eval::<String>("path.join('foo', '', 'bar')"),
        "foo/bar"
    );
    assert_eq!(harness.eval::<String>("path.join('/')"), "/");
    assert_eq!(
        harness.eval::<String>("path.normalize('foo//bar/../baz/')"),
        "foo/baz/"
    );
    assert_eq!(harness.eval::<String>("path.dirname('/foo/bar')"), "/foo");
    assert_eq!(harness.eval::<String>("path.dirname('foo')"), ".");
    assert_eq!(harness.eval::<String>("path.dirname('/')"), "/");
    assert_eq!(
        harness.eval::<String>("path.basename('/foo/bar.txt', '.txt')"),
        "bar"
    );
    assert_eq!(
        harness.eval::<String>("path.extname('/foo/bar.tar.gz')"),
        ".gz"
    );
    assert_eq!(harness.eval::<String>("path.extname('/foo/.hidden')"), "");
    assert!(harness.eval::<bool>("path.isAbsolute('/foo') && !path.isAbsolute('foo')"));
    assert_eq!(
        harness.eval::<String>("path.relative('/a/b/c', '/a/d')"),
        "../../d"
    );
    assert_eq!(harness.eval::<String>("path.relative('/a', '/a')"), "");
    assert_eq!(harness.eval::<String>("path.sep"), "/");
    assert_eq!(
        harness.eval::<String>("path.resolve('..')"),
        Path::new("..")
            .canonicalize()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default()
    );
    assert_eq!(
        harness.eval::<String>("JSON.stringify(path.parse('/a/b.txt'))"),
        r#"{"root":"/","dir":"/a","base":"b.txt","ext":".txt","name":"b"}"#
    );
    assert_eq!(
        harness.eval::<String>("path.format({dir: '/a', name: 'b', ext: '.txt'})"),
        "/a/b.txt"
    );
    // path.posix / path.win32 始终可用（默认导出按平台选择）。
    assert_eq!(harness.eval::<String>("path.posix.join('/x', 'y')"), "/x/y");
    assert_eq!(
        harness.eval::<String>("path.win32.join('C:\\\\x', 'y')"),
        "C:\\x\\y"
    );
}

#[test]
fn path_win32_handles_drives_and_unc() {
    let harness = node_harness();
    assert!(harness.eval::<bool>("path.win32.isAbsolute('C:\\\\x')"));
    assert!(harness.eval::<bool>("path.win32.isAbsolute('\\\\\\\\server\\\\share\\\\x')"));
    assert_eq!(
        harness.eval::<String>("path.win32.dirname('C:\\\\a\\\\b')"),
        "C:\\a"
    );
    assert_eq!(
        harness.eval::<String>("path.win32.basename('C:\\\\a\\\\b.txt', '.txt')"),
        "b"
    );
    assert_eq!(
        harness.eval::<String>("path.win32.normalize('C:/a//b/../c')"),
        "C:\\a\\c"
    );
    assert_eq!(
        harness.eval::<String>("path.win32.relative('C:\\\\a\\\\b', 'C:\\\\a\\\\c')"),
        "..\\c"
    );
    assert_eq!(harness.eval::<String>("path.win32.sep"), "\\");
}

// ---------------------------------------------------------------------------
// fs（同步）
// ---------------------------------------------------------------------------

#[test]
fn fs_sync_roundtrip_and_errors() {
    let dir = temp_dir("fs");
    let harness = node_harness();
    let file = dir.join("hello.txt");
    let missing = dir.join("missing.txt");
    let script = format!(
        r#"
        (function () {{
          fs.writeFileSync({file}, "hello");
          fs.appendFileSync({file}, " world");
          var read = fs.readFileSync({file}, "utf8");
          var bytes = fs.readFileSync({file});
          fs.writeFileSync({file}, Buffer.from([0xff, 0xfe]));
          var binary = Array.prototype.slice.call(fs.readFileSync({file}));
          fs.writeFileSync({file}, "hello");
          var stat = fs.statSync({file});
          var directory = fs.statSync({dir});
          var missingError = null;
          try {{ fs.readFileSync({missing}); }} catch (error) {{ missingError = [error.code, error.errno, error.syscall, error.path]; }}
          return JSON.stringify({{
            read: read,
            isBuffer: Buffer.isBuffer(bytes),
            binary: binary,
            isFile: stat.isFile(),
            fileSize: stat.size,
            isDir: directory.isDirectory(),
            exists: fs.existsSync({file}),
            missingExists: fs.existsSync({missing}),
            missingError: missingError,
          }});
        }})()
        "#,
        file = js_string(&file.to_string_lossy()),
        dir = js_string(&dir.to_string_lossy()),
        missing = js_string(&missing.to_string_lossy()),
    );
    let value = eval_json(&harness, &script);
    assert_eq!(value["read"], "hello world");
    assert_eq!(value["isBuffer"], true);
    assert_eq!(value["binary"], serde_json::json!([255, 254]));
    assert_eq!(value["isFile"], true);
    assert_eq!(value["fileSize"], 5);
    assert_eq!(value["isDir"], true);
    assert_eq!(value["exists"], true);
    assert_eq!(value["missingExists"], false);
    assert_eq!(
        value["missingError"],
        serde_json::json!(["ENOENT", -2, "open", missing.to_string_lossy()])
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fs_readdir_is_not_sorted_and_dirents_do_not_follow_symlinks() {
    let dir = temp_dir("readdir");
    for name in ["zebra", "alpha", "middle"] {
        std::fs::write(dir.join(name), b"x").expect("write");
    }
    std::fs::create_dir(dir.join("folder")).expect("mkdir");
    #[cfg(unix)]
    std::os::unix::fs::symlink(dir.join("folder"), dir.join("link")).expect("symlink");

    // Node does not sort readdir results: compare with std::fs::read_dir's order.
    let expected: Vec<String> = std::fs::read_dir(&dir)
        .expect("read_dir")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();

    let harness = node_harness();
    let script = format!(
        r#"
        (function () {{
          var names = fs.readdirSync({dir});
          var entries = fs.readdirSync({dir}, {{withFileTypes: true}});
          return JSON.stringify({{
            names: names,
            dirents: entries.map(function (entry) {{ return [entry.name, entry.isFile(), entry.isDirectory(), entry.isSymbolicLink()]; }})
          }});
        }})()
        "#,
        dir = js_string(&dir.to_string_lossy()),
    );
    let value = eval_json(&harness, &script);
    let names: Vec<String> = value["names"]
        .as_array()
        .expect("names")
        .iter()
        .map(|value| value.as_str().expect("name").to_owned())
        .collect();
    assert_eq!(names, expected);
    let mut dirent_map = std::collections::HashMap::new();
    for entry in value["dirents"].as_array().expect("dirents") {
        let name = entry[0].as_str().expect("name").to_owned();
        let flags = (
            entry[1].as_bool().expect("isFile"),
            entry[2].as_bool().expect("isDirectory"),
            entry[3].as_bool().expect("isSymbolicLink"),
        );
        dirent_map.insert(name, flags);
    }
    assert_eq!(dirent_map["zebra"], (true, false, false));
    assert_eq!(dirent_map["folder"], (false, true, false));
    #[cfg(unix)]
    assert_eq!(dirent_map["link"], (false, false, true));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fs_mkdir_mkdtemp_unlink_and_rmdir() {
    let dir = temp_dir("mkdtemp");
    let harness = node_harness();
    let script = format!(
        r#"
        (function () {{
          var base = {dir};
          var nested = path.join(base, "a", "b");
          fs.mkdirSync(nested, {{recursive: true}});
          var temporary = fs.mkdtempSync(path.join(base, "tmp-"));
          var file = path.join(temporary, "x");
          fs.writeFileSync(file, "x");
          fs.unlinkSync(file);
          fs.rmdirSync(temporary);
          fs.mkdirSync(path.join(base, "empty"));
          var empty = fs.readdirSync(path.join(base, "empty"));
          fs.rmSync(path.join(base, "a"), {{recursive: true}});
          return JSON.stringify({{
            empty: empty.length,
            temporaryGone: !fs.existsSync(temporary),
            nestedGone: !fs.existsSync(path.join(base, "a")),
          }});
        }})()
        "#,
        dir = js_string(&dir.to_string_lossy()),
    );
    let value = eval_json(&harness, &script);
    assert_eq!(value["empty"], 0);
    assert_eq!(value["temporaryGone"], true);
    assert_eq!(value["nestedGone"], true);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// os / url / util / events / zlib / readline / module
// ---------------------------------------------------------------------------

#[test]
fn os_url_util_and_events() {
    let harness = node_harness();
    assert_eq!(
        harness.eval::<String>("os.platform() === process.platform ? 'ok' : 'bad'"),
        "ok"
    );
    assert!(harness.eval::<bool>("os.homedir().length > 0 && os.tmpdir().length > 0"));
    assert_eq!(harness.eval::<String>("os.EOL"), "\n");

    let dir = temp_dir("url");
    let path = dir.join("a b.txt");
    let script = format!(
        r#"
        (function () {{
          var fileUrl = url.pathToFileURL({path});
          return JSON.stringify({{
            href: fileUrl.href,
            roundtrip: url.fileURLToPath(fileUrl),
            isUrl: fileUrl instanceof URL,
            http: url.fileURLToPath("file:///tmp/x"),
          }});
        }})()
        "#,
        path = js_string(&path.to_string_lossy()),
    );
    let value = eval_json(&harness, &script);
    assert_eq!(value["roundtrip"], path.to_string_lossy().as_ref());
    assert_eq!(value["isUrl"], true);
    assert_eq!(value["http"], "/tmp/x");
    assert!(value["href"].as_str().expect("href").contains("a%20b.txt"));

    assert_eq!(
        harness.eval::<String>("util.format('%s=%d', 'a', 2)"),
        "a=2"
    );
    assert_eq!(
        harness.eval::<String>("JSON.stringify(util.inspect({a: 1, b: [2]}))"),
        "\"{ a: 1, b: [ 2 ] }\""
    );
    assert!(harness.eval::<bool>("util.types.isPromise(Promise.resolve())"));

    assert_eq!(
        harness.eval::<String>(
            r#"
            (function () {
              var emitter = new (require("node:events"))();
              var seen = [];
              function listener(value) { seen.push(value); }
              emitter.on("x", listener);
              emitter.once("y", function () { seen.push("once"); });
              emitter.emit("x", 1);
              emitter.emit("y");
              emitter.emit("y");
              emitter.off("x", listener);
              emitter.emit("x", 2);
              return JSON.stringify(seen);
            })()
            "#
        ),
        r#"[1,"once"]"#
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn file_url_to_path_reports_node_error_shapes() {
    let harness = node_harness();
    let value = eval_json(
        &harness,
        r#"
        (function () {
          function capture(fn) {
            try { return { value: fn() }; }
            catch (error) { return { name: error.name, code: String(error.code), message: error.message }; }
          }
          return JSON.stringify({
            scheme: capture(function () { return url.fileURLToPath("https://example.com/x"); }),
            invalid: capture(function () { return url.fileURLToPath("not a url"); }),
            host: capture(function () { return url.fileURLToPath("file://host/x"); }),
            malformed: capture(function () { return url.fileURLToPath("file:///tmp/%ZZ"); }),
            encodedSlash: capture(function () { return url.fileURLToPath("file:///tmp/a%2Fb"); }),
            localhost: url.fileURLToPath("file://localhost/tmp/x"),
            relative: url.fileURLToPath("file:relative"),
            query: url.fileURLToPath("file:///tmp/x?y#z"),
            escaped: url.fileURLToPath("file:///tmp/%41"),
            backslash: url.fileURLToPath("file:///tmp/a%5Cb"),
            urlObject: url.fileURLToPath(new URL("file:///tmp/obj"))
          });
        })()
        "#,
    );
    assert_eq!(value["scheme"]["name"], "TypeError");
    assert_eq!(value["scheme"]["code"], "ERR_INVALID_URL_SCHEME");
    assert_eq!(value["scheme"]["message"], "The URL must be of scheme file");
    assert_eq!(value["invalid"]["name"], "TypeError");
    assert_eq!(value["invalid"]["code"], "ERR_INVALID_URL");
    assert_eq!(value["invalid"]["message"], "Invalid URL");
    assert_eq!(value["host"]["name"], "TypeError");
    assert_eq!(value["host"]["code"], "ERR_INVALID_FILE_URL_HOST");
    let platform = if cfg!(target_os = "macos") {
        "darwin"
    } else {
        std::env::consts::OS
    };
    assert_eq!(
        value["host"]["message"].as_str().expect("host message"),
        format!("File URL host must be \"localhost\" or empty on {platform}")
    );
    assert_eq!(value["malformed"]["name"], "URIError");
    assert_eq!(value["malformed"]["code"], "undefined");
    assert_eq!(value["malformed"]["message"], "URI malformed");
    assert_eq!(value["encodedSlash"]["name"], "TypeError");
    assert_eq!(value["encodedSlash"]["code"], "ERR_INVALID_FILE_URL_PATH");
    assert_eq!(
        value["encodedSlash"]["message"],
        "File URL path must not include encoded / characters"
    );
    assert_eq!(value["localhost"], "/tmp/x");
    assert_eq!(value["relative"], "/relative");
    assert_eq!(value["query"], "/tmp/x");
    assert_eq!(value["escaped"], "/tmp/A");
    assert_eq!(value["backslash"], "/tmp/a\\b");
    assert_eq!(value["urlObject"], "/tmp/obj");
}

#[test]
fn os_metadata_and_stat_fields_are_populated() {
    let harness = node_harness();
    // Node's os.machine() is uname(3)'s machine on Unix (the Rust ARCH name,
    // except Apple Silicon where Node reports "arm64").
    let expected_machine = if cfg!(target_os = "macos") && std::env::consts::ARCH == "aarch64" {
        "arm64".to_owned()
    } else {
        std::env::consts::ARCH.to_owned()
    };
    assert_eq!(
        harness.eval::<String>("os.machine()"),
        expected_machine,
        "os.machine()"
    );
    assert_eq!(
        harness.eval::<String>(
            r#"JSON.stringify({
              count: os.cpus().length > 0,
              model: typeof os.cpus()[0].model,
              speed: typeof os.cpus()[0].speed,
              times: typeof os.cpus()[0].times.user,
            })"#
        ),
        r#"{"count":true,"model":"string","speed":"number","times":"number"}"#
    );
    assert!(
        harness
            .eval::<bool>("os.totalmem() > 0 && os.freemem() > 0 && os.freemem() <= os.totalmem()")
    );

    let dir = temp_dir("stat-metadata");
    let file = dir.join("file.txt");
    std::fs::write(&file, b"hello").expect("write file");
    set_global_path(&harness, "__statFile", &file);
    // birthtime/ctime must be real metadata from the inode (ctime is not a copy
    // of birthtime on Linux), and nlink/mode/ino must be numeric like Node's.
    assert_eq!(
        harness.eval::<String>(
            r#"JSON.stringify((function () {
              var stat = fs.statSync(__statFile);
              return {
                isFile: stat.isFile(),
                size: stat.size,
                mode: typeof stat.mode,
                ino: typeof stat.ino,
                dev: typeof stat.dev,
                nlink: stat.nlink >= 1,
                mtime: stat.mtimeMs > 0,
                atime: stat.atimeMs > 0,
                ctime: stat.ctimeMs > 0,
                birthtime: Number.isFinite(stat.birthtimeMs),
                ctimeIsFinite: Number.isFinite(stat.ctimeMs),
              };
            })())"#
        ),
        r#"{"isFile":true,"size":5,"mode":"number","ino":"number","dev":"number","nlink":true,"mtime":true,"atime":true,"ctime":true,"birthtime":true,"ctimeIsFinite":true}"#
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn zlib_roundtrip() {
    let harness = node_harness();
    assert_eq!(
        harness
            .eval::<String>(r#"zlib.gunzipSync(zlib.gzipSync(Buffer.from("payload"))).toString()"#),
        "payload"
    );
    assert_eq!(
        harness.eval::<String>(
            r#"zlib.inflateSync(zlib.deflateSync(Buffer.from("hello"))).toString()"#
        ),
        "hello"
    );
}

#[test]
fn module_create_require_resolves_builtins() {
    let harness = node_harness();
    // 没有装载扩展加载器时，createRequire 回退到内建 shim + ERR_UNSUPPORTED。
    assert!(harness.eval::<bool>(
        r#"
        (function () {
          var req = module.createRequire("/tmp/x.js");
          var fsModule = req("fs");
          var eventsModule = req("node:events");
          return fsModule.readFileSync === fs.readFileSync && typeof eventsModule.EventEmitter === "function";
        })()
        "#
    ));
    assert!(harness.eval::<bool>(
        "module.isBuiltin('fs') && module.isBuiltin('node:path') && !module.isBuiltin('left-pad')"
    ));
}

// ---------------------------------------------------------------------------
// readline
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn readline_async_iterator_reads_lines() {
    run_local(async {
        let harness = node_harness();
        harness.eval::<()>(
            r#"
            globalThis.__lines = [];
            (async function () {
              var stream = new (require("node:stream")).PassThrough();
              var rl = require("node:readline").createInterface({ input: stream });
              var iterator = rl[Symbol.asyncIterator]();
              stream.write("first\nsecond\r\nthi");
              stream.write("rd\n");
              stream.end();
              var step;
              while (!(step = await iterator.next()).done) __lines.push(step.value);
            })();
            "#,
        );
        assert!(
            harness
                .drive_until(Duration::from_secs(5), |h| h
                    .eval::<String>("JSON.stringify(__lines)")
                    == r#"["first","second","third"]"#)
                .await
        );
    })
    .await;
}

// ---------------------------------------------------------------------------
// fs.promises / fs.watch（宿主 kind 异步路径）
// ---------------------------------------------------------------------------

fn set_global_path(harness: &Harness, name: &str, path: &Path) {
    harness.eval::<()>(&format!(
        "globalThis.{name} = {};",
        js_string(&path.to_string_lossy())
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn fs_promises_and_watch_use_the_completion_queue() {
    run_local(async {
        let dir = temp_dir("async-fs");
        let file = dir.join("data.txt");
        let harness = node_harness();
        set_global_path(&harness, "__dir", &dir);
        set_global_path(&harness, "__file", &file);
        set_global_path(&harness, "__missing", &dir.join("missing.txt"));
        harness.eval::<()>(
            r#"
            globalThis.__fs = {};
            (async function () {
              try {
                await fs.promises.writeFile(__file, "payload");
                __fs.read = await fs.promises.readFile(__file, "utf8");
                __fs.entries = await fs.promises.readdir(__dir);
                __fs.size = (await fs.promises.stat(__file)).size;
                __fs.access = await fs.promises.access(__file).then(function () { return "ok"; }, function () { return "denied"; });
                __fs.missing = null;
                try { await fs.promises.readFile(__missing); } catch (error) { __fs.missing = error.code; }
                __fs.watcher = fs.watch(__dir, function (eventType, filename) {
                  __fs.event = eventType;
                  __fs.filename = filename;
                });
                __fs.watcher.on("close", function () { __fs.closed = true; });
                __fs.watcher.on("error", function (error) { __fs.watchError = String(error); });
              } catch (error) { __fs.error = String((error && error.stack) || error); }
            })();
            "#,
        );
        assert!(
            harness
                .drive_until(Duration::from_secs(5), |h| h
                    .eval::<bool>("(__fs.watcher && __fs.watcher._handle !== null) || !!__fs.error"))
                .await,
            "fs.promises / fs.watch setup did not settle"
        );
        assert_eq!(harness.eval::<String>("String(__fs.error)"), "undefined");
        assert_eq!(harness.eval::<String>("__fs.read"), "payload");
        assert_eq!(harness.eval::<i32>("__fs.size"), 7);
        assert!(harness.eval::<bool>("__fs.entries.indexOf('data.txt') !== -1"));
        assert_eq!(harness.eval::<String>("__fs.access"), "ok");
        assert_eq!(harness.eval::<String>("__fs.missing"), "ENOENT");

        std::fs::write(dir.join("created.txt"), b"x").expect("write watched file");
        assert!(
            harness
                .drive_until(Duration::from_secs(5), |h| h
                    .eval::<bool>("__fs.filename === 'created.txt'"))
                .await,
            "fs.watch did not report the new file"
        );
        assert!(harness.eval::<bool>("typeof __fs.event === 'string' && __fs.event.length > 0"));

        harness.eval::<()>("__fs.watcher.close();");
        assert!(
            harness
                .drive_until(Duration::from_secs(2), |h| h.eval::<bool>("__fs.closed === true"))
                .await,
            "fs.watch close did not emit 'close'"
        );
        let _ = std::fs::remove_dir_all(&dir);
    })
    .await;
}

// ---------------------------------------------------------------------------
// child_process
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn child_process_spawn_exec_and_sync_variants() {
    run_local(async {
        let harness = node_harness();
        harness.eval::<()>(
            r#"
            globalThis.__child = { out: "", err: "" };
            var child = child_process.spawn("sh", ["-c", "printf out; printf err >&2; exit 3"]);
            child.stdout.on("data", function (chunk) { __child.out += chunk.toString(); });
            child.stderr.on("data", function (chunk) { __child.err += chunk.toString(); });
            child.on("close", function (code, signal) {
              __child.code = code;
              __child.signal = signal;
              __child.spawnDone = true;
            });
            child.on("error", function (error) { __child.spawnError = String((error && error.stack) || error); });

            child_process.exec("printf exec-out", { encoding: "utf8" }, function (error, stdout, stderr) {
              __child.exec = { code: error ? error.code : null, stdout: stdout, stderr: stderr };
            });
            child_process.exec("exit 7", function (error) {
              __child.execFailure = error ? error.code : null;
            });

            var sync = child_process.spawnSync("sh", ["-c", "printf sync-out"]);
            __child.sync = { status: sync.status, stdout: sync.stdout.toString(), stderr: sync.stderr.toString() };
            try { child_process.execSync("exit 5"); } catch (error) { __child.syncFailure = error.status; }
            __child.execSync = child_process.execSync("printf exec-sync", { encoding: "utf8" });

            // An explicit env object replaces the environment; values are stringified
            // (null -> "null", objects -> "[object Object]", undefined is skipped).
            var explicit = child_process.spawnSync(
              "sh",
              ["-c", "printf %s \"$A|$B|$C|${HOME-unset}\""],
              { env: { A: 3, B: null, C: {}, D: undefined } }
            );
            __child.explicitEnv = explicit.stdout.toString();

            // Node inherits the *live* process.env: mutations must reach children.
            process.env.KIANA_CHILD_ENV_ASYNC = "async-mutated";
            var envChild = child_process.spawn("sh", ["-c", "printf %s \"$KIANA_CHILD_ENV_ASYNC\""]);
            envChild.stdout.on("data", function (chunk) { __child.envOut = (__child.envOut || "") + chunk.toString(); });
            "#,
        );
        assert!(
            harness
                .drive_until(Duration::from_secs(10), |h| h.eval::<bool>(
                    "__child.spawnDone === true && !!__child.exec && __child.execFailure !== undefined && !!__child.sync && __child.syncFailure !== undefined && __child.execSync !== undefined && __child.envOut !== undefined"
                ))
                .await,
            "child_process operations did not settle"
        );
        assert_eq!(harness.eval::<String>("String(__child.spawnError)"), "undefined");
        assert_eq!(harness.eval::<String>("__child.out"), "out");
        assert_eq!(harness.eval::<String>("__child.err"), "err");
        assert_eq!(harness.eval::<i32>("__child.code"), 3);
        assert_eq!(harness.eval::<String>("String(__child.signal)"), "null");
        assert_eq!(harness.eval::<String>("String(__child.exec.code)"), "null");
        assert_eq!(harness.eval::<String>("__child.exec.stdout"), "exec-out");
        assert_eq!(harness.eval::<String>("__child.exec.stderr"), "");
        assert_eq!(harness.eval::<i32>("__child.execFailure"), 7);
        assert_eq!(harness.eval::<i32>("__child.sync.status"), 0);
        assert_eq!(harness.eval::<String>("__child.sync.stdout"), "sync-out");
        assert_eq!(harness.eval::<i32>("__child.syncFailure"), 5);
        assert_eq!(harness.eval::<String>("__child.execSync"), "exec-sync");
        assert_eq!(harness.eval::<String>("__child.envOut"), "async-mutated");
        assert_eq!(
            harness.eval::<String>("__child.explicitEnv"),
            "3|null|[object Object]|unset"
        );
    })
    .await;
}

// ---------------------------------------------------------------------------
// net / http / fetch
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn net_server_and_client_echo_over_real_tcp() {
    run_local(async {
        let harness = node_harness();
        harness.eval::<()>(
            r#"
            globalThis.__net = { received: "" };
            var server = net.createServer(function (socket) {
              socket.on("data", function (chunk) { socket.write("echo:" + chunk.toString()); });
              socket.on("end", function () { socket.end(); });
            });
            server.on("error", function (error) { __net.serverError = String(error); });
            server.listen(0, "127.0.0.1", function () {
              __net.port = server.address().port;
              var client = net.connect(__net.port, "127.0.0.1");
              client.on("connect", function () { client.write("hi"); });
              client.on("data", function (chunk) { __net.received += chunk.toString(); client.end(); });
              client.on("end", function () { server.close(function () { __net.closed = true; }); });
              client.on("error", function (error) { __net.clientError = String((error && error.stack) || error); });
            });
            "#,
        );
        assert!(
            harness
                .drive_until(Duration::from_secs(10), |h| h.eval::<bool>(
                    "(__net.received === 'echo:hi' && __net.closed === true) || !!__net.clientError || !!__net.serverError"
                ))
                .await,
            "net echo did not complete"
        );
        assert_eq!(harness.eval::<String>("String(__net.serverError)"), "undefined");
        assert_eq!(harness.eval::<String>("String(__net.clientError)"), "undefined");
        assert_eq!(harness.eval::<String>("__net.received"), "echo:hi");
        assert!(harness.eval::<bool>("__net.closed"));
        assert!(harness.eval::<bool>("__net.port > 0"));
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn http_server_answers_request_with_body_and_headers() {
    run_local(async {
        let harness = node_harness();
        harness.eval::<()>(
            r#"
            globalThis.__http = {};
            var server = http.createServer(function (req, res) {
              var body = "";
              req.on("data", function (chunk) { body += chunk.toString(); });
              req.on("end", function () {
                res.writeHead(200, { "Content-Type": "application/json" });
                res.end(JSON.stringify({
                  method: req.method,
                  url: req.url,
                  body: body,
                  token: req.headers["x-token"],
                }));
              });
            });
            server.on("error", function (error) { __http.serverError = String(error); });
            server.listen(0, "127.0.0.1", function () {
              __http.port = server.address().port;
              var request = http.request({
                hostname: "127.0.0.1",
                port: __http.port,
                method: "POST",
                path: "/echo",
                headers: { "X-Token": "abc" },
              }, function (res) {
                var text = "";
                res.on("data", function (chunk) { text += chunk.toString(); });
                res.on("end", function () {
                  __http.status = res.statusCode;
                  __http.body = JSON.parse(text);
                  __http.done = true;
                  server.close();
                });
              });
              request.on("error", function (error) { __http.requestError = String((error && error.stack) || error); });
              request.end("payload");
            });
            "#,
        );
        assert!(
            harness
                .drive_until(Duration::from_secs(10), |h| h.eval::<bool>(
                    "__http.done === true || !!__http.requestError || !!__http.serverError"
                ))
                .await,
            "http request did not complete"
        );
        assert_eq!(harness.eval::<String>("String(__http.serverError)"), "undefined");
        assert_eq!(harness.eval::<String>("String(__http.requestError)"), "undefined");
        assert_eq!(harness.eval::<i32>("__http.status"), 200);
        assert_eq!(harness.eval::<String>("__http.body.method"), "POST");
        assert_eq!(harness.eval::<String>("__http.body.url"), "/echo");
        assert_eq!(harness.eval::<String>("__http.body.body"), "payload");
        assert_eq!(harness.eval::<String>("__http.body.token"), "abc");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn fetch_reads_text_and_json_from_a_local_server() {
    run_local(async {
        let harness = node_harness();
        harness.eval::<()>(
            r#"
            globalThis.__fetch = {};
            var server = http.createServer(function (req, res) {
              if (req.url === "/text") {
                res.writeHead(200, { "Content-Type": "text/plain" });
                res.end("plain");
                return;
              }
              var body = "";
              req.on("data", function (chunk) { body += chunk.toString(); });
              req.on("end", function () {
                res.writeHead(200, { "Content-Type": "application/json" });
                res.end(JSON.stringify({ method: req.method, body: body, token: req.headers["x-token"] }));
              });
            });
            server.on("error", function (error) { __fetch.serverError = String(error); });
            server.listen(0, "127.0.0.1", function () {
              var base = "http://127.0.0.1:" + server.address().port;
              fetch(base + "/text").then(function (response) {
                __fetch.status = response.status;
                __fetch.contentType = response.headers.get("content-type");
                return response.text();
              }).then(function (text) {
                __fetch.plain = text;
                return fetch(base + "/json", { method: "POST", headers: { "x-token": "abc" }, body: "hello" });
              }).then(function (response) {
                return response.json();
              }).then(function (value) {
                __fetch.json = value;
                __fetch.done = true;
                server.close();
              }).catch(function (error) {
                __fetch.error = String((error && error.stack) || error);
                server.close();
              });
            });
            "#,
        );
        assert!(
            harness
                .drive_until(Duration::from_secs(10), |h| h.eval::<bool>(
                    "__fetch.done === true || !!__fetch.error || !!__fetch.serverError"
                ))
                .await,
            "fetch did not complete"
        );
        assert_eq!(harness.eval::<String>("String(__fetch.serverError)"), "undefined");
        assert_eq!(harness.eval::<String>("String(__fetch.error)"), "undefined");
        assert_eq!(harness.eval::<i32>("__fetch.status"), 200);
        assert_eq!(harness.eval::<String>("__fetch.contentType"), "text/plain");
        assert_eq!(harness.eval::<String>("__fetch.plain"), "plain");
        assert_eq!(harness.eval::<String>("__fetch.json.method"), "POST");
        assert_eq!(harness.eval::<String>("__fetch.json.body"), "hello");
        assert_eq!(harness.eval::<String>("__fetch.json.token"), "abc");
    })
    .await;
}

// ---------------------------------------------------------------------------
// §10.4 第 4 条：定时器不得在微任务阶段执行
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn timers_do_not_run_in_the_microtask_phase() {
    run_local(async {
        let harness = node_harness();
        harness.eval::<()>(
            r#"
            globalThis.__timer = [];
            setTimeout(function () { __timer.push("timer"); }, 10);
            Promise.resolve().then(function () { __timer.push("microtask"); });
            __timer.push("sync");
            "#,
        );
        harness.pump();
        assert_eq!(
            harness.eval::<String>("JSON.stringify(__timer)"),
            r#"["sync","microtask"]"#
        );
        assert!(
            harness
                .drive_until(Duration::from_secs(5), |h| h
                    .eval::<String>("JSON.stringify(__timer)")
                    == r#"["sync","microtask","timer"]"#)
                .await,
            "setTimeout callback never ran"
        );
    })
    .await;
}

// ---------------------------------------------------------------------------
// §10.4 真实 npm 包回归
// ---------------------------------------------------------------------------

/// 1. 多文件 CJS：`shell-quote@1.12.0`（入口 require 了 `quote.js` / `parse.js`）。
#[test]
fn regression_multifile_cjs_shell_quote() {
    let harness = node_harness();
    let entry = fixtures().join("shell-quote");
    set_global_path(&harness, "__entry", &entry);
    let value = eval_json(
        &harness,
        r#"
        (function () {
          var shellquote = require(__entry);
          var quoted = shellquote.quote(["a b", "c"]);
          var parsed = shellquote.parse("echo hi");
          return JSON.stringify({
            quoted: quoted,
            roundTrip: shellquote.parse(quoted),
            parsed: parsed.length,
            modules: Object.keys(__kiana_cjs).filter(function (key) {
              return /(quote|parse)\.js$/.test(key);
            }).length,
          });
        })()
        "#,
    );
    // 设计 §10.4 写的 `a\\ b c` 是旧版 shell-quote 的转义形态；1.12.0（与
    // sandbox-runtime 的 `^1.8.3` 同代）选择单引号，实测 Node 输出相同（见 §7.4）。
    assert_eq!(value["quoted"], "'a b' c");
    assert_eq!(value["roundTrip"], serde_json::json!(["a b", "c"]));
    assert_eq!(value["parsed"], 2);
    // 只测自包含包（如 ms）会漏掉多文件加载：quote.js 与 parse.js 都必须在缓存里。
    assert_eq!(value["modules"], 2);
}

/// 2. `zod@3.25.76`：`safeParse({allowedDomains: 'nope'})` 的 issue code。
#[test]
fn regression_zod_safe_parse_issue_code() {
    let harness = node_harness();
    let entry = fixtures().join("zod");
    set_global_path(&harness, "__entry", &entry);
    let value = eval_json(
        &harness,
        r#"
        (function () {
          var zod = require(__entry);
          var z = zod.z;
          var schema = z.object({ allowedDomains: z.array(z.string()) });
          var network = schema.safeParse({ allowedDomains: "nope" });
          var scalar = z.string().safeParse(42);
          return JSON.stringify({
            networkSuccess: network.success,
            networkCode: network.error.issues[0].code,
            scalarCode: scalar.error.issues[0].code,
            ok: schema.safeParse({ allowedDomains: ["example.com"] }).success,
          });
        })()
        "#,
    );
    assert_eq!(value["networkSuccess"], false);
    assert_eq!(value["networkCode"], "invalid_type");
    assert_eq!(value["scalarCode"], "invalid_type");
    assert_eq!(value["ok"], true);
}

/// 2（续）. `@anthropic-ai/sandbox-runtime@0.0.26` 的 `getDefaultWritePaths()` 有 10 项。
///
/// 该包只有 ESM 构建（`"type": "module"` + `dist/*.js`），CJS require 必须按 Node 语义
/// 拒绝（见下面的 `require_of_an_esm_package_is_rejected`）；因此这里走 loader-runtime
/// 单元的 `ExtensionLoader`（`import`/`Module::declare` 流水线），从语料自带的
/// `node_modules` 解析整条依赖图（lodash-es/zod/…）。设计 §10.4 第 3 条。
#[test]
fn regression_sandbox_runtime_default_write_paths() {
    // 664 个 ESM 模块的链接/求值在 debug 构建下超过默认 100 ms 看门狗（设计 §2.7），
    // 与 corpus 测试一样放宽到 5 s。
    let config = EngineConfig {
        watchdog: Duration::from_secs(5),
        ..EngineConfig::default()
    };
    let harness = Harness::with_config(config);
    js_host::install_intl(&harness.engine).expect("install intl");
    js_host::install(&harness.engine).expect("install node globals");

    // 与 `loader::install` 相同的接线：Node shim 源 → loader 的注册表。
    let loader = ExtensionLoader::new(LoaderConfig::default());
    for (name, source) in harness.engine.module_sources() {
        loader.register_source(name, source);
    }
    let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/corpus");
    let resolver = ModuleResolver::new(ResolverOptions {
        extension_dir: Some(corpus),
        virtual_fs: VirtualFs::empty(),
    });
    let entry = resolver
        .resolve_specifier("", "@anthropic-ai/sandbox-runtime", ResolutionKind::Import)
        .expect("resolve @anthropic-ai/sandbox-runtime");
    loader.install_on(&harness.engine, resolver);

    let count = harness.engine.with_js(|ctx| {
        let module = loader
            .load_module(&ctx, &entry)
            .expect("declare @anthropic-ai/sandbox-runtime");
        let (module, promise) = module
            .eval()
            .expect("evaluate @anthropic-ai/sandbox-runtime");
        if let Err(error) = promise.finish::<()>() {
            let thrown = ctx.catch();
            let message = thrown
                .as_object()
                .and_then(|object| object.get::<_, Option<String>>("message").ok().flatten())
                .unwrap_or_default();
            let text = thrown
                .as_object()
                .and_then(|object| object.get::<_, Option<String>>("stack").ok().flatten())
                .or_else(|| thrown.as_string().and_then(|value| value.to_string().ok()))
                .unwrap_or_default();
            panic!("settle @anthropic-ai/sandbox-runtime: {error:?}\nmessage: {message}\n{text}");
        }
        let namespace = module.namespace().expect("namespace");
        let get_default_write_paths: Function = namespace
            .get("getDefaultWritePaths")
            .expect("export getDefaultWritePaths");
        let paths: Array = get_default_write_paths
            .call(())
            .expect("call getDefaultWritePaths");
        paths.len()
    });
    assert_eq!(count, 10);
}

/// ESM-only 包在 CJS require 下抛出 Node 的 `ERR_REQUIRE_ESM`（loader-host 侧的可测部分）。
#[test]
fn require_of_an_esm_package_is_rejected() {
    let harness = node_harness();
    let entry = fixtures().join("@anthropic-ai/sandbox-runtime");
    set_global_path(&harness, "__entry", &entry);
    harness.eval::<()>(
        r#"
        globalThis.__esm = {};
        try { require(__entry); } catch (error) { __esm.code = error.code; __esm.message = error.message; }
        "#,
    );
    assert_eq!(harness.eval::<String>("__esm.code"), "ERR_REQUIRE_ESM");
    assert!(harness.eval::<bool>(
        "__esm.message.indexOf('require() of ES Module') === 0 && __esm.message.indexOf('Instead change the require') !== -1"
    ));
}

/// 3. `@anthropic-ai/sdk@0.52.0` + 假 fetch 的 6 个 SSE chunk。
///
/// 断言 `text` 事件收到 `["hello"]`、`finalMessage().content[0].text === "hello"`，
/// 且 `AbortController.prototype.abort` 一次都没被调用（抓「定时器立即触发」的 bug）。
#[tokio::test(flavor = "current_thread")]
async fn regression_anthropic_sdk_streams_six_sse_chunks() {
    run_local(async {
        let harness = node_harness();
        let entry = fixtures().join("@anthropic-ai/sdk");
        set_global_path(&harness, "__entry", &entry);
        harness.eval::<()>(
            r#"
            globalThis.__sdk = { texts: [], abortCount: 0 };
            var originalAbort = AbortController.prototype.abort;
            AbortController.prototype.abort = function () {
              __sdk.abortCount++;
              return originalAbort.call(this);
            };

            var events = [
              'event: message_start\ndata: {"type":"message_start","message":{"id":"msg_test","type":"message","role":"assistant","model":"claude-3-5-sonnet-20241022","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":3,"output_tokens":0}}}\n\n',
              'event: content_block_start\ndata: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}\n\n',
              'event: content_block_delta\ndata: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hello"}}\n\n',
              'event: content_block_stop\ndata: {"type":"content_block_stop","index":0}\n\n',
              'event: message_delta\ndata: {"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":1}}\n\n',
              'event: message_stop\ndata: {"type":"message_stop"}\n\n',
            ];

            globalThis.fetch = function (url, init) {
              __sdk.fetchCalls = (__sdk.fetchCalls || 0) + 1;
              __sdk.requestUrl = String(url);
              __sdk.requestBody = init && init.body;
              var encoder = new TextEncoder();
              var stream = new ReadableStream({
                start: function (controller) {
                  for (var i = 0; i < events.length; i++) controller.enqueue(encoder.encode(events[i]));
                  controller.close();
                },
              });
              return Promise.resolve(new Response(stream, {
                status: 200,
                headers: { "content-type": "text/event-stream", "request-id": "req_test" },
              }));
            };

            (function () {
              try {
                var sdk = require(__entry);
                var Anthropic = sdk.Anthropic || sdk.default;
                var client = new Anthropic({ apiKey: "test-key", maxRetries: 0 });
                var stream = client.messages.stream({
                  model: "claude-3-5-sonnet-20241022",
                  max_tokens: 16,
                  messages: [{ role: "user", content: "hi" }],
                });
                stream.on("text", function (delta) { __sdk.texts.push(delta); });
                stream.on("error", function (error) { __sdk.error = error && error.name ? (error.name + ": " + error.message + "\n" + (error.stack || "")) : String(error); });
                stream.on("abort", function () { __sdk.aborted = true; });
                stream.finalMessage().then(function (message) {
                  __sdk.finalText = message.content[0].text;
                  __sdk.stopReason = message.stop_reason;
                  __sdk.done = true;
                }, function (error) {
                  __sdk.error = error && error.name ? (error.name + ": " + error.message + "\n" + (error.stack || "")) : String(error);
                });
              } catch (error) {
                __sdk.error = error && error.name ? (error.name + ": " + error.message + "\n" + (error.stack || "")) : String(error);
              }
            })();
            "#,
        );
        assert!(
            harness
                .drive_until(Duration::from_secs(15), |h| {
                    h.eval::<bool>("__sdk.done === true || !!__sdk.error")
                })
                .await,
            "Anthropic SDK stream did not finish"
        );
        assert_eq!(harness.eval::<String>("String(__sdk.error)"), "undefined");
        assert_eq!(
            harness.eval::<String>("JSON.stringify(__sdk.texts)"),
            r#"["hello"]"#
        );
        assert_eq!(harness.eval::<String>("__sdk.finalText"), "hello");
        assert_eq!(harness.eval::<String>("__sdk.stopReason"), "end_turn");
        assert_eq!(harness.eval::<String>("String(__sdk.aborted)"), "undefined");
        assert_eq!(harness.eval::<i32>("__sdk.abortCount"), 0);
        assert_eq!(harness.eval::<i32>("__sdk.fetchCalls"), 1);
        assert!(harness.eval::<bool>("__sdk.requestUrl.indexOf('/v1/messages') !== -1"));
        assert!(harness.eval::<bool>("JSON.parse(__sdk.requestBody).stream === true"));
    })
    .await;
}

// ---------------------------------------------------------------------------
// CONNECT 隧道（http.Server 的 'connect' 事件）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn http_connect_upgrades_the_socket_to_a_tunnel() {
    run_local(async {
        let harness = node_harness();
        harness.eval::<()>(
            r#"
            globalThis.__proxy = { events: [] };
            var server = http.createServer(function (req, res) {
              __proxy.events.push("request:" + req.method);
              res.end("unexpected");
            });
            server.on("connect", function (req, socket, head) {
              __proxy.events.push("connect:" + req.method + ":" + req.url + ":" + head.length);
              socket.write("HTTP/1.1 200 Connection Established\r\n\r\n");
              socket.on("data", function (chunk) {
                __proxy.events.push("tunnel:" + chunk.toString());
                socket.end();
              });
            });
            server.on("error", function (error) { __proxy.error = String(error); });
            server.listen(0, "127.0.0.1", function () {
              var client = net.connect(server.address().port, "127.0.0.1");
              var received = "";
              client.on("connect", function () {
                client.write("CONNECT example.com:443 HTTP/1.1\r\nHost: example.com:443\r\n\r\n");
              });
              client.on("data", function (chunk) {
                received += chunk.toString();
                if (received.indexOf("\r\n\r\n") !== -1 && !__proxy.sent) {
                  __proxy.sent = true;
                  __proxy.status = received.split("\r\n")[0];
                  client.write("ping");
                }
              });
              client.on("end", function () { __proxy.done = true; server.close(); });
              client.on("error", function (error) {
                __proxy.error = String((error && (error.stack || error.message)) || error);
              });
            });
            "#,
        );
        assert!(
            harness
                .drive_until(Duration::from_secs(10), |h| h
                    .eval::<bool>("__proxy.done === true || !!__proxy.error"))
                .await,
            "CONNECT tunnel did not complete"
        );
        assert_eq!(harness.eval::<String>("String(__proxy.error)"), "undefined");
        assert_eq!(
            harness.eval::<String>("JSON.stringify(__proxy.events)"),
            r#"["connect:CONNECT:example.com:443:0","tunnel:ping"]"#
        );
        assert_eq!(
            harness.eval::<String>("__proxy.status"),
            "HTTP/1.1 200 Connection Established"
        );
    })
    .await;
}

// ---------------------------------------------------------------------------
// 评审回归：path 边界值（Node 26.12.1 实测）
// ---------------------------------------------------------------------------

#[test]
fn path_edge_cases_match_node() {
    let harness = node_harness();
    let cases = [
        ("path.posix.extname('..')", ""),
        ("path.posix.parse('..').ext", ""),
        ("path.posix.parse('..').name", ".."),
        ("path.posix.parse('..').dir", ""),
        ("path.posix.parse('.').dir", ""),
        ("path.posix.parse('a/b/').dir", "a"),
        ("path.posix.parse('a//b//c').dir", "a//b/"),
        ("path.posix.dirname('a//b//c')", "a//b/"),
        ("path.posix.dirname('/a//b')", "/a/"),
        ("path.posix.dirname('//a')", "//"),
        ("path.posix.dirname('///a')", "//"),
        ("path.posix.normalize('./')", "./"),
        ("path.posix.normalize('x/../')", "./"),
        ("path.posix.normalize('a/b/')", "a/b/"),
        ("path.win32.normalize('C:')", "C:."),
        ("path.win32.normalize('C:\\\\a\\\\..')", "C:\\"),
        ("path.win32.normalize('a\\\\..\\\\')", ".\\"),
        (
            "path.win32.normalize('\\\\\\\\server\\\\share')",
            "\\\\server\\share\\",
        ),
        ("path.win32.basename('C:\\\\a\\\\b.TXT', '.txt')", "b.TXT"),
        ("path.win32.extname('..')", ""),
        ("path.win32.relative('C:\\\\a', 'D:\\\\a')", "D:\\a"),
        (
            "path.win32.relative('C:\\\\a\\\\b', 'C:\\\\a\\\\c')",
            "..\\c",
        ),
        ("path.win32.join('C:', 'a')", "C:\\a"),
        ("path.posix.relative('/a/b/c', '/a/d')", "../../d"),
    ];
    for (source, expected) in cases {
        assert_eq!(
            harness.eval::<String>(source),
            expected,
            "path case {source}"
        );
    }
}

// ---------------------------------------------------------------------------
// 评审回归：fs 的 flag、递归 mkdir 返回值、目录 readFile 的 syscall
// ---------------------------------------------------------------------------

#[test]
fn fs_write_file_flags_and_recursive_mkdir_return_value() {
    let dir = temp_dir("fs-flags");
    let existing = dir.join("existing.txt");
    std::fs::write(&existing, b"old").expect("seed");
    let harness = node_harness();
    let missing = dir.join("missing.txt");
    let nested = dir.join("a").join("b").join("c");
    let script = format!(
        r#"
        (function () {{
          var out = {{}};
          try {{ fs.writeFileSync({existing}, "new", {{flag: "wx"}}); out.wx = "wrote"; }}
          catch (error) {{ out.wx = [error.code, error.errno, error.syscall]; }}
          out.kept = fs.readFileSync({existing}, "utf8");
          try {{ fs.writeFileSync({missing}, "x", {{flag: "r+"}}); out.rplus = "wrote"; }}
          catch (error) {{ out.rplus = error.code; }}
          try {{ fs.writeFileSync({existing}, "x", {{flag: "nope"}}); out.bad = "wrote"; }}
          catch (error) {{ out.bad = [error.name, error.code]; }}
          var first = fs.mkdirSync({nested}, {{recursive: true}});
          out.first = first;
          out.second = fs.mkdirSync({nested}, {{recursive: true}});
          try {{ fs.readFileSync({dir}); out.dirRead = "read"; }}
          catch (error) {{ out.dirRead = [error.code, error.syscall]; }}
          fs.writeFileSync({existing}, "append", {{flag: "a"}});
          out.appended = fs.readFileSync({existing}, "utf8");
          return JSON.stringify(out);
        }})()
        "#,
        dir = js_string(&dir.to_string_lossy()),
        existing = js_string(&existing.to_string_lossy()),
        missing = js_string(&missing.to_string_lossy()),
        nested = js_string(&nested.to_string_lossy()),
    );
    let value = eval_json(&harness, &script);
    assert_eq!(value["wx"], serde_json::json!(["EEXIST", -17, "open"]));
    assert_eq!(value["kept"], "old");
    assert_eq!(value["rplus"], "ENOENT");
    assert_eq!(
        value["bad"],
        serde_json::json!(["TypeError", "ERR_INVALID_ARG_VALUE"])
    );
    assert_eq!(
        value["first"].as_str().expect("first created"),
        dir.join("a").to_string_lossy()
    );
    assert_eq!(value["second"], serde_json::Value::Null);
    assert_eq!(value["dirRead"], serde_json::json!(["EISDIR", "read"]));
    assert_eq!(value["appended"], "oldappend");
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// 评审回归：child_process 错误形状与二进制输出
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn child_process_error_messages_and_binary_stdout() {
    run_local(async {
        let dir = temp_dir("child-binary");
        let binary = dir.join("bin");
        std::fs::write(&binary, [0xff, 0xfe, 0x41, 0x42]).expect("write binary");
        let harness = node_harness();
        set_global_path(&harness, "__binary", &binary);
        harness.eval::<()>(
            r#"
            globalThis.__cp = {};
            process.env.KIANA_CHILD_ENV_TEST = "mutated";
            var envChild = child_process.spawnSync("sh", ["-c", "printf %s \"$KIANA_CHILD_ENV_TEST\""]);
            __cp.envValue = envChild.stdout.toString();
            var raw = child_process.spawnSync("cat", [__binary]);
            __cp.spawnStdout = Array.prototype.slice.call(raw.stdout);
            try { child_process.execSync("printf 'bad' >&2; exit 7"); }
            catch (error) {
              __cp.execSyncMessage = error.message;
              __cp.execSyncHasCode = Object.prototype.hasOwnProperty.call(error, "code");
              __cp.execSyncStatus = error.status;
            }
            child_process.exec("printf 'bad' >&2; exit 7", function (error, stdout, stderr) {
              __cp.execMessage = error && error.message;
              __cp.execCode = error && error.code;
              __cp.execStderr = stderr;
              __cp.done = true;
            });
            "#,
        );
        assert!(
            harness
                .drive_until(Duration::from_secs(10), |h| h.eval::<bool>("__cp.done === true"))
                .await,
            "exec did not finish"
        );
        assert_eq!(
            harness.eval::<String>("JSON.stringify(__cp.spawnStdout)"),
            "[255,254,65,66]"
        );
        assert_eq!(harness.eval::<String>("__cp.envValue"), "mutated");
        assert_eq!(
            harness.eval::<String>("JSON.stringify(__cp.execSyncMessage)"),
            r#""Command failed: printf 'bad' >&2; exit 7\nbad""#
        );
        assert!(!harness.eval::<bool>("__cp.execSyncHasCode"));
        assert_eq!(harness.eval::<i32>("__cp.execSyncStatus"), 7);
        assert_eq!(
            harness.eval::<String>("JSON.stringify(__cp.execMessage)"),
            r#""Command failed: printf 'bad' >&2; exit 7\nbad""#
        );
        assert_eq!(harness.eval::<i32>("__cp.execCode"), 7);
        assert_eq!(harness.eval::<String>("__cp.execStderr"), "bad");
        let _ = std::fs::remove_dir_all(&dir);
    })
    .await;
}

// ---------------------------------------------------------------------------
// 评审回归：net 错误形状
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn net_connection_error_matches_node_shape() {
    run_local(async {
        let harness = node_harness();
        // Port 1 is almost never listening; retry a few ephemeral ports if it is.
        harness.eval::<()>(
            r#"
            globalThis.__neterr = {};
            var socket = net.connect(1, "127.0.0.1");
            socket.on("error", function (error) {
              __neterr.message = error.message;
              __neterr.code = error.code;
              __neterr.errno = error.errno;
              __neterr.syscall = error.syscall;
              __neterr.address = error.address;
              __neterr.port = error.port;
              __neterr.done = true;
            });
            socket.on("close", function () {});
            "#,
        );
        assert!(
            harness
                .drive_until(Duration::from_secs(10), |h| h
                    .eval::<bool>("__neterr.done === true"))
                .await,
            "connection error did not fire"
        );
        assert_eq!(
            harness.eval::<String>("__neterr.message"),
            "connect ECONNREFUSED 127.0.0.1:1"
        );
        assert_eq!(harness.eval::<String>("__neterr.code"), "ECONNREFUSED");
        assert_eq!(harness.eval::<i32>("__neterr.errno"), -111);
        assert_eq!(harness.eval::<String>("__neterr.syscall"), "connect");
        assert_eq!(harness.eval::<String>("__neterr.address"), "127.0.0.1");
        assert_eq!(harness.eval::<i32>("__neterr.port"), 1);
    })
    .await;
}

// ---------------------------------------------------------------------------
// 评审回归：HEAD 响应没有 body（客户端与服务器）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn head_requests_have_no_body_on_both_sides() {
    run_local(async {
        let harness = node_harness();
        harness.eval::<()>(
            r#"
            globalThis.__head = { serverBodies: [] };
            var server = http.createServer(function (req, res) {
              res.writeHead(200, { "Content-Length": "5", "Content-Type": "text/plain" });
              res.end("hello");
            });
            server.listen(0, "127.0.0.1", function () {
              var options = { hostname: "127.0.0.1", port: server.address().port, method: "HEAD", path: "/" };
              var request = http.request(options, function (res) {
                var body = "";
                res.on("data", function (chunk) { body += chunk.toString(); });
                res.on("end", function () {
                  __head.status = res.statusCode;
                  __head.body = body;
                  __head.length = res.headers["content-length"];
                  __head.done = true;
                  server.close();
                });
              });
              request.on("error", function (error) { __head.error = String(error); });
              request.end();
            });
            "#,
        );
        assert!(
            harness
                .drive_until(Duration::from_secs(10), |h| h
                    .eval::<bool>("__head.done === true || !!__head.error"))
                .await,
            "HEAD request hung (body parser waited for Content-Length bytes)"
        );
        assert_eq!(harness.eval::<String>("String(__head.error)"), "undefined");
        assert_eq!(harness.eval::<i32>("__head.status"), 200);
        assert_eq!(harness.eval::<String>("__head.body"), "");
        assert_eq!(harness.eval::<String>("__head.length"), "5");
    })
    .await;
}

// ---------------------------------------------------------------------------
// 评审回归：ESM 具名导出表不得指向 undefined
// ---------------------------------------------------------------------------

#[test]
fn node_esm_named_exports_are_defined_on_the_shim_objects() {
    let harness = node_harness();
    let mut checked = 0usize;
    for (name, source) in harness.engine.module_sources() {
        if !name.starts_with("node:") {
            continue;
        }
        let module = &name[5..];
        let mut exports = Vec::new();
        for line in source.lines() {
            if let Some(rest) = line.strip_prefix("export const ") {
                let export = rest.split(" = ").next().unwrap_or("").trim();
                if !export.is_empty() {
                    exports.push(export.to_owned());
                }
            }
        }
        for export in exports {
            let script = format!(
                "__kiana_node_shims[{}] !== undefined && __kiana_node_shims[{}][{}] !== undefined",
                js_string(name.as_str()),
                js_string(name.as_str()),
                js_string(&export)
            );
            assert!(
                harness.eval::<bool>(&script),
                "node:{module} advertises an undefined named export: {export}"
            );
            checked += 1;
        }
    }
    assert!(
        checked > 100,
        "expected many named exports, checked {checked}"
    );
}

// ---------------------------------------------------------------------------
// 评审回归：fetch/Response 的 BodyInit、隐式 Content-Type、clone、data: URL
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn fetch_body_init_content_type_clone_and_data_urls() {
    run_local(async {
        let harness = node_harness();
        harness.eval::<()>(
            r#"
            globalThis.__body = { seen: [] };
            var server = http.createServer(function (req, res) {
              var chunks = [];
              req.on("data", function (chunk) { chunks.push(chunk); });
              req.on("end", function () {
                var body = Buffer.concat(chunks);
                __body.seen.push({
                  contentType: req.headers["content-type"] || null,
                  body: Array.prototype.slice.call(body),
                  text: body.toString("latin1"),
                });
                res.writeHead(200, { "Content-Type": "text/plain" });
                res.end("final");
              });
            });
            server.listen(0, "127.0.0.1", function () {
              __body.base = "http://127.0.0.1:" + server.address().port;
              (async function () {
                try {
                  var form = new FormData();
                  form.append("field", "value");
                  form.append("file", new Blob(["file-bytes"], { type: "text/plain" }), "f.txt");
                  var sequence = [
                    function () { return fetch(__body.base + "/string", { method: "POST", body: "hello" }); },
                    function () { return fetch(__body.base + "/params", { method: "POST", body: new URLSearchParams({ a: "1", b: "x y" }) }); },
                    function () { return fetch(__body.base + "/blob", { method: "POST", body: new Blob(["blob-bytes"], { type: "application/x-test" }) }); },
                    function () { return fetch(__body.base + "/form", { method: "POST", body: form }); },
                    function () {
                      var payload = new TextEncoder().encode("stream-bytes");
                      var streamBody = new ReadableStream({
                        start: function (controller) { controller.enqueue(payload); controller.close(); }
                      });
                      return fetch(__body.base + "/stream", { method: "POST", body: streamBody, duplex: "half" });
                    },
                  ];
                  for (var i = 0; i < sequence.length; i++) await sequence[i]();

                  var cloned = await fetch(__body.base + "/clone");
                  var copy = cloned.clone();
                  __body.cloneTexts = [await cloned.text(), await copy.text()];

                  var formResponse = new Response(form);
                  __body.responseForm = {
                    contentType: formResponse.headers.get("content-type"),
                    text: await formResponse.text(),
                  };
                  __body.responseBlob = await new Response(new Blob(["abc"])).text();

                  var dataResponse = await fetch("data:,hello");
                  __body.data = {
                    status: dataResponse.status,
                    statusText: dataResponse.statusText,
                    text: await dataResponse.text(),
                    contentType: dataResponse.headers.get("content-type"),
                  };
                  __body.done = true;
                  server.close();
                } catch (error) {
                  __body.error = String((error && error.stack) || error);
                  server.close();
                }
              })();
            });
            "#,
        );
        assert!(
            harness
                .drive_until(Duration::from_secs(15), |h| h
                    .eval::<bool>("__body.done === true || !!__body.error"))
                .await,
            "fetch body-init probes did not finish"
        );
        assert_eq!(harness.eval::<String>("String(__body.error)"), "undefined");
        let seen: serde_json::Value =
            serde_json::from_str(&harness.eval::<String>("JSON.stringify(__body.seen)"))
                .expect("seen");
        let seen = seen.as_array().expect("array");

        // string body
        assert_eq!(seen[0]["contentType"], "text/plain;charset=UTF-8");
        assert_eq!(seen[0]["text"], "hello");
        // URLSearchParams body
        assert_eq!(
            seen[1]["contentType"],
            "application/x-www-form-urlencoded;charset=UTF-8"
        );
        assert_eq!(seen[1]["text"], "a=1&b=x+y");
        // Blob body
        assert_eq!(seen[2]["contentType"], "application/x-test");
        assert_eq!(seen[2]["text"], "blob-bytes");
        // FormData body (real multipart, boundary matches the header)
        let form_type = seen[3]["contentType"].as_str().expect("form type");
        assert!(form_type.starts_with("multipart/form-data; boundary="));
        let boundary = form_type.split("boundary=").nth(1).expect("boundary");
        let form_text = seen[3]["text"].as_str().expect("form text");
        assert!(form_text.contains(&format!("--{boundary}\r\n")));
        assert!(form_text.contains("name=\"field\"\r\n\r\nvalue\r\n"));
        assert!(form_text.contains("name=\"file\"; filename=\"f.txt\""));
        assert!(form_text.contains("file-bytes"));
        assert!(!form_text.contains("[object FormData]"));
        // ReadableStream body is not dropped
        assert_eq!(seen[4]["text"], "stream-bytes");

        assert_eq!(
            harness.eval::<String>("JSON.stringify(__body.cloneTexts)"),
            r#"["final","final"]"#
        );
        assert!(harness.eval::<bool>(
            "__body.responseForm.contentType.indexOf('multipart/form-data; boundary=') === 0 && __body.responseForm.text.indexOf('name=\"field\"') !== -1"
        ));
        assert_eq!(harness.eval::<String>("__body.responseBlob"), "abc");
        assert_eq!(
            harness.eval::<String>("JSON.stringify(__body.data)"),
            r#"{"status":200,"statusText":"OK","text":"hello","contentType":"text/plain;charset=US-ASCII"}"#
        );
    })
    .await;
}

// ---------------------------------------------------------------------------
// Reviewer regressions: HTTP framing, headers, net lifecycle, child stdio,
// fs semantics, Intl options, process.kill.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn http_zero_length_messages_complete_and_proxy_roundtrip() {
    run_local(async {
        let harness = node_harness();
        harness.eval::<()>(
            r#"
            globalThis.__z = { events: [] };
            var upstream = http.createServer(function (req, res) {
              req.resume();
              req.on("end", function () {
                __z.upstreamEnd = req.complete;
                res.writeHead(200, { "Content-Type": "text/plain" });
                res.end("upstream-ok");
              });
            });
            upstream.listen(0, "127.0.0.1", function () {
              var up = upstream.address().port;
              var proxy = http.createServer(function (req, res) {
                var preq = http.request({
                  port: up, host: "127.0.0.1", path: req.url, method: req.method, headers: req.headers,
                }, function (pres) {
                  res.writeHead(pres.statusCode, pres.headers);
                  pres.pipe(res);
                });
                preq.on("error", function () { res.writeHead(502); res.end("bad"); });
                req.pipe(preq);
              });
              proxy.listen(0, "127.0.0.1", function () {
                var port = proxy.address().port;
                var req = http.request({
                  port: port, host: "127.0.0.1", method: "POST", path: "/",
                  headers: { "Content-Length": "0" },
                }, function (res) {
                  var body = "";
                  res.on("data", function (chunk) { body += chunk.toString(); });
                  res.on("end", function () {
                    __z.status = res.statusCode;
                    __z.body = body;
                    __z.complete = res.complete;
                    proxy.close();
                    upstream.close();
                  });
                });
                req.on("error", function (error) { __z.error = String((error && error.stack) || error); });
                req.end();
              });
            });
            "#,
        );
        assert!(
            harness
                .drive_until(Duration::from_secs(10), |h| h.eval::<bool>(
                    "__z.body === 'upstream-ok' || !!__z.error"
                ))
                .await,
            "CL:0 proxy roundtrip hung"
        );
        assert_eq!(harness.eval::<String>("String(__z.error)"), "undefined");
        assert_eq!(harness.eval::<i32>("__z.status"), 200);
        assert!(harness.eval::<bool>("__z.complete === true"));
        assert!(harness.eval::<bool>("__z.upstreamEnd === true"));
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn http_header_joining_connection_and_client_defaults() {
    run_local(async {
        let harness = node_harness();
        harness.eval::<()>(
            r#"
            globalThis.__h = { seen: [] };
            function raw(port, text, done) {
              var sock = net.connect(port, "127.0.0.1");
              var out = "";
              sock.on("data", function (chunk) { out += chunk.toString("latin1"); });
              sock.on("close", function () { done(out, true); });
              sock.on("connect", function () { sock.write(text); });
              setTimeout(function () { sock.destroy(); done(out, false); }, 1500);
            }
            var server = http.createServer(function (req, res) {
              __h.seen.push({
                cookie: req.headers.cookie,
                setcookie: req.headers["set-cookie"],
                dup: req.headers["x-dup"],
              });
              res.setHeader("Set-Cookie", ["a=1", "b=2"]);
              res.setHeader("X-Multi", ["p", "q"]);
              __h.getHeader = res.getHeader("set-cookie");
              res.end();
            });
            server.listen(0, "127.0.0.1", function () {
              var port = server.address().port;
              raw(port, "POST / HTTP/1.1\r\nHost: x\r\nConnection: close\r\nCookie: a=1\r\nCookie: b=2\r\nX-Dup: p\r\nX-Dup: q\r\nSet-Cookie: s=1\r\nSet-Cookie: t=2\r\nContent-Length: 0\r\n\r\n", function (text, closed) {
                __h.closeText = text;
                __h.closed = closed;
                server.close();
                // Client default headers against a raw net server.
                var rawServer = net.createServer(function (sock) {
                  var buffer = "";
                  sock.on("data", function (chunk) {
                    buffer += chunk.toString("latin1");
                    if (buffer.indexOf("\r\n\r\n") !== -1) {
                      __h.clientHeads = __h.clientHeads || [];
                      __h.clientHeads.push(buffer.split("\r\n\r\n")[0]);
                      sock.write("HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
                    }
                  });
                });
                rawServer.listen(0, "127.0.0.1", function () {
                  var rport = rawServer.address().port;
                  var get = http.request({ port: rport, host: "127.0.0.1", method: "GET", path: "/g", headers: { Cookie: ["a=1", "b=2"], "X-Multi": ["p", "q"] } }, function (res) {
                    res.resume();
                    res.on("end", function () {
                      var post = http.request({ port: rport, host: "127.0.0.1", method: "POST", path: "/p" }, function (res2) {
                        res2.resume();
                        res2.on("end", function () { rawServer.close(function () { __h.done = true; }); });
                      });
                      post.on("error", function (e) { __h.error = String(e); });
                      post.end();
                    });
                  });
                  get.on("error", function (e) { __h.error = String(e); });
                  get.end();
                });
              });
            });
            "#,
        );
        assert!(
            harness
                .drive_until(Duration::from_secs(10), |h| h
                    .eval::<bool>("__h.done === true || !!__h.error"))
                .await,
            "http header probe did not finish"
        );
        assert_eq!(harness.eval::<String>("String(__h.error)"), "undefined");
        let seen = harness.eval::<String>("JSON.stringify(__h.seen[0])");
        assert_eq!(seen, r#"{"cookie":"a=1; b=2","setcookie":["s=1","t=2"],"dup":"p, q"}"#);
        assert_eq!(
            harness.eval::<String>("JSON.stringify(__h.getHeader)"),
            r#"["a=1","b=2"]"#
        );
        let text = harness.eval::<String>("__h.closeText");
        assert!(text.contains("Set-Cookie: a=1\r\nSet-Cookie: b=2\r\n"), "{text}");
        assert!(text.contains("X-Multi: p\r\nX-Multi: q\r\n"), "{text}");
        assert!(text.contains("Connection: close\r\n"), "{text}");
        assert!(harness.eval::<bool>("__h.closed === true"));
        let heads = harness.eval::<String>("JSON.stringify(__h.clientHeads)");
        assert!(heads.contains(r"Cookie: a=1; b=2"), "{heads}");
        assert!(heads.contains(r"X-Multi: p\r\nX-Multi: q"), "{heads}");
        assert!(heads.contains("Connection: keep-alive"), "{heads}");
        assert!(!heads.contains(r"GET /g HTTP/1.1\r\nContent-Length"), "{heads}");
        assert!(heads.contains(r"Content-Length: 0"), "{heads}");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn net_socket_lifecycle_and_error_shape() {
    run_local(async {
        let harness = node_harness();
        harness.eval::<()>(
            r#"
            globalThis.__l = { events: [], refused: [] };
            var server = net.createServer(function (sock) {
              sock.on("data", function () { sock.end("bye"); });
            });
            server.listen(0, "127.0.0.1", function () {
              var port = server.address().port;
              var client = net.connect(port, "127.0.0.1");
              client.on("data", function () {});
              ["connect", "end", "finish", "close"].forEach(function (name) {
                client.on(name, function (arg) { __l.events.push(name + (name === "close" ? ":" + arg : "")); });
              });
              client.on("error", function (error) { __l.events.push("error:" + error.message); });
              client.on("connect", function () { client.end("x"); });
              client.on("close", function () {
                server.close();
                var refused = net.connect(1, "127.0.0.1");
                ["connect", "error", "close"].forEach(function (name) {
                  refused.on(name, function (error) {
                    var suffix = "";
                    if (error && typeof error === "object") {
                      suffix = ":" + error.message + "|" + error.code + "|" + error.errno + "|" +
                        error.syscall + "|" + error.address + "|" + error.port;
                    } else if (name === "close") {
                      suffix = ":" + error;
                    }
                    __l.refused.push(name + suffix);
                  });
                });
                setTimeout(function () { __l.done = true; }, 300);
              });
            });
            "#,
        );
        assert!(
            harness
                .drive_until(Duration::from_secs(10), |h| h.eval::<bool>("__l.done === true"))
                .await,
            "net lifecycle probe did not finish"
        );
        assert_eq!(
            harness.eval::<String>("JSON.stringify(__l.events)"),
            r#"["connect","finish","end","close:false"]"#
        );
        let refused = harness.eval::<String>("JSON.stringify(__l.refused)");
        assert_eq!(refused.matches(r#""error:"#).count(), 1, "{refused}");
        assert_eq!(refused.matches(r#""close:"#).count(), 1, "{refused}");
        assert!(refused.contains("|ECONNREFUSED|-111|connect|127.0.0.1|1"), "{refused}");
        assert!(!refused.contains("connect:"), "connect fired after refusal: {refused}");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn child_process_stdin_and_failure_fidelity() {
    run_local(async {
        let harness = node_harness();
        harness.eval::<()>(
            r#"
            globalThis.__c = { fail: [] };
            var bad = child_process.spawn("/nonexistent-xyz", []);
            bad.on("error", function (error) {
              __c.fail.push("error:" + error.message + "|" + error.code + "|" + error.errno + "|" + error.syscall);
            });
            bad.on("exit", function (code) { __c.fail.push("exit:" + code); });
            bad.on("close", function (code, signal) { __c.fail.push("close:" + code + ":" + signal); });
            var syncFail = child_process.spawnSync("/nonexistent-xyz", []);
            __c.syncPid = typeof syncFail.pid;
            __c.syncSyscall = syncFail.error && syncFail.error.syscall;
            var child = child_process.spawn("/bin/cat", [], { stdio: ["pipe", "pipe", "pipe"] });
            var out = "";
            child.stdout.on("data", function (chunk) { out += chunk.toString(); });
            child.on("close", function (code) {
              __c.stdinOut = out;
              __c.stdinCode = code;
              child_process.exec("exit 3", function (error) {
                __c.execMessage = error && error.message;
                __c.execStdout = String(error && error.stdout);
                __c.execStdoutKey = error && Object.prototype.hasOwnProperty.call(error, "stdout");
                __c.ignoreStdin = child_process.spawn("/bin/echo", ["x"], { stdio: ["ignore", "pipe", "pipe"] }).stdin;
                __c.done = true;
              });
            });
            child.stdin.end("ping-stdin\n");
            "#,
        );
        assert!(
            harness
                .drive_until(Duration::from_secs(10), |h| h
                    .eval::<bool>("__c.done === true"))
                .await,
            "child probe did not finish"
        );
        assert_eq!(
            harness.eval::<String>("JSON.stringify(__c.fail)"),
            r#"["error:spawn /nonexistent-xyz ENOENT|ENOENT|-2|spawn /nonexistent-xyz","close:-2:null"]"#
        );
        assert_eq!(harness.eval::<String>("__c.syncPid"), "number");
        assert_eq!(
            harness.eval::<String>("__c.syncSyscall"),
            "spawnSync /nonexistent-xyz"
        );
        assert_eq!(harness.eval::<String>("__c.stdinOut"), "ping-stdin\n");
        assert_eq!(harness.eval::<i32>("__c.stdinCode"), 0);
        assert_eq!(harness.eval::<String>("__c.execMessage"), "Command failed: exit 3\n");
        assert_eq!(harness.eval::<String>("__c.execStdout"), "undefined");
        assert!(harness.eval::<bool>("__c.execStdoutKey === false"));
        assert_eq!(harness.eval::<String>("String(__c.ignoreStdin)"), "null");
    })
    .await;
}

#[test]
fn fs_rm_rmdir_copyfile_and_async_options_match_node() {
    let dir = temp_dir("fs-semantics");
    let harness = node_harness();
    set_global_path(&harness, "__dir", &dir);
    harness.eval::<()>(
        r#"
        globalThis.__f = {};
        function code(fn) {
          try { fn(); return "<ok>"; }
          catch (error) {
            return [error.name, error.code, error.errno, error.syscall, error.path, error.dest]
              .map(function (value) { return value === undefined ? "undefined" : value; }).join("|");
          }
        }
        fs.mkdirSync(__dir + "/empty");
        fs.writeFileSync(__dir + "/f.txt", "hello");
        fs.mkdirSync(__dir + "/tree/sub", { recursive: true });
        fs.writeFileSync(__dir + "/tree/a.txt", "a");
        fs.writeFileSync(__dir + "/tree/sub/b.txt", "b");
        __f.readDir = code(function () { fs.readFileSync(__dir); });
        __f.rmdirRecursive = code(function () { fs.rmdirSync(__dir + "/tree", { recursive: true }); });
        __f.rmDir = code(function () { fs.rmSync(__dir + "/empty"); });
        __f.rmMissing = code(function () { fs.rmSync(__dir + "/nope"); });
        __f.mkdirExisting = code(function () { fs.mkdirSync(__dir + "/tree", { recursive: true }); });
        fs.writeFileSync(__dir + "/src.txt", "x");
        fs.writeFileSync(__dir + "/dst.txt", "y");
        __f.copyExcl = code(function () {
          fs.copyFileSync(__dir + "/src.txt", __dir + "/dst.txt", fs.constants.COPYFILE_EXCL);
        });
        __f.writeArray = code(function () { fs.writeFileSync(__dir + "/arr.txt", ["x"]); });
        __f.badEncoding = code(function () { fs.readFileSync(__dir + "/f.txt", "nope"); });
        __f.watchMissing = code(function () { fs.watch(__dir + "/missing", function () {}); });
        __f.mkdirReturn = fs.mkdirSync(__dir + "/tree", { recursive: true });
        var stats = fs.statSync(__dir + "/f.txt");
        __f.statsKeys = Object.keys(stats);
        __f.blocks = stats.blocks;
        __f.direntKeys = Object.keys(fs.readdirSync(__dir, { withFileTypes: true })[0]);
        "#,
    );
    assert_eq!(
        harness.eval::<String>("__f.readDir"),
        "Error|EISDIR|-21|read|undefined|undefined"
    );
    assert_eq!(
        harness.eval::<String>("__f.rmdirRecursive"),
        "TypeError|ERR_INVALID_ARG_VALUE|undefined|undefined|undefined|undefined"
    );
    assert_eq!(
        harness.eval::<String>("__f.rmDir"),
        format!(
            "SystemError|ERR_FS_EISDIR|21|rm|{}/empty|undefined",
            dir.to_string_lossy()
        )
    );
    assert_eq!(
        harness.eval::<String>("__f.rmMissing"),
        format!(
            "Error|ENOENT|-2|lstat|{}/nope|undefined",
            dir.to_string_lossy()
        )
    );
    assert_eq!(harness.eval::<String>("__f.mkdirExisting"), "<ok>");
    assert_eq!(
        harness.eval::<String>("__f.copyExcl"),
        format!(
            "Error|EEXIST|-17|copyfile|{}/src.txt|{}/dst.txt",
            dir.to_string_lossy(),
            dir.to_string_lossy()
        )
    );
    assert_eq!(
        harness.eval::<String>("__f.writeArray"),
        "TypeError|ERR_INVALID_ARG_TYPE|undefined|undefined|undefined|undefined"
    );
    assert_eq!(
        harness.eval::<String>("__f.badEncoding"),
        "TypeError|ERR_INVALID_ARG_VALUE|undefined|undefined|undefined|undefined"
    );
    assert_eq!(
        harness.eval::<String>("__f.watchMissing"),
        "Error|ENOENT|-2|watch|".to_owned()
            + &format!("{}/missing|undefined", dir.to_string_lossy())
    );
    assert_eq!(
        harness.eval::<String>("String(__f.mkdirReturn)"),
        "undefined"
    );
    assert_eq!(
        harness.eval::<String>("JSON.stringify(__f.statsKeys)"),
        r#"["dev","mode","nlink","uid","gid","rdev","blksize","ino","size","blocks","atimeMs","mtimeMs","ctimeMs","birthtimeMs"]"#
    );
    assert_eq!(harness.eval::<i32>("__f.blocks"), 8);
    assert_eq!(
        harness.eval::<String>("JSON.stringify(__f.direntKeys)"),
        r#"["name","parentPath"]"#
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "current_thread")]
async fn fs_promises_recursive_readdir_and_throw_if_no_entry() {
    run_local(async {
        let dir = temp_dir("fs-promises");
        let harness = node_harness();
        set_global_path(&harness, "__dir", &dir);
        std::fs::create_dir_all(dir.join("a/b")).unwrap();
        std::fs::write(dir.join("a/x.txt"), b"x").unwrap();
        std::fs::write(dir.join("a/b/y.txt"), b"y").unwrap();
        harness.eval::<()>(
            r#"
            globalThis.__p = {};
            (async function () {
              try {
                __p.recursive = await fs.promises.readdir(__dir + "/a", { recursive: true });
                __p.missing = await fs.promises.stat(__dir + "/nope", { throwIfNoEntry: false });
                __p.mkdir = await fs.promises.mkdir(__dir + "/a", { recursive: true });
                try { await fs.promises.rmdir(__dir + "/a", { recursive: true }); }
                catch (error) { __p.rmdir = [error.name, error.code, error.message]; }
              } catch (error) { __p.error = String(error); }
            })();
            "#,
        );
        assert!(
            harness
                .drive_until(Duration::from_secs(5), |h| h.eval::<bool>(
                    "!!__p.rmdir || !!__p.error"
                ))
                .await,
            "fs.promises probe did not finish"
        );
        assert_eq!(harness.eval::<String>("String(__p.error)"), "undefined");
        assert_eq!(
            harness.eval::<String>("JSON.stringify(__p.recursive.slice().sort())"),
            r#"["b","b/y.txt","x.txt"]"#
        );
        assert_eq!(harness.eval::<String>("String(__p.missing)"), "undefined");
        assert_eq!(harness.eval::<String>("String(__p.mkdir)"), "undefined");
        assert_eq!(
            harness.eval::<String>("JSON.stringify(__p.rmdir)"),
            r#"["TypeError","ERR_INVALID_ARG_VALUE","The property 'options.recursive' is no longer supported. Received true"]"#
        );
        let _ = std::fs::remove_dir_all(&dir);
    })
    .await;
}

#[test]
fn intl_number_and_date_node_edge_cases() {
    let harness = node_harness();
    let script = r#"
    (function () {
      function n(locale, options, value) { return new Intl.NumberFormat(locale, options).format(value); }
      var date = new Date(2024, 0, 2, 3, 4, 5, 0);
      function d(locale, options) { return new Intl.DateTimeFormat(locale, options).format(date); }
      var result = {
        jpyUs: n("en-US", { style: "currency", currency: "JPY" }, 1234.5),
        jpyDe: n("de-DE", { style: "currency", currency: "JPY" }, 1234.5),
        usdGb: n("en-GB", { style: "currency", currency: "USD" }, 1234.5),
        jpyZh: n("zh-CN", { style: "currency", currency: "JPY" }, 1234.5),
        jpyFr: n("fr-FR", { style: "currency", currency: "JPY" }, 1234.5),
        jpyName: n("en-US", { style: "currency", currency: "JPY", currencyDisplay: "name" }, 1234.5),
        percentDe: n("de-DE", { style: "percent" }, 0.5),
        percentAr: n("ar-EG", { style: "percent" }, 0.5),
        negativeSv: n("sv-SE", {}, -50),
        negativeAr: n("ar-EG", {}, -50),
        compactA: n("en-US", { notation: "compact" }, 12345),
        compactB: n("en-US", { notation: "compact" }, 1234),
        compactC: n("en-US", { notation: "compact" }, 123456),
        scientific: n("en-US", { notation: "scientific" }, 12345),
        engineering: n("en-US", { notation: "engineering" }, 12345),
        unitFi: n("fi-FI", { style: "unit", unit: "kilometer" }, 1234),
        unitHi: n("hi-IN", { style: "unit", unit: "kilometer" }, 1234),
        callable: typeof Intl.NumberFormat("en-US"),
        detached: (function () { var f = new Intl.NumberFormat("en-US").format; return f(1234); })(),
        big: (123456789012345678901234n).toLocaleString("en-US"),
        bigDe: (123456789012345678901234n).toLocaleString("de-DE"),
        bigUsd: (123456789012345678n).toLocaleString("en-US", { style: "currency", currency: "USD" }),
        tokyo: d("en-US", { timeZone: "Asia/Tokyo", hour: "numeric", minute: "2-digit" }),
        newYork: d("en-US", { timeZone: "America/New_York" }),
        tzResolved: new Intl.DateTimeFormat("en-US", { timeZone: "Asia/Tokyo" }).resolvedOptions().timeZone,
        parts: new Intl.DateTimeFormat("en-US", { timeZone: "UTC" }).formatToParts(date),
        fullUtc: d("en-US", { timeZone: "UTC", timeStyle: "full" }),
        longUtc: d("en-US", { timeZone: "UTC", timeStyle: "long" }),
        fractional: d("en-US", { timeZone: "UTC", second: "numeric", fractionalSecondDigits: 3, hour: "numeric", minute: "2-digit" }),
        dayPeriod: d("en-US", { timeZone: "UTC", hour: "numeric", dayPeriod: "long" }),
        hour2: d("en-US", { timeZone: "UTC", hour: "2-digit", minute: "2-digit" }),
        minuteOnly: d("en-US", { timeZone: "UTC", minute: "numeric" }),
        fullBoth: d("en-US", { timeZone: "UTC", dateStyle: "full", timeStyle: "full" }),
        mediumBoth: d("en-US", { timeZone: "UTC", dateStyle: "medium", timeStyle: "short" }),
        canonical: Intl.getCanonicalLocales(["de-DE-u-co-phonebk"]),
        supported: Intl.NumberFormat.supportedLocalesOf(["fr-FR", "xx-YY", "de-DE"]),
      };
      try { new Intl.DateTimeFormat("en-US", { timeZone: "Mars/Base" }); }
      catch (error) { result.badZone = [error.name, error.message]; }
      try { new Intl.NumberFormat("en-US", { useGrouping: "never" }); }
      catch (error) { result.badGrouping = [error.name, error.message]; }
      result.min2 = new Intl.NumberFormat("en-US", { useGrouping: "min2" }).resolvedOptions().useGrouping;
      return JSON.stringify(result);
    })()
    "#;
    let value: serde_json::Value = eval_json(&harness, script);
    assert_eq!(value["jpyUs"], "¥1,235");
    assert_eq!(value["jpyDe"], "1.235\u{a0}¥");
    assert_eq!(value["usdGb"], "US$1,234.50");
    assert_eq!(value["jpyZh"], "JP¥1,235");
    assert_eq!(value["jpyFr"], "1\u{202f}235\u{a0}JPY");
    assert_eq!(value["jpyName"], "1,235 Japanese yen");
    assert_eq!(value["percentDe"], "50\u{a0}%");
    assert_eq!(value["percentAr"], "٥٠٪؜");
    assert_eq!(value["negativeSv"], "−50");
    assert_eq!(value["negativeAr"], "؜-٥٠");
    assert_eq!(value["compactA"], "12K");
    assert_eq!(value["compactB"], "1.2K");
    assert_eq!(value["compactC"], "123K");
    assert_eq!(value["scientific"], "1.235E4");
    assert_eq!(value["engineering"], "12.345E3");
    assert_eq!(value["unitFi"], "1\u{a0}234 km");
    assert_eq!(value["unitHi"], "1,234 कि॰मी॰");
    assert_eq!(value["callable"], "object");
    assert_eq!(value["detached"], "1,234");
    assert_eq!(value["big"], "123,456,789,012,345,678,901,234");
    assert_eq!(value["bigDe"], "123.456.789.012.345.678.901.234");
    assert_eq!(value["bigUsd"], "$123,456,789,012,345,678.00");
    assert_eq!(value["tokyo"], "4:04 AM");
    assert_eq!(value["newYork"], "1/1/2024");
    assert_eq!(value["tzResolved"], "Asia/Tokyo");
    assert_eq!(
        value["parts"].as_array().unwrap().len(),
        5,
        "formatToParts: {}",
        value["parts"]
    );
    assert_eq!(value["fullUtc"], "7:04:05 PM Coordinated Universal Time");
    assert_eq!(value["longUtc"], "7:04:05 PM UTC");
    assert_eq!(value["fractional"], "7:04:05.000 PM");
    assert_eq!(value["dayPeriod"], "7 in the evening");
    assert_eq!(value["hour2"], "07:04 PM");
    assert_eq!(value["minuteOnly"], "4");
    assert_eq!(
        value["fullBoth"],
        "Monday, January 1, 2024 at 7:04:05 PM Coordinated Universal Time"
    );
    assert_eq!(value["mediumBoth"], "Jan 1, 2024, 7:04 PM");
    assert_eq!(value["canonical"][0], "de-DE-u-co-phonebk");
    assert_eq!(value["supported"][0], "fr-FR");
    assert_eq!(value["supported"][1], "de-DE");
    assert_eq!(value["supported"].as_array().unwrap().len(), 2);
    assert_eq!(value["badZone"][0], "RangeError");
    assert_eq!(
        value["badZone"][1],
        "Invalid time zone specified: Mars/Base"
    );
    assert_eq!(value["badGrouping"][0], "RangeError");
    assert_eq!(
        value["badGrouping"][1],
        "Value never out of range for Intl.NumberFormat options property useGrouping"
    );
    assert_eq!(value["min2"], "min2");
}

#[test]
fn process_kill_reports_node_errno() {
    let harness = node_harness();
    harness.eval::<()>(
        r#"
        globalThis.__k = {};
        try { process.kill(999999, "SIGTERM"); }
        catch (error) { __k.kill = [error.name, error.message, error.code, error.errno, error.syscall]; }
        try { process.kill("x", "SIGTERM"); }
        catch (error) { __k.pid = [error.name, error.message, error.code]; }
        try { process.kill(process.pid, "NOPE"); }
        catch (error) { __k.signal = [error.name, error.message, error.code]; }
        "#,
    );
    assert_eq!(
        harness.eval::<String>("JSON.stringify(__k.kill)"),
        r#"["Error","kill ESRCH","ESRCH",-3,"kill"]"#
    );
    assert_eq!(
        harness.eval::<String>("JSON.stringify(__k.pid)"),
        r#"["TypeError","The \"pid\" argument must be of type number. Received type string ('x')","ERR_INVALID_ARG_TYPE"]"#
    );
    assert_eq!(
        harness.eval::<String>("JSON.stringify(__k.signal)"),
        r#"["TypeError","Unknown signal: NOPE","ERR_UNKNOWN_SIGNAL"]"#
    );
}
