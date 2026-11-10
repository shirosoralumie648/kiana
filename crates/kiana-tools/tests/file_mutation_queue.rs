//! 移植 `test/file-mutation-queue.test.ts` 的队列部分。

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kiana_tools::file_mutation_queue::{FileMutationQueue, with_file_mutation_queue};

fn delay(ms: u64) -> impl std::future::Future<Output = ()> {
    tokio::time::sleep(Duration::from_millis(ms))
}

#[tokio::test]
async fn serializes_operations_for_the_same_file() {
    let order: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let path = Path::new("/tmp/file-mutation-queue-same");

    let first_order = Arc::clone(&order);
    let first = with_file_mutation_queue(path, move || async move {
        first_order.lock().expect("lock").push("first:start");
        delay(30).await;
        first_order.lock().expect("lock").push("first:end");
    });
    let second_order = Arc::clone(&order);
    let second = with_file_mutation_queue(path, move || async move {
        second_order.lock().expect("lock").push("second:start");
        second_order.lock().expect("lock").push("second:end");
    });

    let (first_result, second_result) = tokio::join!(first, second);
    first_result.expect("first");
    second_result.expect("second");
    assert_eq!(
        *order.lock().expect("lock"),
        vec!["first:start", "first:end", "second:start", "second:end"]
    );
}

#[tokio::test]
async fn allows_different_files_to_proceed_in_parallel() {
    let order: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));

    let a_order = Arc::clone(&order);
    let b_order = Arc::clone(&order);
    let (a_result, b_result) = tokio::join!(
        with_file_mutation_queue("/tmp/file-mutation-queue-a", move || async move {
            a_order.lock().expect("lock").push("a:start");
            delay(30).await;
            a_order.lock().expect("lock").push("a:end");
        }),
        with_file_mutation_queue("/tmp/file-mutation-queue-b", move || async move {
            b_order.lock().expect("lock").push("b:start");
            delay(30).await;
            b_order.lock().expect("lock").push("b:end");
        }),
    );
    a_result.expect("a");
    b_result.expect("b");

    let recorded = order.lock().expect("lock");
    let position = |needle: &str| {
        recorded
            .iter()
            .position(|entry| *entry == needle)
            .expect(needle)
    };
    assert!(position("a:start") < position("a:end"));
    assert!(position("b:start") < position("b:end"));
    assert!(position("b:start") < position("a:end"));
}

#[cfg(unix)]
#[tokio::test]
async fn uses_the_same_queue_for_symlink_aliases() {
    let dir = tempfile::tempdir().expect("tempdir");
    let target_path = dir.path().join("target.txt");
    let symlink_path = dir.path().join("alias.txt");
    std::fs::write(&target_path, "hello\n").expect("write");
    std::os::unix::fs::symlink(&target_path, &symlink_path).expect("symlink");

    let order: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let target_order = Arc::clone(&order);
    let target = async {
        with_file_mutation_queue(&target_path, move || async move {
            target_order.lock().expect("lock").push("target:start");
            delay(30).await;
            target_order.lock().expect("lock").push("target:end");
        })
        .await
        .expect("target");
    };
    let alias_order = Arc::clone(&order);
    let alias = async {
        with_file_mutation_queue(&symlink_path, move || async move {
            alias_order.lock().expect("lock").push("alias:start");
            alias_order.lock().expect("lock").push("alias:end");
        })
        .await
        .expect("alias");
    };
    tokio::join!(target, alias);

    assert_eq!(
        *order.lock().expect("lock"),
        vec!["target:start", "target:end", "alias:start", "alias:end"]
    );
}

#[tokio::test]
async fn file_urls_are_resolved_literally_like_node_path_resolve() {
    // 回归（TS `getMutationQueueKey` 的 `path.resolve`）：`file:///…` 是普通相对路径，
    // 不会被转成本地文件路径。两个路径在 TS 里是不同的 key，因此操作可以并行。
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("target.txt");
    std::fs::write(&target, "hello\n").expect("write");
    let file_url = format!("file://{}", target.display());

    let queue = Arc::new(FileMutationQueue::new());
    let started = Arc::new(tokio::sync::Notify::new());
    let release_first = Arc::new(tokio::sync::Notify::new());
    let second_started = Arc::new(Mutex::new(false));

    let first = {
        let queue = Arc::clone(&queue);
        let started = Arc::clone(&started);
        let release_first = Arc::clone(&release_first);
        let file_url = file_url.clone();
        tokio::spawn(async move {
            queue
                .with(Path::new(&file_url), move || async move {
                    started.notify_one();
                    release_first.notified().await;
                })
                .await
                .expect("first operation");
        })
    };
    started.notified().await;

    let second = {
        let queue = Arc::clone(&queue);
        let second_started = Arc::clone(&second_started);
        let target = target.clone();
        tokio::spawn(async move {
            queue
                .with(&target, move || async move {
                    *second_started.lock().expect("lock") = true;
                })
                .await
                .expect("second operation");
        })
    };

    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        if *second_started.lock().expect("lock") {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "a file:// path must not share the queue key with the resolved path"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    release_first.notify_one();
    first.await.expect("first operation");
    second.await.expect("second operation");
}

#[tokio::test]
async fn file_url_spellings_normalize_to_the_same_key() {
    // `path.resolve(cwd, "file:///x")` 与 `path.resolve(cwd, "./file:///x")` 在 Node 里
    // 都归一化成 `<cwd>/file:/x`，所以两个写法必须共享同一条队列。
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("target.txt");
    std::fs::write(&target, "hello\n").expect("write");
    let plain = format!("file://{}", target.display());
    let dotted = format!("./{plain}");

    let queue = Arc::new(FileMutationQueue::new());
    let started = Arc::new(tokio::sync::Notify::new());
    let release_first = Arc::new(tokio::sync::Notify::new());
    let second_started = Arc::new(Mutex::new(false));

    let first = {
        let queue = Arc::clone(&queue);
        let started = Arc::clone(&started);
        let release_first = Arc::clone(&release_first);
        tokio::spawn(async move {
            queue
                .with(Path::new(&plain), move || async move {
                    started.notify_one();
                    release_first.notified().await;
                })
                .await
                .expect("first operation");
        })
    };
    started.notified().await;

    let second = {
        let queue = Arc::clone(&queue);
        let second_started = Arc::clone(&second_started);
        tokio::spawn(async move {
            queue
                .with(Path::new(&dotted), move || async move {
                    *second_started.lock().expect("lock") = true;
                })
                .await
                .expect("second operation");
        })
    };

    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !*second_started.lock().expect("lock"),
        "equivalent file:// spellings must share the queue key"
    );

    release_first.notify_one();
    first.await.expect("first operation");
    second.await.expect("second operation");
    assert!(*second_started.lock().expect("lock"));
}

#[tokio::test]
async fn second_waiter_does_not_start_before_the_first_finishes() {
    let queue = Arc::new(FileMutationQueue::new());
    let started = Arc::new(tokio::sync::Notify::new());
    let release_first = Arc::new(tokio::sync::Notify::new());
    let second_started = Arc::new(Mutex::new(false));

    let first = {
        let queue = Arc::clone(&queue);
        let started = Arc::clone(&started);
        let release_first = Arc::clone(&release_first);
        async move {
            queue
                .with(
                    Path::new("/tmp/file-mutation-queue-blocking"),
                    move || async move {
                        started.notify_one();
                        release_first.notified().await;
                    },
                )
                .await
                .expect("first");
        }
    };
    let second = {
        let queue = Arc::clone(&queue);
        let second_started = Arc::clone(&second_started);
        async move {
            queue
                .with(
                    Path::new("/tmp/file-mutation-queue-blocking"),
                    move || async move {
                        *second_started.lock().expect("lock") = true;
                    },
                )
                .await
                .expect("second");
        }
    };

    let joiner = tokio::spawn(async move {
        tokio::join!(first, second);
    });

    started.notified().await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!*second_started.lock().expect("lock"));
    release_first.notify_one();
    joiner.await.expect("join task");
    assert!(*second_started.lock().expect("lock"));
}

#[tokio::test]
async fn injectable_queues_are_independent() {
    let first = FileMutationQueue::new();
    let second = FileMutationQueue::new();
    let order: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));

    let one = Arc::clone(&order);
    let two = Arc::clone(&order);
    let (one_result, two_result) = tokio::join!(
        first.with(Path::new("/tmp/independent"), move || async move {
            one.lock().expect("lock").push("one:start");
            delay(30).await;
            one.lock().expect("lock").push("one:end");
        }),
        second.with(Path::new("/tmp/independent"), move || async move {
            two.lock().expect("lock").push("two:start");
            two.lock().expect("lock").push("two:end");
        }),
    );
    one_result.expect("one");
    two_result.expect("two");

    // 两个独立队列互不等待：第二个不必等第一个结束。
    let recorded = order.lock().expect("lock");
    let position = |needle: &str| {
        recorded
            .iter()
            .position(|entry| *entry == needle)
            .expect(needle)
    };
    assert!(position("one:start") < position("one:end"));
    assert!(position("two:start") < position("two:end"));
    assert!(position("two:end") < position("one:end"));
}

#[tokio::test]
async fn a_panicking_operation_does_not_deadlock_later_waiters() {
    let queue = Arc::new(FileMutationQueue::new());
    let panicking = Arc::clone(&queue);
    let handle = tokio::spawn(async move {
        let _ = panicking
            .with(Path::new("/tmp/file-mutation-queue-panic"), || async {
                panic!("boom")
            })
            .await;
    });
    assert!(handle.await.is_err());

    let value = tokio::time::timeout(
        Duration::from_secs(2),
        queue.with(Path::new("/tmp/file-mutation-queue-panic"), || async { 42 }),
    )
    .await
    .expect("queue must not stay locked after a panic")
    .expect("with");
    assert_eq!(value, 42);
}

#[tokio::test]
async fn dropping_a_waiting_registration_releases_the_queue() {
    // 回归：等待前一个持有者的 future 被取消时，必须打开自己注册的 gate 并清理表项，
    // 否则该路径的后续调用会永久挂起（TS 的 promise 链不可取消，总能走到 finally）。
    let queue = Arc::new(FileMutationQueue::new());
    let path = Path::new("/tmp/file-mutation-queue-cancel").to_path_buf();
    let started = Arc::new(tokio::sync::Notify::new());
    let release_first = Arc::new(tokio::sync::Notify::new());
    let function_ran = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let first = {
        let queue = Arc::clone(&queue);
        let path = path.clone();
        let started = Arc::clone(&started);
        let release_first = Arc::clone(&release_first);
        tokio::spawn(async move {
            queue
                .with(&path, move || async move {
                    started.notify_one();
                    release_first.notified().await;
                })
                .await
                .expect("first operation");
        })
    };
    started.notified().await;

    // 第二个注册先入队并等待第一个；随后它的 future 被 drop（模拟工具调用被取消）。
    let second = {
        let queue = Arc::clone(&queue);
        let path = path.clone();
        let function_ran = Arc::clone(&function_ran);
        tokio::spawn(async move {
            let _ = queue
                .with(&path, move || async move {
                    function_ran.store(true, std::sync::atomic::Ordering::SeqCst);
                })
                .await;
        })
    };
    tokio::time::sleep(Duration::from_millis(20)).await;
    second.abort();
    assert!(second.await.is_err());
    release_first.notify_one();
    first.await.expect("first operation");

    // 第三个调用不应被取消的注册留下的 gate 卡住。
    let value = tokio::time::timeout(Duration::from_secs(2), queue.with(&path, || async { 7 }))
        .await
        .expect("queue must not stay locked after a cancelled waiter")
        .expect("with");
    assert_eq!(value, 7);
    assert!(!function_ran.load(std::sync::atomic::Ordering::SeqCst));
}

#[tokio::test]
async fn key_errors_reject_and_skip_the_callback_like_ts() {
    // 回归（TS `getMutationQueueKey` 的 rethrow）：含 NUL 的路径在 Node 的 `validatePath`
    // 阶段就抛 `ERR_INVALID_ARG_VALUE`，`withFileMutationQueue` reject 且不执行回调。
    let queue = FileMutationQueue::new();
    let ran = Arc::new(AtomicBool::new(false));
    let callback_ran = Arc::clone(&ran);
    let error = queue
        .with(
            Path::new("/tmp/file-mutation-queue-nul\0"),
            move || async move {
                callback_ran.store(true, Ordering::SeqCst);
            },
        )
        .await
        .expect_err("a NUL path must reject before running the callback");
    assert_eq!(error.code.as_deref(), Some("ERR_INVALID_ARG_VALUE"));
    assert!(!ran.load(Ordering::SeqCst));
}

#[cfg(unix)]
#[tokio::test]
async fn realpath_errors_other_than_missing_paths_reject_with_the_node_error() {
    // 回归（TS `getMutationQueueKey`）：`realpath` 的 ELOOP/EACCES/ENAMETOOLONG 会原样
    // 抛出并 reject，回调不执行（只有 ENOENT/ENOTDIR 才回退到词法绝对路径）。
    let dir = tempfile::tempdir().expect("tempdir");
    let first = dir.path().join("a");
    let second = dir.path().join("b");
    std::os::unix::fs::symlink(&second, &first).expect("symlink a");
    std::os::unix::fs::symlink(&first, &second).expect("symlink b");

    let queue = FileMutationQueue::new();
    let ran = Arc::new(AtomicBool::new(false));
    let callback_ran = Arc::clone(&ran);
    let error = queue
        .with(&first, move || async move {
            callback_ran.store(true, Ordering::SeqCst);
        })
        .await
        .expect_err("ELOOP must reject");
    assert_eq!(error.code.as_deref(), Some("ELOOP"));
    assert!(
        error
            .message
            .starts_with("ELOOP: too many symbolic links encountered, realpath '"),
        "{}",
        error.message
    );
    assert!(!ran.load(Ordering::SeqCst));
}

#[tokio::test]
async fn cancelled_waiter_does_not_let_later_mutations_overlap_the_running_one() {
    // 回归：B 在等待 A 时被取消后，C 仍必须等待 A。旧实现在 B 恰为队尾时直接删表项，
    // C 看到空表就会与仍在运行的 A 并发修改同一文件。
    let queue = Arc::new(FileMutationQueue::new());
    let path = Path::new("/tmp/file-mutation-queue-cancel-overlap").to_path_buf();
    let order: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let a_started = Arc::new(tokio::sync::Notify::new());
    let start_third = Arc::new(tokio::sync::Notify::new());

    let first = {
        let queue = Arc::clone(&queue);
        let path = path.clone();
        let order = Arc::clone(&order);
        let a_started = Arc::clone(&a_started);
        tokio::spawn(async move {
            queue
                .with(&path, move || async move {
                    order.lock().expect("lock").push("a:start");
                    a_started.notify_one();
                    delay(100).await;
                    order.lock().expect("lock").push("a:end");
                })
                .await
                .expect("first");
        })
    };
    a_started.notified().await;

    // B 注册后等待 A；20ms 足够完成 key 计算与入队，然后取消。
    let second = {
        let queue = Arc::clone(&queue);
        let path = path.clone();
        tokio::spawn(async move {
            let _ = queue.with(&path, || async {}).await;
        })
    };
    tokio::time::sleep(Duration::from_millis(20)).await;
    second.abort();
    assert!(second.await.is_err());

    // A 仍在运行（约 100ms）时注册 C；C 必须等到 A 结束。
    let third = {
        let queue = Arc::clone(&queue);
        let path = path.clone();
        let order = Arc::clone(&order);
        let start_third = Arc::clone(&start_third);
        tokio::spawn(async move {
            start_third.notified().await;
            queue
                .with(&path, move || async move {
                    order.lock().expect("lock").push("c:start");
                })
                .await
                .expect("third");
        })
    };
    start_third.notify_one();

    first.await.expect("first task");
    third.await.expect("third task");
    assert_eq!(
        *order.lock().expect("lock"),
        vec!["a:start", "a:end", "c:start"],
        "C must wait for A even though B's registration was cancelled"
    );
}
