//! Node 全局（设计 §6.2）：console/process/Buffer/TextEncoder/TextDecoder/URL/
//! fetch/Web Streams/crypto/structuredClone/atob/btoa。
//!
//! 每个用例先 `install_intl`（§9.2 第 3 步）再 `install`（第 4 步）。

mod support;

use std::time::Duration;

use kiana_extensions::loader::js_host;
use support::{Harness, run_local};

fn node_harness() -> Harness {
    let harness = Harness::new();
    js_host::install_intl(&harness.engine).expect("install intl");
    js_host::install(&harness.engine).expect("install node globals");
    harness
}

#[test]
fn installs_the_expected_globals() {
    let harness = node_harness();
    assert!(harness.eval::<bool>(
        "typeof Buffer === 'function' && typeof process === 'object' && typeof console === 'object'"
    ));
    assert!(
        harness
            .eval::<bool>("typeof TextEncoder === 'function' && typeof TextDecoder === 'function'")
    );
    assert!(
        harness.eval::<bool>("typeof URL === 'function' && typeof URLSearchParams === 'function'")
    );
    assert!(harness.eval::<bool>("typeof fetch === 'function' && typeof Response === 'function'"));
    assert!(harness.eval::<bool>(
        "typeof ReadableStream === 'function' && typeof WritableStream === 'function'"
    ));
    assert!(harness.eval::<bool>(
        "typeof structuredClone === 'function' && typeof atob === 'function' && typeof btoa === 'function'"
    ));
    assert!(
        harness.eval::<bool>(
            "typeof crypto === 'object' && typeof crypto.subtle.digest === 'function'"
        )
    );
    assert_eq!(
        harness.eval::<String>("process.platform"),
        if cfg!(target_os = "windows") {
            "win32"
        } else if cfg!(target_os = "macos") {
            "darwin"
        } else {
            "linux"
        }
    );
    assert_eq!(harness.eval::<String>("process.argv[0]"), "kiana");
    assert!(harness.eval::<bool>("process.argv[1].length > 0"));
    assert_eq!(harness.eval::<String>("process.versions.node"), "22.0.0");
    assert!(harness.eval::<bool>("process.env.PATH !== undefined"));
    assert!(harness.eval::<bool>("process.cwd() === __kiana_host.cwd()"));
    assert_eq!(harness.eval::<i32>("process.stderr.columns"), 80);
    assert!(harness.eval::<bool>("global === globalThis"));
}

#[test]
fn buffer_semantics() {
    let harness = node_harness();
    assert_eq!(
        harness.eval::<String>("Buffer.from('hello').toString('hex')"),
        "68656c6c6f"
    );
    assert_eq!(
        harness.eval::<String>("Buffer.from('hello').toString('base64')"),
        "aGVsbG8="
    );
    assert_eq!(
        harness.eval::<String>("Buffer.from('aGVsbG8=', 'base64').toString('utf8')"),
        "hello"
    );
    assert_eq!(harness.eval::<i32>("Buffer.byteLength('漢字')"), 6);
    assert_eq!(
        harness.eval::<String>("Buffer.from([0xe2, 0x28, 0xa1]).toString('utf8')"),
        "\u{fffd}(\u{fffd}"
    );
    assert_eq!(
        harness.eval::<i32>("Buffer.concat([Buffer.from('ab'), Buffer.from('cd')]).length"),
        4
    );
    assert!(harness.eval::<bool>("Buffer.isBuffer(Buffer.alloc(3))"));
    assert!(harness.eval::<bool>("Buffer.alloc(4, 7)[3] === 7"));
    assert_eq!(
        harness.eval::<String>("JSON.stringify(Buffer.from([1,2]).toJSON())"),
        "{\"type\":\"Buffer\",\"data\":[1,2]}"
    );
    assert_eq!(
        harness.eval::<i32>("Buffer.from('abcd').readUInt32BE(0)"),
        1633837924
    );
    assert_eq!(
        harness.eval::<String>("Buffer.from('abcd').subarray(1).toString()"),
        "bcd"
    );
}

#[test]
fn text_encoder_and_streaming_decoder() {
    let harness = node_harness();
    assert_eq!(
        harness.eval::<String>("new TextEncoder().encode('é').length.toString()"),
        "2"
    );
    // { stream: true } 必须保留不完整的 UTF-8 序列（SSE 按块 flush）。
    assert!(harness.eval::<bool>(
        r#"
        (function () {
          var decoder = new TextDecoder();
          var first = decoder.decode(new Uint8Array([0xe6, 0xbc]), { stream: true });
          var second = decoder.decode(new Uint8Array([0xa2]), { stream: true });
          var third = decoder.decode();
          return first === "" && second === "漢" && third === "";
        })()
        "#
    ));
    assert_eq!(
        harness.eval::<String>("new TextDecoder('utf-8').decode(new Uint8Array([0x41, 0x42]))"),
        "AB"
    );
    assert!(harness.try_eval::<()>("new TextDecoder('latin1')").is_err());
}

#[test]
fn url_and_urlsearchparams() {
    let harness = node_harness();
    assert!(harness.eval::<bool>(
        "new URL('https://user:pw@example.com:8443/a/b?x=1&y=2#z').hostname === 'example.com'"
    ));
    assert_eq!(
        harness.eval::<String>("new URL('https://example.com/a/b?x=1#z').pathname"),
        "/a/b"
    );
    assert_eq!(
        harness.eval::<String>("new URL('https://example.com/a/b?x=1#z').search"),
        "?x=1"
    );
    assert_eq!(
        harness.eval::<String>("new URL('https://example.com/a/b?x=1#z').origin"),
        "https://example.com"
    );
    assert_eq!(
        harness.eval::<String>("new URL('/next', 'https://example.com/base').pathname"),
        "/next"
    );
    assert_eq!(
        harness.eval::<String>("new URLSearchParams({a: '1', b: 'x y'}).toString()"),
        "a=1&b=x+y"
    );
    assert_eq!(
        harness.eval::<String>("new URLSearchParams('a=1&a=2').getAll('a').join(',')"),
        "1,2"
    );
    assert_eq!(
        harness.eval::<String>("new URL('https://e.com/?q=%E6%BC%A2').searchParams.get('q')"),
        "漢"
    );
}

#[test]
fn atob_btoa_and_crypto() {
    let harness = node_harness();
    assert_eq!(harness.eval::<String>("btoa('hello')"), "aGVsbG8=");
    assert_eq!(harness.eval::<String>("atob('aGVsbG8=')"), "hello");
    assert_eq!(
        harness.eval::<i32>("crypto.getRandomValues(new Uint8Array(8)).length"),
        8
    );
    assert!(harness.eval::<bool>(
        "/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(crypto.randomUUID())"
    ));
    assert!(harness.eval::<bool>(
        "crypto.getRandomValues(new Uint8Array(16)).some(function (v) { return v !== 0; })"
    ));
}

#[test]
fn structured_clone_supports_the_documented_subset() {
    let harness = node_harness();
    assert!(harness.eval::<bool>(
        r#"
        (function () {
          var source = {
            a: undefined,
            list: [1, undefined, {b: 2}],
            when: new Date(1000),
            map: new Map([["k", {x: 1}]]),
            set: new Set([1, 2]),
            buffer: new Uint8Array([1, 2, 3]).buffer,
            regexp: /a/gi
          };
          source.self = source;
          var clone = structuredClone(source);
          return clone.self === clone
            && clone.list[1] === undefined
            && "a" in clone
            && clone.when.getTime() === 1000
            && clone.map.get("k").x === 1
            && clone.set.has(2)
            && new Uint8Array(clone.buffer)[2] === 3
            && clone.regexp.source === "a"
            && clone.regexp.flags === "gi";
        })()
        "#
    ));
    let error = harness.expect_error("structuredClone(function foo() {})");
    assert_eq!(error.name, "DataCloneError");
    // QuickJS's Function.prototype.toString differs from V8 (§7.4 item 6); the
    // DataCloneError shape and suffix are the portable part.
    assert!(error.message.ends_with(" could not be cloned."));
    let error = harness.expect_error("structuredClone(new WeakMap())");
    assert_eq!(error.name, "DataCloneError");
    assert_eq!(error.message, "#<WeakMap> could not be cloned.");
}

#[test]
fn readable_stream_repumps_after_attaching_the_waiter() {
    let harness = node_harness();
    // 设计 §10.4 第 4 条：pull 在 read() 之前同步 enqueue 不得挂起。
    assert!(harness.eval::<bool>(
        r#"
        (function () {
          var stream = new ReadableStream({
            pull: function (controller) { controller.enqueue("x"); controller.close(); }
          });
          globalThis.__chunk = null;
          stream.getReader().read().then(function (result) { __chunk = result.value; });
          return true;
        })()
        "#
    ));
    harness.pump();
    assert_eq!(harness.eval::<String>("__chunk"), "x");
}

// ---------------------------------------------------------------------------
// 评审回归：TextDecoder / TextEncoder / atob / btoa
// ---------------------------------------------------------------------------

#[test]
fn text_decoder_utf8_replacement_and_streaming_match_whatwg() {
    let harness = node_harness();
    assert_eq!(
        harness.eval::<String>("Buffer.from([0xE4,0xB8,0x41]).toString('utf8')"),
        "\u{fffd}A"
    );
    assert_eq!(
        harness.eval::<String>("Buffer.from([0xE4,0xB8]).toString('utf8')"),
        "\u{fffd}"
    );
    assert_eq!(
        harness.eval::<String>("Buffer.from([0xF0,0x9F,0x41]).toString('utf8')"),
        "\u{fffd}A"
    );
    // Invalid data must be flushed per chunk, not delayed by one chunk.
    assert_eq!(
        harness.eval::<String>(
            r#"
            (function () {
              var decoder = new TextDecoder();
              var first = decoder.decode(new Uint8Array([0xE4, 0x41]), {stream: true});
              var second = decoder.decode(new Uint8Array([0x42]), {stream: true});
              return JSON.stringify([first, second]);
            })()
            "#
        ),
        r#"["�A","B"]"#
    );
    // A BOM split across chunks is still stripped.
    assert_eq!(
        harness.eval::<String>(
            r#"
            (function () {
              var decoder = new TextDecoder();
              var first = decoder.decode(new Uint8Array([0xEF]), {stream: true});
              var second = decoder.decode(new Uint8Array([0xBB, 0xBF, 0x41]), {stream: true});
              return JSON.stringify([first, second]);
            })()
            "#
        ),
        r#"["","A"]"#
    );
    let error = harness
        .expect_error("new TextDecoder('utf-8', {fatal: true}).decode(new Uint8Array([0xFF]))");
    assert_eq!(error.name, "TypeError");
    assert_eq!(
        error.message,
        "The encoded data was not valid for encoding utf-8"
    );
}

#[test]
fn text_encoder_encode_into_does_not_write_partial_code_points() {
    let harness = node_harness();
    assert_eq!(
        harness.eval::<String>(
            r#"
            (function () {
              var encoder = new TextEncoder();
              var first = new Uint8Array(4);
              var second = new Uint8Array(2);
              return JSON.stringify([
                encoder.encodeInto("a😀", first),
                Array.prototype.slice.call(first),
                encoder.encodeInto("😀", second),
                Array.prototype.slice.call(second),
              ]);
            })()
            "#
        ),
        r#"[{"read":1,"written":1},[97,0,0,0],{"read":0,"written":0},[0,0]]"#
    );
}

#[test]
fn atob_btoa_messages_and_validation_match_node() {
    let harness = node_harness();
    let error = harness.expect_error("atob('a')");
    assert_eq!(error.name, "InvalidCharacterError");
    assert_eq!(
        error.message,
        "The string to be decoded is not correctly encoded."
    );
    let error = harness.expect_error("atob('a===')");
    assert_eq!(error.name, "InvalidCharacterError");
    assert_eq!(error.message, "Invalid character");
    let error = harness.expect_error("btoa('\u{100}')");
    assert_eq!(error.name, "InvalidCharacterError");
    assert_eq!(error.message, "Invalid character");
    assert_eq!(harness.eval::<String>("atob('ab')"), "i");
    assert_eq!(harness.eval::<String>("atob('abc')"), "i\u{b7}");
}

#[test]
fn structured_clone_resets_regexp_and_preserves_error_subtypes() {
    let harness = node_harness();
    assert!(harness.eval::<bool>(
        r#"
        (function () {
          var regexp = /a/g;
          regexp.lastIndex = 2;
          var clone = structuredClone(regexp);
          var date = new Date(0);
          var error = structuredClone(new TypeError("boom"));
          return clone.lastIndex === 0 &&
            error instanceof TypeError &&
            error.name === "TypeError" &&
            error.message === "boom" &&
            structuredClone(date).getTime() === 0;
        })()
        "#
    ));
}

// ---------------------------------------------------------------------------
// 评审回归：URL / URLSearchParams
// ---------------------------------------------------------------------------

#[test]
fn url_search_params_are_live_and_statics_exist() {
    let harness = node_harness();
    assert_eq!(
        harness.eval::<String>(
            r#"
            (function () {
              var url = new URL("http://a/b");
              url.searchParams.append("q", "1");
              var first = [url.href, url.search];
              url.searchParams.set("q", "2");
              var second = [url.href, url.searchParams.size];
              var parsed = URL.parse("not a url");
              function message() { try { new URL("not a url"); } catch (error) { return [error.name, error.message, error.code]; } }
              return JSON.stringify({
                first: first, second: second, parsed: parsed,
                canParse: [URL.canParse("http://a"), URL.canParse("nope")],
                error: message(),
              });
            })()
            "#
        ),
        r#"{"first":["http://a/b?q=1","?q=1"],"second":["http://a/b?q=2",1],"parsed":null,"canParse":[true,false],"error":["TypeError","Invalid URL","ERR_INVALID_URL"]}"#
    );
}

// ---------------------------------------------------------------------------
// 评审回归：Web Streams
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn readable_stream_async_pull_and_concurrent_reads_make_progress() {
    run_local(async {
        let harness = node_harness();
        harness.eval::<()>(
            r#"
            globalThis.__stream = { values: [] };
            (async function () {
              var index = 0;
              var stream = new ReadableStream({
                pull: function (controller) {
                  return new Promise(function (resolve) {
                    setTimeout(function () {
                      if (index < 3) controller.enqueue(index++);
                      else controller.close();
                      resolve();
                    }, 1);
                  });
                }
              });
              for await (const value of stream) __stream.values.push(value);
              __stream.done = true;
            })();
            "#,
        );
        assert!(
            harness
                .drive_until(Duration::from_secs(10), |h| h
                    .eval::<bool>("__stream.done === true"))
                .await,
            "for-await over an async pull source hung"
        );
        assert_eq!(
            harness.eval::<String>("JSON.stringify(__stream.values)"),
            "[0,1,2]"
        );

        harness.eval::<()>(
            r#"
            globalThis.__concurrent = [];
            (async function () {
              var index = 0;
              var stream = new ReadableStream({
                pull: function (controller) {
                  return new Promise(function (resolve) {
                    setTimeout(function () {
                      if (index < 2) controller.enqueue(index++);
                      else controller.close();
                      resolve();
                    }, 1);
                  });
                }
              });
              var reader = stream.getReader();
              var results = await Promise.all([reader.read(), reader.read()]);
              __concurrent = results.map(function (result) { return result.value; });
            })();
            "#,
        );
        assert!(
            harness.drive_until(Duration::from_secs(10), |h| h
                .eval::<String>("JSON.stringify(__concurrent)")
                == "[0,1]")
                .await,
            "concurrent read() calls hung"
        );

        harness.eval::<()>(
            r#"
            globalThis.__readerClosed = {};
            (function () {
              var closedA = new ReadableStream({ start: function (c) { c.close(); } });
              var readerA = closedA.getReader();
              readerA.closed.then(function (value) { __readerClosed.closed = String(value); });
              var errored = new ReadableStream({ start: function (c) { c.error(new Error("boom")); } });
              var readerB = errored.getReader();
              readerB.closed.then(
                function () { __readerClosed.errored = "resolved"; },
                function (error) { __readerClosed.errored = error.message; }
              );
            })();
            "#,
        );
        harness.pump();
        assert_eq!(
            harness.eval::<String>("JSON.stringify(__readerClosed)"),
            r#"{"closed":"undefined","errored":"boom"}"#
        );
    })
    .await;
}

// ---------------------------------------------------------------------------
// 评审回归：process.platform / arch 的 Node 名称
// ---------------------------------------------------------------------------

#[test]
fn process_platform_arch_and_os_machine_use_node_names() {
    let harness = node_harness();
    let expected_platform = if cfg!(target_os = "windows") {
        "win32"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    };
    assert_eq!(
        harness.eval::<String>("process.platform"),
        expected_platform
    );
    let expected_arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "x86" => "ia32",
        "aarch64" => "arm64",
        other => other,
    };
    assert_eq!(harness.eval::<String>("process.arch"), expected_arch);
    assert!(harness.eval::<bool>("process.versions.node === '22.0.0'"));
}

/// Node's Buffer copy/validation/UTF-8 semantics (reviewer regressions).
#[test]
fn buffer_from_copies_validation_and_utf8_replacement_match_node() {
    let harness = node_harness();
    assert_eq!(
        harness.eval::<String>(
            r#"
            (function () {
              var source = new Uint8Array([1, 2, 3]);
              var copied = Buffer.from(source);
              source[0] = 9;
              var original = Buffer.from([1, 2, 3]);
              var copiedBuffer = Buffer.from(original);
              original[0] = 9;
              var shared = original.subarray(0, 2);
              return JSON.stringify([
                copied[0], copied.buffer === source.buffer,
                copiedBuffer[0], copiedBuffer.buffer === original.buffer,
                shared.buffer === original.buffer,
              ]);
            })()
            "#
        ),
        "[1,false,1,false,true]"
    );
    assert_eq!(
        harness.eval::<String>(
            r#"
            JSON.stringify([
              Buffer.alloc(3).fill().toString("hex"),
              Buffer.from("abzzcd", "hex").toString("hex"),
              Buffer.from("é", "ascii").toString("hex"),
              Buffer.from([0xff, 0x80]).toString("ascii").length,
              Buffer.from([0xff, 0x80]).toString("ascii").charCodeAt(0),
              Buffer.from(new Uint16Array([0x1234, 0x5678])).toString("hex"),
            ])
            "#
        ),
        r#"["000000","ab","e9",2,127,"3478"]"#
    );
    let errors = harness.eval::<String>(
        r#"
        (function () {
          function code(fn) {
            try { fn(); return "ok"; }
            catch (error) { return error.name + ":" + (error.code || ""); }
          }
          return JSON.stringify([
            code(function () { Buffer.alloc(1).readUInt32BE(0); }),
            code(function () { Buffer.alloc(4).writeUInt32BE(-1, 0); }),
            code(function () { Buffer.from(5); }),
            code(function () { Buffer.alloc("x"); }),
            code(function () { Buffer.byteLength(5); }),
            code(function () { Buffer.concat([Buffer.from("a"), [1, 2]]); }),
          ]);
        })()
        "#,
    );
    assert_eq!(
        errors,
        r#"["RangeError:ERR_BUFFER_OUT_OF_BOUNDS","RangeError:ERR_OUT_OF_RANGE","TypeError:ERR_INVALID_ARG_TYPE","TypeError:ERR_INVALID_ARG_TYPE","TypeError:ERR_INVALID_ARG_TYPE","TypeError:ERR_INVALID_ARG_TYPE"]"#
    );
    assert_eq!(
        harness.eval::<String>(
            r#"
            JSON.stringify([
              Buffer.from([0xe0, 0x80]).toString("utf8"),
              Buffer.from([0xf4, 0x90, 0x80]).toString("utf8"),
              Buffer.from([0xed, 0xa0, 0x80]).toString("utf8"),
              Buffer.from([0x61, 0xe1]).toString("utf8"),
            ])
            "#
        ),
        r#"["��","���","���","a�"]"#
    );
}

/// 回归（major）：`--features webassembly` 必须真的绑定 wasmtime 后端。
///
/// 修复前 feature 只声明了依赖，没有源码引用：`WebAssembly` 仍为 `undefined`，
/// doom-overlay 的 `/doom-overlay` 游戏体永远无法运行。这里用一个手写的
/// `(module (func (export "add") (param i32 i32) (result i32) ...))` 验证
/// `Module` / `Instance` / 导出函数调用是真实现。
#[cfg(feature = "webassembly")]
#[test]
fn installs_a_working_webassembly_backend() {
    let harness = node_harness();
    assert!(harness.eval::<bool>("typeof WebAssembly === 'object'"));
    let state: String = harness.try_eval::<String>(
        r#"(() => {
            const bytes = new Uint8Array([
                0, 97, 115, 109, 1, 0, 0, 0,
                1, 7, 1, 96, 2, 127, 127, 1, 127,
                3, 2, 1, 0,
                7, 7, 1, 3, 97, 100, 100, 0, 0,
                10, 9, 1, 7, 0, 32, 0, 32, 1, 106, 11,
            ]);
            const module = new WebAssembly.Module(bytes);
            const instance = new WebAssembly.Instance(module, {});
            return JSON.stringify({
                valid: WebAssembly.validate(bytes),
                exports: WebAssembly.Module.exports(module).map((entry) => entry.name + ":" + entry.kind),
                sum: instance.exports.add(2, 3),
                compileType: typeof WebAssembly.compile,
                instantiateType: typeof WebAssembly.instantiate,
            });
        })()"#,
    ).unwrap_or_else(|error| panic!("wasm eval failed: {error:?}"));
    let state: serde_json::Value = serde_json::from_str(&state).expect("valid state");
    assert_eq!(state["valid"], true);
    assert_eq!(state["exports"], serde_json::json!(["add:function"]));
    assert_eq!(state["sum"], 5);
    assert_eq!(state["compileType"], "function");
    assert_eq!(state["instantiateType"], "function");

    // `WebAssembly.instantiate(bytes, imports)`（Promise 形式）也走真实 wasmtime。
    let scheduled: String = harness.eval::<String>(
        r#"(() => {
        const bytes = new Uint8Array([
            0, 97, 115, 109, 1, 0, 0, 0,
            1, 7, 1, 96, 2, 127, 127, 1, 127,
            3, 2, 1, 0,
            7, 7, 1, 3, 97, 100, 100, 0, 0,
            10, 9, 1, 7, 0, 32, 0, 32, 1, 106, 11,
        ]);
        WebAssembly.instantiate(bytes, {}).then(
            (result) => { globalThis.__wasmProbe = "ok:" + result.instance.exports.add(4, 5); },
            (error) => { globalThis.__wasmProbe = "err:" + String(error && error.message); },
        );
        return "scheduled";
    })()"#,
    );
    assert_eq!(scheduled, "scheduled");
    harness.pump();
    let probe: String = harness
        .eval::<String>("globalThis.__wasmProbe === undefined ? 'none' : globalThis.__wasmProbe");
    assert_eq!(probe, "ok:9");
}
