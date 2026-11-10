# env / evals / durable-testing 单元移植规范

单元 id: `env-evals-durable-testing`

源码范围（只读参考，不得修改 `/home/ctyun/code/kiana_rev/kiana` 下任何文件）:

- `packages/env/**`（`src/`、`daemon/`、`docs/`、`test/`、`package.json`、`vitest.config.ts`、`tsconfig.build.json`）
- `packages/evals/**`（`src/`、`evals/`、`docker/`、`test/`、配置文件、README）
- `packages/durable/src/testing/**`
- `packages/durable/test-workers/**`

本规范描述如何把这些 TypeScript/Rust 代码原样重写为 Rust（工作区 `/home/ctyun/code/kiana_rev/kiana`）。要求：功能、CLI 参数、磁盘文件格式、线协议、环境变量、退出码、错误文本、渲染文本与顺序完全一致。所有英文标识符、JSON 字段名、错误字符串、注释中的用户可见文案都按原文字面保留。

统计口径说明：`exportedSymbolCount = 121` 指本单元范围内 TypeScript 源文件的**去重导出符号**数量（按文件统计、跨文件去重）：`packages/env/src` 39 个（connection 9、errors 2、list-files 3、remote-env 3、ssh 16、terminal 4、watch 2；`remote-env.ts` 里的 `RemoteInfo` 是对 `connection.ts` 的再导出，只计一次；`index.ts` 的 34 个全是再导出，不新增名字）、`packages/evals/src` 39 个 + `packages/evals/evals` 17 个 = 56 个、`packages/durable/src/testing` 26 个（assertions 2、env-conformance 1、runner 3、storage-benchmark 11、storage-conformance 1、types 8；`index.ts` 的 26 个全是再导出，不新增名字）。daemon 是二进制 crate，其 `pub` 项不对外暴露，未计入。

---

## 1. Overview

### 1.1 三个部分各自的职责

**A. `@earendil-works/kiana-env`（`packages/env`）—— 远程执行环境**

由两块组成：

1. `daemon/`：一个 Rust 二进制 `kiana-env`（Cargo package `kiana-env` v0.1.0，edition 2024）。它运行在“远端机器”上，通过 stdin/stdout 讲一个自定义的二进制帧协议（protocol 1），在本地执行文件系统调用、启动命令、运行 PTY、监视文件。启动方式固定为 `kiana-env serve --token <hex>`；通常由 `ssh` 拉起。
2. `src/`：TypeScript 客户端。`Connection` 管理 daemon 进程与帧收发；`RemoteExecutionEnv` 把 Durable 的 `ExecutionEnv` 接口映射成协议请求，并在客户端复刻 `NodeExecutionEnv` 的路径解析、错误码映射、分块读写、超时/中断语义；`ssh.ts` 负责检测远端平台、部署 daemon、管理 known_hosts；`terminal.ts` 提供 `openTerminal`；`watch.ts` 提供 `RemoteWatcher`；`list-files.ts` 提供 `listFilesOver` 与 `LocalExecutionEnv`。

设计契约（`docs/protocol.md`、`docs/semantics.md`）：`RemoteExecutionEnv` 的结果必须与远端机器上运行的 `NodeExecutionEnv` 一致；只有错误消息文本允许不同，错误 **code** 必须一致。

**B. `@earendil-works/kiana-evals`（`packages/evals`）—— 文档增益评测（documentation-lift eval）**

- `src/cli.ts`：入口（`node --experimental-strip-types src/cli.ts`），解析 CLI/env，构建两个 Docker 镜像，发现用例、规划任务、逐个跑容器、读取 Vitest JSON、汇总报告、写 `.eval/<runId>/` 下的产物。
- `src/docker.ts`：构建镜像、给出 docker run 参数、发现用例、跑单个 arm。
- `src/plan.ts`：解析发现结果、把用例展开成 `(case, variant, model, runNumber)` 任务。
- `src/report.ts`：读取 Vitest JSON + workspace JSON，配对 control/treatment，计算 lift、效率差值、flags、运营总量，格式化文本报告。
- `src/harness.ts`：vitest-evals 适配器，启动真实的 Kiana coding agent 会话，隔离 HOME / agent dir，降权到 UID/GID，校验 system prompt 变体。
- `evals/`：评测定义（`*.docs.eval.ts`、`*.eval.ts`）与 fixture（acme HTTP 服务器、configured runtime 探针、TUI footer 探针）。
- `docker/`：多阶段 Dockerfile、容器 entrypoint（断言镜像内容、降权、运行 vitest）。

**C. Durable 的测试基础设施（`packages/durable/src/testing`、`packages/durable/test-workers`）**

- `src/testing/`：与测试框架无关的一致性套件（`createEnvConformance`、`createStorageConformance`）、适配 Vitest/Jest 的注册函数、断言适配器、存储基准数据集与读/写基准定义。
- `test-workers/`：在 Cloudflare Workers（workerd）里跑的测试：Durable Object 用 alarm 唤醒 Harness 的测试，以及 `SqliteStorage` 的一致性套件与 schema v1→v2 迁移测试。

### 1.2 与其他包的关系与依赖方向

```
durable/env (ExecutionEnv, Result, FileError, decode, line-scan)   ← 被 env 与 testing 依赖
durable (Storage, 记录类型, ids, timers)                          ← 被 testing / test-workers 依赖
chord (Context, BACKGROUND_CONTEXT, JsonValue, Op, delta)          ← 被 env / testing / evals 依赖
kiana-ai / kiana-coding-agent                                            ← 被 evals harness 依赖
vitest-evals / @vitest-evals/core                                  ← 被 evals 依赖
cloudflare:workers / cloudflare:test                               ← 只被 test-workers 依赖
```

- `env` 依赖 `@earendil-works/chord`（仅 `Context`）与 `@earendil-works/kiana-durable`（`env`、`env/node`）。
- `evals` 依赖 `@earendil-works/kiana-ai`、`@earendil-works/kiana-coding-agent`、`vitest-evals`、`@vitest-evals/core`。
- `durable/src/testing` 依赖 `../env/index.ts`、`../types.ts`、`../ids.ts` 与 `@earendil-works/chord`；不依赖任何测试框架（只用结构化断言接口）。
- `test-workers` 依赖 `@earendil-works/kiana-durable` 主包、`@earendil-works/kiana-durable/testing`、`@earendil-works/kiana-durable/storage/sqlite/cloudflare`、`@earendil-works/chord/sqlite/cloudflare` 与 `cloudflare:workers`。

### 1.3 入口清单

| 入口 | 形式 | 说明 |
|---|---|---|
| `packages/env` npm 包 | `dist/index.js` / `dist/index.d.ts`，source condition 指向 `src/index.ts` | 导出 34 个符号 |
| `kiana-env` 二进制 | `kiana-env serve --token <hex>`、`kiana-env --version`、其他 → usage | 退出码 0/1/2 |
| `packages/evals` CLI | `node --experimental-strip-types src/cli.ts [args]` | 失败抛异常（非零退出）；存在 blocked pair 时 `process.exitCode = 1` |
| `packages/evals` host eval | `vitest run --config vitest.evals.config.ts --project host` | 300 s 超时 |
| `packages/env` 测试 | `vitest --run`，daemon 由 `KIANA_ENV_DAEMON` 或 `daemon/target/debug/kiana-env` 指定 | |
| `packages/durable/test-workers` | `vitest`（cloudflareTest 插件，workerd） | binding `SESSIONS` → `TestSession` |

### 1.4 本单元会用到的全局常量（散见各文件，先汇总）

| 常量 | 值 | 位置 |
|---|---|---|
| 帧最大长度 `MAX_FRAME` | 16 MiB = 16777216 | connection.ts / frame.rs |
| 帧头固定部分 | 13 字节（4+1+4+4） | 双方 |
| 帧类型 | REQUEST=1, RESULT=2, ERROR=3, EVENT=4, CANCEL=5, PING=6 | 双方 |
| `MAX_PAYLOAD`（daemon 响应负载上限） | `MAX_FRAME - 64*1024` | frame.rs |
| `CONTROL_PAYLOAD`（超过则走 bulk 队列） | 64 KiB | main.rs |
| `BULK_LIMIT`（未发送 bulk 字节上限） | 4 MiB | output.rs |
| ping 间隔 | 5 s（双方） | connection.ts / main.rs |
| 静默上限 | 30 000 ms | 双方 |
| 启动超时 | 60 000 ms | connection.ts |
| `WORKERS`（daemon 文件操作线程） | 16 | main.rs |
| `MAX_HANDLES` | 4096 | main.rs |
| `SCAN_CHUNK` | 64 KiB | main.rs |
| `READ_CHUNK`（binary reader 每请求） | 256 KiB | remote-env.ts |
| `READ_DEPTH` | 8 | remote-env.ts |
| `WRITE_CHUNK`（writeFile 每块） | 512 KiB | remote-env.ts |
| `WRITE_DEPTH` | 8 | remote-env.ts |
| `READ_FILE_CHUNK` / `READ_FILE_UNKNOWN_CHUNK` | 512 KiB / 64 KiB | remote-env.ts |
| `READ_FILE_MAX` | 2**31-1 | remote-env.ts |
| `LINE_CHUNK` | 64 KiB | remote-env.ts |
| `MAX_TIMEOUT_MS` / `MAX_TIMEOUT_SECONDS` | 2_147_483_647 / 2_147_483.647 | remote-env.ts |
| PTY `WRITE_CHUNK`（客户端） | 1 MiB | terminal.ts |
| PTY `MAX_CELLS` | 4096 | pty.rs |
| watch `DEBOUNCE` / `FSEVENTS_SETTLE` | 50 ms / 500 ms | watch.rs |
| watch `DEFAULT_POLL_MS` / `DEFAULT_MAX_DIRECTORIES` | 2000 / 10 000 | watch.rs |
| watch `HASH_MAX_BYTES` / `HASH_RECENT` | 256 KiB / 5 s | watch.rs |
| watch `CANCEL_POLL` | 100 ms | watch.rs |
| listFiles 默认 | maxEntries 50 000、maxBytes 4 MiB、timeoutMs 10 000 | list.rs |
| exec `EXIT_STDIO_GRACE` | 100 ms | exec.rs / pty.rs |
| exec `UNSENT_POLL` | 20 ms | exec.rs |
| SSH 超时 | `SSH_TIMEOUT_MS` 60 s、`UPLOAD_TIMEOUT_MS` 300 s | ssh.ts |
| watch 重连退避 | 1 000 ms 起，每次 ×2，上限 30 000 ms | watch.ts（客户端） |
| eval 容器沙箱 UID/GID | 65532 | docker.ts |
| eval `TIMING_SCALE`、`STORAGE_MEMORY_SCALES`、`REPLAY_TAILS`、`HISTORY_SEGMENT_LENGTH`、`FORK_DEPTH`、`ENTRIES_PER_FORK`、`BATCH_SIZE` | 见 §2.3.7 | storage-benchmark.ts |

---

## 2. 逐文件规范

### 2.1 `packages/env/src`

#### 2.1.1 `index.ts`（32 行，纯再导出）

导出清单（顺序即文件顺序）：

```
Connection, type ConnectionOptions, isConnectionLost, nodePlatform, RemoteError,
type RemoteInfo, type Reply, type RequestOptions          ← ./connection.ts
LocalExecutionEnv, type LocalExecutionEnvOptions          ← ./list-files.ts
RemoteExecutionEnv, type RemoteExecutionEnvOptions        ← ./remote-env.ts
acceptHostKey, connectSsh, deployDaemon, detectPlatform, forgetHostKey,
HostKeyChangedError, HostKeyUnknownError, localDaemon, packagedDaemon,
type RemotePlatform, type SshConnectOptions, SshError, type SshTarget,
scanHostKey, sshArguments, sshConnection                  ← ./ssh.ts
openTerminal, RemoteTerminal, type TerminalForeground, type TerminalOptions ← ./terminal.ts
RemoteWatcher, type RemoteWatchOptions                    ← ./watch.ts
```

共 34 个符号。Rust 侧对应一个 `lib.rs` 的 `pub use` 清单，必须保持同样的可见性切分（`Json`、`abortResult`、`toFileError`、`listFilesOver`、`remotePath` 不在包入口导出）。

#### 2.1.2 `errors.ts`（34 行）

- `export function abortResult<T>(signal: AbortSignal | undefined, path?: string): Result<T, FileError> | undefined`
  - `signal?.aborted` 为真时返回 `err(new FileError("aborted", "aborted", path))`；否则 `undefined`。
  - 注意消息固定为 `"aborted"`（小写），不是 Node 的 `"The operation was aborted"`。
- `export function toFileError(error: unknown, fallbackPath?: string): FileError`
  - 若 `error instanceof FileError` → 原样返回（不重包装）。
  - 若不是 `RemoteError` → `new FileError("unknown", cause.message, fallbackPath, cause)`，其中 `cause = error instanceof Error ? error : new Error(String(error))`。
  - 若是 `RemoteError`：`path = error.path ?? fallbackPath`，然后按 `error.code` 映射：
    | daemon code | FileError code |
    |---|---|
    | `"aborted"` | `aborted` |
    | `"ENOENT"` | `not_found` |
    | `"EACCES"` / `"EPERM"` | `permission_denied` |
    | `"ENOTDIR"` | `not_directory` |
    | `"EISDIR"` | `is_directory` |
    | `"EINVAL"` / `"SYMLINK"` / `"NOT_REGULAR"` | `invalid` |
    | 其他（含 `ERR_FS_EISDIR`、`EMFILE`、`EBADF`、`ENOTSUP`、`timeout`、`spawn_error` …） | `unknown` |
  - 消息保留 daemon 的 `error.message`；`cause` 保留 `RemoteError` 本身。

#### 2.1.3 `connection.ts`（397 行）

**常量**：`REQUEST=1, RESULT=2, ERROR=3, EVENT=4, CANCEL=5, PING=6`；`MAX_FRAME = 16*1024*1024`；`PING_INTERVAL_MS = 5000`；`SILENCE_LIMIT_MS = 30_000`；`START_TIMEOUT_MS = 60_000`。

**类型**

- `export type Json = Record<string, unknown>`
- `export class RemoteError extends Error`
  - 字段：`readonly code: string`、`readonly path?: string`、`readonly fields: Json`。
  - 构造：`super(typeof fields.message === "string" ? fields.message : "Remote operation failed")`；`this.name = "RemoteError"`；`code = typeof fields.code === "string" ? fields.code : "unknown"`；`path` 仅在 `typeof fields.path === "string"` 时设置；`fields` 保存完整对象。
- `export interface RemoteInfo`：`protocol: number`、`version: string`、`os: string`、`arch: string`、`home: string`、`tmpdir: string`、`separator: string`、`cwd: string`、`driveCwds: Record<string,string>`、`pid: number`、`features?: string[]`。
- `export interface ConnectionOptions`：`command: readonly string[] | (() => Promise<readonly string[]>)`、`onLog?: (text: string) => void`。
- `export interface Reply`：`json: Json`、`payload: Uint8Array`、`session: number`。
- `export interface RequestOptions`：`payload?: Uint8Array`、`signal?: AbortSignal`、`onEvent?: (json, payload) => void`、`onStart?: (id: number, session: number) => void`、`session?: number`。
- 内部 `Pending`：`resolve(value: Reply)`、`reject(error: Error)`、`onEvent?`。

**函数**

- `export function nodePlatform(info: Pick<RemoteInfo,"os"|"arch">): { platform: string; arch: string }`
  - `platforms = { windows: "win32", macos: "darwin" }`；`archs = { x86_64: "x64", aarch64: "arm64", x86: "ia32" }`；未命中保留原名。
- 内部 `wellFormed(_key, value)`：JSON.stringify replacer，字符串先 `value.toWellFormed()`（孤立代理项 → U+FFFD）。
- 内部 `frame(type, id, json, payload = new Uint8Array(0)): Buffer`
  - `body = Buffer.from(JSON.stringify(json, wellFormed), "utf8")`
  - 13 字节头：`writeUInt32BE(9 + body.length + payload.length, 0)`、`writeUInt8(type, 4)`、`writeUInt32BE(id, 5)`、`writeUInt32BE(body.length, 9)`；返回 `header|body|payload`。
  - **没有长度上限检查**（客户端始终把请求控制在 16 MiB 以内，由调用方分块）。
- `lost(message)` → `new RemoteError({ code: "unknown", message, lost: true })`。
- `export function isConnectionLost(error): boolean` → `error instanceof RemoteError && error.fields.lost === true`。
- `closed()` → `new RemoteError({ code: "unknown", message: "Connection closed" })`。

**内部 `class Session`**
字段：`id`、`child: ChildProcessWithoutNullStreams`、`token: string`、`pending = new Map<number, Pending>()`、`chunks: Buffer[]`、`buffered`、`synced=false`、`lastSeen=Date.now()`、`timer`、`live=true`。
- `peek(length)`：若 `chunks[0].length < length` 先 `chunks = [Buffer.concat(chunks)]`；返回 `chunks[0].subarray(0, length)`（不消费）。
- `consume(length)`：`buffered -= length`；循环从队首弹出或切分。

**`export class Connection`**
私有：`#options`、`#session`、`#ready`、`#sessions=0`、`#nextId=1`、`#closed=false`。

- `async info(): Promise<RemoteInfo>` → `(await #connect()).info`
- `async session(): Promise<number>` → `(await #connect()).session.id`
- `async request(op, json, options = {}): Promise<Reply>`
  - `options.session !== undefined` 时：当前 `#session` 必须存在、`id === options.session`、`live`，否则 `throw lost("kiana-env connection lost")`；然后 `#send`。
  - 否则 `const { session } = await #connect(); return #send(...)`。
- `close(): void`：`#closed = true`；若有 session → `#teardown(session, closed())`。
- `kill(id, session): void`：若当前 session 匹配且 live → 写 `frame(CANCEL, id, { mode: "kill" })`。
- `#connect()`：`#closed` → `Promise.reject(closed())`；否则惰性启动 `#ready = #start()`，**失败不记忆**（`ready.catch` 中若 `#ready === ready` 则置 `undefined`），下次请求重试。
- `#send(session, op, json, options)`
  - `!session.live` → reject `lost("kiana-env connection lost")`。
  - `id = #nextId++`；注册 pending；`onAbort = () => #write(session, frame(CANCEL, id, {}))`；`signal.addEventListener("abort", onAbort, { once: true })`；`onStart?.(id, session.id)`；写 `frame(REQUEST, id, { ...json, op }, options.payload)`；若 `signal.aborted` 立即 `onAbort()`（**注意顺序：先 send 再检查**）。
  - pending 的 resolve/reject 都会移除 abort 监听。
- `#write(session, data)`：仅当 `session.live` 时 `session.child.stdin.write(data)`。
- `#start()`：
  1. `command` 可为函数，异常 → `RemoteError({ code: "spawn_error", message, lost: true })`（message 取 error.message 或 String(error)）。
  2. 若 `#closed` 已置位 → `throw closed()`。
  3. `[program, ...args] = command`；`program === undefined` → `RemoteError({ code: "spawn_error", message: "No daemon command" })`（**无 lost 标记**）。
  4. `token = randomBytes(16).toString("hex")`（32 个十六进制字符小写）。
  5. `spawn(program, [...args, "serve", "--token", token], { stdio: ["pipe","pipe","pipe"] })`；`session = new Session(++#sessions, child, token)`；`#session = session`。
  6. 失败 promise（竞态用）：
     - 60 s 后 reject `lost("kiana-env did not answer within 60 s")`（`START_TIMEOUT_MS/1000 = 60`）。
     - `child.once("error")` → `RemoteError({ code: "spawn_error", message: error.message, lost: true })`。
     - `child.once("exit", code)` → `lost(\`kiana-env exited with code ${code} before it was ready\`)`。
  7. **`failed.catch(() => {});`**（在创建 `failed` 后立即注册；`failed` 常先于 `hello` 的竞态输掉，若不吞掉会成为未处理的 rejection 并让 Node 进程崩溃/打印警告——Rust 侧不需要对应物，但移植测试行为时必须知道这条）；随后 stdout data → `#onData`；stderr data → `onLog?.(chunk.toString("utf8"))`；`child.on("error"/"exit")` → `#teardown(session, lost("kiana-env connection lost"))`；`child.stdin.on("error", () => {})`。
  8. `setInterval(5000)`：每次写 `frame(PING, 0, {})`；若 `Date.now() - session.lastSeen > 30_000` → `#teardown(session, lost("kiana-env connection timed out"))`；timer `unref()`。
  9. `await Promise.race([#send(session, "hello", { protocol: 1 }, {}), failed])`；`json.protocol !== 1` → `throw lost(\`Unsupported protocol ${json.protocol}\`)`；成功返回 `{ info: json as RemoteInfo, session }`。
  10. catch → `#teardown(session, error)` 后 rethrow；finally `clearTimeout(startTimer)`。
- `#teardown(session, error)`：已 `!live` 直接返回；置 `live=false`、`clearInterval`；若是当前 session，清 `#session` 与 `#ready`；`child.stdin.end(); child.kill();`；取出全部 pending 并逐个 reject（**先 clear 再 reject**）。
- `#onData(session, chunk)`（核心解析器，必须按字节复刻）：
  1. `!session.live` → 返回。
  2. `lastSeen = Date.now()`；push chunk；`buffered += chunk.length`。
  3. 若 `!synced`：
     - `marker = Buffer.from(\`KIANA-ENV ${token}\n\`)`；`all = peek(buffered)`；`index = all.indexOf(marker)`。
     - `index === -1`：若 `buffered > 1 MiB` 则 `consume(buffered - marker.length)`（保留末尾 marker 长度字节），返回。
     - 找到：`noise = all.subarray(0, index).toString("utf8").trim()`；非空 → `onLog?.(noise)`；`consume(index + marker.length)`；`synced = true`。
  4. 循环 `while (live && buffered >= 4)`：
     - `length = peek(4).readUInt32BE(0)`；`length < 9 || length > MAX_FRAME` → `#teardown(session, lost("Corrupt frame from kiana-env"))` 并返回。
     - `buffered < 4 + length` → 返回（等待更多字节）。
     - `body = peek(4 + length).subarray(4)`；`type = body.readUInt8(0)`；`id = body.readUInt32BE(1)`；`jsonLength = body.readUInt32BE(5)`。
     - JSON 解析：`9 + jsonLength > length` 或 `JSON.parse` 失败，或解析结果不是非 null 非数组对象 → teardown 同上错误。
     - `payload = new Uint8Array(body.subarray(9 + jsonLength))`（**复制**，不是视图）。
     - `consume(4 + length)`；`#dispatch(session, type, id, json, payload)`。
- `#dispatch(session, type, id, json, payload)`
  - `type === EVENT`：`pending?.onEvent?.(json, payload)`（无 pending 也静默）。
  - `type !== RESULT && type !== ERROR` → 返回（含 PING/CANCEL）。
  - 无 pending → 返回。移除 pending；RESULT → `resolve({ json, payload, session: session.id })`；ERROR → `reject(new RemoteError(json))`。

#### 2.1.4 `terminal.ts`（127 行）

- `const WRITE_CHUNK = 1024 * 1024`。
- `export interface TerminalOptions`：`cwd: string`、`argv?: readonly string[]`、`env?: Record<string,string>`、`inheritEnv?: boolean`、`cols: number`、`rows: number`、`onData: (bytes: Uint8Array) => void`。
- `export interface TerminalForeground`：`pid: number`、`name?: string`。
- `export class RemoteTerminal`
  - 公开 `readonly pid: number`、`readonly exited: Promise<number>`；私有 `#connection`、`#id`、`#session`、`#abort: AbortController`。
  - 构造函数签名 `(connection, id, session, pid, exited, abort)`。
  - `write(data: Uint8Array | string): void`：字符串按 UTF-8 编码；按 1 MiB 切片，每片 `#request("ptyWrite", {}, bytes.subarray(offset, offset+WRITE_CHUNK))`。
  - `resize(cols, rows): void` → `#request("ptyResize", { cols, rows })`。
  - `close(): void` → `#abort.abort()`。
  - `async foreground(): Promise<TerminalForeground | undefined>`：请求 `ptyForeground`，`{ target: #id }`，`{ session: #session }`；读 `reply.json.foreground as { pid, name: string|null } | null`；null/缺失 → `undefined`；否则 `{ pid, ...(foreground.name ? { name } : {}) }`。
  - `#request(op, json, payload?)`：`connection.request(op, { ...json, target: #id }, { session: #session, ...(payload ? { payload } : {}) }).catch(() => {})`（**成功/失败都吞掉**，状态由 `exited` 报告）。
- `export function openTerminal(connection, options): Promise<RemoteTerminal>`
  - 新建 `AbortController`；`started = Promise.withResolvers<RemoteTerminal>()`；`request: {id,session} | undefined`。
  - `exited = connection.request("pty", { cwd, ...(argv), ...(env), ...(inheritEnv), cols, rows }, { signal: abort.signal, onStart(id, session){ request = {id, session} }, onEvent(json, payload){ if (json.kind === "output") options.onData(payload); else if (json.kind === "started" && request) started.resolve(new RemoteTerminal(connection, request.id, request.session, Number(json.pid), exited, abort)); } }).then(reply => Number(reply.json.exitCode))`。
  - `exited.catch(started.reject)`：启动前失败 → open 失败；启动后失败 → 只由 `exited` 承载。
  - 返回 `started.promise`。
  - 注意请求体字段**都是可选展开**：未提供的 `argv/env/inheritEnv` 不出现在 JSON 中。

#### 2.1.5 `watch.ts`（158 行）

- `export interface RemoteWatchOptions`：`mode?: "native" | "polling"`、`pollIntervalMs?: number`、`maxDirectories?: number`（各自注释见下）。
- `const RECONNECT_FIRST_MS = 1000`；`const RECONNECT_MAX_MS = 30_000`。
- `export class RemoteWatcher implements FileWatcher`
  - 字段：`#connection`、`#targets`、`#onChange`、`#options`、`#mode = "native"`、`#closed = false`、`#controller?`、`#request?: Promise<void>`、`#wake?`。
  - `static async open(connection, targets, onChange, options = {})`：构造后 `await #start()`，返回实例。
  - `get mode(): "native" | "polling"`。
  - `async close(_context)`：已关闭直接返回；置 `#closed`；`#wake?.()`；`#controller?.abort()`；`await #request`。
  - `#deliver(change)`：关闭后不投递；`onChange` 抛异常被吞（不能中断监视）。
  - `#start(): Promise<void>`
    - 请求 `watch`，body：`targets: [{ path, recursive: target.recursive === true, exclude: { hidden: target.exclude?.hidden === true, names: [...(target.exclude?.names ?? [])] } }]`，再展开 `...this.#options`（可能含 `mode`/`pollIntervalMs`/`maxDirectories`）。
    - **`request()` 在 Promise executor 内同步发起**：`controller = new AbortController()` 先赋值给 `#controller`；`#request = request.then(() => {}, onError)`（**成功路径什么都不做**）。`onError`：`!ready` → reject（**open 失败**）；`#closed` → 返回；`isConnectionLost` → `void #reconnect()`；否则 `#deliver({error: toFileError(error)})` 并 `#closed = true`。
    - **`ready` 事件可能在 `#request` 赋值之前到达**（daemon 先发 `ready`）：因此 `resolve()` 在 `onEvent` 里直接调用，`#request` 只用于 close 时等待。
    - 事件处理：
      - `ready`：`ready = true`；`#mode = event.mode === "polling" ? "polling" : "native"`；resolve。
      - `change`：若 `event.mode === "polling"` 更新 `#mode`；`event.overflow === true` → `#deliver({ overflow: true })`；否则 `#deliver({ paths: event.paths as string[] })`。
      - `error`：`code = event.code === "permission_denied" ? "permission_denied" : "invalid"`；投递 `{ error: new FileError(code, String(event.message)) }`；`#closed = true`（**不关闭底层请求**）。
    - 请求 settle：失败时若 `!ready` → reject；若 `#closed` → 返回；若是连接丢失 → 触发 `#reconnect()`；否则投递 `{ error: toFileError(error) }` 并 `#closed = true`。
  - `#reconnect()`：`for (delay = 1000; !#closed; delay = min(delay*2, 30_000))`
    - 等待 `setTimeout(resolve, delay)`（`unref()`，`#wake` 可提前唤醒并 clearTimeout）。
    - 关闭则返回；尝试 `#start()`；成功后**先投递 `{ overflow: true }`**（断线期间的变化未看到）再返回；失败若是连接丢失继续循环，否则投递错误并关闭。

#### 2.1.6 `list-files.ts`（102 行）

- `const KINDS: Record<number, FileKind> = { 102: "file", 100: "directory", 108: "symlink" }`（即 `f`、`d`、`l`）。
- `export async function listFilesOver(connection, path, options, context): Promise<Result<FileListing, FileError>>`
  1. 逐个校验 `["maxEntries","maxBytes","timeoutMs"]`：`value !== undefined` 且（非安全整数或 < 0）→ `err(new FileError("invalid", \`${key} must be a non-negative safe integer\`, path))`。
  2. abort 检查 → 返回 `aborted`。
  3. `if (!(await connection.info()).features?.includes("listFiles"))` → `err(new FileError("not_supported", "The kiana-env daemon does not list files; update it", path))`。
  4. 请求 `listFiles`，body 为 `{ path, hidden, gitignore, exclude, maxEntries, maxBytes, timeoutMs }`（`undefined` 字段经 JSON 序列化被丢弃 → daemon 用默认值）；`signal` 仅在 `context.abortSignal` 存在时传。
  5. 请求后再次 abort 检查。
  6. 解析 payload：`Buffer.from(payload.buffer, payload.byteOffset, payload.length)`；循环 `start < len`：找 NUL（找不到就 break）；`kind = KINDS[bytes[start]]`；`kind !== undefined` 才 push `{ path: bytes.toString("utf8", start+1, end), kind }`；`start = end + 1`。
  7. 返回 `ok({ entries, truncated: json.truncated === true })`；异常 → `err(toFileError(error, path))`。
- `export type LocalExecutionEnvOptions = ConstructorParameters<typeof NodeExecutionEnv>[0] & { connection: Connection }`。
- `export class LocalExecutionEnv extends NodeExecutionEnv implements ExecutionEnv`
  - 构造：解构出 `connection`，其余传给 `super`。
  - `async listFiles(path, options, context)`：先 `await this.absolutePath(path, context)`（失败原样返回），再 `listFilesOver(this.#connection, resolved.value, options, context)`。

#### 2.1.7 `remote-env.ts`（957 行，客户端语义核心）

**常量**：`MAX_TIMEOUT_MS = 2_147_483_647`、`MAX_TIMEOUT_SECONDS = MAX_TIMEOUT_MS/1000`、`READ_CHUNK = 256*1024`、`READ_DEPTH = 8`、`WRITE_CHUNK = 512*1024`、`WRITE_DEPTH = 8`、`READ_FILE_CHUNK = 512*1024`、`READ_FILE_UNKNOWN_CHUNK = 64*1024`、`READ_FILE_MAX = 2**31-1`、`LINE_CHUNK = 64*1024`。

**导出**：`RemoteExecutionEnvOptions`（`connection`、`id`、`cwd`、`shellPath?`、`shellEnv?`、`watch?`）、`remotePath(connection): Promise<PlatformPath>`（`nodePlatform(info).platform === "win32" ? win32 : posix`）、`RemoteExecutionEnv`、`export type { RemoteInfo }`。

**内部类型**
- `RemoteFileInfo = { name, kind: FileKind|"other", size, mtimeSec, mtimeNsec }`
- `RemoteDirEntry = { name: string; raw?: number[]; info?: RemoteFileInfo; error?: Json }`
- `toInfo(path, remote, paths)`：`kind === "other"` → `err(FileError("invalid", "Unsupported file type", path))`；否则 `ok({ name: paths.basename(path), path, kind, size, mtimeMs: remote.mtimeSec * 1000 + remote.mtimeNsec / 1e6 })`。
- `concat(chunks, total)`：单块直接返回；否则依次 `set`。
- `readRange(read, offset, length, chunk, aborted)`：
  - 返回 `{ chunks, total } | undefined`（undefined 表示被中断）。
  - 并发窗口 `READ_DEPTH = 8`；每块 `size = min(offset+length-next, chunk)`；先入队再 `await` 队首；每块后检查 `aborted()`。
  - 读到 0 字节 → break（EOF）；短读（`bytes.length < head.length`）→ **清空在途队列**并令 `next = offset + total`（顺序读语义）。

**`class Handle`**（内部）
- 字段 `env`、`id`、`session`、`path`。
- `request(op, json = {}, options = {})` → `env.connection.request(op, { ...json, handle: id }, { ...options, session })`。
- `pread(offset | undefined, length)` → `(await request("pread", offset===undefined?{}:{offset}, length)).payload`。
- `close()` → `await request("close").catch(() => undefined)`。

**`class RemoteBinaryReader implements BinaryReader`**
- 关闭后所有操作返回 `FileError("invalid", "Binary reader is closed", path)`；abort 检查**先于**关闭检查（`abortResult(...) ?? closedResult()`）。
- `info(context)`：请求 `fstat`，`toInfo(path, json, await remotePath(connection))`。
- `read(offset, length, context)`：参数需为非负安全整数，否则 `FileError("invalid", "Offset and length must be non-negative safe integers", path)`；`readRange(handle.pread, offset, length, READ_CHUNK, () => context.abortSignal?.aborted === true)`；undefined → `FileError("aborted", "aborted", path)`；否则 `ok(concat(...))`。
- `scanLines({ startLine, endLine? }, context)`：校验 `startLine` 安全整数且 ≥ 0，`endLine` 若给出须安全整数且 `> startLine`，否则 `FileError("invalid", "Invalid line range", path)`；请求 `scanLines`（`endLine` 省略时不发）；返回 `json as LineScan`。
- `close(_context)`：幂等；先置 `#closed` 再 `handle.close()`。

**`class RemoteDirReader implements DirReader`**
- `next(maxEntries, context)`：
  1. abort → 返回。
  2. 关闭 → `FileError("invalid", "Directory reader is closed", path)`。
  3. `maxEntries` 非正安全整数 → `FileError("invalid", "maxEntries must be a positive safe integer", path)`。
  4. 循环：`while (!#done && entries.length < maxEntries)`：请求 `readdir { max: maxEntries - entries.length }`；再次 abort 检查；遍历 `json.entries`：`entryPath = paths.resolve(path, entry.name)`；`entry.error !== undefined` 时：`code === "ENOENT"` → 跳过（枚举期间被删），否则 `return err(toFileError(new RemoteError(entry.error), entryPath))`；否则 `toInfo(...)`，成功才 push（`kind === "other"` 被**静默丢弃**）；`#done = json.done === true`。
  5. 返回 `ok({ entries, done: #done })`。
- `close`：幂等。

**`class RemoteTextLineReader implements TextLineReader`**
- 字段 `#decoder = new StreamDecoder()`（Durable 的 WHATWG 解码器，见 §2.3）、`#offset=0`、`#buffered=""`、`#ended=false`、`#closed=false`。
- `readLine(context)`：
  - abort → 返回；关闭 → `FileError("invalid", "Text line reader is closed", path)`。
  - 循环：缓冲中有 `"\n"` → `ok({ text: 前段, terminated: true })`，缓冲截断。
  - 已 `#ended`：缓冲空 → `ok(undefined)`；否则 `ok({ text: 缓冲, terminated: false })` 并清空。
  - 否则 `pread(#offset, LINE_CHUNK)`；读取后 abort 检查；`#offset += bytes.length`；0 字节 → `#buffered += decoder.decode()`（流结束）、`#ended = true`；否则 `#buffered += decoder.decode(bytes)`。
- `close`：幂等；清空缓冲。

**`export class RemoteExecutionEnv implements ExecutionEnv`**
字段：`readonly id`、`cwd`（可写）、`readonly connection`、`#shellPath`、`#shellEnv`、`#watchOptions`、`#running = new Map<number, number>()`（requestId → session）。

- `#resolve(path)`：`info = await connection.info()`；`windows = nodePlatform(info).platform === "win32"`；`paths = windows ? win32 : posix`。
  - `"~"` → `info.home`。
  - 以 `"~/"`（Windows 亦 `"~\\"`）开头 → `paths.join(info.home, path.slice(2))`。
  - 以 `"file://"` 开头 → `fileURLToPath(path, { windows })`，失败保留原样。
  - 绝对路径 → `paths.resolve(normalized)`。
  - Windows 盘符相对（正则 `^([a-zA-Z]:)(?![\\/])`）且盘符不同于 `this.cwd` 前两字符（不区分大小写）→ `base = info.driveCwds[drive.toUpperCase()] ?? info.cwd`；若 base 盘符不同且 `base[2] === "\\"` → `base = \`${drive}\\\``；`win32.resolve(base, normalized)`。
  - 否则 `paths.resolve(this.cwd, normalized)`。
- `#fileOp(path, context, run, check = "before")`：解析路径（失败 → `toFileError(error, path)`）；abort 检查（path 用 resolved）；执行；`check === "both"` 时再检查一次 abort；异常 → `toFileError(error, resolved)`。
- `#open(resolved, json)`：请求 `open { path: resolved, ...json }`，构造 `Handle(this, reply.json.handle as number, reply.session, resolved)`。
- `absolutePath(path, _context)`：`ok(await #resolve(path))`，异常 → `toFileError(error, path)`。
- `joinPath(parts, _context)`：`try { return ok((await remotePath(connection)).join(...parts)); } catch (error) { return err(toFileError(error)); }`（**异常路径不传 fallback path**，FileError 的 `path` 为 `undefined`）。
- `#readFile(path, encoding, context)`（严格复刻 Node `readFile`）：
  - 打开 `{ mode: "read" }`。`json.statError !== undefined` → `throw new RemoteError(json.statError)`。
  - abort → `FileError("aborted", "The operation was aborted", resolved)`。
  - `stat = json.stat`；`size = stat.kind === "file" ? stat.size : 0`。
  - `size > READ_FILE_MAX` → `FileError("unknown", \`File size (${size}) is greater than 2 GiB\`, resolved)`。
  - `size === 0`（未知大小，如 FIFO/设备）：循环 `pread(undefined, 64 KiB)` 直到 0 字节；累计 bytes（用于 binary）；文本用 `new StringDecoder("utf8")`：`text += decoder.write(bytes)`；结束时 `total === 0 ? "" : text + decoder.end()`。
  - 非编码路径：`size <= 512 KiB` → 单次 `pread(0, size)`；否则 `readRange(0, size, READ_FILE_CHUNK, aborted)`，undefined → 抛 abort。
  - 编码路径：`length = min(size, 512 KiB)`；并发窗口 8 个 `pread(next, length)`，`next += length`；每次出队后 abort 检查；`total += bytes.length`；终止条件 `bytes.length === 0 || total === size || bytes.length !== length`：
    - **第一块直接整体解码**：`first ? Buffer.from(bytes).toString("utf8") : text + decoder.end(Buffer.from(bytes))`（即第一块不经过流式解码器；只在**首块即终止**时生效）。注意 `StringDecoder` **不剥除 BOM**（实测 `write([0xEF,0xBB,0xBF,0x61])` → `"\uFEFFa"`），所以“BOM 保留”不是这条特例带来的；它的作用只是让首块走 Node 的整块 `toString` 收尾路径。Rust 侧照抄分支即可。
    - 否则 `text += decoder.write(Buffer.from(bytes)); first = false`。
  - `finally { await handle.close() }`。
- `readTextFile` / `readBinaryFile`：分别以 `encoding=true/false` 调 `#readFile`。
- `openBinaryReader(path, options, context)`：`#fileOp(..., { noFollow: options?.noFollow === true })`；打开后再 abort 检查，若已 abort 则先 `handle.close()` 再返回 `aborted`。
- `openTextLineReader`：同上但 `{ mode: "read" }`。
- `readTextLines(path, options, context)`：`maxLines !== undefined && maxLines <= 0` → `ok([])`；打开 reader；循环到 EOF 或达到 maxLines；任何 readLine 失败原样返回；finally 关闭。
- `#write(path, content, append, context)`：
  - `bytes = typeof content === "string" ? Buffer.from(content, "utf8") : content`。
  - `signal = append ? undefined : context.abortSignal`（**appendFile 不做 abort 检查**）。
  - `first = bytes.subarray(0, 512 KiB)`；`keep = bytes.length > 512 KiB`；请求 `write { path, append, keep }`，payload=first。
  - `!keep` → 返回。
  - 后续：每 512 KiB 一块，`writeChunk {}`（handle 已带）；先检查 `signal?.aborted`：若中断，`await Promise.allSettled(inFlight)` 找第一个 rejected 并抛其 reason，否则抛 `FileError("aborted", "The operation was aborted", resolved)`；在途深度 8（超过则 `await inFlight.shift()`）。
  - 收尾：逐个 `await inFlight`（第一个失败即抛，后续块被 daemon 拒绝，文件无空隙）；finally `Promise.allSettled(inFlight)` 后 `handle.close()`。
  - `check` 参数：append → `"both"`，write → `"before"`。
- `writeFile` / `appendFile`：转调 `#write`。
- `truncateFile(path, size, context)`：`size` 非负安全整数否则 `FileError("invalid", "File size must be a non-negative safe integer", resolved)`；请求 `truncate { path, size }`；check `"both"`。
- `flushFile`：请求 `fsync { path }`；check `"both"`。
- `renameFile(sourcePath, destinationPath, context)`：两次 `#resolve`（失败 → `toFileError(error, sourcePath)`）；abort（path 用 destination）；请求 `rename { path: source, to: destination }`；异常 → `toFileError(error, source)`（注意用 **source**）。
- `fileInfo(path, context)`：`#fileOp` 内请求 `lstat`，再 `toInfo(resolved, json, await remotePath(connection))`；若 `toInfo` 返回 err 则返回该 err（`result.ok ? result.value : result`）。
- `listDir(path, context)`：`#fileOp` 内：
  1. `opendir`；循环 `readdir { max: 1000 }` 直到 `done === true`；finally close。
  2. POSIX 下按 **entry.raw ?? utf8(name) 的字节序**排序（`Buffer.compare`）；Windows 保留 daemon 顺序。
  3. 逐个 entry：`context.abortSignal?.aborted` → `FileError("aborted", "aborted", resolved)`；`entry.error !== undefined` → `throw toFileError(new RemoteError(entry.error), entryPath)`（**任何错误都失败**，与 DirReader 不同）；`toInfo` 成功才 push。
- `listFiles(path, options, context)`：先 `#resolve`（失败 → `toFileError(error, path)`），再 `listFilesOver`。
- `#openDir(resolved)`：请求 `opendir { path }`，返回 Handle。
- `openDirReader`：`#fileOp` 打开；打开后再 abort 检查（已 abort 则关闭并返回 `aborted`）。
- `canonicalPath`：请求 `realpath { path }`，返回 `json.path as string`。
- `exists`：`fileInfo`；ok → true；`not_found` → false；其他错误原样返回。
- `createDir(path, { recursive? }, context)`：`recursive: options?.recursive ?? true`。
- `remove(path, { recursive?, force? }, context)`：默认均 `false`。
- `createTempDir(prefix, context)`：
  - 先 `abortResult<string>(context.abortSignal)`（**不传 path**，aborted 错误的 `path` 为 undefined）。
  - `tmpdir = (await connection.info()).tmpdir`；`paths.join(tmpdir, prefix ?? "tmp-")`；请求 `mkdtemp { path }`；返回 `json.path`；异常 → `toFileError(error)`（**不传 fallback path**）。
- `createTempFile({ prefix?, suffix? }, context)`：
  - `createTempDir("tmp-")`；`filePath = paths.join(dir, \`${prefix ?? ""}${randomUUID()}${suffix ?? ""}\`)`；请求 `write { path: filePath, append: false, parents: false }`，payload 为空 `Uint8Array`（模拟 Node `writeFile(path, "")`，不创建父目录）；失败 → `toFileError(error, filePath)`。
- `watch(targets, onChange, context)`：
  - abort 检查；`Promise.all` 解析每个 target 的 path；再次 abort；`RemoteWatcher.open(...)`；打开后再 abort 检查（若已 abort 则关闭 watcher 并返回）；异常 → `toFileError(error)`（**不传 fallback path**，即使 `#resolve` 已经知道具体 path）。
- `exec(command, options, context)`（**结果优先级必须精确**）：
  1. `signal?.aborted` → `err(ExecutionError("aborted", "aborted"))`。
  2. `timeout` 校验：`!Number.isFinite(timeout) || timeout <= 0` → `ExecutionError("timeout", "Invalid timeout: must be a finite number of seconds")`；`timeout * 1000 > MAX_TIMEOUT_MS` → `ExecutionError("timeout", \`Invalid timeout: maximum is ${MAX_TIMEOUT_SECONDS} seconds\`)`（即 `"Invalid timeout: maximum is 2147483.647 seconds"`）。
  3. `inheritEnv = options?.inheritEnv ?? true`；`cwd = options?.cwd ? await #resolve(options.cwd) : this.cwd`（解析异常 → `ExecutionError("unknown", message)`）。
  4. 再次 abort 检查。
  5. `env`：`Object.entries(inheritEnv ? { ...this.#shellEnv, ...options?.env } : { ...options?.env })`，值非 undefined 时 `env[key.toWellFormed()] = value`。
  6. 创建内部 `AbortController`；`signal` abort 时转发。
  7. 请求 `exec`，body：`typeof command === "string" ? { command } : { argv: [...command] }` 再展开 `cwd`、`env`、`inheritEnv`、`shellPath?`、`timeoutMs?`（= timeout*1000）、`spill?`、`window?`；`onStart` 记录 `#running.set(requestId, session)`。
  8. `onEvent`：`settled || callbackError !== undefined || event.kind !== "output"` → 忽略；`text = Buffer.from(payload...).toString("utf8")`；`text === ""` 或未给 `onOutput` → 忽略；`info = { stream: event.stream === "stderr" ? "stderr" : "stdout" }`；`event.skipped !== undefined` 时 `info.skipped = event.skipped`；调用 `options.onOutput(text, context, info)`；抛异常 → `callbackError = new ExecutionError("callback_error", cause.message, cause)` 并 `controller.abort()`（**注意：这里 abort 会把请求变成 aborted 错误，但返回值优先 callbackError**）。
  9. 成功：`callbackError` 优先；否则 `ok({ exitCode: json.exitCode, ...(typeof json.spillPath === "string" ? { spillPath } : {}) })`。
  10. 失败：`callbackError` 优先；非 `RemoteError` → `ExecutionError("unknown", message)`；RemoteError 映射：
      - `timeout` → `ExecutionError("timeout", \`timeout:${timeout}\`)`（`timeout` 是秒数，可能为 `undefined` → 字面 `"timeout:undefined"`，但 daemon 只在有 timeoutMs 时才返回该 code）。
      - `aborted` → `ExecutionError("aborted", "aborted")`。
      - `shell_unavailable` / `spawn_error` → 保留 code 与 message。
      - 其他 → `ExecutionError("unknown", error.message)`。
      - 若 `error.fields.spillPath` 是字符串，赋给 `failure.spillPath`。
  11. finally：`settled = true`；移除 abort 监听；`#running.delete(running)`。
- `cleanup(_context)`：对 `#running` 每项 `connection.kill(id, session)`（**不发 abort**，命令以被杀的退出码结束，与 `NodeExecutionEnv` 一致）；清空表。

#### 2.1.8 `ssh.ts`（539 行）

**导出类型/类**
- `RemotePlatform = { platform: "linux"|"android"|"darwin"|"windows"; arch: "x64"|"arm64"; home: string; shell?: "cmd"|"powershell"; warnings: string[] }`。
- `SshTarget = { host, user?, port?, identityFile?, knownHostsFile, hostKeyAlias, ssh?, configFile? }`。
- `class HostKeyUnknownError extends Error {}`（无额外字段）。
- `class HostKeyChangedError extends Error {}`。
- `class SshError extends Error { readonly exitCode: number | null; readonly stderr: string }`，构造 `(message, exitCode, stderr)`。

**内部辅助**
- `packageRoot = fileURLToPath(new URL("..", import.meta.url))`（即 `dist/` 的上一级；source 下即包根）。
- `checkField(name, value)`：`value === ""`、以 `"-"` 开头、或 `!=[\s\x00-\x1f\x7f]` 匹配 → `throw new Error(\`Invalid ${name}: ${JSON.stringify(value)}\`)`。
- `configPath(name, path)`：空或含 `"`/控制字符 → `Invalid ${name}: ${JSON.stringify(path)}`；否则 `"${path.replaceAll("%", "%%")}"`（含双引号）。

**`export function sshArguments(target, strictHostKeys = true, knownHostsFile = target.knownHostsFile): string[]`**

顺序固定：

```
[-F <configFile>] 若提供
-T -a -x
-o BatchMode=yes
-o ClearAllForwardings=yes
-o ForwardAgent=no
-o ForwardX11=no
-o ControlMaster=no
-o ControlPath=none
-o RemoteCommand=none
-o PermitLocalCommand=no
-o SendEnv=-*
-o ServerAliveInterval=15
-o StrictHostKeyChecking=yes|accept-new
-o UserKnownHostsFile="<path with % doubled>"
-o GlobalKnownHostsFile=none
-o HashKnownHosts=no
-o HostKeyAlias=<alias>
[-l <user>]
[-p <port>]
[-o IdentityFile="<path>" -o IdentitiesOnly=yes] 若提供 identityFile
--
<host>
```

校验（**字面参数名逐字**）：`checkField("host", target.host)`；`checkField("host key alias", target.hostKeyAlias)`；`user !== undefined` 时 `checkField("user", target.user)`；`port` 若给须整数 1..65535，否则 `Invalid port: ${port}`。
`configPath` 的两处调用名字逐字：`configPath("known hosts file", knownHostsFile)`（用于 `UserKnownHostsFile=`）与 `configPath("identity file", target.identityFile)`（用于 `IdentityFile=`）。因此报错分别是 `Invalid host key alias: ...`、`Invalid known hosts file: ...`、`Invalid identity file: ...`。

**`runSsh(target, command, { stdin?, args?, timeoutMs? })`**
- `args = options.args ?? sshArguments(target)`；`spawn(target.ssh ?? "ssh", [...args, command], { stdio: ["pipe","pipe","pipe"] })`。
- 超时（默认 60 000 ms）：`child.kill()`；reject `SshError(\`ssh ${host} did not finish within ${timeoutMs/1000} s: ${stderr.trim()}\`, null, stderr)`。
- `child.on("error")` → reject `SshError(error.message, null, "")`。
- `close(code)`：`code === 0` → resolve stdout；否则依次匹配 stderr：
  - `/REMOTE HOST IDENTIFICATION HAS CHANGED/` → `HostKeyChangedError(\`The host key of ${host} changed; remove the old key to continue\`)`。
  - `/Host key verification failed|No .* host key is known/` → `HostKeyUnknownError(\`The host key of ${host} is not trusted yet\`)`。
  - 否则 `SshError(\`ssh ${host} failed with exit code ${code}: ${stderr.trim()}\`, code, stderr)`。
- `child.stdin.end(options.stdin ?? new Uint8Array(0))`。

**`scanHostKey(target)`**
1. `mkdtemp(join(tmpdir(), "kiana-env-hostkey-"))`。
2. `runSsh(target, "exit 0", { args: sshArguments(target, false, scanned) })`，catch 中**仅**吞掉 `SshError` 与 `HostKeyUnknownError`（其他 rethrow）。
3. 读扫描文件（不存在则 `[]`），按 `"\n"` 切分并过滤空行；空 → `throw new Error(\`No host key received from ${target.host}\`)`。
4. `ssh-keygen -lf <scanned>`（stdio `["ignore","pipe","pipe"]`）：
   - `child.stdout.on("data")` 累加 `output += chunk.toString("utf8")`；`child.on("error", reject)`；`child.on("close", () => resolve(output.split("\n").filter(line => line.trim() !== "")))`。
   - **不检查退出码**：`ssh-keygen` 以非零退出（如文件不可读）时仍 resolve（可能为空数组），只有 spawn 的 `error` 才 reject。
   - `fingerprints` 即该数组。
5. finally `rm(directory, { recursive: true, force: true })`。

**`parseHostKeyLine(alias, line)`**：`line.trim().split(/\s+/)` 取前三个 `host type key`；要求 `host === alias`、`type` 匹配 `^(ssh-[a-z0-9-]+|ecdsa-sha2-[a-z0-9-]+|sk-[a-z0-9@.-]+)$`、`key` 匹配 `^[A-Za-z0-9+/]+={0,2}$`；否则 `Not a host key line for ${alias}: ${JSON.stringify(line)}`。

**`changeKnownHosts(file, change)`**
- `knownHostsChanges: Map<string, Promise<unknown>>` 按文件串行化（前一个 promise 先 `.catch(() => {})` 再 `.then`）。
- 流程：`mkdir(dirname(file), { recursive: true, mode: 0o700 })`；读现有非空行（`split("\n").filter(line => line !== "")`，注意与扫描时的 `trim() !== ""` 不同）；`lines = change(existing)`；`temporary = \`${file}.${randomBytes(6).toString("hex")}.tmp\``；`writeFile(temporary, lines.length === 0 ? "" : \`${lines.join("\n")}\n\`, { mode: 0o600, flag: "wx" })`；`rename(temporary, file)`（失败先 `rm(temporary, { force: true })` 再 rethrow）。
- finally 若 map 中仍是自己则删除。

**`acceptHostKey(target, lines)`**
- 对每个输入行先 `trim()` 并 `parseHostKeyLine`（任一行不合法整体失败）。
- 在 change 回调中：从现有行解析 `host type key`（`trim().split(/\s+/)`），建立 `trusted: Map<type, key>`（只收 alias 行）。
- 对每个接受项：同类型已知且 `known === key` → 跳过；已知但不同 → `throw new HostKeyChangedError(\`The ${type} host key of ${target.host} changed; forget the old key first\`)`；否则登记并加入 `added`。
- 返回 `[...existing, ...added]`（保留原有顺序）。

**`forgetHostKey(target)`**：过滤掉 `line.trim().split(/\s+/)[0] === target.hostKeyAlias` 的行。

**平台探测**
- `POSIX_PROBE`（逐字）:
  ```
  sh -c 'echo KIANA-ENV-PROBE; uname -s; uname -m; uname -o 2>/dev/null || echo -; printf "%s\n" "$HOME" "${TMPDIR:--}" "${LD_PRELOAD:--}"'
  ```
- `powershell(script)` → `` `powershell -NoProfile -NonInteractive -EncodedCommand ${Buffer.from(script, "utf16le").toString("base64")}` ``。
- `WINDOWS_PROBE` = `powershell("'KIANA-ENV-PROBE'; 'Windows'; [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString(); '-'; $HOME; '-'; '-'")`。
- `normalizeArch(machine)`：小写后 `x86_64|amd64|x64` → `x64`；`aarch64|arm64` → `arm64`；否则 `Unsupported remote architecture: ${machine}`。
- `parseProbe(target, output)`：按 `/\r?\n/` 切；找 `"KIANA-ENV-PROBE"` 行；找不到 → `Unexpected answer from ${target.host}: ${output.trim()}`；取其后 6 行（缺省 `""`，`tmpdir`/`preload` 缺省 `"-"`）。
- `termuxWarnings(probe)`（三条固定文案，按顺序）：
  1. `tmpdir === "-"` → `"TMPDIR is not set; Termux's sshd normally sets it to $PREFIX/tmp."`
  2. `!probe.preload.includes("termux-exec")` → `"termux-exec is not loaded (LD_PRELOAD); scripts with #!/usr/bin/env shebangs will fail."`
  3. 恒有 → `"Android may suspend Termux; run termux-wake-lock on the device to keep the connection alive."`
- `detectPlatform(target)`：
  1. 先跑 POSIX probe；若抛错（除 HostKey 两类）→ 跑 Windows probe。
  2. 若 system 匹配 `/^(MINGW|MSYS|CYGWIN)/` → 再跑 Windows probe。
  3. `arch = normalizeArch(probe.machine)`。
  4. `system === "Windows"`：跑 `echo %OS%`，输出含 `"Windows_NT"` → `shell: "cmd"`，否则 `"powershell"`；返回 `{ platform: "windows", arch, home, shell, warnings: [] }`。
  5. `Darwin` → `{ platform: "darwin", arch, home, warnings: [] }`。
  6. `Linux`：`android = os === "Android"`；`{ platform: android ? "android" : "linux", arch, home, warnings: android ? termuxWarnings(probe) : [] }`。
  7. 其他 → `Unsupported remote system: ${system}`。

**daemon 路径与部署**
- `packagedDaemon({platform, arch})` → `join(packageRoot, "bin", \`kiana-env-${platform}-${arch}\`, platform === "windows" ? "kiana-env.exe" : "kiana-env")`。
- `localDaemon()`：`platform = process.platform === "win32" ? "windows" : process.platform`；不在 `linux/android/darwin/windows` → `No kiana-env daemon for ${process.platform}`；返回 `packagedDaemon({ platform, arch: normalizeArch(process.arch) })`。
- `quotePosix(text)` → `'${text.replaceAll("'", "'\\''")}'`。
- `quotePowerShell(text)` → `'${text.replaceAll("'", "''")}'`。
- `daemonPath(remote, sha256)`：名字 `kiana-env-${sha256.slice(0, 32)}`；Windows → `${home}\.kiana\mobile\tools\${name}.exe`；POSIX → `${home}/.kiana/mobile/tools/${name}`。
- `POSIX_HASH`（逐字）:
  ```
  hash() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1; elif command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | cut -d' ' -f1; elif command -v openssl >/dev/null 2>&1; then openssl dgst -sha256 "$1" | sed 's/.*= //'; else echo none; fi; }
  ```
- `checkDaemon(target, remote, file, sha256)`：
  - Windows：PowerShell `$f = '<file>'; if ((Test-Path -LiteralPath $f) -and ((Get-FileHash -Algorithm SHA256 -LiteralPath $f).Hash.ToLower() -eq '<sha256>')) { 'present' } else { 'missing' }`；输出 `trim().endsWith("present")` → `"present"` 否则 `"missing"`。
  - POSIX：`${POSIX_HASH}; f='<file>'; if [ -f "$f" ] && [ "$(hash "$f")" = <sha256> ]; then echo present; elif [ "$(hash /dev/null)" = none ]; then echo nohash; else echo missing; fi`，整体包 `sh -c '<quoted>'`；取输出 `trim().split("\n").at(-1) ?? "missing"`。
- `deployDaemon(target, remote, binary = packagedDaemon(remote))`：
  1. 本地不存在 `binary` → `No kiana-env daemon for ${remote.platform}-${remote.arch} at ${binary}`。
  2. 读字节，算 sha256 hex，得 `file`。
  3. `state === "present"` → 返回 `file`；`"nohash"` → `\`${target.host} has no sha256sum, shasum or openssl to verify kiana-env\``。
  4. Windows：PowerShell 脚本（分号连接、单行），步骤：`$ErrorActionPreference='Stop'`；`$f='<file>'`；`$d=Split-Path -Parent $f`；`New-Item -ItemType Directory -Force -Path $d | Out-Null`；`$t=Join-Path $d ('.kiana-env-' + [guid]::NewGuid().ToString() + '.tmp')`；`$text = New-Object System.Text.StringBuilder`；`foreach ($line in $input) { if ($line -eq 'KIANA-ENV-END') { break }; [void]$text.Append($line) }`；`$bytes=[Convert]::FromBase64String($text.ToString())`；`$out=[IO.File]::Open($t,'CreateNew','Write','None'); $out.Write($bytes,0,$bytes.Length); $out.Close()`；哈希不符 → `Remove-Item -LiteralPath $t; throw 'kiana-env upload is corrupt'`；`Move-Item` 重试最多 20 次、间隔 250 ms；删除目录内其他 `kiana-env-*.exe`（`-ErrorAction SilentlyContinue`）；`'deployed'`。stdin 为 base64 每 76 字符换行 + `KIANA-ENV-END\n`；超时 300 000 ms。
  5. POSIX：`sh -c '<quoted multi-line script>'`，脚本行（逐字，顺序）：
     ```
     set -e
     <POSIX_HASH>
     f='<file>'
     d=$(dirname "$f")
     mkdir -p "$d"
     chmod 700 "$d"
     t=$(mktemp "$d/.kiana-env.XXXXXX")
     cat > "$t"
     if [ "$(hash "$t")" != <sha256> ]; then rm -f "$t"; echo "kiana-env upload is corrupt" >&2; exit 1; fi
     chmod 700 "$t"
     mv -f "$t" "$f"
     for old in "$d"/kiana-env-*; do [ "$old" = "$f" ] || rm -f "$old"; done
     echo deployed
     ```
     stdin 为原始二进制，超时 300 000 ms；返回 `file`。
- `SshConnectOptions extends SshTarget { binary?: string; loginShell?: boolean; onLog?: ConnectionOptions["onLog"] }`。
- `launchCommand(remote, file, loginShell)`：
  - Windows PowerShell：`& '<file>'`（PowerShell 单引号转义）；cmd：文件名含空格 → `"<file>"`，否则原样。
  - POSIX `loginShell` → `exec "$SHELL" -lc 'exec "$0" "$@"' '<file>'`；否则 `'<file>'`。
- `sshConnection(options)`：返回 `{ connection, remote() }`，`state: { remote?, verified? }`。
  - `command` 函数：首次 `detectPlatform(options)`（每条 warning 调 `onLog(\`${warning}\n\`)`，注意加换行）；`file = state.verified ?? await deployDaemon(options, state.remote, options.binary ?? packagedDaemon(state.remote))`；`state.verified = undefined`；返回 `[options.ssh ?? "ssh", ...sshArguments(options), launchCommand(state.remote, file, options.loginShell === true)]`。
  - `onLog` 仅在提供时传给 `Connection`。
- `connectSsh(options)`：立即 `detectPlatform` + `deployDaemon`，然后**逐条** warning `onLog(\`${warning}\n\`)`，返回 `{ connection: sshConnectionFor(options, { remote, verified }), remote }`。

#### 2.1.9 `package.json` / `vitest.config.ts` / `tsconfig.build.json`（env）

- 包名 `@earendil-works/kiana-env`，version `1.4.0`，`type: module`，`main ./dist/index.js`，`types ./dist/index.d.ts`，`exports["."]` 四个条件（source/types/import/default），`sideEffects: false`，`files: ["dist","bin","docs","README.md","CHANGELOG.md"]`，`engines.node >= 22.19.0`，依赖 `@earendil-works/chord`、`@earendil-works/kiana-durable`（均 `1.4.0`），devDeps `shx 0.3.4`、`vitest 4.1.12`。
- scripts：`clean`=shx rm -rf dist；`build`=tsc -p tsconfig.build.json；`build:daemon`=cargo build --manifest-path daemon/Cargo.toml --locked；`test`=vitest --run；`prepublishOnly`=clean && build。
- vitest：`environment: "node"`、`resolve.conditions: ["source"]`、`ssr.resolve.conditions: ["source"]`。
- tsconfig.build：extends `../../tsconfig.base.json`，outDir `./dist`，paths 三条：`@earendil-works/chord` → `../chord/dist/index.d.ts`、`@earendil-works/chord/context` → `../chord/dist/context/index.d.ts`、`@earendil-works/kiana-durable/env` → `../durable/dist/env/index.d.ts`；rootDir `./src`，include `src/**/*.ts`，exclude `["node_modules","dist","**/*.d.ts","src/**/*.d.ts"]`。
- 附带：`package.json` 的 `keywords` 为 `["durable-execution","agents","ssh","remote"]`；`description` 为 `"Remote execution environments for Kiana Durable: a small daemon deployed over SSH and its ExecutionEnv client"`；`author` = `"Earendil Works"`、`license` = `"MIT"`、`repository.type` = `"git"`、`repository.url` = `"git+https://github.com/earendil-works/kiana.git"`、`repository.directory` = `packages/env`。这些文本出现在 npm 元数据里，Rust 侧的 `kiana-env-client` crate 的 `Cargo.toml` description 应照抄同一句。

### 2.2 `packages/env/daemon`（Rust 二进制 `kiana-env`）

#### 2.2.1 `Cargo.toml` / `rust-toolchain.toml` / `build.rs`

- package：`name = "kiana-env"`、`version = "0.1.0"`、`edition = "2024"`、`description = "Remote execution environment daemon for Kiana Durable"`、`publish = false`、无 lib target。
- 依赖（**以 workspace pin 为准**，`crates/kiana-env/Cargo.toml` 只写 `{ workspace = true }`）：`encoding_rs = "=0.8.42"`、`ignore = "=0.4.33"`、`notify = "=8.2.1"`、`serde_json = "=1.0.154"`；`[target.'cfg(unix)'.dependencies] libc = "=0.2.190"`；`[target.'cfg(windows)'.dependencies] windows-sys = "=0.61.2"`，features：`Win32_Foundation`、`Win32_Security`、`Win32_Storage_FileSystem`、`Win32_System_IO`、`Win32_System_JobObjects`、`Win32_System_Threading`。TS 仓库的 `daemon/Cargo.lock` 锁的是 `notify 8.2.0`/`serde_json 1.0.151`；工作区统一到 8.2.1/1.0.154（`ARCHITECTURE.md` §4.5/§5 与附录 A）；这两个小版本差异**不得**改变 `listFiles` 的枚举顺序、watch 事件集合与 JSON 数字/键序输出，验收时用同一批 TS 用例对拍（见 §7）。
- `[profile.release] lto = true, codegen-units = 1, strip = true`。
- `daemon/Cargo.lock` 已签入（`version = 4`，约 12.3 KB、55 个 `[[package]]`），`npm run build:daemon` 用 `--locked`。Rust 工作区不复制这份 lock（`crates/kiana-env` 是 workspace 成员，共用根 `Cargo.lock`）；pin 以 workspace 为准，见上一条。`ignore`/`notify` 的版本差异（0.4.33 两者相同、notify 8.2.0→8.2.1）不得改变 `listFiles` 的枚举顺序与 watch 快照，验收用 `packages/env/test` 的同名用例对拍。
- toolchain：TS 的 `daemon/rust-toolchain.toml` 是 `channel = "1.96.0"`；进入工作区后**不设** per-crate toolchain，统一用根 `Cargo.toml` 的 `rust-version = "1.97"`、`edition = "2024"`（`oxc 0.155` 的 MSRV，`ARCHITECTURE.md` §4.10）。`kiana-env` 的代码必须同时满足 1.96 语法（若单独抽回 npm 包），因此**不要**使用 1.97 才稳定的新特性。
- `build.rs`（**已定稿，不要读 `../package.json`**）：
  ```rust
  const NPM_VERSION: &str = "1.4.0";                     // packages/env/package.json#version
  println!("cargo:rerun-if-env-changed=KIANA_ENV_VERSION");
  let version = std::env::var("KIANA_ENV_VERSION").ok().filter(|v| !v.trim().is_empty())
      .unwrap_or_else(|| NPM_VERSION.to_string());
  println!("cargo:rustc-env=KIANA_ENV_VERSION={version}");
  ```
  TS 的 daemon `build.rs` 直接读 `../package.json`，因此参照实现上报的**永远**是 npm 包版本（`1.4.0`），
  不可能是 crate 版本。Rust 工作区不再位于 npm 包内，改为**构建期注入**：
  发布/CI 脚本应把 `KIANA_ENV_VERSION` 设为 `packages/env/package.json` 的 `version`（`1.4.0`）；
  未设置时回退 `NPM_VERSION` 字面量（`packages/env/package.json#version`，当前 `1.4.0`），
  **不回退 `CARGO_PKG_VERSION`**——workspace 的 `0.1.0` 与 npm 包无关，上报它会偏离参照实现。
  npm 包版本变化时同步 `NPM_VERSION`；`env_version_matches_npm_package` 测试在能找到
  `packages/env/package.json` 时对比该文件，否则对比 `RECORDED_NPM_VERSION`。
  文件头注释见 `crates/kiana-env/build.rs`，风险条目见 `ARCHITECTURE.md` §8 第 7 条（`KIANA_ENV_VERSION` 保持构建期注入；`VERSION`/`KIANA_AI_VERSION` 不同，见第 24 条）。

#### 2.2.2 `src/main.rs`（551 行）

常量：`PROTOCOL: u64 = 1`、`FEATURES: &[&str] = &["listFiles"]`、`VERSION = env!("KIANA_ENV_VERSION")`、`PING_INTERVAL = 5s`、`SILENCE_LIMIT_MS = 30_000`、`SCAN_CHUNK = 64*1024`、`WORKERS = 16`、`MAX_HANDLES = 4096`、`CONTROL_PAYLOAD = 64*1024`。

**`struct Server`**（逐字段，与源码一致）：
```rust
struct Server {
    output: Arc<Output>,                       // 注意类型名是 Output（output.rs），不是 Queues
    tmpdir: String,                            // 不是 PathBuf
    handles: Mutex<HashMap<u64, Handle>>,
    next_handle: AtomicU64,                    // 从 1 开始
    controls: Mutex<HashMap<u32, Arc<Control>>>,   // 键是 u32（request id）
    groups: exec::Groups,
    terminals: pty::Terminals,
}
```
- `Handle` 枚举：`File(Arc<FileHandle>)`、`Dir(Arc<Mutex<DirHandle>>)`（**目录句柄自带互斥锁**，让慢速列目录不阻塞其他请求）。
- `FileHandle { file: File, path: String, failed: AtomicBool, serial: Mutex<Serial> }`；`Serial { jobs: VecDeque<Job>, running: bool }`（`#[derive(Default)]`）。
- `DirHandle { entries: ReadDir, path: String }`。
- `Job = Box<dyn FnOnce() + Send>`。
- `fn now_ms() -> u64`：`SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)`。
- `Server::file(json) -> Result<Arc<FileHandle>, Failure>`：`number(json,"handle")` 取 id；`Some(Handle::File(f))` → `Ok(f.clone())`；`Some(Handle::Dir(_))` → `Err(Failure::new("EBADF","not a file handle"))`；`None` → `Err(Failure::new("EBADF","unknown handle"))`。
- `Server::insert(handle) -> Result<u64, Failure>`：`handles.len() >= MAX_HANDLES(4096)` → `Failure::new("EMFILE", "EMFILE: too many open files")`；否则 `next_handle.fetch_add(1, Relaxed)` 作 id。
- `Server::insert_file(file, path)` → `insert(Handle::File(Arc::new(FileHandle { file, path, failed: AtomicBool::new(false), serial: Mutex::new(Serial::default()) })))`。
- **没有 `hello()` 方法**：`hello` 的 JSON 直接内联在 `dispatch` 的 match 分支里（`json!({...})`）。
- `Server::read_dir(json)`：`max = number(json,"max")?.max(1) as usize`；只锁该目录（`Arc<Mutex<DirHandle>>`）；循环 `while entries.len() < max`，`next()` 为 `None` → `done = true` 并 break，`Some(Err(e))` → `Failure::io(&e, "readdir", &dir.path)`，`Some(Ok(entry))` → `fs::dir_entry(&dir.path, &entry.file_name())`。

**请求解析辅助（逐字签名）**
- `fn field<'a>(json: &'a Value, key: &str) -> Result<&'a str, Failure>`：`json[key].as_str()`，否则 `Failure::new("EINVAL", format!("missing field {key}"))`。
- `fn number(json: &Value, key: &str) -> Result<u64, Failure>`：`json[key].as_u64()`，否则 `Failure::new("EINVAL", format!("missing or invalid field {key}"))`。**返回 u64**（负数/小数/字符串都失败）。
- `fn flag(json: &Value, key: &str) -> bool`：`json[key].as_bool().unwrap_or(false)`（**不是 Result，缺失即 false，不报错**）。
- 没有 `string()` 辅助；字符串一律用 `field`。

**`fn dispatch(&self, id, json, payload, control) -> Result<(Value, Vec<u8>), Failure>` 操作表**（顺序即 match 顺序）

前置：`json.is_null()` → `EINVAL "invalid JSON"`；`op = json["op"].as_str().unwrap_or_default()`。

| op | 请求字段 | 行为 / 响应 |
|---|---|---|
| `hello` | （无） | `{protocol: 1, version, os: env::consts::OS, arch: env::consts::ARCH, home: sys::home(), separator: sys::PATH_SEPARATOR, tmpdir: self.tmpdir, cwd: sys::cwd(), driveCwds: sys::drive_cwds(), pid, features: ["listFiles"]}`。**`os`/`arch` 是 Rust 常量原值**（`linux`、`macos`、`windows`、`x86_64`、`aarch64`），由客户端的 `nodePlatform` 转成 Node 名。**没有 `info` 操作**。 |
| `lstat` | `path` | `fs::lstat` |
| `realpath` | `path` | `fs::realpath` → `{path}` |
| `write` | `path`, `parents`(bool, **默认 true**), `append`(bool, 默认 false), `keep`(bool, 默认 false), payload | `fs::write(path, append, parents, payload)`；`keep === false` → `{}`（不保留句柄）；否则 `{handle}` |
| `writeChunk` | `handle`, payload | 若 `handle.failed` 已置位 → `EBADF "an earlier write failed"`；否则写入并串行执行；写失败置 failed 并 `io` 错误 |
| `truncate` | `path`, `size` | `fs::truncate` |
| `fsync` | `path` | `fs::fsync` |
| `rename` | `path`, `to` | `fs::rename` |
| `mkdir` | `path`, `recursive`(bool, **默认 false**) | `fs::mkdir_recursive` / `fs::mkdir` |
| `rm` | `path`, `recursive`(默认 false), `force`(默认 false) | `fs::rm` |
| `mkdtemp` | `path` | `fs::mkdtemp` → `{path}` |
| `open` | `path`, `mode`, `noFollow` | `mode == "read"` → `sys::open(path, Read)`；成功 `{stat: fs::info(...), handle}`，`metadata()` 失败则 `{statError: Failure::io(err,"fstat",path).to_json(), handle}`（**始终有 handle**）。其他情况 → `fs::open_reader(path, noFollow)` → `{handle, info}`。 |
| `pread` | `handle`, `offset`(可缺省), `length` | `length` 截到 `frame::MAX_PAYLOAD`；`offset` 是 u64 时 `fs::pread`，缺省时 `fs::read`（顺序读）；返回 `({}, bytes)` |
| `fstat` | `handle` | `handle.file.metadata()` → `fs::info` |
| `scanLines` | `handle`, `startLine`, `endLine?` | `endLine <= startLine` → `EINVAL "Invalid line range"`；`scan::LineScanner` 逐块 `SCAN_CHUNK` 读；每轮检查 `control.aborted` → `Failure("aborted","aborted")`；EOF → `scanner.finish()` |
| `opendir` | `path` | `sys::read_dir(path).map_err(|error| Failure::io(&error, "opendir", path))?` → `{handle}`（**syscall 是 `opendir`，不是 `readdir`**） |
| `readdir` | `handle`, `max` | `max = number(...).max(1)`（**钳制到 ≥1，不报错**）；`entries:[{name, raw?, info?, error?}], done`；`EBADF "not a directory handle"` |
| `listFiles` | 见 `list.rs` | `{count, truncated}` + payload |
| `close` | `handle` | 移除（不存在也不报错）→ `{}` |
| `exec` | 见 `exec.rs` | |
| `watch` | 见 `watch.rs` | |
| `pty` | 见 `pty.rs` | |
| 其他 | | `EINVAL \`unknown operation {op}\`` |

**句柄错误文案**（`Server::file`）：`EBADF "not a file handle"`（句柄是目录）、`EBADF "unknown handle"`（不存在）；`readdir` 用 `EBADF "not a directory handle"`。句柄上限：`EMFILE "EMFILE: too many open files"`（**消息带 `EMFILE: ` 前缀**，与 libuv 风格一致）。

**调度与并发**（逐条对应 `main.rs`）

- `start_workers()`：`mpsc::channel::<Job>()`，`Queue = Arc<Mutex<Receiver<Job>>>`，起 `WORKERS = 16` 个线程循环 `queue.lock().unwrap().recv()`；sender 关闭即退出。`Job = Box<dyn FnOnce() + Send>`。
- `respond(server, id, request, control)`：
  - `is_exec = matches!(op, Some("exec" | "pty"))`。
  - `dispatch` 成功 → RESULT 帧（带 payload），失败 → ERROR 帧（`failure.to_json()`）。
  - `server.controls.remove(&id)`（**先 dispatch 再移除**）。
  - `is_exec || reply.payload.len() > CONTROL_PAYLOAD(64 KiB)` → `output.bulk(reply, None)`，否则 `output.control(reply)`。
  - 注意：`respond` 里传给 `output.bulk` 的 accounting 是 `None` —— 即 4 MiB 背压的字节计数由 `Queues` 自己维护（`None` 表示不需要外部计数），不是 `Arc<AtomicUsize>`。
- `run_in_order(jobs, handle, job)`：把 `job` 推入 `handle.serial.jobs`；若 `serial.running` 已为真则直接返回（由正在跑的那个池任务继续 drain）；否则置 `running = true` 并投递一个 drain 任务：循环 pop；pop 到 `None` 时置 `running = false` 并返回。**保证同一 handle 的 writeChunk 严格按到达顺序**。
- `serve_frames(server, input)`：
  - `jobs = start_workers()`。
  - `while let Some(request) = frame::read_frame(input)?`（EOF → `Ok(())`）：
    - `REQUEST` 且 `op ∈ {ptyWrite, ptyResize, ptyForeground}`：**在读线程内联执行**并直接 `output.control` 写回 RESULT/ERROR（保证到达顺序；注释明确“through the worker pool, two writes could swap”）。`ptyWrite` 传 payload。
    - `REQUEST`（其他）：先 `Arc::new(Control::default())` 并 `server.controls.insert(id, control.clone())`（**先注册再执行，避免紧随其后的 CANCEL 丢失**）。若 `op ∈ {exec, watch, pty}` → `thread::Builder::new().stack_size(16*1024*1024).spawn(respond)`；spawn 失败 → 移除 control 并 `output.bulk(ERROR, Failure("spawn_error", error.to_string()))`。否则：`op == "writeChunk"` 且能取到 file handle → `run_in_order`；其余直接 `jobs.send(job)`。
    - `CANCEL`：按 `request.id` 找 control；`control.cancel(json["mode"] == Some("kill"))`（`kill` → killed，否则 aborted）；找不到则忽略。
    - 其他类型（RESULT/ERROR/EVENT/PING）：忽略。
- `serve(token)`：
  1. `writeln!(stdout, "KIANA-ENV {token}")` + flush（**握手行**）。
  2. `output = Arc::new(Output::default())`；起写线程：`output.run(&mut BufWriter::with_capacity(1 << 20, io::stdout().lock()))`（1 MiB 缓冲）。
  3. 构造 `Server { output, tmpdir: sys::tmpdir(), handles: Mutex::new(HashMap::new()), next_handle: AtomicU64::new(1), controls: Mutex::new(HashMap::new()), groups: Arc::new(Mutex::new(Default::default())), terminals: Arc::new(Mutex::new(HashMap::new())) }`。
  4. `last_seen = Arc::new(AtomicU64::new(now_ms()))`；起 ping 线程：无限循环 `sleep(5s)` → `output.control(Frame::new(PING, 0, json!({})))`；若 `now_ms() - last_seen > 30_000` → `kill_all(&groups)` 后 `std::process::exit(0)`。
  5. `input = BufReader::with_capacity(1 << 20, Seen { inner: stdin, last_seen })`；`Seen::read` 每次读到 `> 0` 字节就更新 `last_seen`。
  6. `result = serve_frames(&server, &mut input)`；然后 `kill_all(&server.groups)`、`output.close()`、`writer.join()`；返回 result。
- `kill_all(groups)`：遍历 `groups`（`Mutex<HashSet<u32>>`，`exec::Groups`）逐个 `exec::kill_group(pid)`。**注意：main.rs 的 `kill_all` 只杀进程组，不遍历 terminals/controls**（pty 在 `pty.rs` 里由控制流结束；30 s 静默直接 `exit(0)`，进程组被 SIGKILL）。
- `Seen<R>`：包装 reader，记录最后读到字节的毫秒时间（`Arc<AtomicU64>`，Relaxed）。

**`fn main()`**（逐字）
- `args = env::args().collect::<Vec<String>>()`；`match args.get(1).map(String::as_str)`：
  - `Some("serve")`：`token = args.iter().position(|arg| arg == "--token").and_then(|i| args.get(i + 1))`。**`--token` 可以出现在 `serve` 之后的任意位置；多余的未知参数被忽略**。缺失 → `eprintln!("usage: kiana-env serve --token <hex>")` 并 `exit(2)`。`serve(token)` 返回 `Err` → `eprintln!("kiana-env: {error}")` 并 `exit(1)`；成功 → `std::process::exit(0)`（注释：请求线程可能仍阻塞在 FIFO 打开上，但没有可答复的客户端了）。
  - `Some("--version")` → `println!("kiana-env {VERSION}")`（**退出码 0，隐式返回**）。
  - `_`（无参数或其他） → usage 到 stderr，`exit(2)`。

#### 2.2.3 `src/frame.rs`（120 行）

```rust
pub const REQUEST: u8 = 1;  pub const RESULT: u8 = 2;  pub const ERROR: u8 = 3;
pub const EVENT: u8 = 4;    pub const CANCEL: u8 = 5;  pub const PING: u8 = 6;
pub const MAX_FRAME: usize = 16 * 1024 * 1024;
pub const MAX_PAYLOAD: usize = MAX_FRAME - 64 * 1024;

pub struct Frame { pub kind: u8, pub id: u32, pub json: Value, pub payload: Vec<u8> }
impl Frame {
    pub fn new(kind: u8, id: u32, json: Value) -> Frame;                              // payload 空
    pub fn with_payload(kind: u8, id: u32, json: Value, payload: Vec<u8>) -> Frame;
}
fn invalid(message: &str) -> io::Error;                                              // ErrorKind::InvalidData
pub fn read_frame(input: &mut impl Read) -> io::Result<Option<Frame>>;
pub fn write_frame(output: &mut impl Write, frame: &Frame) -> io::Result<()>;
```

- `read_frame`：
  1. `read_exact(&mut [0u8;4])`：`UnexpectedEof` → `Ok(None)`（**只在读长度头时**）；其他错误原样返回。注意 `read_exact` 在读 1..3 字节后 EOF 也报 `UnexpectedEof`，因此**部分长度头也被当作干净结束**。
  2. `length = u32::from_be_bytes(...) as usize`；`!(9..=MAX_FRAME).contains(&length)` → `Err(invalid("frame length out of range"))`。
  3. `read_exact(&mut body)`（`length` 字节）：中途 EOF 是**错误**（不是 `None`）。
  4. `kind = body[0]`；`id = u32::from_be_bytes(body[1..5])`；`json_length = u32::from_be_bytes(body[5..9]) as usize`。
  5. `9 + json_length > length` → `Err(invalid("JSON length out of range"))`。
  6. `json_length == 0` → `Value::Object(Default::default())`；否则 `serde_json::from_slice(&body[9..9+json_length]).unwrap_or(Value::Null)`（**解析失败不报错**，保留 `Null` 让上层回 `EINVAL "invalid JSON"`）。
  7. `payload = body[9+json_length..].to_vec()`；返回 `Ok(Some(Frame))`。
- `write_frame`：`serde_json::to_vec(&frame.json)`，失败 → `invalid("unserializable JSON")`；`length = 9 + json.len() + payload.len()`；`> MAX_FRAME` → `invalid("frame too large")`；写 13 字节头（`length as u32` 大端、`kind`、`id` 大端、`json.len() as u32` 大端）、json、payload，最后 `output.flush()`。
- 单元测试（1 个）：`round_trips_json_and_payload` — 写 `Frame::with_payload(RESULT, 7, json!({"a": 1}), vec![0, 255, 10])` 到 `Vec<u8>`，读回断言 `(kind, id) == (RESULT, 7)`、json 相等、payload 相等；再对空切片读 → `None`。

#### 2.2.4 `src/errors.rs`（126 行）

```rust
pub struct Failure { pub code: String, pub message: String, details: Option<Box<Details>> }
struct Details { syscall: Option<String>, path: Option<String>, extra: Option<Value> }

impl Failure {
    pub fn new(code: &str, message: impl Into<String>) -> Failure;
    pub fn extra(self, extra: Value) -> Failure;                       // 合并进错误 JSON（如 spillPath）
    pub fn io(error: &io::Error, syscall: &str, path: &str) -> Failure;
    pub fn io_between(error: &io::Error, syscall: &str, path: &str, destination: Option<&str>) -> Failure;
    pub fn path(self, path: &str) -> Failure;
    pub fn to_json(&self) -> Value;
}
fn describe(code: &str) -> Option<&'static str>;                       // libuv uv_strerror 文案
```

- `Failure::io(err, syscall, path)` = `io_between(err, syscall, path, None)`。
- `io_between`：`code = sys::error_name(error)`；`description = describe(code)`，未知 code 时取 `error.to_string()` 中 `" (os error"` **之前**的部分并 `to_lowercase()`；`target = destination.map(|d| format!("'{path}' -> '{d}'")).unwrap_or(format!("'{path}'"))`；`message = format!("{code}: {description}, {syscall} {target}")`；`details.syscall = syscall`；`path(path)`。
- `to_json()`：`{"code","message"}`，有 details 时追加 `syscall`、`path`，最后把 `extra` 的**对象字段逐个**写入（覆盖同名键）。
- `describe` 的完整表（**逐字**，注意 `ENOTSUP` 是 "operation not supported on socket"）：
  EACCES→permission denied、EAGAIN→resource temporarily unavailable、EBADF→bad file descriptor、EBUSY→resource busy or locked、EEXIST→file already exists、EFBIG→file too large、EINVAL→invalid argument、EIO→i/o error、EISDIR→illegal operation on a directory、ELOOP→too many symbolic links encountered、EMFILE→too many open files、EMLINK→too many links、ENAMETOOLONG→name too long、ENFILE→file table overflow、ENOENT→no such file or directory、ENOMEM→not enough memory、ENOSPC→no space left on device、ENOSYS→function not implemented、ENOTDIR→not a directory、ENOTEMPTY→directory not empty、ENOTSUP→operation not supported on socket、EPERM→operation not permitted、EROFS→read-only file system、ETXTBSY→text file is busy、EXDEV→cross-device link not permitted；其余 `None`。
- 无单元测试。

#### 2.2.5 `src/fs.rs`（258 行）

`pub type Outcome<T> = Result<T, Failure>`。

- `pub fn info(path: &str, metadata: &Metadata) -> Value`：`kind` 由 `metadata.file_type()` 判定（`file`/`directory`/`symlink`/`other`）；`name = Path::new(path).file_name().map(to_string_lossy).unwrap_or_default()`（**根路径得到空串**）；`{name, kind, size: metadata.len(), mtimeSec, mtimeNsec, dev, ino}`（后四个来自 `sys::mtime`/`sys::identity`）。
- `pub fn lstat(path)`：`symlink_metadata` → `info`；错误 `io(err, "lstat", path)`。
- `pub fn realpath(path)`：`sys::realpath` → `{path}`；错误 `io(err, "realpath", path)`。
- `pub fn mkdir_recursive(path: &Path) -> io::Result<()>`（**算法逐字**）：
  1. `create_dir(path)` 成功 → Ok。
  2. `AlreadyExists`：`fs::metadata(path).is_dir()` → Ok，否则返回原始错误（`EEXIST`）。
  3. `NotFound`：取 `parent()` 且父路径非空；没有父 → 返回原错误。递归 `mkdir_recursive(parent)`：
     - 父返回 `AlreadyExists` → **返回 `sys::synthetic("ENOTDIR")`**（父是文件但不是目录的情况）；
     - 其他错误 `?`。
     然后再 `create_dir(path)`；`AlreadyExists` 且 `is_dir` → Ok，否则原样。
  4. 其他错误原样。
- `pub fn mkdir(path, recursive) -> Outcome<Value>`：`recursive ? mkdir_recursive : create_dir`；错误 `io(err, "mkdir", path)`；成功 `{}`。
- `pub fn write(path, append, parents, content) -> Outcome<File>`：
  - `parents` 且父路径非空 → `mkdir_recursive(parent)`；错误 `io(err, "mkdir", parent)`（**syscall 是 mkdir，path 是父目录**）。
  - `sys::open(path, Append|Write)`；错误 `io(err, "open", path)`。
  - `write_all(content)`；错误 `io(err, "write", path)`；返回 `File`（供 `writeChunk`）。
- `fn open_existing_for_write(path)`：`sys::open(path, ReadWrite)`（Unix `read(true).write(true)`；Windows `FILE_GENERIC_READ | FILE_GENERIC_WRITE` + `OPEN_EXISTING`——**不创建、不截断**），错误 `io(err, "open", path)`。
- `pub fn truncate(path, size: u64)`：`open_existing_for_write(path)?`（打开失败 → `io(err, "open", path)`；**不创建缺失文件**），然后 `set_len(size)`；`set_len` 错误 `io(err, "ftruncate", path)`；成功 `{}`。
- `pub fn fsync(path)`：先 `metadata(path).is_ok_and(is_dir)` → `Failure::new("EISDIR", format!("EISDIR: illegal operation on a directory, open '{path}'")).path(path)`（**消息含路径，且是 "open" 不是 "fsync"**）；否则 `open_existing_for_write(path)?`（打开失败 → `io(err, "open", path)`；**文件不存在是 ENOENT，不是创建**）后 `sync_all()`，错误 `io(err, "fsync", path)`。
- `pub fn rename(from, to)`：`sys::rename`；错误 `io_between(err, "rename", from, Some(to))`。
- `pub fn rm(path, recursive, force)`：
  - `symlink_metadata`：`NotFound && force` → `Ok({})`；其他错误 `io(err, "lstat", path)`（**syscall 是 lstat**）。
  - 是目录且 `!recursive` → `Failure::new("ERR_FS_EISDIR", format!("Path is a directory: rm returned EISDIR (is a directory) {path}")).path(path)`。
  - 目录 → `remove_dir_all`；其他 → `sys::remove_file`。
  - 错误 `io(err, "rm", path)`。
- `pub fn mkdtemp(prefix)`：`sys::mkdtemp` → `{path}`；错误 `io(err, "mkdtemp", &format!("{prefix}XXXXXX"))`（**错误里的 path 带 `XXXXXX` 后缀**）。
- `pub fn open_reader(path, no_follow) -> Outcome<(File, Value)>`：`sys::open_reader` 后 `file.metadata()`（错误 `io(err,"fstat",path)`）；目录 → `Failure::new("EISDIR", "EISDIR: illegal operation on a directory, read").path(path)`（**消息无路径**）；非普通文件 → `Failure::new("NOT_REGULAR", "Not a regular file").path(path)`；否则返回 `(file, info(path, metadata))`。
- `pub fn pread(file, path, offset: u64, length) -> Outcome<Vec<u8>>`：循环 `sys::read_at` 直到填满或 0 字节；`Interrupted` 重试；其他错误 `io(err,"read",path)`；最后 `truncate(filled)`。
- `pub fn read(mut file, path, length) -> Outcome<Vec<u8>>`：**单次** `read` 调用；`Interrupted` 重试；错误 `io(err,"read",path)`；`truncate(read)`。
- `pub fn dir_entry(dir, file_name: &OsStr) -> Value`：
  - `name = to_string_lossy()`；`entry_path = Path::new(dir).join(&name)`（**注意用解码后的 name 去 lstat**，所以非 UTF-8 名字会得到 ENOENT，与 Node 一致）。
  - `symlink_metadata` 成功 → `{name, info}`；失败 → `{name, error: Failure::io(err, "lstat", &entry_path).to_json()}`。
  - `sys::raw_name(file_name)` 存在且 `!= name.as_bytes()` → 追加 `raw`（原始字节数组）。
- 无单元测试。

#### 2.2.6 `src/output.rs`（100 行）

```rust
const BULK_LIMIT: usize = 4 * 1024 * 1024;

#[derive(Default)] struct Queues { control: VecDeque<Frame>,
    bulk: VecDeque<(Frame, Option<Arc<AtomicUsize>>)>, bulk_bytes: usize, closed: bool }

#[derive(Default)] pub struct Output {
    queues: Mutex<Queues>,
    queued: Condvar,     // 有帧入队或 writer 停止时唤醒
    drained: Condvar,    // bulk 字节写出后唤醒
}

impl Output {
    pub fn control(&self, frame: Frame);
    pub fn bulk(&self, frame: Frame, unsent: Option<Arc<AtomicUsize>>);
    pub fn wait_for_room(&self, stop: &AtomicBool);
    pub fn close(&self);
    pub fn run(&self, writer: &mut impl Write);
}
```

- `control(frame)`：`push_back` 到 control 队列，`queued.notify_one()`。
- `bulk(frame, unsent)`：`bulk_bytes += frame.payload.len()`；若给了 `unsent` → `counter.fetch_add(1, SeqCst)`；`push_back((frame, unsent))`；`queued.notify_one()`。
- `wait_for_room(stop)`：在 `queues.bulk_bytes > BULK_LIMIT && !closed && !stop` 期间 `drained.wait_timeout(100ms)` 循环（**返回时未必满足条件，调用方需自己复核**）。
- `close()`：`closed = true`，`queued.notify_all()`，`drained.notify_all()`。
- `run(writer)`（**单写线程**）：
  1. 加锁后内循环：control 非空 → pop（`unsent = None`）；否则 bulk 非空 → pop 并 `bulk_bytes -= frame.payload.len()`；否则若 `closed` → 返回；否则 `queued.wait()` 释放锁等待。
  2. 解锁后 `frame::write_frame(writer, &frame)`；记录是否失败。
  3. 若 `unsent` 有计数 → `fetch_sub(1, SeqCst)`；`drained.notify_all()`。
  4. 失败 → `self.close()` 并返回（不重试）。

**关键语义**：control 严格优先于 bulk（即使 bulk 队列很长）；bulk 的字节计数在**出队时**减少（不是写出后），因此 `wait_for_room` 反映的是「尚未交给 writer 的字节」；`write_frame` 内部会 `flush`（每次写帧都 flush，依赖 `BufWriter` 的缓冲）。

#### 2.2.7 `src/exec.rs`（483 行）

**常量**：`EXIT_STDIO_GRACE = 100ms`、`UNSENT_POLL = 20ms`。**类型**：`pub type Groups = Arc<Mutex<HashSet<u32>>>`。

```rust
pub enum Message { Data(usize, Vec<u8>), Eof(usize), Exit(ExitStatus),
                   WaitFailed(String), Cancel, Kill }
#[derive(Default)] pub struct Control { pub aborted: AtomicBool, pub killed: AtomicBool,
                                        sender: Mutex<Option<Sender<Message>>> }
impl Control { pub fn cancel(&self, kill: bool); pub fn attach(&self, sender: Sender<Message>); }

pub struct ExecRequest { pub command: Option<String>, pub argv: Option<Vec<String>>,
    pub cwd: String, pub env: Map<String, Value>, pub inherit_env: bool,
    pub shell_path: Option<String>, pub timeout: Option<Duration>,
    pub spill: Option<(u64, u64)>, pub window: Option<Window> }
```

- `Control::cancel(kill)`：先置位（`killed` 或 `aborted`，SeqCst），再向 `sender`（若有）发 `Message::Kill` / `Message::Cancel`。
- `Control::attach(sender)`：保存 sender。
- `ExecRequest::from_json`（逐字）：
  - `command = json["command"].as_str()`；`argv = json["argv"].as_array()` 的每项 `as_str` 收集（**任一项非字符串 → `argv = None`**，后续按空处理）。
  - `cwd` 缺失 → `Failure::new("EINVAL", "exec needs cwd")`。
  - `env = json["env"].as_object().cloned().unwrap_or_default()`（**Map 保序**）。
  - `inherit_env = json["inheritEnv"].as_bool().unwrap_or(true)`。
  - `shell_path = json["shellPath"].as_str()`。
  - `timeout = json["timeoutMs"].as_f64().map(|ms| Duration::from_secs_f64(ms / 1000.0))`。
  - `spill = json["spill"].as_object().and_then(|s| Some((s["afterBytes"].as_u64()?, s["afterLines"].as_u64()?)))`（**两个字段都必需**，否则 `None`）。
  - `window = Window::from_json(&json["window"])`。
- `fn random_uuid()`：`sys::random_bytes()` 取 16 字节；`bytes[6] = (b & 0x0f) | 0x40`；`bytes[8] = (b & 0x3f) | 0x80`；拼成 `8-4-4-4-12` 小写十六进制。
- `struct Spill { after_bytes, after_lines, seen_bytes, seen_newlines, prefix: Vec<Vec<u8>>, file: Option<File>, path: Option<String>, failed: Option<String> }`：
  - `push(chunk, tmpdir)`：`failed.is_some() || chunk.is_empty()` → 返回。若 `file.is_none()`：累加 `seen_bytes` 与 `seen_newlines`（`\n` 计数）；**`lines = seen_newlines + (chunk.last() != Some(&b'\n')) as u64`**（末尾无换行时 +1，模拟 Node 的行计数）；若 `seen_bytes <= after_bytes && lines <= after_lines` → 缓存到 `prefix` 并返回；否则 `start(tmpdir)`，失败记 `failed`。之后每块 `write_all`，失败记 `failed = Some(error.to_string())`。
  - `start(tmpdir)`：`prefix_dir = Path::new(tmpdir).join("tmp-")`；`sys::mkdtemp`；`path = directory/kiana-output-{uuid}.log`；`OpenOptions::new().append(true).create(true).open(path)`；把 `prefix` 逐块写入；保存 `file`/`path`。
- `fn spawn(request, program, args, piped_stdin) -> io::Result<Child>`：`current_dir(cwd)`；`stdin`（`piped` 或 `null`）；stdout/stderr piped；`sys::configure(&mut command, args, &request.env, request.inherit_env)`；`spawn()`；`sys::after_spawn(&child)`。
- `fn spawn_reader(stream, pipe, sender, stop: Arc<AtomicBool>, output: Option<Arc<Output>>) -> JoinHandle<()>`：线程里 `sys::read_pipe(pipe, &stop, |bytes| { sender.send(Data(stream, bytes.to_vec())).is_ok(); if let Some(output) { output.wait_for_room(&stop) } ; delivered })`，结束后 `sender.send(Eof(stream))`。
  - **注意**：只有**没有 window** 时才传 `output`（`gate`），即无窗口输出靠 4 MiB 背压节流；有 window 时不背压（由 window 裁剪）。
- `fn output_frame(id, stream, text, skipped) -> Frame`：EVENT 帧，json `{kind:"output", stream, ...(skipped)}`，payload = `text.into_bytes()`。
- `pub fn run(id, request, tmpdir, output, control, groups) -> Result<Value, Failure>`（**完整流程**）：
  1. `mpsc::channel()`；`control.attach(sender.clone())`；若 `control.aborted` 已置位 → `Err(Failure::new("aborted","aborted"))`。
  2. 决定程序与参数：
     - `(Some(command), _)`：`shell = sys::shell_config(shell_path)`；若 `shell.command_on_stdin` → `(shell.program, shell.args, Some(command))`；否则 `args.push(command)` → `(program, args, None)`。
     - `(None, Some(argv))`：`argv.split_first()`；空 → `Failure::new("spawn_error", "Empty argv: no program to run")`；否则 `program = sys::resolve_program(first, &request.cwd, &request.env)?`，`args = rest`。
     - `(None, None)` → `EINVAL "exec needs command or argv"`。
  3. `!Path::new(&cwd).exists()` → `Failure::new("spawn_error", format!("Working directory does not exist: {}\nCannot execute bash commands.", cwd))`。
  4. `spawn(...)`；错误 → `Failure::new("spawn_error", error.to_string())`。
  5. 若 `stdin_command` 与 `child.stdin.take()` 都有 → 写入命令文本（不关闭；由 drop 关闭）。
  6. `pid = child.id()`；`groups.lock().insert(pid)`；`deadline = timeout.map(|t| Instant::now() + t)`；`stop = Arc::new(AtomicBool::new(false))`。
  7. `gate = if window.is_none() { Some(output.clone()) } else { None }`；起两个 reader（stdout=0、stderr=1）。
  8. 起 waiter 线程：`child.wait()` → `Message::Exit(status)` 或 `Message::WaitFailed(error.to_string())`。然后 `drop(sender)`。
  9. 两个 `StreamDecoder::new()`；`spill`；`pending = window.map(Pending::new)`；`unsent = Arc::new(AtomicUsize::new(0))`；`names = ["stdout","stderr"]`。
  10. `emit(pending, stream, text)`：空文本返回；有 pending → `pending.push(names[stream], text)`；否则 `output.bulk(output_frame(id, names[stream], text, None), None)`。
  11. `deliver(pending)`：对 `pending.take()` 的每个事件 `output.bulk(output_frame(...), Some(unsent.clone()))`。
  12. 状态机：`ended = [false,false]`、`status: Option<Result<ExitStatus,String>>`、`idle_until: Option<Instant>`、`timed_out`、`aborted`、`killed`；`kill` 闭包（只杀一次，`kill_group(pid)`）；若 `control.killed` 已置位 → 先 kill。
  13. 主循环（每轮）：
      - `now = Instant::now()`。
      - 超时且未标记：`timed_out = true`；`kill`。
      - `status.is_some() && ended[0] && ended[1]` → break。
      - `idle_until` 到期 → break（后代进程可能仍持有管道）。
      - 有 pending、非空、`now >= next_send()`、`unsent == 0` → `deliver`。
      - `wait` 初始 3600 s；取 min（deadline 剩余、idle 剩余、pending 时 `unsent > 0 ? 20ms : next_send 剩余`）。
      - `recv_timeout(wait)`：
        - `Data(stream, bytes)`：`emit(pending, stream, decoders[stream].decode(&bytes, false))`；有 spill → `spill.push(&bytes, tmpdir)`，失败则 kill；若 `status.is_some()` → `idle_until = now + 100ms`。
        - `Eof(stream)`：`ended[stream] = true`。
        - `Exit(exit)`：`status = Some(Ok(exit))`；`idle_until = now + 100ms`。
        - `WaitFailed(error)`：`status = Some(Err(error))`；`idle_until = Some(now)`。
        - `Cancel`：`aborted = true`；kill。
        - `Kill`：kill。
        - `Timeout`：继续。
        - `Disconnected`：break。
  14. 收尾：`stop = true`；对每个 reader `sys::interrupt_reader(reader)`；`groups.remove(&pid)`；`emit` 两个 decoder 的 `decode(&[], true)`（flush 残留）；还有 pending 就 `deliver`。
  15. `spill_path = spill.path.clone()`；`with_spill(failure)` 在有 spillPath 时 `failure.extra(json!({"spillPath": path}))`。
  16. 结算顺序（**严格**）：`timed_out` → `Failure("timeout", "timeout")`（**message 就是 `"timeout"`**）；`aborted` → `Failure("aborted","aborted")`；spill 失败 → `Failure("unknown", format!("Failed to preserve complete shell output: {error}"))`；`status` 为 `Err(e)` → `Failure("spawn_error", e)`；`status` 为 `None` → `Failure("spawn_error", "process did not report its exit")`；否则 `{exitCode: sys::exit_code(status), ...(spillPath)}`。
  - 注意顺序：**timeout 与 aborted 都会带上 spillPath**（`with_spill` 应用在前两者上）；spill 失败的错误**不带** spillPath。

#### 2.2.8 `src/sys/mod.rs`（91 行，`pub use unix::*` / `pub use windows::*`）

```rust
#[cfg(unix)] mod unix;  #[cfg(unix)] pub use unix::*;
#[cfg(windows)] mod windows;  #[cfg(windows)] pub use windows::*;

pub struct ShellConfig { pub program: String, pub args: Vec<String>, pub command_on_stdin: bool }
#[derive(Clone, Copy)] pub enum OpenMode { Read, Write, Append, ReadWrite }
pub struct Synthetic(pub &'static str);          // Display 直接输出 code；impl Error
pub fn synthetic(code: &'static str) -> std::io::Error;   // io::Error::other(Synthetic(code))

pub fn error_name(error: &std::io::Error) -> &'static str {
    // 先 downcast 到 Synthetic，命中返回其 code；否则 os_error_name(error)
}
pub fn path_exists(path: &str) -> bool;          // Path::new(path).metadata().is_ok()（跟随符号链接）
pub fn random_bytes() -> [u8; 16];               // RandomState + 静态 AtomicU64（fetch_add 每次 +1）+ SystemTime 纳秒；跑两轮，每轮 SipHash(计数, 纳秒) 取 finish().to_le_bytes() 填 8 字节
pub fn cwd() -> String;                          // current_dir lossy，失败返回空串（不是 "."）
```

注意 `cwd()` 失败返回**空字符串**（`unwrap_or_default()`），不是 `"."`。

#### 2.2.9 `src/sys/unix.rs`（323 行）

- `pub fn os_error_name(error) -> &'static str`：无 `raw_os_error` → `"UNKNOWN"`；否则 match libc 常量到名字，覆盖 `EPERM、ENOENT、EIO、EBADF、EAGAIN、ENOMEM、EACCES、EBUSY、EEXIST、EXDEV、ENOTDIR、EISDIR、EINVAL、ENFILE、EMFILE、ETXTBSY、EFBIG、ENOSPC、EROFS、EMLINK、ENAMETOOLONG、ENOTEMPTY、ELOOP、ENOSYS、ENOTSUP`，其余 `"UNKNOWN"`。（**用 libc 常量而不是数字**，天然处理 macOS 的 ENOTEMPTY=35 等差异。）
- `pub const PATH_SEPARATOR: &str = "/"`。
- `pub fn read_at(file, buffer, offset) -> io::Result<usize>`：`file.read_at(buffer, offset)`（`FileExt`）。
- `pub fn mtime(metadata) -> (i64, i64)`：`(metadata.mtime(), metadata.mtime_nsec())`。
- `pub fn identity(metadata) -> (u64, u64)`：`(metadata.dev(), metadata.ino())`。
- `pub fn tmpdir() -> String`：依次 `TMPDIR`、`TMP`、`TEMP` 取第一个**非空**；都没有时：Android → `format!("{}/tmp", PREFIX 或 "/data/data/com.termux/files/usr")`，其他 → `"/tmp"`；`path.len() > 1 && ends_with('/')` → 去掉一个尾斜杠。
- `pub fn home() -> String`：`HOME` 或空串。
- `pub fn mkdtemp(prefix) -> io::Result<String>`：`template = prefix.bytes + b"XXXXXX"` → `CString`（内含 NUL → `EINVAL` 的 io 错误）；`libc::mkdtemp`；失败 `last_os_error`；返回 lossy 字符串。
- `pub fn open_reader(path, no_follow) -> Result<File, Failure>`：`OpenOptions::new().read(true).custom_flags(O_NONBLOCK | (no_follow ? O_NOFOLLOW : 0))`；错误时若 `no_follow && errno ∈ {ELOOP, EMLINK}` → `Failure::new("SYMLINK","Refusing to follow a symbolic link").path(path)`，否则 `Failure::io(err,"open",path)`。
- `pub fn open(path, mode)`：`Read` → `read(true)`；`Write` → `write(true).create(true).truncate(true)`；`Append` → `append(true).create(true)`；`ReadWrite` → `read(true).write(true)`（**不创建**）。
- `pub fn realpath` → `std::fs::canonicalize` lossy；`pub fn rename` → `std::fs::rename`；`pub fn remove_file` → `std::fs::remove_file`。
- `pub trait Pipe: io::Read + AsRawFd {}` + blanket impl。
- `pub fn read_pipe(mut pipe, stop, on_data) -> ()`：
  - `fcntl(F_GETFL)` 后 `F_SETFL | O_NONBLOCK`。
  - 循环 `while !stop`：`poll(POLLIN, 100ms)`；`ready == 0` 或 `EINTR` → continue；`ready < 0` → return；`read` 到 64 KiB 缓冲：`Ok(0)` → return；`Ok(n)` → `on_data(&buf[..n])` 为 false 则 return；`WouldBlock`/`Interrupted` → 继续；其他错误 → return。
- `pub fn interrupt_reader(_) {}`（unix 空实现；reader 靠 `stop` 标志轮询）。
- `pub fn resolve_program(program, _cwd, _env) -> Result<String, Failure>`：**原样返回**（依赖 `Command` 的 `execvp` PATH 查找）。
- `fn which_bash() -> Option<String>`：`which bash`（stdin null、stderr null）；非零 → None；取 stdout 第一行 trim 后 `path_exists` → Some。
- `pub fn shell_config(shell_path) -> Result<ShellConfig, Failure>`：
  - `shell(program)` 闭包 = `{program, args: ["-c"], command_on_stdin: false}`。
  - 配置了 shell：存在 → 用它；不存在 → `Failure::new("shell_unavailable", format!("Custom shell path not found: {configured}"))`。
  - 否则 `/bin/bash` 存在 → 用它；否则 `which_bash() ?? "sh"`。
- `pub fn configure(command, args, env, inherit_env)`：
  - `command.args(args)`；`!inherit_env` → `env_clear()`；逐条 `env(key, value)`（只接受字符串值）。
  - `pre_exec`：`libc::setsid()`（**忽略返回值**）；`1..32` 中除 SIGKILL/SIGSTOP 的信号 `SIG_DFL`；`sigemptyset` + `sigprocmask(SIG_SETMASK)`。**unix 不设 umask**。
- `pub fn after_spawn(_) {}`。
- `pub fn kill_tree(pid: u32)`：`kill(-pid, SIGKILL) != 0` → 再 `kill(pid, SIGKILL)`（两次都忽略结果）。
- `pub fn exit_code(status) -> i64`：`code().unwrap_or(128 + signal().unwrap_or(0))`。
- `pub fn raw_name(name) -> Option<Vec<u8>>`：`Some(name.as_bytes().to_vec())`。
- `pub fn drive_cwds() -> Map`：空 Map。
- `pub fn read_dir(path)` → `std::fs::read_dir`。
- `pub fn path_identity(_path, metadata, _follow) -> (u64,u64)`：`identity(metadata)`（**三参数签名**：path、metadata、follow）。

#### 2.2.10 `src/pty.rs`（556 行）

常量：`EXIT_GRACE = 100ms`、`MAX_CELLS = 4096`。

```rust
struct Terminal {
    input: Sender<Vec<u8>>,
    #[cfg(unix)] master: Arc<std::fs::File>,
    #[cfg(unix)] pid: u32,   // 程序 pid，也是其进程组 id（它领导终端的 session）
}
pub type Terminals = Arc<Mutex<HashMap<u32, Arc<Terminal>>>>;   // 键是 pty 请求的 id
pub struct PtyRequest { argv: Option<Vec<String>>, cwd: String, env: Map<String,Value>,
                        inherit_env: bool, cols: u16, rows: u16 }
fn size(json, key) -> Result<u16, Failure>;
impl PtyRequest { pub fn from_json(json: &Value) -> Result<PtyRequest, Failure>; }
pub fn write(terminals, json, payload: Vec<u8>) -> Result<Value, Failure>;
pub fn resize(terminals, json) -> Result<Value, Failure>;
pub fn foreground(terminals, json) -> Result<Value, Failure>;
pub fn run(id, request, output, control, groups, terminals) -> Result<Value, Failure>;   // unix / non-unix 两版
```

- `size(json, key)`：`as_u64()` 且在 `1..=4096` → `u16`；否则 `EINVAL "{key} must be between 1 and 4096"`。
- `PtyRequest::from_json`：
  - `argv`：`Value::Null` → `None`；否则必须是「字符串数组且非空」，失败 → `EINVAL "argv must be a non-empty array of strings"`。
  - `cwd` 缺失 → `EINVAL "pty needs cwd"`。
  - `env` 默认空对象；`inherit_env` 默认 **true**；`cols`/`rows` 用 `size`。
- `find(terminals, json)`：`target` 缺失 → `EINVAL "missing field target"`；不存在 → `EBADF "no running terminal with this id"`。
- `write`：`terminal.input.send(payload)`；失败（接收端已 drop）→ `EBADF "the terminal has ended"`。
- `resize`：`size` 校验后 `unix::set_size(&master, cols, rows)`；错误 **`Failure::io(err, "ioctl", "pty")`**（syscall 固定 `ioctl`，path 固定 `pty`）。非 unix 直接 Ok。
- `foreground`：`unix::foreground(&master)` 即 `tcgetpgrp`；错误 `Failure::io(err, "tcgetpgrp", "pty")`；`group <= 0 || group as u32 == terminal.pid` → `{"foreground": null}`；否则 `{"foreground": {"pid": group, "name": unix::process_name(group)}}`（**name 字段总是存在，值可为 null**）。非 unix → `{"foreground": null}`。
- 非 unix `run` → `Err(Failure::new("ENOTSUP", "Terminals are not supported on this system yet"))`。
- unix `run`（逐条）：
  1. `mpsc::channel()`；`control.attach(sender.clone())`；`control.aborted` → `Failure("aborted","aborted")`。
  2. cwd 不存在 → **`Failure("spawn_error", format!("Working directory does not exist: {}", cwd))`**（**没有**“Cannot execute bash commands.”第二行，与 exec 不同）。
  3. `unix::spawn(&request)`；错误 → `Failure("spawn_error", error.to_string())`；得到 `(master, child)`。
  4. `pid = child.id()`；`groups.insert(pid)`；新建 `(input, inputs) = mpsc::channel::<Vec<u8>>()`；`terminals.insert(id, Arc::new(Terminal { input, master: master.clone(), pid }))`。
  5. **先发 `started` 事件**：`output.control(Frame::new(EVENT, id, json!({"kind":"started","pid":pid})))`（走 control 队列，不是 bulk）。
  6. `stop = Arc::new(AtomicBool::new(false))`；起 reader 线程：`unix::read(&master, &stop, |bytes| { sender.send(Message::Data(0, bytes.to_vec())).is_ok(); output.wait_for_room(&stop); delivered })`，结束发 `Message::Eof(0)`。**注意：PTY 输出无窗口时也总是 `wait_for_room`（4 MiB 背压），且不去重/不合并**。
  7. 起 writer 线程：`for bytes in inputs { if !unix::write_all(&master, &bytes, &stop) { return; } }`（按到达顺序写）。
  8. 起 waiter 线程：`child.wait()` → `Message::Exit` / `Message::WaitFailed`；然后 `drop(sender)`。
  9. 主循环：`status.is_some() && ended` → break；`wait = idle_until`（到期直接 break，注释：程序退出后后台任务可能仍开着终端）否则 3600 s；`recv_timeout`：
     - `Data(_, bytes)` → `output.bulk(Frame::with_payload(EVENT, id, json!({"kind":"output"}), bytes), None)`（**原始字节，不解码**）；若 `status.is_some()` → `idle_until = now + 100ms`。
     - `Eof` → `ended = true`；`Exit` → `status = Some(Ok(exit))` 且 `idle_until = now + 100ms`；`WaitFailed` → `status = Some(Err(error))` 且 `idle_until = Some(now)`；`Cancel` → `aborted = true` + kill；`Kill` → kill；`Timeout` 继续；`Disconnected` break。
  10. 收尾：`stop = true`；`terminals.remove(&id)`（**移除终端，后续 ptyWrite 得到 `EBADF "no running terminal with this id"`**）；`groups.remove(&pid)`；`reader.join()`。
  11. `aborted` → `Failure("aborted","aborted")`；否则 `Ok({"exitCode": sys::exit_code(status)})`；`Err(error)` → `Failure("spawn_error", error)`；`None` → `Failure("spawn_error", "process did not report its exit")`。
- unix 子模块：
  - `fn check(result: libc::c_int) -> io::Result<c_int>`（`< 0` → `last_os_error`）。
  - `fn winsize(cols, rows) -> libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 }`。
  - `pub fn set_size(master, cols, rows)`：`ioctl(TIOCSWINSZ)`。
  - `pub fn foreground(master) -> io::Result<pid_t>`：`tcgetpgrp`；`< 0` → 错误。
  - `pub fn process_name(pid) -> Option<String>`：`#[cfg(target_vendor = "apple")]` 用 `libc::proc_name(pid, buf, 256)`（`length > 0` 才返回，lossy）；`#[cfg(any(target_os = "linux", target_os = "android"))]` 读 `/proc/{pid}/comm` 并 `trim_end()`，空则 `None`；其他平台 `None`。
  - `fn cloexec(fd)`：`fcntl(F_GETFD)` 后 `F_SETFD | FD_CLOEXEC`。
  - `fn command(request) -> Command`：有 `argv` → `Command::new(&argv[0])` + `args(&argv[1..])`；无 `argv` → `shell = request.env["SHELL"]（字符串） ?? std::env::var("SHELL") ?? "/bin/sh"`，但**过滤掉空串与不存在的路径**（`sys::path_exists`）；`name = shell.rsplit('/').next()`；`command.arg0(format!("-{name}"))`（登录 shell 约定）。
  - `pub fn spawn(request) -> io::Result<(File, Child)>`：
    - `openpty(&mut master, &mut slave, null, null, &winsize)`；转 `OwnedFd`；两个 fd 都 `cloexec`；slave 转 `File`。
    - `Command` 的 stdin/stdout/stderr 都 `Stdio::from(slave.try_clone()?)`；`current_dir(cwd)`。
    - `!inherit_env` → `env_clear()`；然后逐条 `env(key, value)`（只接受字符串值）。
    - `pre_exec`：`setsid()`；`ioctl(0, TIOCSCTTY, 0)`；把 `1..32` 中除 `SIGKILL`/`SIGSTOP` 的信号重置为 `SIG_DFL`；`sigemptyset` + `sigprocmask(SIG_SETMASK)` 清掩码。
  - `pub fn read(master, stop, on_bytes) -> ()` / `pub fn write_all(master, bytes, stop) -> bool`（非阻塞 + poll，与 `sys::read_pipe` 同风格）。

#### 2.2.11 `src/sys/windows.rs`（665 行）

- **`os_error_name`**（`GetLastError` 码 → libuv 名，逐条含注释）：
  | code | name | 注释 |
  |---|---|---|
  | 1 | EISDIR | ERROR_INVALID_FUNCTION |
  | 2, 3, 15, 123, 161, 4392 | ENOENT | FILE_NOT_FOUND, PATH_NOT_FOUND, INVALID_DRIVE, INVALID_NAME, BAD_PATHNAME, INVALID_REPARSE_DATA |
  | 4 | EMFILE | ERROR_TOO_MANY_OPEN_FILES |
  | 5, 1314 | EPERM | ACCESS_DENIED, PRIVILEGE_NOT_HELD |
  | 6 | EBADF | ERROR_INVALID_HANDLE |
  | 8, 14 | ENOMEM | NOT_ENOUGH_MEMORY, OUTOFMEMORY |
  | 17 | EXDEV | ERROR_NOT_SAME_DEVICE |
  | 19 | EROFS | ERROR_WRITE_PROTECT |
  | 32, 33 | EBUSY | SHARING_VIOLATION, LOCK_VIOLATION |
  | 39, 82, 112 | ENOSPC | HANDLE_DISK_FULL, CANNOT_MAKE, DISK_FULL |
  | 50 | ENOTSUP | ERROR_NOT_SUPPORTED |
  | 80, 183 | EEXIST | FILE_EXISTS, ALREADY_EXISTS |
  | 13, 87, 122, 131, 1464 | EINVAL | INVALID_DATA, INVALID_PARAMETER, INSUFFICIENT_BUFFER, NEGATIVE_SEEK, SYMLINK_NOT_SUPPORTED |
  | 126 | ENOENT | ERROR_MOD_NOT_FOUND |
  | 145 | ENOTEMPTY | ERROR_DIR_NOT_EMPTY |
  | 206 | ENAMETOOLONG | ERROR_FILENAME_EXCED_RANGE |
  | 267 | ENOENT | ERROR_DIRECTORY（libuv 映射为 ENOENT） |
  | 998 | EFAULT | ERROR_NOACCESS |
  | 740, 1920 | EACCES | ELEVATION_REQUIRED, CANT_ACCESS_FILE |
  | 1921 | ELOOP | ERROR_CANT_RESOLVE_FILENAME |
  | 其他 | UNKNOWN | |
- `PATH_SEPARATOR = "\\"`。
- `read_at`：`file.seek_read(buffer, offset)`。
- `mtime`：`metadata.modified()` 失败 → `(0,0)`；1970 后 `(secs, nanos)`；1970 前 `before.duration()`，`nanos == 0` → `(-(secs), 0)`，否则 `(-(secs) - 1, 1_000_000_000 - nanos)`。
- `identity(metadata)`：`(0, metadata.creation_time())`（注释：file index 需要句柄；creation time 足以区分被替换的文件）。
- `tmpdir()`：`TEMP` → `TMP` → `format!("{}\\temp", SystemRoot 或 windir 或 "")`；`len > 1 && ends_with('\\') && !ends_with(":\\")` → 去掉一个尾斜杠。
- `home()`：`USERPROFILE` 或空串。
- `wide(text)`：含 `\0` → `io::Error::new(InvalidInput, "path contains a NUL byte")`；否则 `encode_wide() + [0]`。
- `mkdtemp(prefix)`：`CHARACTERS = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"`；`COUNTER: AtomicU64` + `RandomState`；**最多 1000 次**：每次 6 个字符（`random % 62`，再 `random /= 62`），`create_dir` 成功返回路径，`AlreadyExists` 重试，其他错误返回，用尽 → `io::Error::from_raw_os_error(80)`。
- `open_reader(path, no_follow)`：`no_follow` 且 `symlink_metadata.is_symlink()` → `SYMLINK "Refusing to follow a symbolic link"`；`metadata.is_dir()` → `EISDIR "EISDIR: illegal operation on a directory, read"`；否则 `File::open`，错误 `io("open")`。
- `open(path, mode)`（`CreateFileW`）：
  | mode | access | disposition |
  |---|---|---|
  | Read | `FILE_GENERIC_READ` | `OPEN_EXISTING` |
  | Write | `FILE_GENERIC_WRITE` | `CREATE_ALWAYS` |
  | Append | `(FILE_GENERIC_WRITE & !FILE_WRITE_DATA) | FILE_APPEND_DATA` | `OPEN_ALWAYS` |
  | ReadWrite | `FILE_GENERIC_READ | FILE_GENERIC_WRITE` | `OPEN_EXISTING` |
  - share：`FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE`；flags：`FILE_ATTRIBUTE_NORMAL | FILE_FLAG_BACKUP_SEMANTICS`（目录也能打开）。
  - `INVALID_HANDLE_VALUE`：若 `raw_os_error() == Some(80)` 且 mode 是 Write/Append → `synthetic("EISDIR")`；否则原错误。
- `realpath`：`canonicalize` lossy；去 `\\?\UNC\` → `\\`，否则去 `\\?\`。
- `rename`：`MoveFileExW(..., MOVEFILE_REPLACE_EXISTING)`；0 → `last_os_error`。
- `remove_file`：`symlink_dir`（junction/目录符号链接）→ `remove_dir`；`PermissionDenied` → 取 permissions、`set_readonly(false)`、`set_permissions`、再 `remove_file`；其他原样。
- `trait Pipe: io::Read`（**没有 AsRawFd**）。
- `read_pipe`：`while !stop` 用**阻塞** `read` 到 64 KiB；`Ok(0) | Err(_)` → return；`!on_data` → return。
- `interrupt_reader(reader)`：`CancelSynchronousIo(reader.as_raw_handle())`。
- `resolve_program(program, cwd, env) -> Result<String, Failure>`：
  - `has_extension = Path::new(program).extension().is_some()`。
  - `candidates(base)`：有扩展名时先加 base 本身；再对 `["com","exe"]` 各加 `.{ext}`。
  - 含 `\`、`/` 或 `:` → `candidates(Path::new(cwd).join(program))` 找第一个 `is_file()`。
  - 否则：`PATH` 取 `env` 中忽略大小写匹配 `PATH` 的字符串值，否则进程 `PATH`；候选顺序 `[cwd] + split_paths(PATH)`，每个目录 `candidates(dir.join(program))`，找第一个 `is_file()`。
  - 找不到 → `Failure("spawn_error", format!("spawn {program} ENOENT"))`。
  - 找到了但路径**小写**以 `.bat`/`.cmd` 结尾 → `Failure("spawn_error", format!("spawn {program} EINVAL"))`（拒绝批处理）。
- `is_legacy_wsl_bash(path)`：`path.replace('/', "\\").to_lowercase()`；首字符是 ascii 小写字母，余下匹配 `":\\windows\\system32\\bash.exe"` 或 `":\\windows\\sysnative\\bash.exe"`。
- `bash_config(program)`：legacy → `{args: ["-s"], command_on_stdin: true}`；否则 `{args: ["-c"], command_on_stdin: false}`。
- `where_bash()`：`where bash.exe`（`CREATE_NO_WINDOW`、stdin null、stderr null）；取 stdout 第一行 `trim()` 且 `path_exists`。
- `shell_config(shell_path)`：
  - 配置了：存在 → `bash_config(configured)`；否则 `shell_unavailable Custom shell path not found: {configured}`。
  - 候选：`%ProgramFiles%\Git\bin\bash.exe`、`%ProgramFiles(x86)%\Git\bin\bash.exe`；第一个 `path_exists` 的用。
  - `where_bash()`。
  - 否则 `shell_unavailable`，message **逐字**：
    ```
    No bash shell found. Options:
      1. Install Git for Windows: https://git-scm.com/download/win
      2. Add your bash to PATH (Cygwin, MSYS2, etc.)
      3. Configure an explicit shellPath

    Searched Git Bash in:
      <candidate1>
      <candidate2>
    ```
    构造方式：`format!("...Searched Git Bash in:\n{}", searched.trim_end())`，其中 `searched` 是每个候选 `format!("  {candidate}\n")` 拼接后去掉末尾换行（**因此两个候选时是两行，一个候选时是一行；`candidates` 只包含环境变量存在的那些**）。
- `quote_argument(argument)`（libuv `quote_cmd_arg`，**逐字算法**）：
  1. 空 → `""`（含两个引号）。
  2. 不含空格/制表/双引号 → 原样。
  3. 不含双引号也不含反斜杠 → `"{argument}"`。
  4. 否则**反向**遍历字符：`quote_hit = true` 初始；每字符压栈；若 `quote_hit && c == '\\'` → 再压一个 `\\`；否则若 `c == '"'` → `quote_hit = true` 并压 `\\`；否则 `quote_hit = false`。最后反转、前后加 `"`。
  - 7 个单测 `quotes_arguments_like_libuv`（逐字断言）：`""→"\"\""`；`"plain"→"plain"`；`"a b"→"\"a b\""`；`"*.txt"→"*.txt"`；`"say \"hi\""→"\"say \\\"hi\\\"\""`；`"C:\\dir with space\\"→"\"C:\\dir with space\\\\\""`；`"a\\\\b c"→"\"a\\\\b c\""`。
- `REQUIRED_VARIABLES: [&str; 11] = ["HOMEDRIVE","HOMEPATH","LOGONSERVER","PATH","SYSTEMDRIVE","SYSTEMROOT","TEMP","USERDOMAIN","USERNAME","USERPROFILE","WINDIR"]`。
- `child_environment(env, inherit_env)`：
  1. `entries: Vec<(String,String)>`；`set(key, value)`：**精确同名**的已有项改值，否则 push（不做大小写合并）。
  2. `inherit_env` → 依次 `std::env::vars_os()`（lossy）`set`。
  3. 再 `env` 中字符串值 `set`（覆盖同名）。
  4. `entries.sort_by(|a, b| a.0.encode_utf16().cmp(b.0.encode_utf16()))`（UTF-16 码元序）。
  5. `retain`：`seen.insert(name.to_uppercase())` 第一次出现保留（**大小写去重保留先出现的**）。
  6. 对每个 `REQUIRED_VARIABLES`：若没有忽略大小写匹配的项且 `std::env::var(required)` 成功 → push `(required, value)`（**追加到末尾，顺序固定**）。
- `configure(command, args, env, inherit_env)`：每个 `arg` → `command.raw_arg(quote_argument(arg))`；`env_clear()`；逐条设 `child_environment` 的结果；`creation_flags(CREATE_NO_WINDOW)`。
- `job()`：`OnceLock<Option<isize>>`；`CreateJobObjectW(null, null)`；`JOBOBJECT_EXTENDED_LIMIT_INFORMATION.BasicLimitInformation.LimitFlags = BREAKAWAY_OK | SILENT_BREAKAWAY_OK | DIE_ON_UNHANDLED_EXCEPTION | KILL_ON_JOB_CLOSE`；`SetInformationJobObject(JobObjectExtendedLimitInformation, ...)`。
- `after_spawn(child)`：`AssignProcessToJobObject(job, child.as_raw_handle())`（失败忽略）。
- `kill_tree(pid)`：`"{SystemRoot 或 C:\Windows}\System32\taskkill.exe" /F /T /PID {pid}`，`CREATE_NO_WINDOW`、三个 stdio 都 null，`spawn()` 后**不等待**。
- `exit_code(status)`：`i64::from(status.code().unwrap_or(1) as u32)`。
- `raw_name(_)`：`None`。
- `drive_cwds()`：遍历 `vars_os()`，键 `to_str()` 且 3 字节、`[0] == b'='`、`[1]` 是 ascii 字母、`[2] == b':'` → `(key[1..].to_ascii_uppercase(), value)`。
- `read_dir(path)`：`read_dir` 错误且 `raw_os_error() == Some(267)` → `synthetic("ENOTDIR")`。
- `path_identity(path, metadata, follow)`：`wide(path)` 失败 → `identity(metadata)`；flags = `FILE_FLAG_BACKUP_SEMANTICS | (follow ? 0 : FILE_FLAG_OPEN_REPARSE_POINT)`；`CreateFileW(0 access, share all, OPEN_EXISTING, flags)`；失败 → `identity`；`GetFileInformationByHandle` 失败 → `identity`；成功 → `(dwVolumeSerialNumber as u64, (nFileIndexHigh << 32) | nFileIndexLow)`；关闭句柄。

#### 2.2.12 `src/window.rs`（269 行）

```rust
#[derive(Clone, Copy)]
pub struct Window { pub max_bytes: usize, pub max_lines: usize,
                    pub min_interval: Duration, pub bytes_per_second: f64 }
impl Window {
  pub fn from_json(json: &Value) -> Option<Window>  // 四个字段全部必需
}
struct Skipped { bytes: u64, newlines: u64, ends_with_newline: bool }
pub struct Event { pub stream: &'static str, pub text: String, pub skipped: Option<Value> }
pub struct Pending { window, segments: Vec<(&'static str, String)>,
                     bytes: usize, newlines: usize, skipped: Option<Skipped>, next_send: Instant }
```

- `Window::from_json`：`json.as_object()?`；`max_bytes = object["maxBytes"].as_u64()? as usize`；`max_lines` 同理；`min_interval = Duration::from_secs_f64(object["minIntervalMs"].as_f64()?.max(0.0) / 1000.0)`（**浮点毫秒**，允许小数）；`bytes_per_second = object["bytesPerSecond"].as_f64()?.max(1.0)`。任一缺失/类型不符 → `None`。
- `fn newline_count(text) -> usize`：`text.bytes().filter(|b| *b == b'\n').count()`。
- `Pending::new(window)`：空段、计数 0、`skipped = None`、`next_send = Instant::now()`。
- `is_empty()` / `next_send()`。
- `push(stream, text: String)`：
  1. `text.is_empty()` → 直接返回。
  2. `bytes += text.len()`（**UTF-8 字节数**）；`newlines += newline_count(&text)`。
  3. 若最后一段同 `stream` → `push_str` 合并；否则 `segments.push((stream, text))`。
  4. 若 `bytes > 4 * (max_bytes + 1) || newlines > 4 * (max_lines + 1)` → `trim()`。
- `fn proves(&self, bytes, newlines) -> bool`：`bytes > max_bytes || newlines > max_lines`。
- `fn trim()`（精确算法，必须照抄）：
  1. `removed = String::new()`。
  2. **整段移除**：`while segments.len() > 1`：取首段的 `bytes` 与 `newlines`；若 `!proves(self.bytes - bytes, self.newlines - newlines)` → break；`segments.remove(0)`；减少计数；`removed.push_str(&text)`。
  3. **裁剪首段前缀**（按字符边界）：
     - `rest_bytes = bytes - segments[0].1.len()`；`rest_newlines = newlines - newline_count(&segments[0].1)`。
     - `keep_bytes = (max_bytes + 1).saturating_sub(rest_bytes)`；若 `first.len() > keep_bytes`：`index = first.len() - keep_bytes`，向前退到 `is_char_boundary`；`cut = cut.max(index)`。
     - `keep_newlines = (max_lines + 1).saturating_sub(rest_newlines)`；若 `keep_newlines > 0`：收集首段中所有 `\n` 的字节位置；若 `positions.len() >= keep_newlines` → `cut = cut.max(positions[positions.len() - keep_newlines])`；否则不裁剪（本段不够这么多换行，说明不能证明被删部分在窗口外）。若 `keep_newlines == 0` → `cut = first.len()`。
     - `cut > 0`：`first.drain(..cut)` 得到的 `prefix` 从段中删除；更新计数；`removed.push_str(&prefix)`；段空了就 `segments.remove(0)`。
  4. `removed` 非空 → 累加到 `skipped`（`bytes`、`newlines`、`ends_with_newline = removed.ends_with('\n')`；**用 `get_or_insert_with` 累积，多次 trim 会相加**）。
- `fn take(&mut self) -> Vec<Event>`：
  - `segments = mem::take(&mut self.segments)`；`delivered = self.bytes`；计数清零。
  - `pace = Duration::from_secs_f64(delivered.min(max_bytes) as f64 / bytes_per_second)`；`next_send = Instant::now() + min_interval.max(pace)`。
  - 有 `skipped` → **返回单个事件**：`stream` 取最后一段的 stream（无段则 `"stdout"`）；`text` 为所有段按序拼接；`skipped = {"bytes","newlines","endsWithNewline"}`。
  - 无 `skipped` → **每个段返回一个事件**（`skipped: None`），保持段顺序。**不是把段拼成一个事件**。
- 单元测试 1 `counts_everything_and_keeps_more_than_the_window_after_a_skip`：`window(10, 2)`，500 行 `format!("line {index} é😀\n")`，每 7 行切到 `stderr`，每 50 行 `take()`；最终 `bytes` 与 `newlines` 总数等于完整文本、`full.ends_with(&after)`、`after` 长度 > 10 或换行 > 2、至少一个事件带 `skipped`。
- 单元测试 2 `delivers_streams_apart_without_skips`：`window(1000,1000)`，push `stdout "a"`、`stderr "b"`、`stdout "c"` → 三个事件，stream 依次 `["stdout","stderr","stdout"]`（证明**不合并且不跳过**）。

#### 2.2.13 `src/decode.rs`（83 行）

```rust
pub struct StreamDecoder { decoder: encoding_rs::Decoder }   // 只有 decoder，没有额外的结束标记
impl StreamDecoder {
    pub fn new() -> StreamDecoder;                                  // UTF_8.new_decoder_with_bom_removal()
    pub fn decode(&mut self, bytes: &[u8], last: bool) -> String;   // 第三参是「流是否结束」
}
pub fn range_decoder() -> encoding_rs::Decoder;                     // UTF_8.new_decoder_without_bom_handling()
pub fn decode_into(decoder: &mut encoding_rs::Decoder, bytes: &[u8], last: bool) -> String;  // 内部按 max_utf8_buffer_length 预分配
pub fn starts_with_bom(bytes: &[u8]) -> bool;                       // EF BB BF
```

`StreamDecoder::decode(bytes, last)` 直接用 `decode_to_string(..., last)` 的替换语义：不完整序列在 `last = false` 时**留在解码器内部**，只有 `last = true` 才输出一次 U+FFFD。

三个单测（**逐字**）：
1. `keeps_a_feff_that_does_not_start_the_stream`：字节 `[0xe2, 0x82, 0xef, 0xbb, 0xbf, 0x61]` 逐字节 `decode(&[*byte], false)` 后 `decode(&[], true)` → 结果 `"\u{fffd}\u{feff}a"`（U+FEFF 不在流首，保留）。
2. `drops_only_a_leading_mark_and_never_sniffs_utf16`：
   - `decode(&[0xef,0xbb], false) + decode(&[0xbf,0x61], true)` → `"a"`（跨块的 BOM 也被移除）。
   - 新解码器 `decode(&[0xff,0xfe,0x41,0x00], true)` → `"\u{fffd}\u{fffd}A\u{0}"`（不嗅探 UTF-16）。
3. `replaces_an_incomplete_sequence_once`：
   - `decode(&[0xe2,0x82,0x41], true)` → `"\u{fffd}A"`（一个替换符 + A）。
   - `decode(&[0xe2,0x82], true)` → `"\u{fffd}"`（单个替换符，不是两个）。

注意：`StreamDecoder::decode` 的 `last` 是**调用方传入**，不是对象内部状态，Rust 侧照此即可（daemon 的 `exec.rs` 在收尾时调 `decode(&[], true)` 冲刷残留）。客户端 `RemoteTextLineReader` 复用的是 Durable 的 `StreamDecoder`（`durable/src/env/decode.ts`，不是本文件），两边的解码规则必须同源，否则差分测试会在非法字节上分歧。

#### 2.2.14 `src/scan.rs`（215 行）

```rust
pub struct LineScanner { /* start_line, end_line, 计数与偏移 */ }
impl LineScanner {
    pub fn new(start_line: u64, end_line: Option<u64>) -> LineScanner;
    pub fn push(&mut self, bytes: &[u8]);
    pub fn finish(self) -> Value;   // {newlines, start, end, firstLineEnd, lastLineStart, selectedBytes, firstLineBytes}
}
```
- `end_line <= start_line` 由调用方（`main.rs::scan_lines`）校验并返回 `EINVAL "Invalid line range"`；构造器不做校验。
- `push`：处理跨块的 BOM（文件前 3 字节为 `EF BB BF` 时跳过）、逐字节统计 `\n`、记录首行结束偏移、当前行号、末行开始、选中字节数。
- `finish`：先 `release_head()`（若还有未处理的头部缓冲）；`size = position`。
  - **没有 start 行时提前返回**（选中范围在文件结束之后）：`{newlines: self.newlines, start: size, end: size, firstLineEnd: size, lastLineStart: size, selectedBytes: 0, firstLineBytes: 0}`——所有偏移都等于 `size`、两个计数为 0。
  - 否则补齐 `end_first_line(size)`/`end_selection(size)`（若尚未设置），输出七个字段：`start`；`end = self.end.unwrap_or(size)`；`firstLineEnd = self.first_line_end.unwrap_or(size)`；`lastLineStart = self.last_line_start.unwrap_or(self.line_start)`（**回退到当前行起点，不是 0**；注释："A selection that reaches past the last line ends with the last line."）；`selectedBytes`、`firstLineBytes`。
  - `end` 在 `end_line = None` 时为文件总字节数；`start`/`end` 是**解码后视图中**的字节偏移（BOM 不计入）。

单元测试 `locates_and_measures_lines_like_whole_file_decoding`：
- 文件字节 `[0xEF,0xBB,0xBF,0x61,0x0A,0xE2,0x82,0x0A,0x0A,0xEF,0xBB,0xBF,0x62,0x0A,0xC3,0xA9]`（注释：BOM、`a`、非法序列、空行、后面的 U+FEFF、无末尾换行）。
- 对 `chunk in 1..6` 全部跑一遍（**任意分块都必须同结果**）：
  - `(0, None)` → `newlines = 4`、`selectedBytes = 14`、`firstLineBytes = 1`。注释给出解码后文本 `"a\n\u{fffd}\n\n\u{feff}b\né"` 为 1+1+3+1+1+3+1+1+2 = 14 字节。
  - `(1, Some(3))` → `start = 5`、`end = 8`、`selectedBytes = 4`、`lastLineStart = 8`。
  - `(9, None)` → `start = 16`。

#### 2.2.15 `src/list.rs`（368 行）

常量：`DEFAULT_MAX_ENTRIES = 50_000`、`DEFAULT_MAX_BYTES = 4 * 1024 * 1024`、`DEFAULT_TIMEOUT_MS = 10_000`。

```rust
pub struct Options { pub hidden: bool, pub gitignore: bool, pub exclude: Vec<OsString>,
                     pub max_entries: u64, pub max_bytes: u64, pub timeout: Duration }
impl Options { pub fn from_json(json: &Value) -> Result<Options, Failure>; }
pub struct Listing { pub payload: Vec<u8>, pub count: u64, pub truncated: bool }
fn relative(root: &Path, path: &Path) -> Option<String>;
pub fn list(root: &str, options: &Options, aborted: &dyn Fn() -> bool) -> Result<Listing, Failure>;
pub fn run(json: &Value, aborted: &dyn Fn() -> bool) -> Result<(Value, Vec<u8>), Failure>;
```

- `Options::from_json`：
  - **整个函数没有 `json["path"]` 的读取**——path 由 `run` 单独取（缺失 → `EINVAL "missing field path"`）。
  - `hidden = json["hidden"].as_bool().unwrap_or(true)`；`gitignore` 同理默认 true。
  - `exclude`：`Null` → 空；数组 → 每项 `as_str` 转 `OsString`，任一项失败 → `EINVAL "invalid field exclude"`；非数组 → 同错误。
  - `count(key, default)`：`Null` → default；否则必须 `as_u64`（非负整数），失败 → `EINVAL "invalid field {key}"`。
  - `max_bytes = count("maxBytes", 4MiB).min(MAX_PAYLOAD as u64)`（**静默钳制**）。
  - `timeout = Duration::from_millis(count("timeoutMs", 10_000)?)`。
- `fn relative(root, path)`：`strip_prefix(root)`；逐 component 只接受 `Component::Normal`（其他 → `None`）；用 `/` 连接；空结果 → `None`（根目录本身被跳过）。
- `pub fn list(...)`：
  1. `fs::metadata(root)` 失败 → `Failure::io(err, "scandir", root)`。
  2. 非目录 → `Failure::new("ENOTDIR", format!("ENOTDIR: not a directory, scandir '{root}'")).path(root)`。
  3. `WalkBuilder::new(root)`：`hidden(!options.hidden)`（**注意反义**）、`ignore(false)`、`parents(gitignore)`、`git_ignore(gitignore)`、`git_exclude(gitignore)`、`git_global(gitignore)`、`require_git(true)`（**只在 git work tree 内生效**）、`follow_links(false)`、`current_dir(root)`（**全局 excludes 相对被列目录查找**）、`sort_by_file_name(|a, b| a.cmp(b))`（字节序）、`filter_entry(|entry| entry.file_name() != ".git" && !exclude.contains(name))`。
  4. 遍历：`aborted()` → `Failure("aborted","aborted")`（**每条前都检查**）；`Err` entry → 跳过；`depth() == 0`（根本身）→ 跳过；`file_type()` 无 → 跳过。
  5. `kind`：symlink → `b'l'`；dir → `b'd'`；file → `b'f'`；其他（FIFO/socket/device）→ **跳过**。
  6. `relative` 失败 → 跳过。
  7. `size = path.len() + 2`；若 `count >= max_entries || payload.len() + size > max_bytes || started.elapsed() > timeout` → `truncated = true` 并 break（**在写出该条前判断**）。
  8. push `kind`、path 字节、`0`；`count += 1`。
- `pub fn run(json, aborted)`：`path` 缺失 → `EINVAL "missing field path"`；返回 `(json!({"count", "truncated"}), payload)`。
- 五个单元测试（逐字断言）：
  1. `lists_files_and_directories_depth_first_in_name_order`：建 `b.txt`、`a/z.txt`、`a/b/c.txt`、`empty/`、`.hidden/x`、`A.txt`；期望顺序 **`d .hidden`、`f .hidden/x`、`f A.txt`、`d a`、`d a/b`、`f a/b/c.txt`、`f a/z.txt`、`f b.txt`、`d empty`**（注意 `d a` 在 `f A.txt` 之后：字节序 `A`(0x41) < `a`(0x61)，且深度优先）；`hidden: false` 时不含 `.hidden`。
  2. `skips_what_git_ignores_only_in_work_trees`：建 `keep.txt`、`out/x.js`、`sub/keep.txt`、`sub/drop.log`、`node_modules/m/i.js`，写 `.gitignore` = `"out/\n*.log\n"`、`sub/.gitignore` = `"!keep.txt\n"`；**没有 `.git` 时全部列出**（含 `f out/x.js`）；写 `.git/HEAD` 与 `.git/info/exclude`（`"keep.txt\n"`）后，用 `exclude: ["node_modules"]` 得到**精确列表** `["f .gitignore", "d sub", "f sub/.gitignore", "f sub/keep.txt"]`；`gitignore: false` 时 `f out/x.js` 恢复、`.git/` 仍不列出；对 `sub` 子目录单独 `list` → payload **精确等于 `b"f.gitignore\0fkeep.txt\0"`**（父级规则生效）。
  3. `lists_symbolic_links_without_following_them`（unix）：`dir/inner.txt` + `link → dir` + `dangling → missing` + `dir/loop → 根`；期望 `["l dangling","d dir","f dir/inner.txt","l dir/loop","l link"]`。
  4. `stops_at_bounds_with_truncated`：三文件 `a`、`b`、`c`；`max_entries 2` → `(2, true)`；`max_entries 3` → `(3, false)`；`max_bytes 6` → `(["f a","f b"], true)`（每条 size = len+2 = 3，6 字节正好两条）；`timeout 0` → `truncated && empty`。
  5. `fails_on_missing_roots_files_and_aborts`：缺失 → code `ENOENT`；文件 → `ENOTDIR`；`aborted` 恒真 → `aborted`。

#### 2.2.16 `src/watch.rs`（778 行）

常量：`DEBOUNCE = 50ms`、`FSEVENTS_SETTLE = 500ms`（macOS）、`DEFAULT_POLL_MS = 2000`、`DEFAULT_MAX_DIRECTORIES = 10_000`、`HASH_MAX_BYTES = 256*1024`、`HASH_RECENT = 5s`、`CANCEL_POLL = 100ms`；`UNRELIABLE_FILE_SYSTEMS: [u64; 12]`（NFS 0x6969、SMB 0x517b、CIFS 0xff534d42、SMB2 0xfe534d42、FUSE 0x65735546、9P 0x01021997、Lustre 0x0bd00bd0、GPFS 0x47504653、Ceph 0x00c36400、OpenAFS 0x5346414f、kAFS 0x6b414653、sdcardfs 0x5dca2df5），仅 linux/android 编译。

**类型**
```rust
struct Target { path: String, recursive: bool, hidden: bool, names: HashSet<String> }
#[derive(Clone, Copy, PartialEq, Eq)] enum Kind { File, Directory, Symlink, Other }
#[derive(Clone, PartialEq, Eq)] struct Entry { kind: Kind, dev: u64, ino: u64,
                                               size: u64, mtime: (i64,i64), hash: Option<u64> }
type Snapshot = HashMap<String, Entry>;
enum Mode { Native, Polling }        // name() -> "native" | "polling"
enum ScanError { Budget(usize), Io(io::Error, &'static str, String) }
struct Scan { snapshot: Snapshot, linked_files: HashSet<String>, counted: HashSet<String>,
              traversed: HashSet<(usize, String)>, listed: HashMap<String, Kind> }
struct Installed { dev: u64, ino: u64, canonical: Option<String> }
struct FileWatcher<'a> { id, output, control, targets, mode, poll_interval, max_directories,
                         snapshot, native: Option<RecommendedWatcher>,
                         events_sender: Sender<notify::Result<Event>>,
                         installed: HashMap<String, Installed>, events: HashSet<String>,
                         settle_at: Option<Instant> }
```

- `ScanError::failure()`：`Budget(max)` → `Failure::new("EINVAL", format!("Watched paths exceed {max} directories"))`；`Io(error, syscall, path)` → `Failure::io(...)`。
- `ScanError::code()`：`Io` 且 `error.kind() == PermissionDenied` → `"permission_denied"`，否则 `"invalid"`。
- `kind_of(metadata)`：file / dir / symlink / other。
- `entry_of(path, metadata, follow, hash)`：`(dev, ino) = sys::path_identity(path, metadata, follow)`；**目录的 `size = 0` 且 `mtime = (0,0)`**（注释：目录自身的时间戳会因无关的兄弟项变化）；其他用真实值。
- `ancestors(path)`：`Path::ancestors().skip(1)`（**最近优先，不含自身**）。
- `with_separator(directory)`：已以 `PATH_SEPARATOR` 结尾则原样，否则加一个。
- `is_within(path, ancestor)`：`path == ancestor || path.starts_with(with_separator(ancestor))`。
- `excluded(target, name)`：`(target.hidden && name.starts_with('.')) || target.names.contains(name)`。
- `sort_like_javascript(paths)`：按 `encode_utf16()` 序排序。
- `any_unreliable(targets)`（linux/android）：对每个 target，从 `target.path` 及其 `ancestors` 依次 `statfs`（CString 失败则 break），第一个成功的 `f_type & 0xffff_ffff` 命中表 → true，否则 break（只看最近的可 statfs 祖先）；其他平台恒 false。
- `FileWatcher::send(json)`：`output.control(Frame::new(EVENT, id, json))`（**watch 事件走 control 队列，不走 bulk**）。
- `cancelled()`：`control.aborted`。
- `record(scan, path, metadata, follow)`：polling 模式 + 普通文件 + `len <= 256 KiB` + 修改时间在 5 s 内（`modified()` 失败视为 not recent）→ 计算 `DefaultHasher`（SipHash）内容哈希；`scan.snapshot.insert(path, entry_of(...))`。
- `count(scan, path)`：`counted.insert`；`len > max_directories` → `ScanError::Budget(max)`。
- `scan_directory(scan, index, directory)`：
  1. `traversed.insert((index, directory))` 已存在 → 直接返回（**同一 target 内不重复遍历**）。
  2. `skip(error)` 闭包：若 `directory == target.path` 且 `PermissionDenied` → `Err(Io(error, "scandir", directory))`；否则 `NotFound | PermissionDenied | NotADirectory` → `Ok(())`（跳过）；其他 → `Err(Io(..., "scandir", directory))`。
  3. `fs::read_dir(directory)`；每个 entry 取 `file_name().to_string_lossy()`。
  4. 对每个 name：`excluded` → continue；`path = prefix + name`；kind 先查 `scan.listed`，未命中则 `symlink_metadata`（失败 continue）→ `kind_of` → 存 `listed`；**若 `!scan.snapshot.contains_key(&path)` 则 `record(..., follow=false)`**（注释：target 自身经跟随链接的条目优先于其他 target 的列表）。
  5. `target.recursive && kind == Directory` → `count` 然后递归 `scan_directory`。
- `scan()`：
  1. 新建 `Scan`。
  2. 对每个 `(index, target)`：对每个 `ancestors(target.path)`，若 snapshot 已有则跳过；否则 `symlink_metadata` 成功时插入「identity only」条目（`size: 0, mtime: (0,0), ..entry_of(..., false, None)`）。
  3. `fs::metadata(&target.path)`（**跟随符号链接**）：`PermissionDenied` → `Err(Io(error, "stat", target.path))`；其他错误（含 NotFound）→ **continue**（缺失的 target 等待被创建）。
  4. `record(&mut scan, &target.path, &metadata, true)`；若 metadata 是文件且 `symlink_metadata` 是符号链接 → `linked_files.insert(target.path)`。
  5. 是目录 → `count` 然后 `scan_directory`。
- `reported(path)`：若存在 target 使 `is_within(path, target.path)`（path 在 target 内或等于）→ 返回 path；否则找第一个使 `is_within(target.path, path)` 的 target → 返回 `target.path`（祖先变化时报告整个 target）；都没有 → path。
- `diff(previous, next)`：对 `next` 中 `previous.get(path) != Some(entry)` 的 path → `reported`；再对 `previous` 中不在 `next` 的 path → `reported`。返回 `Vec<String>`（**可能含重复**，`sync` 用 HashSet 去重）。
- `sync(report: bool) -> Result<HashSet<String>, ScanError>`：
  - 最多 **10 轮**：cancelled → break；`scan()`；`report` 时 `changed.extend(diff(&self.snapshot, &scan.snapshot))`；`self.snapshot = scan.snapshot`；若 polling 或 `!reconcile(&scan.linked_files)` → break；macOS 设 `settle_at = now + 500ms`；`report = true`（新目录里先前写入的内容下一轮可见）。
- `reconcile(linked_files) -> bool`（返回是否新增了 watcher）：
  - `per_directory = cfg!(linux|android)`。
  - `wanted: HashMap<String,bool>`：先放 `linked_files` 的所有路径（`false`）。
  - 对每个 target：所有 snapshot 中是目录的 `ancestors` → `wanted.entry(ancestor).or_insert(false)`；若 target 本身在 snapshot 中且是目录 → `wanted.insert(target.path, target.recursive && !per_directory)`；若 `target.recursive && per_directory` → 所有 snapshot 中是目录且在 target 之下的 path 插入 `false`。
  - **stale**：`installed` 中不在 `wanted`，或 snapshot 里 dev/ino 与安装时不同的 path → `unwatch` 并移除。
  - 对 `wanted` 中每个 path（snapshot 有条目、未安装）：若 `native` 为 None → `RecommendedWatcher::new(move |event| drop(sender.send(event)), Config::default())`，失败 → `switch_to_polling()` 并返回 `false`。
  - `watch(path, if recursive { Recursive } else { NonRecursive })`：成功 → `canonical = fs::canonicalize(path)`（**与 path 不同才保存**）；`installed.insert(path, Installed { dev, ino, canonical })`；`added = true`。失败 → 若 `PathNotFound` 或 `Io(NotFound|PermissionDenied)` 视为 gone（静默忽略），否则 `switch_to_polling()` 并返回 `false`（watcher 名额耗尽/不支持）。
- `switch_to_polling()`：已是 polling → 返回；否则 `mode = Polling`、`native = None`、`installed.clear()`、**发 `{"kind":"change","overflow":true,"mode":"polling"}`**（**不是 error**）。
- `in_scope(path)`：对每个 target：`is_within(target.path, path)`（path 是 target 的祖先）→ true；若 `!is_within(path, target.path) || path == target.path` → continue；`relative = path[with_separator(target.path).len()..]`；按 `PATH_SEPARATOR` 切分；`!target.recursive && components.len() > 1` → continue；任一段 `excluded` → continue；否则 true。全不满足 → false。
- `watched_spelling(path)`：找 `installed` 中 canonical 是 path 祖先且最长的一个 → `format!("{watched}{}", &path[canonical.len()..])`；否则原样。
- `on_event(event) -> bool`（是否需要 rescan）：
  - `Err(error)` → 从 `installed` 删除 error.paths 对应项，返回 true。
  - `EventKind::Access(_)` → false。
  - `event.need_rescan() || event.paths.is_empty()` → true。
  - 对每个 path（转 `watched_spelling`）：`in_scope` → `events.insert(reported(path))` 并 `flush = true`。
- `flush() -> bool`：
  - `events = mem::take`；`sync(true)`：
    - Ok(changed)：合并 events；非空且未取消 → 排序（UTF-16）后发 `{"kind":"change","paths":[...]}`；返回 true。
    - Err(error) → 发 `{"kind":"error","code":error.code(),"message":error.failure().message}`；返回 false（**调用方结束 watcher**）。
- `parse_targets(json)`：`targets` 非数组 → `EINVAL "watch needs targets"`（**闭包 `invalid` 复用**：每个 target 缺 `path` 也是同一条消息）；每项 `{path, recursive 默认 false, exclude.hidden 默认 false, exclude.names 默认空}`；`names` 数组里**非字符串项被静默丢弃**（`filter_map`），`names` 非数组 → 空集（**不报错**）。
- `run(id, json, output, control)`：
  1. `parse_targets`。
  2. `mode`：`"native"` → Native；`"polling"` → Polling；未给且（Windows 或 `any_unreliable`）→ Polling，否则 Native。
  3. `poll_interval = json["pollIntervalMs"].as_u64().unwrap_or(2000)`；`max_directories = json["maxDirectories"].as_u64().map_or(10_000, |m| m as usize)`。
  4. `watcher.sync(false)?`（失败 → `error.failure()`）。
  5. 发 `{"kind":"ready","mode":watcher.mode.name()}`。
  6. 主循环：`flush_at`、`poll_at = now + poll_interval`；每轮：
     - `cancelled()` → `Ok({})`（**正常结束，不是 aborted 错误**）。
     - `wake = now + CANCEL_POLL`，再与 `flush_at`、`settle_at`、polling 时的 `poll_at` 取 min。
     - `events.recv_timeout(wake - now)`：Ok(event) → `on_event(event)` 且 `flush_at.is_none()` 且 `mode == Native` → `flush_at = now + DEBOUNCE`；Timeout → 继续；Disconnected → `unreachable!`。
     - 若 `flush_at`/`settle_at` 到期或 polling 到期：清对应计时；`!watcher.flush()` → `Ok({})`（error 已发）；polling 到期 → `poll_at = now + poll_interval`。

#### 2.2.17 `packages/env/docs/`（daemon 的协议与语义文档；**注意目录在包根，不在 `daemon/` 下**）

这两个文件随 npm 包发布（`files` 里有 `docs`），因此 Rust 侧也要生成同样内容；下文的路径均为 `packages/env/docs/protocol.md` 与 `packages/env/docs/semantics.md`。

- `docs/protocol.md`（151 行）是线协议的权威文本：帧布局（`u32 length`、`u8 type`、`u32 id`、`u32 jsonLength`、json、payload；`length` 为 `type..payload` 的总字节数）、类型表、cancel、调度（16 线程池、ptyWrite/ptyResize/ptyForeground 在读线程按到达顺序、chunk 写保序、control 优先于 bulk、>64 KiB 负载进 bulk、4 MiB 背压）、liveness（5 s ping、30 s 静默退出）、每个操作的请求/响应形状（与 §2.2.2 表一致）、`info` 形状、`readdir` 条目形状、句柄上限 4096（`EMFILE`）、exec 事件与结果、watch 事件、pty 操作。
- `docs/semantics.md`（68 行）是行为规则清单：
  - work split 表（哪部分在客户端、哪部分在 daemon）。
  - 不复用标准库的规则：recursive mkdir（已存在目录 OK）、rm（`ERR_FS_EISDIR`）、mkdtemp（POSIX 用 `path.join`）、tmpdir 查找顺序、listDir 字节序、`openBinaryReader`、`read*` 用 `open("r")`、`listFiles` 与 `readdir` 的差异、`readBinaryFile` 返回 `Buffer` 而 reader 返回 `Uint8Array`、Windows 全部规则。
  - Commands：`shellPath` 不存在 → `shell_unavailable`；`/bin/bash` → `which bash` → `sh`；argv 在 Windows 走 search_path + `.com`/`.exe` 并拒绝批处理；cwd 不存在 → `spawn_error`；env 顺序 daemon → shellEnv → env；setsid；100 ms grace；aborted；cleanup → 137；128+signal；spill。
  - Watching：native vs polling、debounce、overflow 语义。
- `README.md`（66 行）与 `CHANGELOG.md`（26 行，1.4.0 / 1.3.0 / 1.2.0（listFiles + PTY）/ 1.1.0 / 1.0.4）需要逐字保留（npm `files` 包含它们，CI 校验打包文件清单）。

### 2.3 `packages/durable/src/testing`

#### 2.3.1 `index.ts`（27 行，纯再导出）

逐字（注意 `TIMING_SCALE` 与四个基准类型也在公开面里；`registerEnvConformance`/`registerStorageConformance`/`StorageConformanceRunner` 同一行）：

```ts
export { createExpectAssertions, type ExpectLike } from "./assertions.ts";
export { createEnvConformance } from "./env-conformance.ts";
export { registerEnvConformance, registerStorageConformance, type StorageConformanceRunner } from "./runner.ts";
export {
  STORAGE_MEMORY_SCALES, STORAGE_READ_BENCHMARKS, STORAGE_WRITE_BENCHMARKS,
  type StorageBenchmarkDataset, type StorageBenchmarkScale, type StorageReadBenchmark, type StorageWriteBenchmark,
  seedStorageBenchmark, seedStorageWriteBenchmark, storageBenchmarkPrimaryRecordCount, TIMING_SCALE,
} from "./storage-benchmark.ts";
export { createStorageConformance } from "./storage-conformance.ts";
export type {
  EnvConformanceAssertions, EnvConformanceCase, EnvConformanceOptions, EnvConformanceProvider,
  StorageConformanceAssertions, StorageConformanceCase, StorageConformanceOptions, StorageConformanceProvider,
} from "./types.ts";
```

注意没有 `export *`：所有再导出都是显式名单，Rust 侧对应一个 `pub use` 列表的 `mod prelude`。

#### 2.3.2 `types.ts`（46 行，全部为类型）

```ts
export interface StorageConformanceAssertions {
  ok(value: unknown, message?: string): void;
  strictEqual(actual: unknown, expected: unknown): void;
  deepEqual(actual: unknown, expected: unknown): void;
  partialDeepEqual(actual: unknown, expected: unknown): void;
  greaterThan(actual: number, expected: number): void;
  rejects(operation: Promise<unknown>, messageIncludes: string): Promise<void>;
}
export type StorageConformanceProvider = (use: (storage: Storage) => Promise<void>) => Promise<void>;
export interface StorageConformanceOptions { readonly assertions: StorageConformanceAssertions;
                                              readonly withStorage: StorageConformanceProvider; }
export interface StorageConformanceCase { readonly name: string; run(): Promise<void>; }
export type EnvConformanceAssertions = StorageConformanceAssertions;
export type EnvConformanceProvider = (use: (env: ExecutionEnv) => Promise<void>) => Promise<void>;
export interface EnvConformanceOptions {
  readonly assertions: EnvConformanceAssertions;
  readonly withEnv: EnvConformanceProvider;
  readonly shell?: readonly string[];      // 默认 ["sh","-c"]
  readonly symlinks?: boolean;             // 默认 true
  readonly listFiles?: boolean;            // 默认 false
}
export interface EnvConformanceCase { readonly name: string; readonly timeoutMs?: number; run(): Promise<void>; }
```

重要细节（与早期草稿不同）：
- `StorageConformanceAssertions` 的方法**没有 `message?` 参数**（除 `ok` 有 `message?: string`）；`rejects(operation, messageIncludes)` 的第二个参数是**必填**，语义是“错误消息包含该子串”。
- `StorageConformanceOptions` **没有 `name?` 前缀字段**（`env-conformance`/`storage-conformance` 也都只接受 `assertions`/`withStorage`/`withEnv` 等）。
- `StorageConformanceProvider`/`EnvConformanceProvider` 是**函数类型别名**（形参名 `use`），不是接口。
- `EnvConformanceOptions.shell` 类型是 `readonly string[]`（不是 `readonly [string, ...string[]]`），文档注释为 "Program and flag that run a POSIX shell script from the next argument; default `["sh", "-c"]`"。
- `EnvConformanceCase.timeoutMs` 的注释："Cases that wait for an environment's watch latency need longer than a test runner's default timeout."

#### 2.3.3 `assertions.ts`（39 行）

```ts
export type ExpectLike = (actual: unknown, message?: string) => unknown;
type ExpectResult = {   // 仅内部
  toBe(expected: unknown): unknown;  toBeGreaterThan(expected: number): unknown;
  toEqual(expected: unknown): unknown;  toMatchObject(expected: unknown): unknown;
  toBeTruthy(): unknown;
  readonly rejects: { toThrow(expected?: string | RegExp): unknown };
};
export function createExpectAssertions(expect: ExpectLike): StorageConformanceAssertions;
```

- 文档注释逐字："Adapts a Vitest/Jest-compatible `expect` function without importing either runner."
- 实现：`const result = (actual, message?) => expect(actual, message) as ExpectResult`；返回对象方法：
  `ok(value, message)` → `result(value, message).toBeTruthy()`；
  `strictEqual(actual, expected)` → `result(actual).toBe(expected)`（**不传 message**）；
  `deepEqual` → `toEqual`；`partialDeepEqual` → `toMatchObject`；`greaterThan` → `toBeGreaterThan`；
  `async rejects(operation, messageIncludes)` → `await result(operation).rejects.toThrow(messageIncludes)`。
- 所以 Rust 侧断言适配器需要 6 个操作，其中只有 `ok` 接受 message；`rejects` 匹配子串（Vitest 的 `toThrow(string)` 是“消息包含”语义）。

#### 2.3.4 `runner.ts`（38 行）

```ts
export interface StorageConformanceRunner {
  readonly describe: (name: string, suite: () => void) => unknown;
  readonly expect: ExpectLike;
  readonly it: (name: string, test: () => Promise<void>, timeoutMs?: number) => unknown;
}
export function registerStorageConformance(runner: StorageConformanceRunner, name: string,
                                           withStorage: StorageConformanceProvider): void;
export function registerEnvConformance(runner: StorageConformanceRunner, name: string,
  withEnv: EnvConformanceProvider,
  options: Pick<EnvConformanceOptions, "shell" | "symlinks" | "listFiles"> = {}): void;
```

- 两个 `register*` 的签名是**位置参数** `(runner, name, provider[, options])`，不是 options 对象。
- `registerStorageConformance`：`createStorageConformance({ assertions: createExpectAssertions(runner.expect), withStorage })` → `runner.describe(name, () => { for (case) runner.it(case.name, case.run); })`（**不传 timeout**）。
- `registerEnvConformance`：`createEnvConformance({ ...options, assertions: createExpectAssertions(runner.expect), withEnv })` → 每个 case `runner.it(case.name, case.run, case.timeoutMs)`（**传 timeoutMs**，可能是 `undefined`）。
- `options` 默认 `{}`，只透传 `shell`/`symlinks`/`listFiles`（不含 `assertions`/`withEnv`）。

#### 2.3.5 `env-conformance.ts`（633 行）

**内部常量/辅助（逐字）**
- `const context = BACKGROUND_CONTEXT`；`decoder = new TextDecoder()`；`encoder = new TextEncoder()`。
- `type EnvTest = (env: ExecutionEnv) => Promise<void>`。
- `abortedContext(): Context`：`new AbortController()` + `controller.abort()` + `withAbortSignal(controller.signal, BACKGROUND_CONTEXT)`。
- `errorCode(result)`：`result.ok ? undefined : result.error.code`。
- `readAll(env, path, maxEntries) -> { pages: FileInfo[][]; done: boolean }`：`openDirReader`；最多 1000 次 `next(maxEntries)`；`next.done` → `{pages, done:true}`；超 1000 → `{pages, done:false}`；finally `close`。
- `listed(env, path, options?) -> { entries: string[]; truncated }`：`env.listFiles` 未定义 → `throw new Error("The environment does not implement listFiles")`；letter 映射 `{file:"f",directory:"d",symlink:"l"}`；每项 `${letter} ${path}`。
- `covers(change, path)`：有 `overflow` → true；无 `paths` → false；否则 `paths.some(reported => path === reported || path.startsWith(`${reported}/`) || path.startsWith(`${reported}\\`))`。
- `watching(env, targets, run)`：`changes: WatchChange[] = []`；`env.watch(targets, change => changes.push(change), context)`；helpers 提供 `changes`、`absolute(path) = getOrThrow(await env.absolutePath(path, context))`、`expectChange(path, change)`：
  - 记录 `from = changes.length`；`await change()`；`deadline = Date.now() + 3000`。
  - 循环直到 `changes.slice(from).some(entry => covers(entry, target))`；超时 → `` throw new Error(`No change reported ${target}; got ${JSON.stringify(changes.slice(from))}`) ``；间隔 **20 ms**。
  - finally `watcher.close(context)`。
- `execCollect(env, command, cwd?) -> { result, stdout, stderr }`：`onOutput` 按 `info.stream` 累加。
- `createEnvConformance(options)`：`shell = options.shell ?? ["sh","-c"]`；`symlinks = options.symlinks ?? true`；`createCase(name, test, timeoutMs?)` → `{name, ...(timeoutMs === undefined ? {} : { timeoutMs }), run: () => options.withEnv(test)}`（**`undefined` 时完全不写该键**，不是 `timeoutMs: undefined`）；`watchCase(name, test) = createCase(name, test, 30_000)`。

**用例（29 个；名称逐字，已从源码核对）**

Binary reader（4）:
1. `"binary reader reads byte ranges of the opened file"`：写 `data.txt` = `"hello world"`；`info` 部分匹配 `{name:"data.txt", kind:"file", size:11}`；`read(0,5)` → `"hello"`；`read(6,100)` → `"world"`；`read(11,4)` 长度 0；`read(50,1)` 长度 0；`read(3,0)` 长度 0；`read(-1,1)` → `invalid`；`read(0,1.5)` → `invalid`；`read(0,1, abortedContext())` → `aborted`；`close` 两次；关闭后 `read`/`info` → `invalid`。
2. `"binary reader scans lines like decoding the whole file"`：字节 `[0xef,0xbb,0xbf,0x61,0x0a,0xe2,0x82,0x0a,0x0a,0xef,0xbb,0xbf,0x62,0x0a,0xc3,0xa9]`；`lines = decoder.decode(bytes).split("\n")`；范围 `[0,undefined]`、`[0,1]`、`[1,3]`、`[2,3]`、`[3,undefined]`、`[4,9]`；对每个范围：`scan.newlines === lines.length - 1`；`range(scan.start, scan.end) === selected.join("\n")`；`scan.selectedBytes === encoder.encode(selected.join("\n")).length`；`range(scan.start, scan.firstLineEnd) === lines[startLine]`；`scan.firstLineBytes === encoder.encode(lines[startLine]).length`。其中 `range(from,to) = new TextDecoder("utf-8", { ignoreBOM: from > 0 }).decode(bytes.subarray(from,to))`。另：`{startLine: 9}` 部分匹配 `{start: bytes.length, end: bytes.length, selectedBytes: 0}`；`{startLine:2,endLine:2}` → `invalid`。
3. `"binary reader keeps reading the file it opened after a rename"`：`a.txt`=`"one"`；打开 reader；`renameFile("a.txt","b.txt")`；`writeFile("a.txt","two")`；`read(0,10)` 仍为 `"one"`。
4. `"binary reader refuses directories, missing files and aborted opens"`：`dir` → `is_directory`；`missing.txt` → `not_found`；`file.txt` + aborted context → `aborted`。

Directory reader（4）:
5. `"directory reader pages every entry exactly once"`：5 个文件 + `sub` 目录；`readAll(env, ".", 2)`；`done` 为真；每页 ≤ 2；合并后 name 排序等于 `[...names, "sub"].sort()`；`sub` 的 kind 为 `directory`；`a.txt` 部分匹配 `{kind:"file", size:5}`。
6. `"directory reader reports the end and refuses use after close"`：空目录；`next(10)` 两次都 `{entries: [], done: true}`；`next(0)` → `invalid`；`next(1, abortedContext())` → `aborted`；`close` 两次；再 `next(1)` → `invalid`。
7. `"directory reader refuses missing paths and files"`：`openDirReader("missing")` → `not_found`；`openDirReader("file.txt")` → `not_directory`；`openDirReader(".", abortedContext())` → `aborted`。
8. `"directory reader skips entries removed during enumeration"`：建 `dir/x,y,z`；打开 reader；删除全部三个；最多 10 页收集，断言 entries 为 `[]`（**daemon 的 `readdir` 对 ENOENT 条目跳过，客户端 DirReader 同**）。

Watch（8，全部 `timeoutMs: 30_000`）:
9. `"watch reports a missing file's creation, changes, replacement and removal"`：target `[{path:"AGENTS.md"}]`；依次 expectChange：创建 `"one"`、改 `"two!"`、写 `AGENTS.md.tmp` 后 rename 覆盖、改 `"four"`、删除。
10. `"watch reports a missing target whose ancestors are created"`：target `a/b/c/AGENTS.md`，一次写入 `"x"` 即应报告。
11. `"watch follows directories created together with their contents"`：先 createDir `skills`；target `{path:"skills", recursive:true}`；写 `skills/a/b/SKILL.md`（**在任何新目录 watcher 存在之前写入**）、再改、再写更深 `skills/a/b/c/SKILL.md`。
12. `"watch keeps watching a path whose parent is renamed and recreated"`：`proj/.kiana/skills/x.md`；target `proj/.kiana/skills` recursive；rename `proj/.kiana` → `proj/old`（应报告 target 本身）；写 `proj/.kiana/skills/y.md`；再改。
13. `"watch skips excluded entries and reports a rename out of them"`：target `{path:"skills", recursive:true, exclude:{hidden:true, names:["node_modules"]}}`；先写 `skills/node_modules/dep/SKILL.md` 与 `skills/.SKILL.md.tmp`（**都不应报告**）；然后 rename `.SKILL.md.tmp` → `skills/SKILL.md` 应报告；并遍历所有 change 的 paths 断言不等于且不以 `skills/node_modules`、`skills/.SKILL.md.tmp` 为前缀。
14. `"watch keeps recursive coverage where a non-recursive target overlaps"`：targets `[{path:"skills"},{path:"skills",recursive:true}]`；写 `skills/a/two.md` 应报告。
15. `"watch follows a directory replaced at the same path"`：`skills/a/x.md`；target `skills` recursive；rename `skills/a`→`skills-old` 并 createDir `skills/a`（报告 `skills/a`）；写 `skills/a/y.md`；再改。
16. `"watch stops reporting once closed"`：`watcher.mode` 必须是 `"native"|"polling"`；`close` 两次；写 `file.txt`；等 300 ms；`changes` 仍为 `[]`。

Shell（6）:
17. `"argv exec passes arguments to the program without shell parsing"`：`hostile = "it's $(touch pwned) \`touch pwned\` *; touch pwned"`；argv 为 `[...shell, 'printf "%s|%s" "$1" "$2"', "argv0", hostile, "a b"]`；exitCode 0；stdout **精确等于** `${hostile}|a b`；`exists("pwned")` 为 false。
18. `"exec reports the stream of every chunk in both forms"`：脚本 `"printf out; printf err >&2; printf more"`；argv 形式与字符串形式都：exit 0、stdout `"outmore"`、stderr `"err"`。
19. `"argv exec honors cwd and exit codes"`：cwd `sub`，脚本 `"printf x > made.txt; exit 3"`；exit 3；`sub/made.txt` 内容 `"x"`。
20. `"argv exec reports missing programs and empty argv as spawn errors"`：`["kiana-durable-conformance-missing-program"]` → `spawn_error`；`[]` → `spawn_error`。
21. `"windowed exec keeps the exact tail and counts what it skips"`：2000 行 `echo line-$i`；window `{maxBytes:200, maxLines:5, minIntervalMs:0, bytesPerSecond:1_000_000_000}`；累计 `bytes`/`newlines`；遇到 `skipped` 时断言「其后文本 > window」并清空 `tail`；最终 `bytes === expected.join("").length`、`newlines === 2000`、`tail.endsWith(expected.slice(-5).join(""))`。
22. `"argv exec distinguishes timeout from abort"`：`sleep 2` + `timeout: 0.1` → `timeout`；`sleep 2` + 100 ms 后 abort → `aborted`。

listFiles（5，仅 `listFiles: true`）:
23. `"listFiles lists files and directories depth first in name order"`：写 `b.txt`、`a/z.txt`、`a/b/c.txt`、`.hidden/x`、`A.txt`，建 `empty`；`listed(".")` **精确** `{entries: ["d .hidden","f .hidden/x","f A.txt","d a","d a/b","f a/b/c.txt","f a/z.txt","f b.txt","d empty"], truncated:false}`；`{hidden:false}` → 去掉 `.hidden` 两项；`listed("a")` 与 `listed(<absolute a>)` 都等于 `{entries:["d b","f b/c.txt","f z.txt"],truncated:false}`；`listed("empty")` → `{entries:[],truncated:false}`。
24. `"listFiles skips what git ignores in work trees, .git, and excluded names"`：写 `keep.txt`、`out/x.js`、`sub/keep.txt`、`sub/drop.log`、`node_modules/m/i.js`、`.gitignore`=`"out/\n*.log\n"`、`sub/.gitignore`=`"!keep.txt\n"`；无 `.git` 时 `f out/x.js` 在列；建 `.git/HEAD` 与 `.git/info/exclude`=`"keep.txt\n"` 后 `exclude:["node_modules"]` **精确** `["f .gitignore","d sub","f sub/.gitignore","f sub/keep.txt"]`；`listed("sub")` **精确** `["f .gitignore","f keep.txt"]`；`{gitignore:false}` 时三个被忽略项都出现且不含 `.git/` 或 `d .git`。
25. `"listFiles stops at its bounds and reports truncated"`：三文件 `a`、`b`、`c`；`maxEntries:2` → `{["f a","f b"], true}`；`maxEntries:3` → 三个、false；`maxBytes:6` → `{["f a","f b"], true}`；`maxEntries:0` → `{[], true}`；`maxEntries:-1` → `invalid`；`timeoutMs:1.5` → `invalid`。
26. `"listFiles fails on missing directories, files, and aborts"`：`missing` → `not_found`；`file.txt` → `not_directory`；aborted → `aborted`。
27. `"listFiles lists symbolic links without following them"`（`symlinks && listFiles`）：`ln -s dir link && ln -s missing dangling && ln -s .. dir/loop`；`listed(".")` **精确** `["l dangling","d dir","f dir/inner.txt","l dir/loop","l link"]`；`listed("link")` **精确** `["f inner.txt","l loop"]`（被列目录本身可以是链接）。

符号链接（2，`symlinks: true`）:
28. `"binary reader follows symlinks unless noFollow refuses the final one"`：`target.txt`=`"target"`；`sub/inner.txt`=`"inner"`；`ln -s target.txt link.txt && ln -s sub dirlink`；`openBinaryReader("link.txt")` 读到 `"target"`；`{noFollow:true}` → `invalid`；`dirlink/inner.txt` + `noFollow` 仍能读到 `"inner"`（**只拒绝最后一段**）。
29. `"watch reports changes to the file a watched symbolic link points to"`：`data/real.md`=`"one"`；`ln -s ../data/real.md config/AGENTS.md`；target `config/AGENTS.md`；改 `data/real.md` 为 `"two!"` 应报告。

#### 2.3.6 `storage-conformance.ts`（1813 行）

- `const adopt = (): void => {}`（存储层提交没有内存 owner 需要 adopt）。
- `createRoot(storage)`：`commit([{ type: "conversation", value: { id: ROOT_CONVERSATION_ID } }], adopt, context)` 后返回 `ROOT_CONVERSATION_ID`（= 1）。
- `pendingTask(id, conversationId, phase = "ready")`：`{id, conversationId, kind: "test.task", version: 1, input: {value: id}, state: {status: "pending", checkpoint: {phase}}, background: false, abortRequested: false}`。
- `entry(id, conversationId, kind = "message", extra = {})`：`{id, conversationId, kind, ...extra}`。
- `assertionFacade(assertions)`：把断言包装成 `expect(actual)` 返回 `{toBe, toBeDefined, toBeGreaterThan, toBeUndefined, toEqual, toHaveLength, toMatchObject, rejects.toThrow, resolves.toBe}`：
  - `toBe` → `strictEqual`；`toBeDefined` → `ok(actual !== undefined, "Expected value to be defined")`；`toBeGreaterThan` → `greaterThan`；`toBeUndefined` → `strictEqual(actual, undefined)`；`toEqual` → `deepEqual`；`toHaveLength` → `strictEqual(actual.length, n)`；`toMatchObject` → `partialDeepEqual`；`rejects.toThrow(msg)` → `assertions.rejects(Promise.resolve(actual), msg)`；`resolves.toBe(v)` → `strictEqual(await actual, v)`。
- `checkAdoption(storage, assertions)`：返回用 `Proxy` 包装的 storage，拦截 `commit`：
  - `recording(seq)`：`assertions.ok(!settled, "Storage.commit() adopted after it settled")`；记录并调用原 `adopt`。
  - 成功：`settled = true`；`assertions.deepEqual(adopted, [seq])`；返回 seq。
  - 失败：`settled = true`；`assertions.deepEqual(adopted, [])`；rethrow。
  - 其他属性：函数绑定到原对象（`Reflect.get(target, property, target)`）。
- `createCase(options, name, test)`：`{name, run: () => options.withStorage(storage => test(checkAdoption(storage, options.assertions)))}`。
- `createStorageConformance(options)`：`expect = assertionFacade(options.assertions)`；返回 26 个 case（顺序即源码顺序）。

**用例（26 个，名称逐字；顺序即 `createStorageConformance` 返回数组的顺序）与关键断言**

1. `"reserves ID 1 for the immutable root conversation"`：`await mintId<ConversationId>()` → `2`；`await expect(createRoot(storage)).resolves.toBe(1)`；`conversation(1)` → `{id: 1}`；`commit([{type:"conversation", value:{id:1}}])` reject，消息包含 `ID 1 already belongs to conversation`（`ID ${ROOT_CONVERSATION_ID} already belongs to conversation`）。
2. `"adopts a durable commit once before resolving, and never a failed one"`：`createRoot`；commit 一条 entry，adopt 回调里 `events.push(\`adopt ${adopted}\`)` 并发起 `storage.entry(entryId)` 读（读取结果必须在 adopt 后可见，即 `commitSeq === seq`）；成功后 `events` 精确为 `[\`adopt ${seq}\`, \`resolve ${seq}\`]`（**adopt 先于 resolve**）；失败的 commit（重复 conversation id）→ `events` 保持 `[]`；随后一次成功 commit → `events` 为 `[\`adopt ${next}\`]` 且 `next > seq`。
3. `"commits mixed table writes atomically and rolls all of them back on failure"`：一次 commit entry+task+submission；读回逐字段相等；失败的混合 commit（含重复 conversation）→ **全部回滚**（task/submission 仍是旧值，`entry(transientEntryId)` 为 undefined）；回滚后的新 commit seq 大于初始 seq。
4. `"detaches retained writes and every returned record"`：commit 后修改原对象（`entryData.nested.push(3)`、`checkpoint.nested.count = 2`、`detail.codes.push("mutated")`）不影响存储；再修改**读回**的对象也不影响后续读取（`readEntry`/`readTask`/`readInput` 均深拷贝）。
5. `"detaches prototype-like JSON keys without changing object prototypes"`：commit `JSON.parse('{"__proto__":{"polluted":false},"constructor":{"label":"stored"},"toString":"value"}')`；修改**原对象**的 `__proto__.polluted` 与 `constructor.label` 后读取：`Object.getPrototypeOf(read) === Object.prototype`、`Object.hasOwn(read, "__proto__") === true`、`Reflect.get(read,"__proto__")` 等于 `{polluted:false}`、`constructor` 等于 `{label:"stored"}`、`toString` 等于 `"value"`、`({}).polluted` 为 `undefined`；再修改**读回对象**的 `__proto__.polluted` 后第二次读取仍是 `{polluted:false}`（Rust 侧用有序 Map 存 JSON 即可，不能把 `__proto__` 当特殊键）。
6. `"indexes entries committed out of ID order"`：一次 commit id 30、10、20（20 是 `kind:"marker"`、`head: 10`）；`scanEntries({conversationId}, 10, undefined)` 的 id 序列**精确等于 `[30, 20, 10]`**（newest-first）；`findLatestHeadMarker(rootId)` 的 id 是 `20`。
7. `"continues an entry cursor below its last item after a newer commit"`：mint oldest/middle/newest 一次 commit；`scanEntries(..., 2)` → `[newest, middle]`，`first.next` 有值；再 commit `appended`；用 `first.next` 继续 `scanEntries(..., 2)` → `[oldest]`（新 commit 的 appended 不在这一页），`second.next` 为 `undefined`。
8. `"paginates conversations by opaque cursor in ascending ID order"`：commit `thirdId`、`secondId`（乱序）；`scanConversations({}, 2, undefined)` → `[rootId, secondId]`，`first.next` 有值；把 cursor 用 `JSON.parse(JSON.stringify(...))` 往返后继续 → `[thirdId]`，`second.next` 为 `undefined`（cursor 必须可 JSON 往返）。
9. `"filters and pages conversations by durable owner edges"`：mint `otherOwnerId`、`firstTaskId`、`secondTaskId`、`firstId`、`secondId`、`thirdId`；一次 commit 4 个 conversation：`{id: otherOwnerId}`、`{id: firstId, owner:{conversationId: rootId, taskId: firstTaskId}}`、`{id: secondId, owner:{conversationId: rootId, taskId: secondTaskId}}`、`{id: thirdId, owner:{conversationId: otherOwnerId, taskId: firstTaskId}}`。
   - `scanConversations({ownerConversationId: rootId}, 1, undefined).items` → `[firstId]`（**不含 otherOwnerId**，它没有 owner 边）；`next` 有值；续页 → `[secondId]`，`next` 为 undefined。
   - `scanConversations({ownerTaskId: firstTaskId}, 10)` → `[firstId, thirdId]`。
   - `scanConversations({ownerConversationId: rootId, ownerTaskId: firstTaskId}, 10)` → `[firstId]`（两个过滤条件是 AND）。
10. `"scans deep fork history newest-first through every ancestor cap"`（最长用例）：root 一次 commit 三条 entry `rootFirst`、`rootForkPoint`（`kind:"marker"`、`head: rootFirst`）、`rootExcludedSameCommit`（与 fork point 同一 commit，**fork 时被排除**）；child conversation `parent: {conversationId: rootId, at: rootForkPoint}`；child commit `childForkPoint`（`kind:"note"`）+ `childExcluded`；root 再 commit `rootExcludedLater`；grandchild conversation `parent: {conversationId: childId, at: childForkPoint}`；grandchild 一次 commit `grandchildHead`（`kind:"marker"`、`head: grandchildHead`）+ `grandchildTail`；child 再 commit `childExcludedLater`。
    - 降序（默认）：`scanEntries({conversationId: grandchildId}, 2)` → `[grandchildTail, grandchildHead]`；续页 → `[childForkPoint, rootForkPoint]`；再续 → `[rootFirst]`，`next` 为 undefined。注意跨过 fork 点后继续沿祖先链，但排除每个 fork 点的同 commit 与之后提交。
    - 升序 `{conversationId: grandchildId, order: "ascending"}`：→ `[rootFirst, rootForkPoint]` → `[childForkPoint, grandchildHead]` → `[grandchildTail]`。
    - 范围 `{...ascending, minEntryId: rootForkPoint, maxEntryId: childForkPoint}` → `[rootForkPoint, childForkPoint]`（闭区间）。
    - 用降序 cursor 续升序查询 → reject，消息包含 `cursor`。
    - `findLatestHeadMarker(grandchildId, undefined)` → `{id: grandchildHead, head: grandchildHead}`；`(grandchildId, childForkPoint)` → `{id: rootForkPoint, head: rootFirst}`；`(grandchildId, rootFirst)` → `undefined`。
    - `scanEntries({conversationId: grandchildId, minEntryId: currentMarker.head}, 1)` → `[grandchildTail]`，续页 → `[grandchildHead]`，`next` undefined。
    - `scanEntries({conversationId, minEntryId: historicalMarker.head, maxEntryId: childForkPoint}, 10)` → `[childForkPoint, rootForkPoint, rootFirst]`（降序）。
    - `entry(rootFirst)` → `{entry: entry(rootFirst, rootId), commitSeq: rootEntriesSeq}`（**包含 `commitSeq`，是提交序号而非记录 ID**）；`entry(rootForkPoint).commitSeq` 同样是 `rootEntriesSeq`；grandchild 两条同 seq；`entry(id 999_999)` → `undefined`。
    - 祖先视图 `entry(grandchildId, rootFirst)` → 同 root 记录；`entry(grandchildId, childForkPoint).entry.conversationId === childId`；`entry(grandchildId, grandchildTail).commitSeq === grandchildEntriesSeq`；`entry(grandchildId, rootExcludedSameCommit|rootExcludedLater|childExcluded|childExcludedLater)` → 全部 `undefined`；`entry(rootId, grandchildHead)` → `undefined`（不能往下看）；`entry(grandchildId, 999_999)` → `undefined`；`entry(未知 conversation, rootFirst)` reject 消息含 `Unknown conversation`；`scanEntries({conversationId: 未知})` 同样 reject `Unknown conversation`。
11. `"scans tables in either ID order and continues a cursor in its order"`：root + 3 轮（每轮一次 commit：conversation、`pendingTask`、submission `{type:"input", status:"queued"}`）。对 `scanConversations`/`scanTasks`/`scanSubmissions` 三个扫描器，`pageSize = 2`：
    - `order` 缺省与 `"ascending"` 都返回升序 id 全表；`"descending"` 返回反序。
    - 续页时**查询不带 order**，只把 `JSON.parse(JSON.stringify(page.next))` 作为 cursor（cursor 自带 order）。
    - `scan("descending", next)` 的结果是反序数组的 `slice(2,4)`；`scan("ascending", descendingCursor)` reject，消息含 `cursor`。
12. `"replaces complete task records and pages filtered task scans"`：三个 task `first`（额外 `memos:{winner:"first"}`）、`second`（`background:true`）、`third`（`abortRequested:true`）一次 commit；把 first 整体替换成 `state: {status:"running", checkpoint:{phase:"effect", attempt:1}}, abortRequested: true`（保留 `memos`）→ `task(firstId)` 精确等于替换后的完整记录（**是替换而非合并**：随后再替换为 `terminal`，其字段集不含 `memos`，读回也没有 `memos`）。
    - `scanTasks({status:"pending"}, 1)` → `[secondId]`，续页 → `[thirdId]`（`second` 仍 pending，`third` 仍 pending）。
    - `scanTasks({status:"terminal", abortRequested:true}, 10).items` → `[terminal]` 整条记录（对象级 `toEqual`）。
    - `scanTasks({background:true}, 10)` → `[secondId]`。
13. `"stores owners and scans waiting and completing tasks by status"`：`owner = pendingTask(ownerId)`；`waiting = {...pendingTask(waitingId), owner: ownerId, state:{status:"waiting", checkpoint:{phase:"next"}, on:[ownerId], policy:"allSettled"}, memos:{kept:true}}`；`completing = {...base, owner: ownerId, state:{status:"completing", outcome:{status:"failed", error:{message:"held"}}}}`（`base` 是从 pendingTask 解构掉 `state` 后的其余字段）。
    - `task(waitingId)`、`task(completingId)` 精确相等；`scan("waiting")` → `[waiting]`；`scan("completing")` → `[completing]`；`scan("pending")` → `[ownerId]`。
    - 把 completing 改写为 `state: {status:"terminal", outcome: completing.state.outcome}` → `scan("completing")` 变空，`scan("terminal")` → `[terminal]`。
14. `"stores and scans sleeping tasks by status"`：`sleeping = {...pendingTask(sleepingId), state:{status:"sleeping", checkpoint:{phase:"next"}, until: 61_000}, memos:{kept:true}}`；读回相等；`scan("sleeping")` → `[sleeping]`；改写为 `{status:"running", checkpoint:{phase:"next"}}` → `scan("sleeping")` 空、`scan("running")` → `[running]`。
15. `"indexes request IDs per conversation and replaces complete submission records"`：两个 conversation（root + second）；三条 submission：`(root,"same")`、`(root,"other")`、`(second,"same")`。`submissionByRequest(root,"same")` → 第一条；`submissionByRequest(second,"same")` → 第三条（**唯一索引是 (conversationId, requestId)**）。
    - 把第二条改写为 `status:"placed", entry: <mint>` → `submission(secondId)` 等于新记录；`submissionByRequest(root,"other")` 随之更新。
    - 用 `scanSubmissions(query, 1, cursor)` 收集：`{}` → `[first, second, other]`（升序 id）；`{conversationId: root}` → `[first, second]`；`{status:"queued"}` → `[first, other]`；`{status:"placed"}` → `[second]`；`{conversationId: second, status:"queued"}` → `[other]`；`{conversationId: second, status:"placed"}` → `[]`（**状态变化会在状态索引间移动**）；`scanSubmissions({status:"placed"}, 10).items` → `[placedSecond]` 整条。
16. `"stores passive write submissions without input-only lifecycle states"`：两条 `type:"write"` 的 queued submission；一次 commit 改写为 `{status:"done", entry:<mint>}` 与 `{status:"unanswered", reason:"closed", detail:{retryable:false}}`；`submission(id)` 与 `submissionByRequest(conversationId, requestId)` 都精确等于新记录（write 型没有 `placed`/`answered` 这类 input-only 状态）。
17. `"reconstructs rewindable documents and preserves half-open incarnations"`：见 §2.3.6 末尾的完整步骤说明（下方“文档语义补充”）。核心断言：`document(id, createdAt)` 部分匹配 `{version:1, value:{items:["a"], nested:{count:1}}, deltasSinceBase:0}`；`changedAt` 处 `value = {items:["a","b"], nested:{count:2}}`、`deltasSinceBase = 1`；修改读回对象不影响后续读取；base version 2 后 `checkpointAt` → `{version:2, value:{...count:3}, deltasSinceBase:0}`；`["r", …]` 后 `deltasSinceBase = 1`；提交 `document.retire` 与同批 `document.change` 后：`findDocument(address, changedAt)` → `firstId`；`findDocument(address, retiredAt)` → `{id: secondId, createdAt: retiredAt}`；`scanDocuments({scope, at: changedAt})` → `[firstId]`；`scanDocuments({scope, at: retiredAt})` → `[secondId]`；`document(firstId, retiredAt)` → `undefined`；`document(secondId, "current").value` → `{items:["new"]}`（**同批 create+retire 的 first 文档在 retiredAt 已不可见，而新文档可见**）。
18. `"streams long document tails across root replacement deltas"`：base `{revision:0, rows: 512 个 {value, stable: "row-N"}}`；24 次 delta（每次 `["s", ["rows", (rev*17)%512, "value"], -rev]` 与 `["s", ["revision"], rev]`）；然后一次 `["r", {revision:100, rows: 512 个 {value: 10000+N, stable: "new-N"}}]`（root replacement）；再 24 次 delta（index 用 `(rev*19)%512`）。断言：`document(createdAt).value` 等于初始（**未被调用方后续 mutation 污染**）；`beforeReplacementAt` 等于逐步构造的快照；`replacementAt` 等于 replacement 快照（提交后修改 replacement 对象不影响）；`current` 等于全部应用后的值；修改读回的 `current.value.rows[0].value` 后再次读 `current` 不变。
19. `"copies stored document bases independently and rejects ambiguous sources"`：source 文档（`kind:"copy.source"`、conversation scope、`history:"rewindable"`、`fork:"asOf"`）base version 2 `{count:1, rows:[{value:"base"}]}`，再 delta `["s",["count"],2]` + `["p",["rows"],1,0,[{value:"current"}]]`。然后 `document.copy` 到三个 child conversation（historical/current/retired 三个 copy id），并检查：copy 独立持有 base（后续 source 改成 version 4 base + retire 不影响 copy 的 `{version:4, value:{retained:"copy"}}`）；**同一批里 `document.copy` + `document.retire(currentCopyId)` 因冲突整体失败**（`conflictError instanceof Error`，conflict 文档不存在，`currentCopy` 仍是 `{count:2, rows:[{value:"base"},{value:"current"}]}`）；kind 不匹配的 copy → 失败且文档不存在。
20. `"uses bases for version transitions and rejects historical reads of current-only documents"`：`session.settings` 文档（无 `history`/`fork`，即 current-only）；base version 1 `{count:1}`；delta version 1 `["s",["count"],2]`；再 base version 2 `{count:3}`。`document(id, "current")` → `{version:2, value:{count:3}}`；`document(id, migratedAt)` reject，消息含 `does not retain historical content`；再提交 version 1 的 delta → reject，含 `version transition requires a base`；`current` 仍是 `{count:3}`；`document.retire` 后 `current` → `undefined`。
21. `"indexes logical addresses and exact-scope scans independently"`：一次 commit 6 个文档 + 1 个 task：session-scope `cache` key `__proto__` 与 `constructor`；conversation-scope `cache` key `__proto__`（`history:"latest"`, `fork:"current"`）；task-scope `task.cache` 无 key（singleton）；task-scope `task.cache` key `member`；task-scope `task.other`。断言：`findDocument({kind:"cache", scope:{kind:"session"}, key:"__proto__"})` → 第一个；session scope 扫描分页（页大小 1）合并为 `[firstId, secondId]`；conversation scope → `[conversationId]`；`findDocument({kind:"task.cache", scope:{kind:"task", taskId}})` → singleton；带 `key:"member"` → family；`scanDocuments({scope:{kind:"task", taskId}, at:"current", kind:"task.cache"})` → `[taskSingletonId, taskFamilyId]`（**不含 `task.other`**）；`document(taskSingletonId, createdAt)` reject 含 `does not retain historical content`（current-only）。
22. `"keeps document lifecycle failures atomic and gives create-plus-retire an empty lifetime"`：singleton scope 文档 `value:1`；一次 commit 里 `create` 同 kind/singleton 的第二份 + 对第一份的 delta → reject 含 `already has a current incarnation`；第一份仍 `{value:1}`，第二份不存在。再建 `key:"empty"` 的可回退文档并在**同一批** `document.retire`：`document(emptyId,"current")` → `undefined`，`document(emptyId, emptyAt)` → `undefined`，`findDocument({kind, scope, key:"empty"}, emptyAt)` → `undefined`（create+retire 得到空生命周期）。
23. `"rolls back record tables and secondary indexes when a document command fails"`：baseline 一次 commit task+submission+document（base v1 `{count:1}`），记 `baselineSeq`；再提交一批：task 改 running、submission 改 unanswered、新 entry、`document.create` 同 kind/scope 冲突 → reject 含 `already has a current incarnation`。
    - 回滚后：`task(taskId)` 等于原 task；`scanTasks({status:"pending"})` → `[task]`；`submissionByRequest(root,"atomic")` 等于原 submission；`entry(entryId)` → `undefined`；冲突文档不存在；`findDocument({kind, scope})` 仍是原 `documentId`（记录表与二级索引一起回滚）。
    - 随后一次成功 delta commit 的 seq `> baselineSeq`（失败提交不消耗可见 seq 顺序）。
24. `"keeps indexed string identities lossless"`：`first = "\ud800"`、`second = "\ud801"`（**孤立代理项**）分别作为 task kind、submission requestId、document kind、document key；断言 `scanTasks({kind:first})` → `[firstTaskId]`、`{kind:second}` → `[secondTaskId]`、`task()` 的 kind 往返无损、`submissionByRequest(root, first/second)` 各自命中、`findDocument({kind:first/second, scope:{kind:"session"}})` 各自命中、`findDocument({kind:"family", key:first/second, scope})` 各自命中、`scanDocuments({scope, at:"current", kind:first})` → `[firstKindDocumentId]`（**Rust 侧必须用 WTF-8 或显式 UTF-16 存储，不能用 `String` 直接存，也不能在索引键上做 Unicode 规范化**）。
25. `"keeps one global record ID namespace and rejects exhausted ID minting"`：显式 commit `id=100` 的 entry → `mintId<EntryId>()` 返回 `101`；用同一数字做 task id（不同表）→ reject 含 `ID 100 already belongs to entry`（**所有表共享一个 ID 命名空间**）；显式 commit `id = Number.MAX_SAFE_INTEGER` 的 entry 后，`mintId` 连续两次都 reject 含 `ID space is exhausted`。
26. `"rejects every operation after close"`：`createRoot` 后 `close(context)`；`conversation(ROOT_CONVERSATION_ID)`、`commit([], adopt, context)`、`mintId<ConversationId>()` 全部 reject，消息含 `closed`。

**文档语义补充（case 17 的精确步骤，Rust 实现文档子系统时按此对齐）**
- 建 `firstRecord = {id, kind:"conversation.notes", scope:{kind:"conversation", conversationId: rootId}, history:"rewindable", fork:"asOf"}`，base `{kind:"base", version:1, value:{items:["a"], nested:{count:1}}}`，返回 `createdAt`。
- delta `ops = [["p", ["items"], 1, 0, ["b"]], ["s", ["nested","count"], 2]]`，返回 `changedAt`（`"p"` = 在路径处 splice 插入；`"s"` = set；`"r"` = 整棵替换）。
- 提交后修改调用方的 `initial.items` 与 `appended` 数组：历史读取必须不受影响（深拷贝）。
- base version 2 `{items:["checkpoint"], nested:{count:3}}` → `checkpointAt`，`deltasSinceBase = 0`。
- delta version 2 `ops = [["r", {items:["replacement"], nested:{count:4}}]]` → `replacedAt`，`deltasSinceBase = 1`。
- 同批 `create(secondId, 同 kind/scope)` + `retire(firstId)` + 对 firstId 的 delta：`retire` 使 first 在 `retiredAt` 及之后不可见（即使同批还有 change），新文档在同批可见；`findDocument` 按 `at` 解析当时有效的 incarnation。

#### 2.3.7 `storage-benchmark.ts`（502 行）

```ts
const adopt = (): void => {};   // Storage 层提交没有内存 owner 可 adopt

export type StorageBenchmarkScale = {
  readonly name: string; readonly entryCount: number; readonly taskCount: number; readonly documentCount: number;
};
export const STORAGE_MEMORY_SCALES: readonly StorageBenchmarkScale[] = [
  { name: "1k",  entryCount: 1_000,  taskCount: 200,  documentCount: 200 },
  { name: "10k", entryCount: 10_000, taskCount: 2_000, documentCount: 2_000 },
];
export const TIMING_SCALE: StorageBenchmarkScale =
  { name: "timing", entryCount: 1_000, taskCount: 300, documentCount: 300 };

const REPLAY_TAILS = [0, 16, 128, 1_024] as const;
const HISTORY_SEGMENT_LENGTH = 128;
const FORK_DEPTH = 8;
const ENTRIES_PER_FORK = 32;
const BATCH_SIZE = 100;

export function storageBenchmarkPrimaryRecordCount(scale: StorageBenchmarkScale): number;
type StoredTask = TaskRecord<JsonValue, JsonValue, JsonValue>;
export type StorageBenchmarkDataset = {
  readonly firstEntryId: EntryId; readonly filteredTaskCount: number;
  readonly exactDocumentId: DocumentId; readonly exactDocumentKey: string;
  readonly replayDocumentIds: Readonly<Record<(typeof REPLAY_TAILS)[number], DocumentId>>;
  readonly historicalDocumentId: DocumentId; readonly ancientAt: Seq; readonly recentAt: Seq;
  readonly deepestConversationId: ConversationId; readonly ancestorHeadEntryId: EntryId;
};
export async function seedStorageBenchmark(storage: Storage,
  scale: StorageBenchmarkScale = TIMING_SCALE): Promise<StorageBenchmarkDataset>;
export type StorageReadBenchmark = { readonly name: string;
  run(storage: Storage, dataset: StorageBenchmarkDataset): Promise<number>;
  expected(dataset: StorageBenchmarkDataset): number; };
export const STORAGE_READ_BENCHMARKS: readonly StorageReadBenchmark[];
export type StorageWriteBenchmark = { readonly name: string; readonly expected: number;
  run(storage: Storage): Promise<number>; };
export async function seedStorageWriteBenchmark(storage: Storage): Promise<void>;
export const STORAGE_WRITE_BENCHMARKS: readonly StorageWriteBenchmark[];
```

**`storageBenchmarkPrimaryRecordCount(scale)`** =
`1 + scale.entryCount + scale.taskCount + scale.documentCount + REPLAY_TAILS.length + 1 + FORK_DEPTH * (1 + ENTRIES_PER_FORK)`
（root conversation 1 条；每条 replay 文档 1 条；historical 文档 1 条；每个 fork 1 个 conversation + 32 条 entry）。

**`task(id, index)`（内部夹具，`StoredTask`）**：`statuses = ["pending","running","terminal"]`，`status = statuses[index % 3]`；
公共字段顺序 `{id, conversationId: ROOT_CONVERSATION_ID, kind: index % 4 === 0 ? "benchmark.filtered" : "benchmark.other", version: 1, input: {index}, background: index % 5 === 0, abortRequested: index % 7 === 0}`；
`terminal` → `state: {status, outcome: {status: "completed", result: {index}}}`；其余 → `state: {status, checkpoint: {index, payload: "x".repeat(64)}}`。
（注意 `task()` 在 mixed 写基准里被调用为 `task(taskId, taskId)`，即 index 传的是 TaskId 本身，与读基准无关。）

**`seedStorageBenchmark(storage, scale = TIMING_SCALE)`**（全部只用公开 `Storage` 契约，`adopt` 为空函数，context 恒为 `BACKGROUND_CONTEXT`）：
1. `commit([{type:"conversation", value:{id: ROOT_CONVERSATION_ID}}])`。
2. entries：`for start in 0..entryCount step 100`，每批内 `index` 从 start 到 min(start+100, entryCount)-1；每条 `{type:"entry", value:{id: await mintId<EntryId>(), conversationId: ROOT, kind:"benchmark.entry", ...(index===0 ? {head:id} : {}), data:{index, text: \`entry-${index}-${"x".repeat(96)}\`}}}`；`index === 0` 时记 `firstEntryId`。每批一次 commit。
3. tasks：同样分页，每批 `{type:"task", value: task(await mintId<TaskId<JsonValue>>(), index)}`；每批一次 commit。
4. documents：分页；每条 `{type:"document.create", record:{id (mint), kind:"benchmark.family", key:\`key-${index}\`, scope:{kind:"session"}}, content:{kind:"base", version:1, value:{index, text:"x".repeat(128)}}}`；**`exactDocumentId` 每轮被覆盖，最终是最后一个 document id**（即 index = documentCount-1）；`exactDocumentKey = \`key-${documentCount-1}\``。
5. replay 文档：先 `Promise.all` 为 4 个 tail 各 mint 一个 id；一次 commit 建 4 个文档 `{kind:"benchmark.replay", key:String(tail), scope:{kind:"conversation", conversationId: ROOT}, history:"rewindable", fork:"asOf"}`，content `{kind:"base", version:1, value:{count:0, text:"x".repeat(64)}}`。
   然后 `for count in 1..=1024`：**每次单独 commit** 一个数组，包含所有 `tail >= count` 的文档的 `{type:"document.change", id, content:{kind:"delta", version:1, ops:[["s",["count"],count]]}}`（因此共 1024 次 commit；count 超过某 tail 后该文档不再被更新）。
6. historical 文档：mint id；一次 commit 建 `{kind:"benchmark.history", scope:{kind:"conversation", conversationId: ROOT}, history:"rewindable", fork:"asOf"}`，content `{kind:"base", version:1, value:{count:0}}`。
   随后 `for count in 1..=128`：每次单独 commit 一条 delta `ops:[["s",["count"],count]]`，**`ancientAt` 取最后一次 commit 返回的 Seq**；若从未提交则抛 `Benchmark history seed produced no commits`。
   再 commit 一次 `{kind:"base", version:1, value:{count:128}}`（覆盖为新的 base，**不记录 seq**）。
   再 `for count in 129..=256`：每次单独 commit delta，`recentAt` 取最后一次返回的 Seq（`recentAt` 初值为 `ancientAt`；循环至少跑一次，所以总是被覆盖）。
7. fork 链：`parentConversationId = ROOT`，`parentAt = firstEntryId`（若 `firstEntryId === undefined` 抛 `Benchmark scale must create entries`），`deepestConversationId` 初值为 `ROOT`。
   `for depth in 0..8`：mint conversationId；commit `{type:"conversation", value:{id: conversationId, parent:{conversationId: parentConversationId, at: parentAt}}}`；
   再 `Promise.all` mint 32 个 EntryId；一次 commit 32 条 `{type:"entry", value:{id, conversationId, kind:"benchmark.fork", data:{depth, index}}}`；
   更新 `parentConversationId = conversationId`、`parentAt = ids.at(-1)`、`deepestConversationId = conversationId`。
8. 返回（`exactDocumentId` 未定义 → `Benchmark scale must create documents`）：
   `{firstEntryId, filteredTaskCount: Math.min(50, Math.ceil(taskCount / 60)), exactDocumentId, exactDocumentKey: \`key-${documentCount-1}\`, replayDocumentIds: Object.fromEntries(tail→id), historicalDocumentId, ancientAt, recentAt, deepestConversationId, ancestorHeadEntryId: firstEntryId}`。

**`STORAGE_READ_BENCHMARKS`（8 + 4 = 12 项，顺序逐字；数组长度 12：4 个基础项 + `REPLAY_TAILS.map` 展开的 4 项 + 2 个 historical + 2 个 fork）**：
1. `"exact entry lookup"`：`(await storage.entry(dataset.firstEntryId, BACKGROUND_CONTEXT))?.entry.id ?? -1`；`expected = firstEntryId`。
2. `"entry page scan (100)"`：`scanEntries({conversationId: ROOT}, 100, undefined, BACKGROUND_CONTEXT)` 的 `items.length`；`expected = 100`。
3. `"filtered task scan (50)"`：`scanTasks({kind:"benchmark.filtered", status:"pending", background:true}, 50, undefined, BACKGROUND_CONTEXT)` 的 `items.length`；`expected = filteredTaskCount`。
4. `"exact document address among many"`：`findDocument({kind:"benchmark.family", key: dataset.exactDocumentKey, scope:{kind:"session"}}, "current", BACKGROUND_CONTEXT)?.id ?? -1`；`expected = exactDocumentId`。
5–8. `\`document replay tail (${tail})\``（tail ∈ 0,16,128,1024，按此顺序，由 `REPLAY_TAILS.map(...)` 展开）：`Number((await storage.document(dataset.replayDocumentIds[tail], "current", BACKGROUND_CONTEXT))?.value.count)`；`expected = tail`。
9. `"ancient historical read before newer base"`：`Number((await storage.document(dataset.historicalDocumentId, dataset.ancientAt, BACKGROUND_CONTEXT))?.value.count)`；`expected = HISTORY_SEGMENT_LENGTH`（= 128）。
10. `"recent historical read after newer base"`：`Number((await storage.document(dataset.historicalDocumentId, dataset.recentAt, BACKGROUND_CONTEXT))?.value.count)`；`expected = HISTORY_SEGMENT_LENGTH * 2`（= 256）。
11. `"fork-depth history scan (100)"`：`scanEntries({conversationId: dataset.deepestConversationId}, 100, undefined, BACKGROUND_CONTEXT).items.length`；`expected = 100`。
12. `"fork-depth head lookup"`：`findLatestHeadMarker(dataset.deepestConversationId, undefined, BACKGROUND_CONTEXT)?.id ?? -1`；`expected = ancestorHeadEntryId`（= firstEntryId）。

**`seedStorageWriteBenchmark(storage)`**：先 commit root conversation；再 `Promise.all` 生成 100 条 `{type:"entry", value:{id: await mintId<EntryId>(), conversationId: ROOT, kind:"benchmark.baseline", data:{index}}}` 一次性 commit。

**`STORAGE_WRITE_BENCHMARKS`（3 项）**：
- `{name:"commit one entry", expected:1}`：mint id，commit 一条 `kind:"benchmark.write"`, `data:{text:"x".repeat(128)}`，返回 1。
- `{name:"commit 100 entries", expected:100}`：`Promise.all` 生成 100 条（`data:{index, text:"x".repeat(128)}`），一次 commit，返回 `writes.length`。
- `{name:"commit mixed entry/task/submission/document", expected:4}`：一次 commit 4 条写：entry（`kind:"benchmark.mixed"`，无 data）、task（`task(taskId, taskId)`）、submission（`{id, conversationId: ROOT, requestId:\`benchmark-${submissionId}\`, type:"write", status:"done", entry: entryId}`）、document.create（`{kind:"benchmark.mixed", key:String(documentId), scope:{kind:"session"}}`，content `{kind:"base", version:1, value:{entryId, taskId}}`），返回 4。

### 2.4 `packages/durable/test-workers`

#### 2.4.1 `worker.ts`（逐字核对）

模块头注释："A Durable Object that hosts a Harness the way an application would: opened in the constructor, woken by its alarm through `onWake`. Tests drive it with `runInDurableObject()`, evict it, and run its alarm."

- `const context = BACKGROUND_CONTEXT`。
- `export const Reminder = defineTask<{ at: number }, { phase: "wait" } | { phase: "deliver" }, number>({ name: "test.reminder", version: 1, initial: () => ({ phase: "wait" }), phases: {...}, abort: ... })`
  - `wait`：`(task, runtime, ctx) => runtime.sleepUntil(task.input.at, { phase: "deliver" }, ctx)`。
  - `deliver`：`async (_task, runtime, ctx) => { const woke = runtime.now(); await runtime.commit(() => ({ status: "terminal", outcome: { status: "completed", result: woke } }), ctx); }`。
  - `abort`：`(_task, runtime, ctx) => runtime.commit(() => ({ status: "terminal", outcome: { status: "aborted" } }), ctx)`。
- `type Script = { responses: string[]; next: number }`（内部类型；"Scripted model responses, kept in storage so a new instance continues the script: `hang` waits for an abort."）。
- `export const HEARTBEAT_MS = 60_000`。
- `export class TestSession extends DurableObject`（`extends`，不是 `implements`）：
  - 字段：`readonly #state: DurableObjectState`、`#harness!: Harness`（definite assignment）、`#offset = 0`（"Added to `Date.now()`, kept in storage, so a test can move the Harness clock across evictions."）、`readonly wakes: Wake[] = []`、`readonly hanging = Promise.withResolvers<void>()`。
  - `constructor(ctx: DurableObjectState, env: unknown)`：`super(ctx, env)`；`this.#state = ctx`；`void ctx.blockConcurrencyWhile(async () => { … })`：
    1. `const fallback = Date.now() + HEARTBEAT_MS`；`const current = await ctx.storage.getAlarm()`；`if (current === null || current > fallback) await ctx.storage.setAlarm(fallback)`（注释："A fallback before anything can throw: an object whose open or recovery fails is started again."）。
    2. `this.#offset = ctx.storage.kv.get<number>("offset") ?? 0`。
    3. `const faux = fauxProvider()`；`faux.setResponses(Array.from({ length: 8 }, () => this.#step()))`（**8 个 step 回调，不是 8 条响应**）。
    4. `const models = createModels()`；`models.setProvider(faux.provider)`。
    5. `const registry = createRegistry()`；`registry.install(defineExtension({ name: "test", tasks: [Reminder] }))`。
    6. `this.#harness = await Harness.open(await openDurableObjectSqliteStorage(ctx.storage), { models, registry, now: () => this.now(), onWake: (wake) => { this.wakes.push(wake); void this.arm(wake).catch(() => ctx.abort("Alarm write failed")); } }, context)`。
    7. `await this.arm(this.#harness.wake())`（注释："Armed before `resume()` runs anything, as for any call that can start work."）；然后 `this.#harness.resume()`。
  - `get harness(): Harness` → `this.#harness`。
  - `now(): number` → `Date.now() + this.#offset`。
  - `advance(ms: number): void`：`this.#offset += ms`；`this.#state.storage.kv.put("offset", this.#offset)`（**不 await**）。
  - `script(responses: string[]): void`：`this.#state.storage.kv.put("script", { responses, next: 0 } satisfies Script)`。
  - `async alarm(): Promise<void>`：`await this.arm({ ...this.#harness.wake(), active: true }); this.#harness.resume();`。
  - `async arm(wake: Wake): Promise<void>`（**参数是 Wake 对象，不是时间**）：`heartbeat = wake.active ? Date.now() + HEARTBEAT_MS : undefined`；`sleep = wake.at === undefined ? undefined : wake.at - this.#offset`；`at = 两者都定义 ? Math.min(...) : (heartbeat ?? sleep)`；`at === undefined` → `await this.#state.storage.deleteAlarm()`，否则 `await this.#state.storage.setAlarm(at)`。
  - `async root(): Promise<Conversation>` → `this.#harness.root(context, { agent: { model: { provider: "faux", modelId: "faux-1" } } })`。
  - `async submit(text: string, requestId: string): Promise<void>`：**先** `await this.arm({ ...this.#harness.wake(), active: true })` **再** `await (await this.root()).submit({ type: "input", content: text, requestId }, context)`（注释："Arm before admitting work: a process that dies after the commit is still woken."）。
  - `async remind(at: number): Promise<TaskId<number>>`：`root = await this.root()`；`id = await root.commit((tx) => tx.createTask(Reminder, { at }, { ownership: { kind: "conversation" } }), context)`；`this.#harness.resume()`；返回 `id`。
  - `#step(): FauxResponseStep`：返回 `(_request, options) => {...}`：从 kv 读 `script = get<Script>("script") ?? { responses: [], next: 0 }`；`text = script.responses[script.next] ?? "unscripted"`；先 `put("script", { ...script, next: script.next + 1 })`（注释："the script position is stored before the response is used"）；`text !== "hang"` → `fauxAssistantMessage(text)`；否则 `this.hanging.resolve()` 并返回一个在 abort 时 reject `new DOMException("Request interrupted", "AbortError")` 的 Promise（已 aborted 则立即 reject，否则 `signal.addEventListener("abort", abort, { once: true })`）。
- `export default { fetch: () => new Response("kiana-durable test worker") }`。

#### 2.4.2 `wake.test.ts`（3 个用例，名称逐字）

辅助 `eventually(stub, check)`：**最多 200 次**，每次 `runInDurableObject(stub, check)` 返回真则结束，否则 `setTimeout(10)` 后重试；用尽 → `throw new Error("Condition not reached")`。`context = BACKGROUND_CONTEXT`；`fresh = () => env.SESSIONS.get(env.SESSIONS.newUniqueId())`。套件名 `"a Harness woken by its Durable Object alarm"`。

1. `"sleeps through an eviction and resumes the task from its alarm"`：
   - `sleep = 600_000`；在对象内 `at = instance.now() + sleep`，`id = await instance.remind(at)`。
   - `eventually` 等到 `harness.getTask(id)` 的 `state.status === "sleeping"` **且** `state.storage.getAlarm() === at`。
   - 再在对象内断言 `harness.wake()` 与 `wakes.at(-1)` 都 `toEqual({ active: false, at })`（**`wakes` 记录的是最近一次 onWake 的 Wake**）。
   - **第一次** `evictDurableObject`；新实例内断言 `harness.wake()` 仍是 `{ active: false, at }`、`getAlarm()` 仍是 `at`，然后 `instance.advance(sleep)`（**时钟推进发生在 sleep 之后、第二次 evict 之前**）。
   - **第二次** `evictDurableObject`；`expect(await runDurableObjectAlarm(stub)).toBe(true)`（**alarm 真的跑了**）。
   - `waitForTask(id, context)` 的 `state.outcome` `toMatchObject({ status: "completed" })` 且 `result >= at`。
   - `eventually` 等到 `getAlarm() === null`（没有剩余工作）。
2. `"recovers a turn an eviction cut off mid-stream through the heartbeat alarm"`：
   - `instance.script(["hang", "recovered"])`；`await instance.submit("hi", "turn-1")`；`await instance.hanging.promise`；断言 `harness.wake().active === true` 且 `getAlarm() !== null`（"The heartbeat alarm armed before the input was admitted."）。
   - `evictDurableObject` 后 `runDurableObjectAlarm(stub) === true`。
   - 对象内 `root = await instance.root()`；**用同一个 `requestId: "turn-1"` 再次 submit** 得到同一 submission；`submission.wait(context)`；`status !== "done" || type !== "input"` → `throw new Error(\`Unexpected ${settled.status}\`)`；`entry = await root.commit((tx) => tx.entry(AssistantEntry, settled.answer), context)`；`message = entry?.model?.[0]`；返回 `message?.role === "assistant" ? message.content : undefined`。
   - 断言 `toEqual([{ type: "text", text: "recovered" }])`；再 `eventually` 等 `getAlarm() === null`。
3. `"can evict an object whose Harness keeps idle documents and contexts in memory"`：
   - `script(["answer"])`；`submit("hi", "turn-1")`；再 `await (await root.submit({...same requestId...}, context)).wait(context)`（**幂等重放**）。
   - `expect(nativeTimer(() => {}, 10, { background: true })).toBeUndefined()`（注释："Retention timers would keep the object running; the default declines them in Workers."）。
   - `Promise.race([evictDurableObject(stub).then(() => "evicted"), new Promise(resolve => setTimeout(() => resolve("still running"), 2_000))])` 必须为 `"evicted"`。

#### 2.4.3 `storage.test.ts`

- `const fresh = () => env.SESSIONS.get(env.SESSIONS.newUniqueId())`；`let suite = 0`。
- `registerStorageConformance({ describe, expect, it }, "SqliteStorage on Durable Object SQLite in workerd", (use) => runInDurableObject(fresh(), async (_instance, state) => { await use(await openDurableObjectSqliteStorage(state.storage, { prefix: \`conformance${suite++}_\` })); }))`（每个 case 用独立表前缀，与被测对象自己的表并存）。
- `describe("Durable Object SQLite schema")` 中一个用例，名称逐字 `migrates a version 1 schema to admit sleeping tasks, keeping tasks and indexes`：
  - 用 `DurableObjectSqliteDatabase` + `createSqliteSchema("legacy_")`；`applySqliteMigrationsForSchema(database, schema, createSqliteMigrations(schema).slice(0, 1))`（只应用到 v1）。
  - `INSERT INTO tasks (id, conversation_id, kind, status, abort_requested, background, record) VALUES (?, 1, 'test', ?, 0, 0, '{}')`：插入 `(5, "waiting")` 成功；`(6, "sleeping")` reject（v1 的 CHECK 不接受）。
  - 再 `openDurableObjectSqliteStorage(state.storage, { prefix: "legacy_" })` 触发迁移 → `SELECT version FROM durable_schema` 得 `{version: 2}`；`SELECT status FROM tasks WHERE id = 5` 仍是 `{status:"waiting"}`；此时可插入 `(6, "sleeping")`。
  - 5 个索引名断言：`tasksByAbortRequested`、`tasksByBackground`、`tasksByConversation`、`tasksByKind`、`tasksByStatus`。

#### 2.4.4 配置文件

- `cloudflare.d.ts`（逐条，模块归属必须照抄）：
  - 文件头注释："The parts of the Workers runtime these tests use, typed for the repository's Node program. The tests run in workerd itself (`vitest.config.ts`); full Workers types would conflict with Node's. `DurableObject` itself is declared by packages/codemode/src/cloudflare-modules.d.ts, with an untyped `ctx`."
  - `declare module "cloudflare:workers"`：
    - `type DurableObjectStorage = DurableObjectSqliteStorage & { getAlarm(): Promise<number | null>; setAlarm(scheduledTime: number): Promise<void>; deleteAlarm(): Promise<void>; readonly kv: { get<T>(key: string): T | undefined; put<T>(key: string, value: T): void } }`（`kv` 注释："Synchronous key-value storage of a SQLite-backed Durable Object."；`DurableObjectSqliteStorage` 从 `@earendil-works/chord/sqlite/cloudflare` 导入）。
    - `type DurableObjectState = { readonly storage: DurableObjectStorage; blockConcurrencyWhile<T>(callback: () => Promise<T>): Promise<T>; abort(reason?: string): void }`（`abort` 注释："Reset the object: its instance is dropped, and the next event starts a new one."）。
    - `type DurableObjectId = { toString(): string }`；`type DurableObjectStub = { readonly id: DurableObjectId }`；`type DurableObjectNamespace = { newUniqueId(): DurableObjectId; get(id: DurableObjectId): DurableObjectStub }`。
    - **`export const env: { readonly SESSIONS: DurableObjectNamespace }`**（**在 `cloudflare:workers` 里，不是 `cloudflare:test`**；`worker.ts`/`wake.test.ts`/`storage.test.ts` 都从这里导入）。
  - `declare module "cloudflare:test"`：只声明三个函数（`DurableObjectState`/`DurableObjectStub` 从 `cloudflare:workers` 导入）：
    - `runInDurableObject<O, R>(stub: DurableObjectStub, callback: (instance: O, state: DurableObjectState) => R | Promise<R>): Promise<R>`。
    - `runDurableObjectAlarm(stub: DurableObjectStub): Promise<boolean>`（注释："Run the object's alarm now if one is set; whether one ran."）。
    - `evictDurableObject(stub: DurableObjectStub): Promise<void>`（注释："Tear the object's instance down, keeping its storage."）。
  - **注意**：`DurableObject` 基类不在本文件声明，而在 `packages/codemode/src/cloudflare-modules.d.ts`（`ctx` 无类型）。
- `vitest.config.ts`（逐字）：
  ```ts
  import { fileURLToPath } from "node:url";
  import { cloudflareTest } from "@cloudflare/vitest-plugin";
  import { defineConfig } from "vitest/config";
  const source = (path: string) => fileURLToPath(new URL(path, import.meta.url));
  export default defineConfig({
    root: source("."),
    plugins: [cloudflareTest({ wrangler: { configPath: source("./wrangler.jsonc") } })],
    test: { include: ["*.test.ts"] },
    resolve: { conditions: ["source"], alias: [
      { find: /^@earendil-works\/kiana-durable$/, replacement: source("../src/index.ts") },
      { find: /^@earendil-works\/kiana-durable\/testing$/, replacement: source("../src/testing/index.ts") },
      { find: /^@earendil-works\/kiana-durable\/storage\/sqlite\/cloudflare$/, replacement: source("../src/storage/sqlite/cloudflare.ts") },
    ] },
    ssr: { resolve: { conditions: ["source"] } },
  });
  ```
  文件头注释："Runs in workerd: the Durable Object storage, alarms, and eviction are Cloudflare's own."
- `wrangler.jsonc`（逐字）：`{ "name": "kiana-durable-test-workers", "main": "./worker.ts", "compatibility_date": "2026-10-01", "compatibility_flags": ["nodejs_compat"], "durable_objects": { "bindings": [{ "name": "SESSIONS", "class_name": "TestSession" }] }, "migrations": [{ "tag": "v1", "new_sqlite_classes": ["TestSession"] }] }`。

### 2.5 `packages/evals/src`

#### 2.5.1 `plan.ts`（59 行）

```ts
export const DOCUMENTATION_VARIANTS = ["without_docs", "with_docs"] as const;
export type DocumentationVariant = (typeof DOCUMENTATION_VARIANTS)[number];

export type DiscoveredEvalCase = { file: string; fullName: string; evalSet: string; caseId: string };
export type EvalTask = DiscoveredEvalCase & { variant: DocumentationVariant; model: string; runNumber: number };

export function parseDiscoveredCases(value: unknown): DiscoveredEvalCase[];
export function createTaskPlan(cases: readonly DiscoveredEvalCase[], model: string, runsPerVariant: number): EvalTask[];
```

- 内部 `isRecord(value)`：`typeof === "object" && !== null && !Array.isArray`。
- `parseDiscoveredCases(value)`：
  1. 非数组 → `TypeError("Discovered eval cases must be an array.")`。
  2. `identities = new Set<string>()`；对每项：
     - `!isRecord(item) || typeof item.name !== "string" || typeof item.file !== "string"` → `TypeError("Discovered eval case is invalid.")`（**`file` 也必须是字符串**）。
     - `const [evalSet, caseId, ...extra] = item.name.split(" > ")`；`!evalSet?.trim() || !caseId?.trim() || extra.length > 0` → `` TypeError(`Documentation eval must use "<eval set> > <case>": ${item.name}`) ``。**存下来的 `evalSet`/`caseId` 是未 trim 的原值。**
     - `identity = JSON.stringify([evalSet, caseId])`；已存在 → `` TypeError(`Duplicate eval case identity: ${item.name}`) ``。
     - 返回 `{ file: item.file, fullName: item.name, evalSet, caseId }`（键顺序固定，影响后续 `JSON.stringify`）。
- `createTaskPlan(cases, model, runsPerVariant)`（**三个位置参数，不是对象**）：
  1. `!model.includes("/") || model.startsWith("/") || model.endsWith("/")` → `TypeError("Model identity must contain a provider and model.")`。
  2. `!Number.isSafeInteger(runsPerVariant) || runsPerVariant < 1` → `TypeError("Runs per variant must be a positive integer.")`。
  3. 外层 `for (const evalCase of cases)`，内层 `for (let runNumber = 1; runNumber <= runsPerVariant; runNumber += 1)`；变体顺序由 runNumber 奇偶决定：**奇数** `["without_docs","with_docs"]`，**偶数** `["with_docs","without_docs"]`；push `{ ...evalCase, variant, model, runNumber }`（展开顺序决定 JSON 键序：`file, fullName, evalSet, caseId, variant, model, runNumber`）。
  4. 单 case、2 次 run 的完整顺序是 `[without,1]`、`[with,1]`、`[with,2]`、`[without,2]`（`plan.test.ts` 的 `creates one isolated task per case, variant, model, and repetition` 精确断言）。

#### 2.5.2 `cli.ts`（192 行）

**没有导出任何符号**（模块顶层直接执行）。

**内部常量**：`RUNNER_OPTIONS = new Set(["--provider","--model","--runs-per-variant"])`、`FILTER_OPTIONS = new Set(["-t","--testNamePattern"])`、`packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..")`。

**`type EvalCliOptions = { provider?: string; model?: string; runsPerVariant: number; requestedFiles: string[]; discoveryArgs: string[] }`**

**`function parseEvalCli(args, environment = process.env)`**（内部，不导出）逐参循环：

1. `argument.endsWith(".docs.eval.ts")` → `requestedFiles.push(argument)`，continue。
2. `FILTER_OPTIONS.has(argument)` → 取 `args[index+1]`；**falsy**（缺失或空串）→ `` `Missing value for ${argument}.` ``；否则 `discoveryArgs.push(argument, value)`，`index += 1`。
3. `argument.startsWith("--testNamePattern=")` → 长度恰等于前缀（即空值）→ `Missing value for --testNamePattern.`；否则原样推入 `discoveryArgs`。
4. `equals = argument.indexOf("=")`；`name = equals === -1 ? argument : argument.slice(0, equals)`；若 `RUNNER_OPTIONS.has(name)`：
   - `value = equals === -1 ? args[index+1] : argument.slice(equals+1)`。
   - `!value || (equals === -1 && value.startsWith("-"))` → `` `Missing value for ${name}.` ``。
   - 赋值：`--provider` → `provider`；`--model` → `model`；`--runs-per-variant` → `runsText`。
   - `name !== "--runs-per-variant"` → `cliSelectedModel = true`。
   - `equals === -1` → `index += 1`。
5. 其他 → `` `Unsupported eval argument: ${argument}` ``。

后处理：
- `provider = provider?.trim() || undefined`；`model` 同理。
- `cliSelectedModel` 为真：缺任一 → `CLI model selection requires both --provider and --model.`
- 否则从 `KIANA_PROVIDER` / `KIANA_MODEL` 取（`trim() || undefined`）；二者布尔值不同 → `Set both KIANA_PROVIDER and KIANA_MODEL, or neither.`
- `configuredRuns = (runsText ?? environment.KIANA_EVAL_RUNS_PER_VARIANT)?.trim()`；`runsPerVariant = configuredRuns ? Number(configuredRuns) : 1`；非安全整数或 `< 1` → `Runs per variant must be a positive integer.`
- 返回 `{ provider, model, runsPerVariant, requestedFiles, discoveryArgs }`。

**`function artifactRunId()`**：`` `${new Date().toISOString().replaceAll(":", "-")}_${randomUUID()}` ``（例 `2026-10-26T12-34-56.789Z_<uuid>`）。

**`function containerPath(path)`**：`absolute = resolve(packageRoot, path)`；`packageRelative = relative(packageRoot, absolute)`；以 `".."` 开头 → `` `Eval file must be inside ${packageRoot}: ${path}` ``；返回把 `\` 换成 `/` 的相对路径。注意结果是**相对 `packages/evals` 的路径**（如 `evals/smoke.eval.ts`），不是 `/repo/...` 前缀。

**`function normalizeDiscoveredFile(path)`**：前缀固定 `"/repo/packages/evals/"`；不以它开头 → `` `Discovered eval path is outside the container package: ${path}` ``；返回去掉前缀的部分。

**`function compareDiscovery(left, right)`**：两边的 `cases.map(({fullName, file}) => JSON.stringify([fullName, normalizeDiscoveredFile(file)])).sort()` 后用 `JSON.stringify` 比较；不同 → `Documentation variants discovered different eval cases.`

**顶层执行（`cli.ts` 模块体，不是 `main()` 函数）**：
1. `cli = parseEvalCli(process.argv.slice(2))`。
2. 缺 provider 或 model → `Set KIANA_PROVIDER and KIANA_MODEL, or pass --provider and --model.`
3. `modelIdentity = `${provider}/${model}``。
4. `files = (cli.requestedFiles.length > 0 ? cli.requestedFiles : globSync("evals/**/*.docs.eval.ts", { cwd: packageRoot })).map(containerPath).sort()`。
5. 空 → `No documentation eval files were selected.`
6. 任一不以 `.docs.eval.ts` 结尾 → `` `Documentation runner cannot execute non-doc eval: ${file}` ``（file 是容器内相对路径）。
7. `artifactDirectory = resolve(packageRoot, ".eval", artifactRunId())`。
8. `authPath = requireEvalAuthFile(provider)`。
9. `await mkdir(artifactDirectory, { recursive: true, mode: 0o700 })`。
10. `images = buildImages()`；两 id 相同 → `Documentation variants resolved to the same image.`
11. `docker = createDockerContext(images, artifactDirectory, authPath, provider, model, runsPerVariant)`。
12. 对 `DOCUMENTATION_VARIANTS`（`without_docs` 先）逐个 `discoverCases(docker, variant, files, cli.discoveryArgs)` → 读 `discovered-tests.json`（UTF-8）→ `parseDiscoveredCases(JSON.parse(...))`；然后 `compareDiscovery(discoveries[0], discoveries[1])`。
13. `cases = discoveries[0].map(c => ({ ...c, file: normalizeDiscoveredFile(c.file) }))`；空 → `No documentation eval cases matched the selection.`
14. `tasks = createTaskPlan(cases, modelIdentity, cli.runsPerVariant)`。
15. `protocol = { schemaVersion: 1, model: modelIdentity, runsPerVariant, images, files, cases: cases.map(({evalSet, caseId, file}) => ({evalSet, caseId, file})), tasks }`。
16. `protocolText = JSON.stringify(protocol)`（**紧凑、无空格**）；`protocolDigest = sha256(protocolText)` 十六进制。
17. 写 `protocol.json` = `` `${JSON.stringify({ ...protocol, protocolDigest }, null, 2)}\n` ``（2 空格缩进 + 末尾换行；`protocolDigest` **追加在最后**）。
18. 写 `expected-runs.json` = `` `${JSON.stringify(tasks, null, 2)}\n` ``（tasks 含 `fullName`/`file`）。
19. 逐 task：
    - `console.log(\`\n[${index+1}/${tasks.length}] ${evalSet} > ${caseId} | ${variant} | ${model} | run ${runNumber}\`)`。
    - `reportPath = runTask(docker, task)`；有则 `readTaskObservation(task, reportPath, artifactDirectory)`，否则 `erroredObservation(task)`。
    - push 后**重写** `observations.jsonl` = `` `${observations.map(JSON.stringify).join("\n")}\n` ``（每次全量重写，不是追加）。
20. `report = summarizeEvalObservations(protocolDigest, tasks, observations)`；`reportText = formatEvalComparisonReport(report)`。
21. 写 `report.json` = `` `${JSON.stringify(report, null, 2)}\n` ``；`report.txt` = `` `${reportText}\n` ``。
22. `console.log(\`\n${reportText}\`)`；`console.log(\`\nArtifacts: ${artifactDirectory}\`)`。
23. `report.blockedPairs.length > 0` → `process.exitCode = 1`。

#### 2.5.3 `docker.ts`（175 行）

**导出**：`BuiltImages`（`Record<DocumentationVariant, { name: string; id: string }>`）、`buildImages()`、`createDockerContext(...)`、`discoverCases(...)`、`requireEvalAuthFile(provider)`、`runTask(...)`。

**内部**：`packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..")`（源码运行时是 `packages/evals/src/..` = `packages/evals`；编译后是 `dist/..`）；`repositoryRoot = resolve(packageRoot, "../..")`。

- `function execute(command, args, capture = false)`：`spawnSync(command, args, { cwd: packageRoot, stdio: capture ? ["ignore","pipe","inherit"] : "inherit", encoding: "utf8" })`；`result.error` → throw；返回 `{ status: result.status ?? 1, stdout: capture ? result.stdout : "" }`。
- `function requireSuccess(command, args)`：`status !== 0` → `` `${command} exited with status ${status}.` ``。
- `export function buildImages(): BuiltImages`：
  - `prefix = \`kiana-evals-${sha256(repositoryRoot).slice(0,12)}\``。
  - 名称：`` `${prefix}-without-docs:local` `` / `` `${prefix}-with-docs:local` ``。
  - 对每个 variant：`docker build --target <variant> --tag <name> --file <packageRoot>/docker/Dockerfile <repositoryRoot>`（**用 `--file`，不是 `-f`；构建上下文是仓库根**）。
  - `docker image inspect --format "{{.Id}}" <name>`（capture）；`status !== 0 || stdout.trim()` 为空 → `` `Cannot inspect ${name}.` ``；id = `stdout.trim()`。
- `function environment(name, value)` → `["--env", `${name}=${value}`]`（**`--env`，不是 `-e`**）。
- `export function requireEvalAuthFile(provider): string`：
  - `path = join(process.env.KIANA_CODING_AGENT_DIR ? resolve(KIANA_CODING_AGENT_DIR) : join(homedir(), ".kiana", "agent"), "auth.json")`。
  - 不存在或不是普通文件 → `` `Eval authentication file does not exist: ${path}` ``。
  - `JSON.parse(readFileSync(path,"utf8"))` 失败 → `` `Eval authentication file is invalid: ${path}` ``（带 `cause`）。
  - 非对象 / null / 数组 / 无自有属性 `provider` → `` `Eval authentication file has no credential for provider ${provider}.` ``。
- `function dockerArgs(context, variant, outputDirectory, entrypointArgs)`：
  - `mkdirSync(outputDirectory, { recursive: true, mode: 0o700 })`。
  - 参数顺序逐字：
    ```
    run --rm --read-only
    --tmpfs /tmp:rw,exec,mode=1777
    --tmpfs /repo/node_modules/.vite-temp:rw,exec,mode=1777
    --mount type=bind,source=<outputDirectory>,target=/artifacts
    --env KIANA_EVAL_ARTIFACT_DIR=/artifacts
    --env KIANA_EVAL_RUNS_PER_VARIANT=<n>
    --env KIANA_EVAL_SANDBOX_UID=65532
    --env KIANA_EVAL_SANDBOX_GID=65532
    --env KIANA_PROVIDER=<provider>
    --env KIANA_MODEL=<model>
    （若宿主有 getuid/getgid）--env KIANA_EVAL_ARTIFACT_UID=<uid> --env KIANA_EVAL_ARTIFACT_GID=<gid>
    --mount type=bind,source=<authPath>,target=/run/kiana-eval-secrets/auth.json,readonly
    <image name> <entrypointArgs...>
    ```
  - 注意键是 `source=`/`target=`，不是 `src=`/`dst=`。
- `export function createDockerContext(images, artifactDirectory, authPath, provider, model, runsPerVariant): DockerContext`（`DockerContext` 是内部类型，未导出）。
- `export function discoverCases(context, variant, files, vitestArgs): string`：
  - `outputDirectory = join(artifactDirectory, "discovery", variant)`。
  - entrypoint 参数：`["--discover", "--project", "docs", ...files, ...vitestArgs]`。
  - `status !== 0` → `` `${variant} eval discovery failed.` ``。
  - 返回 `join(outputDirectory, "discovered-tests.json")`。
- `function taskDirectoryName(task)`：`sha256(JSON.stringify([evalSet, caseId, variant, model, runNumber]))` 十六进制。
- `export function runTask(context, task): string | undefined`：
  - `outputDirectory = join(artifactDirectory, "tasks", taskDirectoryName(task))`。
  - `testName = \`${evalSet} ${caseId}\``；`exactName = \`^${testName.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}$\``。
  - 参数：`["--project", "docs", task.file, "--testNamePattern", exactName]`。
  - `execute("docker", args)`（**忽略退出码**）；`reportPath = join(outputDirectory, "vitest.json")`；`existsSync` 则返回，否则 `undefined`。

#### 2.5.4 `report.ts`（483 行，已逐行核对）

其余细节见源码逐字（重要点补充）：
- `KIANA_SESSION_SNAPSHOT_ARTIFACT = "kianaSessionJsonl"`。
- 类型定义（逐字，字段顺序即序列化顺序）：
  ```ts
  export type EvalRunIdentity = { evalSet: string; caseId: string; variant: DocumentationVariant; model: string; runNumber: number };
  export type ExpectedEvalRun = EvalRunIdentity;          // 纯别名，无额外字段
  type EvalMetrics = { inputTokens?: number; outputTokens?: number; cacheReadTokens?: number; cacheWriteTokens?: number; totalTokens?: number; toolCalls?: number; totalMs?: number; estimatedCostUsd?: number };
  export type EvalObservation = EvalRunIdentity & EvalMetrics &
    ({ outcome: "scored"; score: number } | { outcome: "unscored" | "skipped" | "pending" | "errored" });
  export type PairedMetricSummary = { eligiblePairs: number; controlMean: number | null; treatmentMean: number | null; meanDelta: number | null };
  export type EvalSetComparison = {
    evalSet: string; totalPairs: number; eligiblePairs: number; blockedPairs: number;
    controlPassRate: number | null; treatmentPassRate: number | null; lift: number | null;
    flags: Array<"no-lift" | "negative-delta" | "control-saturated" | "treatment-saturated" | "flaky">;
    totalTokens: PairedMetricSummary; toolCalls: PairedMetricSummary; totalMs: PairedMetricSummary; estimatedCostUsd: PairedMetricSummary;
  };
  export type BlockedPair = Omit<EvalRunIdentity, "variant"> & { reasons: string[] };
  export type OperationalMetricTotal = { availableRuns: number; total: number | null };
  export type VariantTotals = { variant: DocumentationVariant; runs: number; inputTokens: OperationalMetricTotal; outputTokens: OperationalMetricTotal; cacheReadTokens: OperationalMetricTotal; cacheWriteTokens: OperationalMetricTotal; totalTokens: OperationalMetricTotal; toolCalls: OperationalMetricTotal; totalMs: OperationalMetricTotal; estimatedCostUsd: OperationalMetricTotal };
  export type EvalComparisonReport = { schemaVersion: 3; protocolDigest: string; control: "without_docs"; treatment: "with_docs"; comparisons: EvalSetComparison[]; blockedPairs: BlockedPair[]; operationalTotals: VariantTotals[] };
  ```
  `ExpectedEvalRun` 与 `EvalRunIdentity` 结构完全等价（Rust 中用一个类型 + 别名或直接复用）；`type EvalMetrics` 未导出。
- `EvalObservation` 的 `outcome` 联合：`"scored" | "unscored" | "skipped" | "pending" | "errored"`；`scored` 附带 `score: number`。
- `optionalMetric`（`report.ts` 内部，**未导出**）：`undefined` → `undefined`；非 number / 非有限 / `< 0` → `throw new TypeError(\`${name} must be a finite non-negative number.\`)`（**注意 `TypeError`，不是 Error**）；`0` 是合法值。
- `validateScore`（内部，**未导出**）：`null`/`undefined` → `undefined`；非 number / 非有限 / `< 0` / `> 1` → `throw new TypeError("Eval score must be between 0 and 1.")`；`0` 与 `1` 都合法。
- `classifyCaseStatus`：`failed → "errored"`；`skipped|todo|disabled → "skipped"`；`pending → "pending"`；`passed` 及其他 → `undefined`。
- `persistSession`：目录 `<artifact>/<variant>/sessions/<sha256(JSON.stringify([evalSet,caseId,variant,model,runNumber]))>`，mkdir 0o700，文件 `session.jsonl` 0o600。
- `readTaskObservation` 的**实际执行顺序**（与判定表同样重要）：
  1. `loaded = await Promise.all([readReportWorkspace(reportPath), readVitestJsonReportFile(reportPath)]).catch(() => undefined)`；`!loaded` → `errored`。
  2. `reportedFullName = \`${task.evalSet} ${task.caseId}\``；`assertions = rawReport.testResults.flatMap(({assertionResults}) => assertionResults)`。
  3. `assertions.length !== 1` 或 `assertion.fullName !== reportedFullName` → `errored`。
  4. `classifyCaseStatus(assertion.status)` 为 `"skipped"`/`"pending"` → **立即返回** `{...identity, outcome}`（**早于**任何 `workspace.cases` 检查与 harness 检查：跳过/待定的用例即使没有 workspace 记录也是 skipped/pending，不是 errored）。
  5. 之后才检查 `workspace.cases.length !== 1`、`caseResult.fullName`、`caseResult.status !== assertion.status` → `errored`。
  6. `run = caseResult.harness?.run`；`!run` → `errored`。
  7. **`await persistSession(caseResult, task, artifactDirectory)`**（**位置关键**：在模型一致性检查**之前**调用，所以模型不匹配或后续 metrics/score 校验失败时，`session.jsonl` 仍会被写出）。
  8. 模型检查、metrics、errors、score 依次判定（见下表）。
- `readTaskObservation` 的失败条件（任一满足 → `errored`，但 metrics 若有则先保留再来判定）：
  1. `readReportWorkspace` 或 `readVitestJsonReportFile` 抛错。
  2. `assertions.length !== 1`。
  3. `assertion.fullName !== \`${evalSet} ${caseId}\``。
  4. `workspace.cases.length !== 1`、`caseResult.fullName` 不符、`caseResult.status !== assertion.status`。
  5. 没有 `caseResult.harness?.run`。
  6. `run.usage.provider/model` 拼出的 model 与 `task.model` 不同。
  7. metrics 校验抛错。
  8. `statusOutcome === "errored"` 或 `run.errors.length > 0`。
  9. score 校验抛错。
  - `score === undefined`（`avgScore` 为 null/undefined）→ `unscored`；`score === 0` 仍算 `scored`（测试明确断言）。
  - 会话快照缺失不影响 `scored`。
- 配对与分组：`pairKey = JSON.stringify([evalSet, caseId, model, runNumber])`；`groupPairs` 按 `evalSet.localeCompare`、`caseId.localeCompare`、`model.localeCompare`、`runNumber` 升序排序（**`localeCompare` 与 Rust `str::cmp` 在小写 ASCII 上一致，但非 ASCII 需用 locale 无关的 ICU 排序或固定 `en-US` collation**）。
- `resolvePair` 的原因文案：
  - `expected !== 1` → `` `${variant}: design expected 1 run, found ${expected}` ``。
  - `observed.length !== expected` → `` `${variant}: expected ${expected} observation${expected === 1 ? "" : "s"}, found ${observed.length}` ``（`observations` 后直接跟逗号，无空格；`expected === 1` 时用单数 `observation`）。
  - `resolvePair` 的检查顺序固定为 `VARIANTS = [without_docs, with_docs]`，每个 variant 先查 `expected !== 1`，再查 `observed.length !== expected`，最后才查 outcome（仅当 `expected === 1 && observed.length === 1` 时）。reason 按此顺序累积，用 `"; "` 连接。
  - 所有 reasons 都在 `blockedPairs` 中，`BlockedPair` 是 `Omit<EvalRunIdentity, "variant"> & {reasons: string[]}`（**没有 variant 字段**）。
  - outcome 非 scored → `` `${variant}: ${outcome}` ``。
- `summarizeEvalObservations(protocolDigest, expectedRuns, observations)`：
  - `groupPairs` 的 key 是 `JSON.stringify([evalSet, caseId, model, runNumber])`；`expected` Map 记录每个 variant 的期望次数（可 >1），`observations` Map 记录实际列表。
  - 遍历所有 group（已排序）时**先** `totalsByEvalSet.set(evalSet, (prev ?? 0) + 1)`（统计该 evalSet 的设计 pair 总数，包括被 blocked 的）。
  - `comparisons` 按 `evalSet.localeCompare` 排序生成；`blockedPairCount = totalPairs - pairs.length`；`publishHeadline = blockedPairCount === 0 && pairs.length > 0`。
  - `controlPassRate`/`treatmentPassRate` 只在 `publishHeadline` 时计算（`score >= 1` 的比例），否则为 `null`；`lift = difference(treatmentPassRate, controlPassRate)`（`difference` = `Number((t - c).toPrecision(15))`），任一为 null 时 `lift = null`。
  - `flags` 来自 `comparisonFlags(pairs, controlPassRate, treatmentPassRate)`：仅当两 rate 都非 null 才检查 `no-lift`/`negative-delta`/`control-saturated`/`treatment-saturated`；`flaky` 独立于 rate，对所有 pair 检查（key = `JSON.stringify([caseId, variant])`，集合里放 `String(score >= 1)`，任一集合大小 > 1 → `flaky`；注意这里的 pair 数组为空时不会加 `flaky`）。
  - 返回对象键顺序固定：`schemaVersion`、`protocolDigest`、`control`、`treatment`、`comparisons`、`blockedPairs`、`operationalTotals`（JSON 序列化顺序必须一致）。
  - `operationalTotals` 恒为 `[variantTotals(observations, "without_docs"), variantTotals(observations, "with_docs")]`（顺序固定，即使某变体 0 次）。
- `summarizeMetric` → `PairedMetricSummary`：只统计两端都有该指标的 pair；`controlMean`/`treatmentMean` 为 `null`（无数据）或算术平均；`meanDelta = controlMean === null || treatmentMean === null ? null : difference(treatmentMean, controlMean)`。
- `comparisonFlags`：`no-lift`（相等）、`negative-delta`（treatment < control）、`control-saturated`（control === 1）、`treatment-saturated`（treatment === 1）、`flaky`（同一 caseId+variant 的 `score >= 1` 布尔值集合大小 > 1）。
- `operationalMetric`：`unavailable (0/${runs} measured)` 或 `${format(total)} (${available}/${runs} measured)`（全覆盖时无括号）。
- `formatEvalComparisonReport(report)` 逐行构造（`lines` 数组，最后 `lines.join("\n")`；**无末尾换行**，调用方 `report.txt` 补 `\n`）。空格数标注为 `SP*n`：
  1. `styleText("bold", "Documentation Eval Comparisons")`（TTY 时 `\x1b[1m…\x1b[22m`）。
  2. 对每个 comparison：
     - `SP*2 + evalSet`
     - `SP*9 + "Pairs  " + eligiblePairs + "/" + totalPairs + " eligible"`
     - 若 `lift === null`：`blockedPairs > 0` → `SP*5 + "Pass rate  withheld because pairs are blocked"`；否则 `SP*5 + "Pass rate  unavailable"`。
     - 否则：`SP*5 + "Pass rate  " + signed(lift*100, 1) + " pp (with " + percentage(treatmentPassRate) + ", without " + percentage(controlPassRate) + ")"`。
     - 若 flags 非空：`SP*9 + "Flags  " + flags.join(", ")`。
     - `pairedMetric("Tokens", totalTokens)`、`pairedMetric("Tools", toolCalls)`、`pairedMetric("Latency", totalMs, "ms")`。
     - 成本行：三值任一 null → `SP*5 + "Est. cost  unavailable"`；否则 `SP*5 + "Est. cost  " + (meanDelta >= 0 ? "+" : "-") + "$" + abs(meanDelta).toFixed(4) + " (with $" + treatmentMean.toFixed(4) + ", without $" + controlMean.toFixed(4) + ", " + eligiblePairs + " pairs)"`（**符号与 `$` 分开**，负号来自三元的 `"-"`，所以不会出现 `+-`）。
  3. `SP*2 + "Operational totals"`。
  4. 每个 totals：`SP*4 + variant + ": " + runs + " runs, " + tokens + ", " + tools + ", " + latency + ", " + cost`，其中四个 metric 用 `operationalMetric`：
     - tokens 格式 `${total} tokens`；tools `${total} tools`；latency `${(total/1000).toFixed(2)}s`；cost `$${total.toFixed(4)} cost`。
     - 全覆盖 → 无后缀；否则 ` (${availableRuns}/${runs} measured)`；`total === null` → `unavailable (0/${runs} measured)`（**注意这里没有格式函数**）。
  5. 若 blockedPairs 非空：`SP*2 + "Blocked pairs"`，然后每项 `SP*4 + evalSet + "/" + caseId + "/" + model + "/run-" + runNumber + ": " + reasons.join("; ")`。
- `pairedMetric(label, metric, unit = "")`：
  - 任一 null → `SP*4 + label.padStart(10) + "  unavailable"`。
  - 否则 `SP*4 + label.padStart(10) + "  " + signed(meanDelta, 1) + unit + " (with " + treatmentMean.toFixed(1) + unit + ", without " + controlMean.toFixed(1) + unit + ", " + eligiblePairs + " pairs)"`。
- `percentage(value)`：`null` → `"unavailable"`；否则 `(v * 100).toFixed(1) + "%"`。
- `signed(value, digits)`：`(value >= 0 ? "+" : "") + value.toFixed(digits)`（**负值时 `toFixed` 自带 `-`**）。
- 无 comparison 时函数返回 `""`（调用方仍打印一个空行 + Artifacts 行）。
- `schemaVersion: 3`、`control: "without_docs"`、`treatment: "with_docs"`（字面量类型）。

#### 2.5.5 `harness.ts`（537 行）

**导出（12 个）**：`KianaCodingAgentInput`、`KianaCodingAgentModelSelection`、`KianaCodingAgentHarnessOptions`、`KianaCodingAgentHarnessWithOutput<TOutput>`、`resolveModelSelection`、`applyIsolatedEnvironment`、`verifySystemPrompt`、`createKianaCodingAgentHarness`（2 个重载 + 实现）、`DOCUMENTATION_EVAL_TOOLS`、`resolveDocumentationVariant`、`excludeKianaDocumentation`、`createKianaDocumentationEvalHarness`（2 个重载 + 实现）。

**类型（逐字）**
```ts
type KianaRunDiagnostics = { events: TranscriptEvent[]; metadata: Record<string, unknown>; usage: UsageSummary };
export type KianaCodingAgentInput = string | Array<{ type: "prompt"; content: string } | { type: "reload" }>;
export type KianaCodingAgentModelSelection = { provider: string; id: string };
export type KianaCodingAgentHarnessOptions = {
  name?: string;
  model?: KianaCodingAgentModelSelection;
  noTools?: CreateAgentSessionOptions["noTools"];
  tools?: CreateAgentSessionOptions["tools"];
  customTools?: CreateAgentSessionOptions["customTools"];
  workspaceFiles?: Readonly<Record<string, string>>;
  transformSystemPrompt?: (defaultPrompt: string) => string;
  expectedKianaDocumentation?: boolean;
};
export type KianaCodingAgentHarnessWithOutput<TOutput extends JsonValue> = KianaCodingAgentHarnessOptions & {
  output: (args: { response: string; session: AgentSession; systemPrompt: string; agentDir: string })
    => TOutput | Promise<TOutput>;
};
```

**函数（逐字签名与行为）**

- `export function resolveModelSelection(explicitModel: KianaCodingAgentModelSelection | undefined, environment: { KIANA_PROVIDER?: string; KIANA_MODEL?: string } = process.env): KianaCodingAgentModelSelection`
  - `provider = (explicitModel?.provider ?? environment.KIANA_PROVIDER)?.trim()`；`id = (explicitModel?.id ?? environment.KIANA_MODEL)?.trim()`。
  - 任一为空 → `Select a harness model explicitly or set both KIANA_PROVIDER and KIANA_MODEL as defaults.`
  - 返回 `{ provider, id }`（**字段名是 `id`，不是 `model`**）。
- `export function applyIsolatedEnvironment(home: string, agentDir: string): () => void`（**两个位置参数**）
  - 覆盖 `HOME`、`USERPROFILE`（都设为 `home`）、`KIANA_CODING_AGENT_DIR`（设为 `agentDir`）。
  - 先删除所有以 `KIANA_EVAL_` 开头的环境变量（记录原值）。
  - 返回 restore：逐个恢复（原值 `undefined` 则删除该键）。
- `function parseSandboxId(name: "KIANA_EVAL_SANDBOX_UID" | "KIANA_EVAL_SANDBOX_GID"): number | undefined`：未设 → undefined；`Number(value)` 非安全整数或 `< 1` → `` `${name} must be a positive integer.` ``。
- `function resolveSandboxIdentity(): SandboxIdentity | undefined`（`type SandboxIdentity = { uid: number; gid: number }`）：两者都未设 → undefined；只设一个 → `Set both KIANA_EVAL_SANDBOX_UID and KIANA_EVAL_SANDBOX_GID, or neither.`
- `async function chownTree(path, identity)`：`lstat` 后若是目录且**不是**符号链接 → `Promise.all` 递归每个子项；最后 `chown(path, uid, gid)`（对符号链接本身调用 chown，不跟随）。
- `async function protectTransformedModules(): Promise<string[]>`（**无参数**）：遍历 `tmpdir()` 下的目录项（`withFileTypes`），每个目录取 `<dir>/ssr`；`readdir` 失败且 code 为 `ENOENT` → 跳过，其他 rethrow；`chmod(cacheRoot, 0o755)` 与 `chmod(ssrRoot, 0o755)`；对 `ssr` 下的每个普通文件 `chmod 0o600` 并收集路径返回。
- `async function enterToolSandbox(root: string, identity: SandboxIdentity | undefined): Promise<void>`
  - `!identity` → 直接返回（**没有是合法的**：非容器/未配置沙箱时不降权）。
  - 缺 `process.getuid/geteuid/setuid/setgid/setgroups` 任一 → `The eval filesystem sandbox requires POSIX user APIs.`
  - `getuid() !== 0 || geteuid() !== 0` → `The eval runner must start as root before entering the unprivileged tool sandbox.`
  - `protectedPaths = await protectTransformedModules()`；`chownTree(root, identity)`；`setgroups([])`、`setgid`、`setuid`。
  - 降权后 `getuid() !== identity.uid || geteuid() !== identity.uid` → `Failed to enter the unprivileged eval tool sandbox.`
  - 对每个受保护路径尝试 `readFile`：抛 `EACCES`/`ENOENT` → 继续（符合预期）；其他错误 rethrow；**读成功** → `` `The unprivileged eval process can read a transformed eval module: ${path}` ``。
- `function toTranscriptEvents(messages): TranscriptEvent[]`（按消息顺序，逐条）：
  - `user` → `{type:"message", role:"user", content: contentText(content)}`。
  - `assistant` → 若 `contentText(content)` 非空先 push `{type:"message", role:"assistant", content: text}`；再遍历 content parts，`type === "toolCall"` 的 → `{type:"tool_call", id, name, arguments: normalizeRecord(part.arguments)}`。
  - `toolResult` → `{type:"tool_result", toolCallId, name: toolName, content: 全部 part 都是 text ? text : toJsonValue(content), ...(isError ? {error: {message: text || "Tool failed"}} : {})}`。
  - 其他 role 忽略。
- `async function seedWorkspace(workspace, files)`（按 `Object.entries(files ?? {})` 顺序）：
  - `!name || isAbsolute(name)` → `` `Invalid workspace fixture path: ${name}` ``。
  - `path = resolve(workspace, name)`；`relative(workspace, path)` 若为 `".."`、以 `..${sep}` 开头、或绝对路径 → `` `Workspace fixture escapes the workspace: ${name}` ``。
  - `mkdir(dirname(path), { recursive: true })`；`writeFile(path, content, { mode: 0o600 })`。
- `async function promptAgent(session, input, signal): Promise<string>`：
  - `signal?.throwIfAborted()`。
  - 记录 `previousMessageCount`；`await session.prompt(input)`。
  - 从新增消息里**反向**找最后一条 assistant；没有 → `Agent run completed without an assistant message.`
  - `stopReason !== "stop" && !== "toolUse"` → `assistant.errorMessage ?? \`Agent run ended with unexpected stop reason: ${stopReason}.\``。
  - `output = session.getLastAssistantText()`；`!output && stopReason === "stop"` → `Agent run produced no assistant text.`；返回 `output ?? ""`。
- `export function verifySystemPrompt(systemPrompt: string, options: Pick<KianaCodingAgentHarnessOptions, "name" | "expectedKianaDocumentation">): string`
  - `expectedKianaDocumentation === undefined` → 原样返回（不做检查）。
  - 缺 `"\n<rules>\n"` → `` `Kiana system prompt lost its rules in the ${options.name} eval variant.` ``。
  - `hasDocumentation = includes("\n<docs>\nKiana documentation (read only")`；`hasDocumentation !== options.expectedKianaDocumentation` → `` `Kiana system prompt does not match the ${options.name} eval variant.` ``。
- `async function runKianaCodingAgent<TOutput extends JsonValue>(input, signal, setArtifact, options)`：
  1. `startedAt = performance.now()`；`signal?.throwIfAborted()`。
  2. `selection = resolveModelSelection(options.model)`；`hostAgentDir = getAgentDir()`；`sandboxIdentity = resolveSandboxIdentity()`。
  3. `root = await mkdtemp(join(tmpdir(), "kiana-eval-"))`；`workspace = root/workspace`；`isolatedHome = root/home`；`agentDir = isolatedHome/.kiana/agent`。
  4. 若 `options.transformSystemPrompt`：注册 inline 扩展工厂 `{ name: "eval-system-prompt-transform", hidden: true, factory }`，在 `before_agent_start` 里 `forcedSystemPrompt = transform(systemPrompt)` 并返回 `{ systemPrompt }`。
  5. `const restoreEnvironment = applyIsolatedEnvironment(isolatedHome, agentDir)`（**位置注意**：它在 `resolveModelSelection`/`getAgentDir`/`resolveSandboxIdentity`/`mkdtemp` 之后、在第 6 步读 `auth.json` 与 `ModelRuntime.create` 之前；即它是**第一次读进程环境**之后的调用——前三个函数已经读过 `KIANA_PROVIDER`/`KIANA_MODEL`/`KIANA_EVAL_SANDBOX_UID`/`KIANA_EVAL_SANDBOX_GID`/`KIANA_CODING_AGENT_DIR`。旧稿的“在任何环境读取之前”是错的）。
  6. `authPath = join(hostAgentDir, "auth.json")`；`InMemoryCredentialStore`；`readStoredCredential(provider, authPath)` 有则 `credentials.modify`。
  7. `modelRuntime = await ModelRuntime.create({ credentials })`；`mkdir(workspace)`；`mkdir(agentDir, { recursive: true })`；`seedWorkspace(workspace, options.workspaceFiles)`。
  8. `model = modelRuntime.getModel(provider, id)`；缺失 → `` `Eval model not found: ${provider}/${id}` ``。
  9. `auth = await modelRuntime.getAuth(model)`；缺失 → `` `Eval model has no configured authentication: ${provider}/${id}` ``。
  10. 无存储凭证且有 `auth.auth.apiKey` → `setRuntimeApiKey(provider, apiKey)`。
  11. 若有沙箱身份：`rm(authPath, { force: true })`；若 `auth.source` 匹配 `/^[A-Z][A-Z0-9_]*$/` 且 `process.env[auth.source]` 有值 → 记录 `hiddenCredentialEnvironment` 并 `delete process.env[auth.source]`（cleanup 时恢复）。
  12. `services = await createAgentSessionServices({ cwd: workspace, modelRuntime, resourceLoaderOptions: { extensionFactories } })`；`signal?.throwIfAborted()`。
  13. `sessionManager = SessionManager.create(workspace, join(root, "sessions"))`；`setArtifact("runId", sessionManager.getSessionId())`。
  14. `createAgentSessionFromServices({ services, sessionManager, model, thinkingLevel: "off", tools: options.tools, noTools: options.noTools, customTools: options.customTools })` → `session`。
  15. 期望的内联扩展路径集合 `new Set(extensionFactories.map(({name}) => \`<inline:${name}>\`))`；`session.extensionRunner.getExtensionPaths()` 中不在集合里的 → `` `Isolated eval loaded unexpected extensions: ${paths.join(", ")}` ``。
  16. `await enterToolSandbox(root, sandboxIdentity)`。
  17. steps：`typeof input === "string" ? [{type:"prompt", content: input}] : input`；signal abort → `abortPromise ??= session.abort()`（只调用一次）。
  18. 循环 steps：`"reload"` → `await session.reload()`；否则 `response = await promptAgent(session, step.content, signal)`。
  19. `response === undefined` → `Kiana eval input must include at least one prompt step.`
  20. `systemPrompt = forcedSystemPrompt ?? getCurrentSystemPrompt(session.messages)`；`stats = session.getSessionStats()`。
  21. `hasPricing = [model.cost, ...(model.cost.tiers ?? [])].some(({input,output,cacheRead,cacheWrite}) => input>0||output>0||cacheRead>0||cacheWrite>0)`。
  22. `runDiagnostics = { events: toTranscriptEvents(session.messages), metadata: { systemPromptSha256: sha256(systemPrompt) }, usage: { provider: model.provider, model: model.id, inputTokens: stats.tokens.input, outputTokens: stats.tokens.output, totalTokens: stats.tokens.total, toolCalls: stats.toolCalls, metadata: { cacheReadTokens: stats.tokens.cacheRead, cacheWriteTokens: stats.tokens.cacheWrite, ...(hasPricing ? { estimatedCostUsd: stats.cost } : {}) } } }`。
  23. `verifySystemPrompt(systemPrompt, options)`；`output = "output" in options ? await options.output({response, session, systemPrompt, agentDir}) : response`；`result = { output, ...runDiagnostics }`。
  24. `catch` → `runError = error`。
  25. `finally`：
      - 有 `sessionManager`：`sessionPath = getSessionFile()`；`!sessionPath || !existsSync` → `cleanupErrors.push(new Error("Kiana eval produced no session file."))`；否则 `setArtifact(KIANA_SESSION_SNAPSHOT_ARTIFACT, await readFile(sessionPath, "utf8"))`（失败入 cleanupErrors）。
      - `session?.dispose()`（失败入 cleanupErrors）。
      - `rm(root, { recursive: true, force: true })`（失败入 cleanupErrors）。
      - `restoreEnvironment()`；恢复 `hiddenCredentialEnvironment`。
  26. 错误聚合：`runError !== undefined && cleanupErrors.length > 0` → `AggregateError([runError, ...cleanupErrors], "Agent run failed and cleanup also failed.")`；只有 cleanupErrors 且长度 1 → 该错误；长度 > 1 → `AggregateError(cleanupErrors, "Agent cleanup failed.")`。
  27. `failure !== undefined`：有 `runDiagnostics` 时构造 `partialRun = normalizeHarnessRun(input, {...runDiagnostics, errors: [failure], timings: { totalMs: performance.now() - startedAt }})` 并 `throw attachHarnessRunToError(failure, partialRun)`；否则直接 throw。
  28. `!result` → `Kiana eval completed without a result.`；返回 `{ ...result, timings: { totalMs: performance.now() - startedAt } }`。
- `export function createKianaCodingAgentHarness(...)`：两个重载（有 `output` → `Harness<Input, TOutput>`；无 → `Harness<Input, string>`）；实现 `createHarness({ name: options.name ?? "kiana-coding-agent", run: ({ input, signal, setArtifact }) => runKianaCodingAgent(input, signal, setArtifact, options) })`。
- `export const DOCUMENTATION_EVAL_TOOLS = ["read", "write", "edit", "grep", "find", "ls"] as const`（注释：文档评测刻意排除 shell 与无限制网络工具）。
- `export function resolveDocumentationVariant(value: string | undefined = process.env.KIANA_EVAL_VARIANT): DocumentationVariant`：合法值原样返回；否则 `TypeError('KIANA_EVAL_VARIANT must be "without_docs" or "with_docs".')`。
- `export function excludeKianaDocumentation(defaultPrompt: string): string`：
  - `documentationStart = indexOf("\n<docs>\n")`；`-1` → `Default Kiana system prompt has no Kiana documentation section.`
  - `documentationEnd = indexOf("\n</docs>", documentationStart)`；`-1` → `Default Kiana system prompt has no complete Kiana documentation section.`
  - `cwdStart = lastIndexOf("\n<cwd>\n")`；`cwdStart < documentationEnd` → `Default Kiana system prompt has no working-directory section.`
  - 返回 `defaultPrompt.slice(0, documentationStart) + defaultPrompt.slice(documentationEnd + "\n</docs>".length)`（**删除 docs 段，保留前后一切**）。
- `export function createKianaDocumentationEvalHarness(...)`：`process.env.KIANA_EVAL_CONTAINER !== "1" || !resolveSandboxIdentity()` → `Documentation evals must run in the isolated container sandbox.`；`variant = resolveDocumentationVariant()`；`createKianaCodingAgentHarness({ ...options, name: variant, tools: options.tools ?? [...DOCUMENTATION_EVAL_TOOLS], ...(variant === "without_docs" ? { transformSystemPrompt: excludeKianaDocumentation } : {}), expectedKianaDocumentation: variant === "with_docs" })`。

#### 2.5.6 `packages/evals/evals/`

**`acme-server.ts`（154 行；导出 11 个）**

- 常量（逐字）：
  ```ts
  export const OPENAI_PROVIDER_ID = "acme";
  export const OPENAI_MODEL_ID = "acme-chat";
  export const OPENAI_PROBE_PROMPT = "Reply with ACME_OK.";
  export const OPENAI_PROBE_RESPONSE = "ACME_OK";
  export const STREAM_PROVIDER_ID = "acme-stream";
  export const STREAM_MODEL_ID = "acme-stream-chat";
  export const STREAM_PROBE_PROMPT = "Reply with ACME_STREAM_OK.";
  export const STREAM_PROBE_RESPONSE = "ACME_STREAM_OK";
  ```
- `export const STREAM_API_DOCUMENTATION`：`` `${JSON.stringify({...}, null, 2)}\n` ``，对象逐字为
  ```
  {
    name: "Acme Streaming API",
    request: {
      method: "POST",
      path: "/generate",
      headers: { "content-type": "application/json", "x-acme-key": "resolved credential" },
      body: { model: STREAM_MODEL_ID, messages: [{ role: "user", content: "Hello" }], stream: true }
    },
    response: {
      contentType: "application/x-ndjson",
      events: [
        { type: "text_delta", text: "Hello" },
        { type: "usage", input_tokens: 3, output_tokens: 2 },
        { type: "done", reason: "stop" }
      ]
    }
  }
  ```
  末尾有 `\n`。注意文档里的 usage 是 3/2，实际响应是 4/3（故意不同，用于检测实现是否照文档抄错）。`STREAM_API_DOCUMENTATION` 里出现的 `STREAM_MODEL_ID` 会展开成 `"acme-stream-chat"`，其余字符串逐字。
- `export type AcmeServer = { start(): Promise<void>; stop(): Promise<void>; reset(): void; origin(): string; baseUrl(): string; validRequestReceived(): boolean }`。
- `export function createAcmeServer(mode: "openai" | "stream"): AcmeServer`（`ServerMode` 是内部类型）。

**`readJson(request)`（内部；逐字）**：`let body = ""`；`for await (const chunk of request) body += chunk.toString()`；`return JSON.parse(body)`。**没有大小上限**、按 chunk 的 `toString()`（默认 UTF-8）拼接，再由调用方 catch 成 `400 "Invalid JSON"`。

**请求处理（顺序严格）**
1. `expectedPath = mode === "openai" ? "/v1/chat/completions" : "/generate"`；`request.url !== expectedPath` → `404 "Unknown endpoint"`。
2. 非 `POST` → `405 "Expected POST"`。
3. `content-type` 不以 `application/json` 开头 → `415 "Expected application/json"`。
4. JSON 解析失败 → `400 "Invalid JSON"`。
5. 非对象 → `422 "Expected a JSON object"`。
6. 取 `messages`（非数组则空）中第一条 `role === "user"` 的记录，`prompt = typeof content === "string" ? content : undefined`。
7. **stream 模式**：`x-acme-key !== "resolved-stream-key"` → `401 "Invalid Acme Stream credential"`；`payload.model !== "acme-stream-chat" || prompt === undefined || payload.stream !== true` → `422 "Invalid Acme Stream request"`；`receivedValidRequest = prompt === STREAM_PROBE_PROMPT`；响应 `200` + `content-type: application/x-ndjson`，四个 NDJSON 行：
   ```
   {"type":"text_delta","text":"ACME_"}\n
   {"type":"text_delta","text":"STREAM_OK"}\n
   {"type":"usage","input_tokens":4,"output_tokens":3}\n
   {"type":"done","reason":"stop"}\n
   ```
   （**文本被拆成两段** `ACME_` + `STREAM_OK`，拼接后为 `ACME_STREAM_OK`。）
8. **openai 模式**：`authorization !== "Bearer resolved-acme-key"` → `401 "Invalid Acme credential"`；`payload.model !== "acme-chat" || prompt === undefined || payload.stream !== true` → `422 "Invalid OpenAI-compatible request"`；`receivedValidRequest = prompt === OPENAI_PROBE_PROMPT`；响应 `200` + `content-type: text/event-stream`、`cache-control: no-cache`，两个 data 块 + `[DONE]`：
   ```
   data: {"id":"chatcmpl-acme","object":"chat.completion.chunk","created":0,"model":"acme-chat","choices":[{"index":0,"delta":{"role":"assistant","content":"ACME_OK"},"finish_reason":null}]}\n\n
   data: {"id":"chatcmpl-acme","object":"chat.completion.chunk","created":0,"model":"acme-chat","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":2}}\n\n
   data: [DONE]\n\n
   ```
   （**第一块就直接带 `content: "ACME_OK"`**；`created: 0`；`usage` 只在第二块。）
- 错误响应统一 `content-type: text/plain` + 纯文本、无换行。
- `start()`：`createServer` 监听 `0` 与 `127.0.0.1`；未绑定 TCP 端口 → `Acme fixture server did not bind a TCP port.`；`serverOrigin = \`http://127.0.0.1:${port}\``。
- `stop()`：未启动直接返回；`close` 失败则 reject。
- `reset()`：只清 `receivedValidRequest`。
- `origin()`：返回 `serverOrigin`；`baseUrl()`：**恒为 `` `${serverOrigin}/v1` ``（两种模式相同）**。

**`configured-runtime.ts`（120 行；导出 6 个）**

```ts
type ModelFields = { id: string; name: string; provider: string; reasoning: boolean;
  input: Array<"text" | "image">;
  cost: { input: number; output: number; cacheRead: number; cacheWrite: number };
  contextWindow: number; maxTokens: number };

export type ProviderRuntimeOutput = { result:
  | { validRequestReceived: boolean; model: ModelFields;
      response: { text: string; stopReason: string; inputTokens: number; outputTokens: number } }
  | { error: string } };

export type AddedModelOutput = { result:
  | { model: ModelFields; existingModelsPreserved: boolean }
  | { error: string } };

export type ProviderScenario = { providerId: string; modelId: string;
  createContext(): Context; options?: ModelsSimpleStreamOptions; validRequestReceived(): boolean };

export function loadConfiguredModelRuntime(agentDir: string): Promise<ModelRuntime>;
export async function inspectProvider(runtime: ModelRuntime, scenario: ProviderScenario): Promise<ProviderRuntimeOutput>;
export async function inspectAddedModel(runtime: ModelRuntime, providerId: string, modelId: string): Promise<AddedModelOutput>;
```

- `modelFields(model)`：逐字段复制（`input: [...model.input]`，`cost` 四个字段）。
- `loadConfiguredModelRuntime(agentDir)`：`ModelRuntime.create({ modelsPath: join(agentDir,"models.json"), authPath: join(agentDir,"auth.json"), modelsStorePath: join(agentDir,"models-store.json"), allowModelNetwork: false })`（**字段是三个 path 参数，不是目录**）。
- `inspectProvider(runtime, scenario)`：
  1. `await runtime.refresh({ allowNetwork: false })`。
  2. `runtime.getError()` 非空 → `{ result: { error } }`（**不抛异常，错误作为数据返回**）。
  3. `runtime.getModel(scenario.providerId, scenario.modelId)` 缺失 → `` { result: { error: `Model ${providerId}/${modelId} is unavailable after reload.` } } ``。
  4. `completeSimple(model, scenario.createContext(), scenario.options)`；成功返回 `{ result: { validRequestReceived: scenario.validRequestReceived(), model: modelFields(model), response: { text: contentText(response.content), stopReason: response.stopReason, inputTokens: response.usage.input, outputTokens: response.usage.output } } }`。
  5. 抛错 → `{ result: { error: error instanceof Error ? error.message : String(error) } }`。
- `inspectAddedModel(runtime, providerId, modelId)`：
  1. `pristineRuntime = await ModelRuntime.create({ modelsPath: null, allowModelNetwork: false })`。
  2. `pristineRuntime.getProvider(providerId)` 缺失 → `` { result: { error: `Built-in provider ${providerId} is unavailable.` } } ``。
  3. `existingModelIds = pristineProvider.getModels().map(({id}) => id)`；为空 → `` { result: { error: `Built-in provider ${providerId} has no models.` } } ``。
  4. `runtime.refresh({ allowNetwork: false })`；`getError()` → `{ result: { error } }`。
  5. `runtime.getModel(providerId, modelId)` 缺失 → `` `Model ${providerId}/${modelId} is unavailable after reload.` ``。
  6. 返回 `{ result: { model: modelFields(model), existingModelsPreserved: existingModelIds.every(id => runtime.getModel(providerId, id) !== undefined) } }`。
- `models.json` 解析失败的错误文本来自 `ModelRuntime.getError()`（测试断言其为 `Failed to parse models.json`）。

**其余 eval 文件（`.eval.ts` / `.docs.eval.ts`；`describeEval` 的套件名会影响 `evalSet`，必须逐字）**

行数参考：`smoke.eval.ts` 18、`custom-provider.docs.eval.ts` 72、`documentation-audit.eval.ts` 66、`extensions.docs.eval.ts` 60、`models.docs.eval.ts` 40、`openai-provider.docs.eval.ts` 62、`tui.docs.eval.ts` 277。


| 文件 | 关键点（逐字） |
|---|---|
| `smoke.eval.ts` | `const harness = createKianaCodingAgentHarness({ noTools: "all" })`；`describeEval("Answer a basic prompt", { harness }, ...)`；用例 `it("returns the expected answer")`；prompt `"What's the capital of France? Respond with only the city name."`；断言 `result.output.trim() === "Paris"`、`result.errors` 为空、`result.usage` 部分匹配 `{provider: process.env.KIANA_PROVIDER, model: process.env.KIANA_MODEL}`、`result.usage.totalTokens > 0`。 |
| `custom-provider.docs.eval.ts` | `DOCUMENTATION_PATH = "fixtures/acme-stream-api.json"`、`STREAM_API_KEY = "resolved-stream-key"`；`beforeAll` 设 `process.env.ACME_STREAM_API_KEY` 并 `server.start()`；`beforeEach` `server.reset()`；`afterAll` 删除 env 并 `server.stop()`。expected（`ProviderRuntimeOutput`）：`result.model = {id: STREAM_MODEL_ID, name: "Acme Stream Chat", provider: STREAM_PROVIDER_ID, reasoning: false, input: ["text"], cost: 全 0, contextWindow: 16384, maxTokens: 2048}`、`response = {text: "ACME_STREAM_OK", stopReason: "stop", inputTokens: 4, outputTokens: 3}`、`validRequestReceived: true`。harness：`workspaceFiles: {[DOCUMENTATION_PATH]: STREAM_API_DOCUMENTATION}`，`output: ({session}) => inspectProvider(session.modelRuntime, {providerId, modelId, createContext: () => ({messages:[{role:"user", content: STREAM_PROBE_PROMPT, timestamp: 0}]}), options: {env:{ACME_STREAM_API_KEY: STREAM_API_KEY}, maxTokens: 32}, validRequestReceived: server.validRequestReceived})`。judge `StructuredOutputJudge({expected, match:"strict", allowExtras:false})`、`judgeThreshold: null`；套件名 `"Add custom streaming provider"`；用例 `"implements a provider from an API fixture"`；`run([{type:"prompt", content: <多行模板>}, {type:"reload"}])`（模板里含 provider ID、`server.origin()`、`./${DOCUMENTATION_PATH}`、`ACME_STREAM_API_KEY`、模型名 `"Acme Stream Chat"`、16,384 / 2,048）。 |
| `documentation-audit.eval.ts` | `TOOL_NAME = "submit_documentation_audit"`；工具 `label: "Submit documentation audit"`、`description: "Submit the final verdict after completing the documentation investigation."`、`promptSnippet: "Submit the final documentation audit as validated structured data"`；参数 `verdict ∈ {"match","mismatch","inconclusive"}`、`explanation: string`（minLength 1、maxLength 2000）、`additionalProperties: false`；`constrainedSampling: {type:"json_schema", strict:"prefer"}`；execute 返回 `{content:[{type:"text", text:"Documentation audit submitted."}], details: params, terminate: true}`。`repositoryRoot = resolve(import.meta.dirname, "../../..")`；`docsRoot = resolve(repositoryRoot, "packages/coding-agent/docs")`；`pages = globSync("**/*.md", {cwd: docsRoot})` 映射为 `{path}`（`\` → `/`）并按 `localeCompare` 排序。harness `name: "documentation-page-audit"`、`tools: ["read","grep","find","ls", TOOL_NAME]`、`customTools: [submitAudit]`。套件名 `"Audit documentation against implementation"`；`it.for(pages)("$path matches the implementation")`；run 的 prompt 是长模板（含文档绝对路径、repo 根、判定规则、`Call ${TOOL_NAME} exactly once as your final action. Do not return prose.`）；断言 `toolCalls(result).filter(c => c.name === TOOL_NAME)` 恰 1 条、`status === "ok"`、`verdict === "match"`（explanation 作为失败消息传入）。 |
| `extensions.docs.eval.ts` | `TOOL_NAME = "hello"`、`TOOL_ARGUMENTS = {name:"Bob"}`、`TOOL_RESULT = "Hello, Bob!"`；harness `tools: [...DOCUMENTATION_EVAL_TOOLS, TOOL_NAME]`；output 读 `session.resourceLoader.getExtensions()` 的 `errors`、`extensions.some(({tools}) => tools.has(TOOL_NAME))`、最后一条非错误 `toolResult` 的 `contentText`。judges：`StructuredOutputJudge({expected: {response: TOOL_RESULT, extensionErrors: [], toolLoaded: true, toolResult: TOOL_RESULT}, match:"strict", allowExtras:false})` + `ToolCallJudge({expectedTools: [{name: TOOL_NAME, arguments: TOOL_ARGUMENTS}]})`；`judgeThreshold: null`。套件 `"Create and use a tool extension"`；用例 `"creates, reloads, and invokes the extension"`；`run([prompt(配置扩展), {type:"reload"}, prompt("Use the hello tool to greet Bob. Respond with exactly the tool's greeting and nothing else.")])`。 |
| `models.docs.eval.ts` | `PROVIDER_ID = "openai"`、`MODEL_ID = "fixture-chat"`、`MODEL_NAME = "Fixture Chat"`；expected `{model:{id, name, provider, reasoning: true, input:["text"], cost 全 0, contextWindow: 32768, maxTokens: 4096}, existingModelsPreserved: true}`；harness `output: ({session}) => inspectAddedModel(session.modelRuntime, PROVIDER_ID, MODEL_ID)`；judge `StructuredOutputJudge({expected, match:"strict", allowExtras:false})`、`judgeThreshold: null`；套件 `"Add model to existing provider"`；用例 `"adds the model without replacing existing models"`；prompt 描述 `openai/fixture-chat`、名字 `Fixture Chat`、文本输入、支持 reasoning、32768/4096、无费用；`run([prompt, {type:"reload"}])`。 |
| `openai-provider.docs.eval.ts` | `createAcmeServer("openai")`；`beforeAll(() => server.start())`、`beforeEach(() => server.reset())`、`afterAll(() => server.stop())`（**不设任何环境变量**，与 stream 版不同）；expected `{validRequestReceived:true, model:{id:"acme-chat", name:"Acme Chat", provider:"acme", reasoning:false, input:["text"], cost 全 0, contextWindow:32768, maxTokens:4096}, response:{text:"ACME_OK", stopReason:"stop", inputTokens:3, outputTokens:2}}`；harness `inspectProvider(..., {options: {env:{ACME_API_KEY:"resolved-acme-key"}, maxTokens:32}, validRequestReceived: server.validRequestReceived})`；套件 `"Add OpenAI-compatible provider"`；用例 `"configures a provider that works through Kiana"`；prompt 用 `server.baseUrl()`（=`<origin>/v1`）。 |
| `tui.docs.eval.ts` | `CONTEXT_WINDOW = 272_000`；`BUILT_IN_CONTEXT_PATTERN = /\d+(?:\.\d+)?%\/272k(?: \(auto\))?/`；`CONTEXT_FIXTURES = [{percent: 42.2, expectedBar: "████░░░░░░ 42%"}, {percent: 65, expectedBar: "███████░░░ 65%"}, {percent: 120, expectedBar: "██████████ 100%"}]`；`levenshteinSimilarity`（归一化编辑距离，空串 → 1）；`RecordingTerminal`（columns 100、rows 30、`kittyProtocolActive: true`，`takeRenders()` 等 30 ms、用 `\x1b[?2026h`/`\x1b[?2026l` 切分、`stripVTControlCharacters`，缺结束标记 → `TUI emitted an incomplete synchronized render.`）；`inspectContextFooter(session, agentDir)` 用 `createAgentSessionFromServices` + `AgentSessionRuntime` + `InteractiveMode({tuiMode:"regular", terminal})`，临时 `KIANA_OFFLINE=1`（结束时恢复/删除），先 append 一条 `usage {input:106_000, output:5_800, cacheRead:606_000, cacheWrite:0, totalTokens:717_800, cost:{input:1.008, …, total:1.008}}` 的消息取 `builtInStatusLine`，再对三个 fixture 分别 `agent.state.messages = [createContextMessage(model, percent)]`（`tokens = round(contextWindow * percent / 100)`）并收集 `renderedOutput` 与 `getContextUsage()`；finally `mode.stop("resume-hint")` → `runtime.dispose()`。judge `createJudge("ContextFooterJudge")`：要求 `extensionErrors` 为空、`reference`（内置状态行把 context 段替换为 `{context}`）含标记、每个 observation 的 `contextWindow === 272_000` 且 `round(percent*10)/10 === percent`、渲染行含 `expectedBar` 且不再匹配内置模式、不含其他 fixture 的 bar；相似度取最小值 `retained`；score 1/0，metadata `{rationale: valid ? `Footer retained ${round(retained*100)}% of the built-in status text.` : "Context progress behavior was incorrect.", retainedPercent: round(retained*100)}`。套件 `"Customize the interactive context footer"`；用例 `"replaces numeric context usage with a progress bar"`；prompt 逐字："Change the Kiana footer replacing the built-in context info, e.g. `42.2%/272k (auto)`, with a ten-cell progress bar to make it show ████░░░░░░ 42% instead. Clamp percentages to the 0-100 range. Just replace the context. Otherwise leave everything else the same."；`run([prompt, {type:"reload"}])`。 |

#### 2.5.7 `packages/evals/docker/`

- `Dockerfile`（5 个 `FROM`，阶段链 `builder` → `without-docs-install`、`evaluator` → `without_docs`/`with_docs`），逐字要点：
  - `FROM node:24-slim AS builder`；`WORKDIR /work`；`RUN --mount=type=bind,source=.,target=/repo,rw --mount=type=cache,target=/root/.npm cd /repo && npm ci --ignore-scripts && npm run build:offline && node packages/evals/docker/install-runtime.mjs /opt/evaluator`（构建用完即弃，只有 `/opt/evaluator` 保留）。
  - `FROM builder AS without-docs-install`：`rm -rf` 掉 `/opt/evaluator/install/node_modules/@earendil-works/kiana-coding-agent/` 下的 `README.md`、`CHANGELOG.md`、`docs`、`examples`。
  - `FROM node:24-slim AS evaluator`：`COPY --from=builder /opt/evaluator/root /repo`；`WORKDIR /repo/packages/evals`；`RUN chmod -R go-rwx /repo/packages/evals/{src,evals,docker} && chmod go-rwx /repo/vitest.base.ts /repo/packages/evals/vitest.evals.config.ts && mkdir -p /run/kiana-eval-secrets && chmod 0700 /run/kiana-eval-secrets`；`ENTRYPOINT ["node","--experimental-strip-types","docker/entrypoint.ts"]`。
  - `FROM evaluator AS without_docs`：`COPY --from=without-docs-install /opt/evaluator/install/node_modules /repo/node_modules`；`ENV KIANA_EVAL_VARIANT=without_docs`。
  - `FROM evaluator AS with_docs`：`COPY --from=builder /opt/evaluator/install/node_modules /repo/node_modules`；`ENV KIANA_EVAL_VARIANT=with_docs`。
- `Dockerfile.dockerignore`（逐行）：`.git`、`**/.eval`、`**/node_modules`、`**/dist`、`**/coverage`、`**/.env`、`**/.env.*`。
- `entrypoint.ts`（152 行；`node --experimental-strip-types docker/entrypoint.ts`，在 `packageRoot = packages/evals` 下运行）：
  - 顶部：`variant = process.env.KIANA_EVAL_VARIANT`；`parseId(name)`：`Number(env)` 必须为安全整数且 `>= 1`，否则 `` `${name} must be a positive integer.` ``；`sandboxUid = parseId("KIANA_EVAL_SANDBOX_UID")`、`sandboxGid = parseId("KIANA_EVAL_SANDBOX_GID")`（在模块顶层立即求值，缺失就直接抛）。
  - `assertDirectoryEntries(path, expected)`：`readdirSync(path).sort()` 与期望排序后 `JSON.stringify` 比较，不同 → `` `Unexpected eval image contents in ${path}: ${actual.join(", ")}` ``。
  - `assertWorkspace()`（顺序固定）：
    1. `/repo` 只能有 `node_modules`、`package.json`、`packages`、`vitest.base.ts`。
    2. `/repo/packages` 只能有 `evals`。
    3. `/repo/packages/evals` 只能有 `docker`、`evals`、`package.json`、`src`、`vitest.evals.config.ts`。
    4. `/repo/node_modules/@earendil-works/kiana-coding-agent` 下必须有 `package.json` 与 `dist/index.js`，缺失 → `` `Installed coding-agent is missing ${name}.` ``。
    5. `/repo/node_modules/@earendil-works/*`（跳过 `kiana-coding-agent`）任意条目：目录名为 `docs`/`examples`/`src`/`test`/`tests`，或文件名匹配 `/^(?:readme|changelog)(?:\..+)?$/i` → `` `Internal dependency documentation is model-visible: ${packageName}/${entry.name}` ``。
    6. `variant === "without_docs"`：coding-agent 目录下不得存在 `README.md`、`CHANGELOG.md`、`docs`、`examples`，否则 `` `without_docs contains coding-agent ${name}.` ``，然后 return。
    7. `variant !== "with_docs"` → `Eval image has no valid KIANA_EVAL_VARIANT.`。
    8. with_docs 必须存在且非空：`README.md`、`CHANGELOG.md`、`docs/models.md`、`examples/README.md`，否则 `` `Missing documentation: ${path}` ``。
  - `chownTree(path, uid, gid)`：`lstatSync`；是目录且不是符号链接则先递归子项；最后 `chownSync`（**不跟随符号链接**）。
  - `assertRootOnly(path)`：`statSync`；`uid !== 0` 或 `(mode & 0o077) !== 0` → `` `Evaluator source must be readable only by root: ${path}` ``。
  - `assertSandboxCannotRead(path)`：`method = statSync(path).isDirectory() ? "readdirSync" : "readFileSync"`；`spawnSync(process.execPath, ["--eval", \`require("node:fs").${method}(${JSON.stringify(path)})\`], {uid: sandboxUid, gid: sandboxGid, stdio: "ignore"})`；`status === 0` → `` `The model-facing user can read evaluator source: ${path}` ``。
  - 顶层流程：`assertWorkspace()`；对 `/repo/packages/evals/evals`、`.../src`、`.../docker`、`.../vitest.evals.config.ts`、`/repo/vitest.base.ts` 每个都 `assertRootOnly` + `assertSandboxCannotRead`。
  - `codingAgentEntry = fileURLToPath(import.meta.resolve("@earendil-works/kiana-coding-agent"))`；必须等于 `/repo/node_modules/@earendil-works/kiana-coding-agent/dist/index.js`，否则 `` `Eval does not resolve kiana-coding-agent from dist: ${codingAgentEntry}` ``。
  - `agentDir = "/tmp/kiana-eval-host-agent"`（mkdir recursive）；`authSource = "/run/kiana-eval-secrets/auth.json"`，存在则 copy 到 `agentDir/auth.json`；`chownTree(agentDir)` 与 `chownTree("/artifacts")` 到 sandbox uid/gid。
  - 环境：`HOME = "/tmp/kiana-eval-bootstrap"`、`USERPROFILE = HOME`、`KIANA_CODING_AGENT_DIR = agentDir`、`KIANA_EVAL_CONTAINER = "1"`；`process.umask(0o022)`。
  - `vitestCli = resolve(dirname(require.resolve("vitest/package.json")), "vitest.mjs")`；`inputArgs = process.argv.slice(2)`；`discover = inputArgs.includes("--discover")`；`vitestArgs = inputArgs 去掉 "--discover"`；`evalFiles = vitestArgs 中以 ".eval.ts" 结尾的项`；空 → `The container runner requires an explicit eval file.`；每个 evalFile 也要 `assertRootOnly` + `assertSandboxCannotRead`。
  - 命令：
    - discover：`[vitestCli, "list", "--config", "vitest.evals.config.ts", ...vitestArgs, "--json=/artifacts/discovered-tests.json"]`。
    - run：`[vitestCli, "run", "--config", "vitest.evals.config.ts", ...vitestArgs, "--reporter=vitest-evals/reporter", "--reporter=json", "--outputFile=/artifacts/vitest.json"]`。
  - `spawnSync(process.execPath, commandArgs, {cwd: packageRoot, stdio: "inherit", env: process.env})`；`result.error` → throw；若 `KIANA_EVAL_ARTIFACT_UID` 与 `KIANA_EVAL_ARTIFACT_GID` **都**存在则再次 `chownTree("/artifacts", parseId(...), parseId(...))`；`process.exit(result.status ?? 1)`。
- `install-runtime.mjs`（70 行；`node packages/evals/docker/install-runtime.mjs <output-directory>`）：
  - 参数：`outputDirectory = process.argv[2]`；缺失或 `process.argv.length !== 3` → `Usage: node packages/evals/docker/install-runtime.mjs <output-directory>`。
  - `repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../../..")`；读 `packages/evals/package.json`。
  - `produceArtifactSet({ build: false, outDir: join(outputDirectory, "artifacts"), repoRoot: repositoryRoot, source: null })`（来自 `scripts/package-artifacts.mjs`）。
  - `artifactNames = new Set(artifactSet.packages.map(p => p.name))`；`evaluatorDependencies = Object.entries(evalPackage.devDependencies).filter(([name]) => !artifactNames.has(name))`。
  - `installDirectory = join(outputDirectory, "install")`；`installConsumer({ artifactSet, directory: installDirectory, packageNames: [codingAgentName] })`。
  - `execFileSync("npm", ["install","--ignore-scripts","--omit=dev","--no-audit","--no-fund","--no-save", ...evaluatorDependencies.map(([n,v]) => `${n}@${v}`)], { cwd: installDirectory, stdio: "inherit" })`。
  - 对每个 artifact 包（跳过 `codingAgentName`）：若 `installDirectory/node_modules/<pkg>` 存在，删除其 `docs`/`examples`/`src`/`test`/`tests` 目录与匹配 `/^(?:readme|changelog)(?:\..+)?$/i` 的文件（与 entrypoint 的断言对称）。
  - `runtimeRoot = join(outputDirectory, "root")`；`runtimeEvalRoot = runtimeRoot/packages/evals`；mkdir `docker`。
  - 复制：`package.json`、`vitest.base.ts` → `runtimeRoot`；`packages/evals/package.json`、`vitest.evals.config.ts` → `runtimeEvalRoot`；`src`、`evals` 目录 → `runtimeEvalRoot`（recursive）；`entrypoint.ts` → `runtimeEvalRoot/docker/entrypoint.ts`。
  - 依赖 `scripts/local-package-install.mjs`、`scripts/package-artifacts.mjs`、`scripts/coding-agent-smoke.mjs`（属于仓库根脚本，不在本单元范围内，但 Rust 移植时若保留 Node 构建链则原样使用）。

#### 2.5.8 `packages/evals` 配置

- `package.json`：`@earendil-works/kiana-evals` 1.4.0，`private: true`，`type: module`；scripts 逐字：`clean`=`shx rm -rf .eval`、`eval`=`npm run eval:host && npm run eval:docs --`、`eval:host`=`vitest run --config vitest.evals.config.ts --project host`、`eval:docs`=`node --experimental-strip-types src/cli.ts`、`test`=`vitest run --config vitest.test.config.ts`；devDeps：`@earendil-works/kiana-ai 1.4.0`、`@earendil-works/kiana-coding-agent 1.4.0`、`@types/node 22.19.19`、`@vitest-evals/core 0.15.0`、`shx 0.3.4`、`vitest 4.1.12`、`vitest-evals 0.15.0`。
- `README.md`（129 行）：用法、产物、变体说明、flag 列表（`--provider`、`--model`、`--runs-per-variant`、`-t`/`--testNamePattern`）；"Results" 段落列出 `.eval/<timestamp>_<id>/` 的 6 类产物（与 §4.2 一致）；"Each repetition cannot establish stability" 的说明。
- `vitest.evals.config.ts`：`test` 段 `watch: false`、`passWithNoTests: false`、`pool: "forks"`、`maxWorkers: 1`、`fileParallelism: false`；`projects` 两个：
  - `docs`：`include: ["evals/**/*.docs.eval.ts"]`、`sequence.concurrent: false`、`testTimeout: 300_000`、`hookTimeout: 300_000`。
  - `host`：`include: ["evals/**/*.eval.ts"]`、`exclude: ["evals/**/*.docs.eval.ts"]`、同样的并发/超时设置。
  本地分支（非容器）merge 了别名 `^@earendil-works/kiana-coding-agent$` → `workspaceSourcePaths.codingAgentIndex`；`process.env.KIANA_EVAL_CONTAINER === "1"` 时只导出 evalConfig。
- `vitest.test.config.ts`：merge `vitest.base.ts` 与 `{ test: { include: ["test/**/*.test.ts"] }, resolve.alias: [coding-agent → workspaceSourcePaths.codingAgentIndex] }`。
- `tsconfig.json`：extends `../../tsconfig.json`，`noEmit: true`，`module`/`moduleResolution` = `NodeNext`，`types: ["node","vitest"]`，include `src/**/*.ts`、`evals/**/*.ts`、`docker/**/*.ts`、`test/**/*.ts`、`*.config.ts`。
- `.gitignore`：单行 `.eval/`（评测产物目录不入库；Rust 侧对应 `.gitignore` 也应只忽略产物目录，不要忽略 fixture）。

---

## 3. 用户可见面（逐字）

本单元没有 TUI 组件，但有三类“用户可见面”：daemon 二进制 CLI、`kiana-evals` CLI 的控制台输出、以及测试/基准报告文本。以下全部逐字列出。

### 3.1 `kiana-env` 二进制 CLI

| 调用 | stdout | stderr | 退出码 |
|---|---|---|---|
| `kiana-env --version` | `kiana-env {VERSION}\n`（例 `kiana-env 1.4.0`） | — | 0 |
| `kiana-env serve --token <hex>`（握手成功） | `KIANA-ENV <token>\n` 后为二进制帧 | 无 | 会话结束后 0 |
| `kiana-env serve`（缺 token） | — | `usage: kiana-env serve --token <hex>\n` | 2 |
| `kiana-env serve --token`（值为空） | — | 同上 | 2 |
| `kiana-env serve --token <hex> --extra` | 正常握手（**多余参数被忽略**；token 取 `--token` 后一项） | 无 | 0 |
| `kiana-env` / `kiana-env foo` | — | 同上 | 2 |
| serve 内部错误 | — | `kiana-env: {error}\n` | 1 |
| stdin EOF | — | — | 0 |
| 30 s 静默 | — | — | 0 |

注意：`--token` 用 `args.iter().position(|a| a == "--token").and_then(|i| args.get(i+1))` 定位，**必须在 `serve` 之后的任意位置**；后续多余参数不报错。`--version` 只在 `args[1]` 位置生效（`kiana-env --version extra` 仍打印版本）。`serve` 成功返回后 `std::process::exit(0)`（可能仍有阻塞在 FIFO open 的请求线程）。

### 3.2 daemon 的行内握手标记

- 客户端期望的确切字节：`KIANA-ENV ` + token(32 个小写 hex) + `\n`。
- 客户端会把标记之前的内容 trim 后转给 `onLog`（用于显示 ssh 的 banner/警告），超过 1 MiB 仍未找到标记则丢弃多余部分。
- 客户端从不向 stderr 打印任何东西；`onLog` 由使用方决定（`evals` 不用它；`ssh-external` 测试用它打日志）。

### 3.3 客户端错误消息（用户可直接看到的 `message`）

| 场景 | 消息 |
|---|---|
| 连接已关闭 | `Connection closed` |
| session 失效后的请求 | `kiana-env connection lost` |
| 子进程退出（握手前） | `kiana-env exited with code {code} before it was ready` |
| 握手超时 | `kiana-env did not answer within 60 s` |
| 静默超时 | `kiana-env connection timed out` |
| 协议版本不符 | `Unsupported protocol {protocol}` |
| 帧损坏 | `Corrupt frame from kiana-env` |
| 无 daemon 命令 | `No daemon command` |
| 握手失败（spawn 错误） | `spawn_error` + 具体 message |
| daemon 不支持 listFiles（`hello.features` 缺 `listFiles`） | `The kiana-env daemon does not list files; update it` |
| 参数非法（listFiles / reader） | `{key} must be a non-negative safe integer`；`maxEntries must be a positive safe integer` |
| 读取器已关闭 | `Binary reader is closed` / `Directory reader is closed` / `Text line reader is closed` |
| 行范围非法 | `Invalid line range` |
| 读取偏移非法 | `Offset and length must be non-negative safe integers` |
| 文件过大 | `File size ({size}) is greater than 2 GiB` |
| 截断尺寸非法 | `File size must be a non-negative safe integer` |
| abort | `aborted`（FileError）/ `The operation was aborted`（readFile / write 路径） |
| 不支持的文件类型 | `Unsupported file type` |
| exec 超时参数 | `Invalid timeout: must be a finite number of seconds` / `Invalid timeout: maximum is 2147483.647 seconds` |
| exec 超时 | `timeout:{seconds}`（例如 `timeout:0.3`） |
| exec 中断 | `aborted` |
| 输出回调抛错 | code `callback_error`，message 为回调错误文本 |

### 3.4 SSH 层错误消息

| 场景 | 消息 |
|---|---|
| 字段校验 | `Invalid {name}: {JSON.stringify(value)}` |
| 端口非法 | `Invalid port: {port}` |
| ssh 超时 | `ssh {host} did not finish within {n} s: {stderr.trim()}` |
| host key 变更（ssh 输出） | `The host key of {host} changed; remove the old key to continue` |
| host key 未知（ssh 输出） | `The host key of {host} is not trusted yet` |
| ssh 非零退出 | `ssh {host} failed with exit code {code}: {stderr.trim()}` |
| 未收到 host key | `No host key received from {host}` |
| 非法 host key 行 | `Not a host key line for {alias}: {JSON.stringify(line)}` |
| 同类型 key 冲突 | `The {type} host key of {host} changed; forget the old key first` |
| 架构不支持 | `Unsupported remote architecture: {machine}` |
| probe 输出异常 | `Unexpected answer from {host}: {output.trim()}` |
| 系统不支持 | `Unsupported remote system: {system}` |
| Termux TMPDIR | `TMPDIR is not set; Termux's sshd normally sets it to $PREFIX/tmp.` |
| Termux exec | `termux-exec is not loaded (LD_PRELOAD); scripts with #!/usr/bin/env shebangs will fail.` |
| Termux 挂起 | `Android may suspend Termux; run termux-wake-lock on the device to keep the connection alive.` |
| 无本地 daemon | `No kiana-env daemon for {process.platform}` |
| 无匹配 daemon 二进制 | `No kiana-env daemon for {platform}-{arch} at {binary}` |
| 远端无哈希工具 | `{host} has no sha256sum, shasum or openssl to verify kiana-env` |
| 上传损坏 | POSIX: 向 stderr `kiana-env upload is corrupt` 并 `exit 1`；Windows: `throw 'kiana-env upload is corrupt'` |

### 3.5 `kiana-evals` CLI 控制台输出

1. 每个 task 开始前：
   ```
   \n[<i>/<n>] <evalSet> > <caseId> | <variant> | <model> | run <runNumber>
   ```
   （前导空行；`i` 从 1 开始；variant 为 `without_docs`/`with_docs`。）
2. 结束时先打印报告文本 `console.log(\`\n${reportText}\`)`，**再**打印 `console.log(\`\nArtifacts: ${artifactDirectory}\`)`
   （顺序：报告在前、Artifacts 在后；两者都带前导空行）。`reportText` 为空字符串时仍打印一个空行。
   ```
   \n<formatEvalComparisonReport 的文本>
   \nArtifacts: <absolute artifact directory>
   ```
   文本中标题用 `styleText("bold", …)`，非 TTY 时 Node 不产出 ANSI（`styleText` 在 `!process.stdout.hasColors()` 时返回原文）。Rust 侧需复刻该行为：仅当 stdout 是 TTY（且 `NO_COLOR` 未设、`FORCE_COLOR` 未禁用）时输出 `\x1b[1m…\x1b[22m`。
3. 失败时抛出异常：Node 会打印堆栈并非零退出。Rust 侧应 `eprintln!("{message}")` 并以 1 退出（差异点见 §6.6）。
4. `process.exitCode = 1` 仅由 `blockedPairs.length > 0` 触发（报告已写出之后）；发现/构建/容器错误是抛异常（Node 打印堆栈、退出码 1）。

### 3.6 测试/基准输出

- `storage-benchmark` 不在生产路径上；测试会用它的名称做快照。
- `EnvConformanceCase` / `StorageConformanceCase` 的 `name` 会出现在测试报告里，必须逐字保留（§2.3.5 / §2.3.6 名单）。

### 3.7 环境变量

| 变量 | 读取方 | 语义 |
|---|---|---|
| `KIANA_ENV_DAEMON` | env 测试 | 指定 daemon 二进制路径 |
| `KIANA_PROVIDER` / `KIANA_MODEL` | evals CLI、harness | 模型选择默认值（`trim()`） |
| `KIANA_EVAL_RUNS_PER_VARIANT` | evals CLI | 每个变体跑几次（正整数） |
| `KIANA_CODING_AGENT_DIR` | harness、`requireEvalAuthFile` | agent 目录（含 `auth.json`） |
| `KIANA_EVAL_ARTIFACT_DIR` | docker.ts（作为 `--env` 传入） | 容器内产物目录，值恒为 `/artifacts` |
| `KIANA_EVAL_ARTIFACT_UID` / `KIANA_EVAL_ARTIFACT_GID` | docker.ts 设置、entrypoint 读取 | artifacts 属主；仅当宿主有 `process.getuid`/`getgid` 时以 `--env` 传入 |
| `KIANA_EVAL_SANDBOX_UID` / `KIANA_EVAL_SANDBOX_GID` | harness、docker.ts（固定传 `65532`） | 工具沙箱降权（必须成对；解析规则见 §2.5.5 的 `parseSandboxId`） |
| `KIANA_EVAL_CONTAINER` | harness、vitest 配置 | `"1"` 表示在容器内 |
| `KIANA_EVAL_VARIANT` | harness | `without_docs` / `with_docs` |
| `KIANA_ENV_SSH_HOST` / `_USER` / `_PORT` / `_KEY` / `_PROGRAM` / `_SHELL` | ssh-external 测试 | 外部 SSH 目标 |
| `TMPDIR` / `TMP` / `TEMP` | daemon | tmpdir 查找顺序 |
| `HOME` / `USERPROFILE` | daemon / harness | home 目录；harness 会覆盖 |
| `SHELL` | daemon pty | 无 argv 时的登录 shell |
| `PREFIX` | daemon（Android） | `$PREFIX/tmp` 默认 tmpdir |
| `LD_PRELOAD` | 平台探测 | 判断 termux-exec |
| `ProgramFiles` / `ProgramFiles(x86)` / `SystemRoot` / `PATHEXT` / `PATH` | daemon（Windows） | bash 查找、子环境、程序解析 |
| `NO_COLOR` / `FORCE_COLOR` | 报告着色 | 与控制台颜色一致 |

---

## 4. 外部格式（逐字）

### 4.1 线协议（stdin/stdout，protocol 1）

帧（全部大端）：
```
offset 0   u32  length           = 1 + 4 + 4 + jsonLength + payloadLength
offset 4   u8   type             1=REQUEST 2=RESULT 3=ERROR 4=EVENT 5=CANCEL 6=PING
offset 5   u32  id               1..2^32-1（PING 用 0）
offset 9   u32  jsonLength
offset 13  bytes json            UTF-8 JSON 对象（可为 0 长度 → 空对象）
offset 13+jsonLength bytes payload
```
约束：`length >= 9`、`length <= 16 MiB`（16777216）、`9 + jsonLength <= length`。CANCEL 与 PING 的 id 语义：CANCEL 携带被取消的 request id；PING 的 id 为 0 且 json 为 `{}`。客户端不需要回复 PING。

请求/响应/事件 JSON 形状：

- `hello` → `{protocol, version, os, arch, home, separator, tmpdir, cwd, driveCwds, pid, features}`（`serde_json` 的 `json!` 保持字面插入顺序；`separator` 在 `tmpdir` 之前）。
- 错误帧 JSON 为 `{code, message, syscall?, path?, ...extra}`。
- `exec` 事件：`{kind:"output", stream:"stdout"|"stderr", skipped?:{bytes,newlines,endsWithNewline}}` + 原始字节 payload。
- `exec` 结果：`{exitCode, spillPath?}`。
- `watch` 事件（键序逐字）：`{kind:"ready", mode:"native"|"polling"}`、`{kind:"change", paths:string[]}`、`{kind:"change", overflow:true, mode:"polling"}`、`{kind:"error", code:string, message:string}`。`paths` 与 `overflow` 互斥。
- `pty` 事件：`{kind:"started", pid}`、`{kind:"output"}` + payload；结果 `{exitCode}`。
- `ptyForeground` 结果：`{foreground: {pid, name?} | null}`。
- `listFiles` 结果：`{count, truncated}` + payload（`<kind: u8><path UTF-8><0x00>` 重复）。
- `readdir` 结果：`{entries:[{name, raw?:number[], info?, error?}], done}`。
- `info`/`fstat`/`lstat` 结果：`{name, kind:"file"|"directory"|"symlink"|"other", size, mtimeSec, mtimeNsec, dev, ino}`。
- `open` 结果（两种情况，都**必有** `handle`）：`mode == "read"` → `{handle, stat}`，`metadata()` 失败 → `{handle, statError}`；其他 mode → `{handle, info}`。即非 read 路径**没有** `stat`/`statError`，而是 `info`。

### 4.2 磁盘文件

- daemon 部署路径：`~/.kiana/mobile/tools/kiana-env-<sha256 前 32 位>`（Windows 加 `.exe`）；目录权限 0o700，文件 0o700；上传临时文件 `.kiana-env.<随机>.tmp`（同目录），POSIX 用 `mktemp "$d/.kiana-env.XXXXXX"`。
- known_hosts：标准 OpenSSH 格式 `host type key`（alias 作 host 字段）；文件权限 0o600（`flag: "wx"` 创建临时文件，再 rename）；每行末尾 `\n`；空则写空字符串。
- spill：`<tmpdir>/tmp-XXXXXX/kiana-output-<uuid>.log`（append 写入）。
- mock-ssh 相关：`test/ssh.test.ts` 用临时 sshd 配置（ed25519 host/client key），不在生产路径。
- evals 产物（`.eval/<artifactId>/`，artifactId = `artifactRunId()`）：
  ```
  protocol.json                      # 2 空格缩进 + 末尾换行
  expected-runs.json                 # JSON.stringify(tasks, null, 2) + "\n"；含 fullName/file
  observations.jsonl                 # 每次全量重写，每行一个 observation，末尾 "\n"
  report.json                        # 2 空格缩进 + 末尾换行
  report.txt                         # formatEvalComparisonReport + "\n"
  discovery/<variant>/discovered-tests.json                 # vitest --discover 的 --json 输出
  tasks/<sha256([evalSet,caseId,variant,model,runNumber])>/vitest.json   # 每个隔离 arm 的 --outputFile
  <variant>/sessions/<sha256(同上 identity)>/session.jsonl  # 目录 0o700，文件 0o600
  ```
  注意 `discovery/` 与 `tasks/` 都在 artifact 根下，不在 `<variant>/` 下；只有 `sessions/` 位于 `<variant>/` 下。
  `protocol.json` 中 `images` 的形状是 `{ without_docs: {name, id}, with_docs: {name, id} }`（`BuiltImages`），
  `cases` 是 `{evalSet, caseId, file}`，`tasks` 是 `EvalTask[]`，`model` 是 `"<provider>/<model>"` 字符串，`schemaVersion: 1`；
  `protocolDigest` 是紧凑 `JSON.stringify(protocol)` 的 sha256 hex，仅写入磁盘文件、不参与 digest 计算。
- `eval` fixture：`agentDir/auth.json`（从宿主机复制）、`models.json`/`auth.json`/`models-store.json`（configured-runtime 测试）。

### 4.3 报告 JSON schema（`report.json`）

```
{
  "schemaVersion": 3,
  "protocolDigest": "<sha256 hex>",
  "control": "without_docs",
  "treatment": "with_docs",
  "comparisons": [{
    "evalSet": string, "totalPairs": number, "eligiblePairs": number, "blockedPairs": number,
    "controlPassRate": number|null, "treatmentPassRate": number|null, "lift": number|null,
    "flags": ["no-lift"|"negative-delta"|"control-saturated"|"treatment-saturated"|"flaky"],
    "totalTokens": PairedMetricSummary, "toolCalls": …, "totalMs": …, "estimatedCostUsd": …
  }],
  "blockedPairs": [{ "evalSet", "caseId", "model", "runNumber", "reasons": string[] }],
  "operationalTotals": [VariantTotals, VariantTotals]
}
```
`PairedMetricSummary = { eligiblePairs, controlMean|null, treatmentMean|null, meanDelta|null }`。
`OperationalMetricTotal = { availableRuns, total|null }`。
`VariantTotals = { variant, runs, inputTokens, outputTokens, cacheReadTokens, cacheWriteTokens, totalTokens, toolCalls, totalMs, estimatedCostUsd }`。

### 4.4 RPC / HTTP（eval fixture）

见 §2.5.6：`POST /v1/chat/completions`（SSE）与 `POST /generate`（NDJSON）。SSE 解析要求：客户端（kiana-ai）按 `data: ` 前缀切分，`[DONE]` 结束；NDJSON 按行解析 `type` 字段。

---

## 5. 跨单元接口

### 5.1 本单元消费的类型（来自 `@earendil-works/kiana-durable` 的 `src/env/index.ts`）

```ts
type Result<T, E> = { ok: true; value: T } | { ok: false; error: E };
function ok<T>(value: T): Result<T, never>;
function err<E>(error: E): Result<never, E>;
type FileKind = "file" | "directory" | "symlink";
type FileErrorCode = "not_found" | "permission_denied" | "not_directory" | "is_directory"
                   | "invalid" | "aborted" | "not_supported" | "unknown";
class FileError extends Error { code: FileErrorCode; path?: string; cause? }
type ExecutionErrorCode = "timeout" | "aborted" | "spawn_error" | "shell_unavailable"
                        | "callback_error" | "unknown";
class ExecutionError extends Error { code; spillPath? }
type FileInfo = { name; path; kind; size; mtimeMs };
type TextLine = { text: string; terminated: boolean };
interface TextLineReader { readLine(context?): Promise<Result<TextLine|undefined, FileError>>; close(context?): Promise<void> }
interface BinaryReader { info(context?): Promise<Result<FileInfo, FileError>>;
  read(offset, length, context?): Promise<Result<Uint8Array, FileError>>;
  scanLines(range: {startLine; endLine?}, context?): Promise<Result<LineScan, FileError>>;
  close(context?): Promise<void> }
interface DirReader { next(maxEntries, context?): Promise<Result<{entries: FileInfo[]; done: boolean}, FileError>>;
  close(context?): Promise<void> }
type WatchTarget = { path; recursive?; exclude?: { hidden?: boolean; names?: readonly string[] } };
type WatchChange = { paths?: string[] } | { overflow: true } | { error: FileError };
interface FileWatcher { readonly mode: "native"|"polling"; close(context?): Promise<void> }
type LineScan = { newlines; start; end; firstLineEnd; lastLineStart; selectedBytes; firstLineBytes };
type ListFilesOptions = { hidden?; gitignore?; exclude?: readonly string[]; maxEntries?; maxBytes?; timeoutMs? };
type FileListEntry = { path: string; kind: FileKind };
type FileListing = { entries: FileListEntry[]; truncated: boolean };
interface FileSystem { /* absolutePath, joinPath, readTextFile, readBinaryFile, openBinaryReader,
  openTextLineReader, readTextLines, writeFile, appendFile, truncateFile, flushFile, renameFile,
  fileInfo, listDir, listFiles, openDirReader, canonicalPath, exists, createDir, remove,
  createTempDir, createTempFile, watch, exec, cleanup */ }
interface Shell { exec(...), cleanup() }
type ExecutionEnv = FileSystem & Shell & { id: string; cwd: string };
```

`Context`（来自 chord）：`{ abortSignal?: AbortSignal; telemetry?; value?; toString() }`；`BACKGROUND_CONTEXT`；`withAbortSignal(context, signal)`。

`StreamDecoder` / `rangeDecoder` / `startsWithBom` / `LineScanner` 也由 `durable/env` 导出，客户端直接复用——**Rust 侧必须让 client 与 daemon 共用同一套 decode/scan 实现**，否则差分测试会失败。

### 5.2 本单元暴露的类型（Rust 侧必须一一对应）

- `kiana-env` 客户端 crate 的公共面 = `packages/env/src/index.ts` 的 34 个再导出符号（`index.ts` 本身不新增名字；`src` 目录去重后 39 个，其中未再导出的 5 个是 `Json`、`abortResult`、`listFilesOver`、`remotePath`、`toFileError`——统计口径见开头说明）。
- daemon crate 无公共库面（只暴露二进制）。
- `kiana-evals` 的公共面 = `src/plan.ts`（6）、`src/report.ts`（15）、`src/docker.ts`（6）、`src/harness.ts`（12）、`src/cli.ts`（**0 个导出**，模块顶层执行）共 39 个；加上 `evals/acme-server.ts`（11）与 `evals/configured-runtime.ts`（6）这两个 fixture 模块共 56 个。
- `kiana-durable-testing`（Rust feature 或独立 crate）= `packages/durable/src/testing/index.ts` 的导出（26 个）。

### 5.3 依赖单元

| 单元/包 | 用途 |
|---|---|
| `@earendil-works/kiana-durable`（`env`、`env/node`、`types`、`ids`、`timers`） | 接口与 `NodeExecutionEnv` 参照实现 |
| `@earendil-works/kiana-durable`（`storage`、`storage/sqlite`、`storage/sqlite/cloudflare`） | `storage-conformance` 的被测对象；test-workers |
| `@earendil-works/chord`（`Context`、`JsonValue`、`Op`、`delta`、`sqlite/cloudflare`） | 客户端上下文、存储实现 |
| `@earendil-works/kiana-ai` | 模型运行时、`ModelRuntime.create`、`InMemoryCredentialStore` |
| `@earendil-works/kiana-coding-agent` | agent 会话、扩展注册表、系统提示 |
| `vitest-evals` / `@vitest-evals/core` | harness 接口、reporter JSON、`readReportWorkspace`/`readVitestJsonReportFile` |
| `cloudflare:workers` / `cloudflare:test` | Durable Object、alarm、kv |

---

## 6. npm 依赖 → Rust crate 映射与差异桥接

### 6.1 映射表

| npm | Rust crate | 备注 |
|---|---|---|
| `node:child_process`（spawn、stdio） | `tokio::process::Command` / `std::process::Command` | 需要 `setsid`（nix 或 libc）、进程组 kill、Windows Job Object |
| `node:fs/promises` | `tokio::fs` / `std::fs` | 语义差异必须手工抹平（§6.2） |
| `node:net`（测试用 sshd 端口） | `tokio::net::TcpListener` | 仅测试 |
| `node:crypto`（randomBytes、createHash、randomUUID） | `rand` + `sha2` + `uuid` | UUID v4 格式一致 |
| `node:path`（posix/win32） | 自实现或 `path-clean`/`typed-path` | **必须**同时实现 posix 与 win32 两套语义 |
| `node:url`（fileURLToPath） | `url` crate | Windows 盘符处理需对齐 Node |
| `node:stream`（StringDecoder） | `encoding_rs` | 见 §6.3 |
| `Buffer` | `bytes::Bytes` / `Vec<u8>` | 索引语义：JS 是 UTF-16 字符串索引，字节数组是 u8 |
| `@earendil-works/kiana-durable` 的 `NodeExecutionEnv` | 本单元已有的 `kiana-durable` Rust 移植 | 作为差分参照 |
| `notify`（daemon 已用） | `notify`（同 crate） | 保持同版本以保证事件集合一致 |
| `ignore`（daemon 已用） | `ignore`（同 crate） | `WalkBuilder` 行为必须一致 |
| `encoding_rs` | `encoding_rs` | 同 crate，行为天然一致 |
| `serde_json` | `serde_json`（`preserve_order`） | **必须开启 `preserve_order`**，否则 JSON 键顺序变化会破坏协议测试 |
| `libc` / `windows-sys` | 同 | |
| `reqwest`（SSE/NDJSON 客户端） | `reqwest`（rustls） | 评测 fixture 里被 kiana-ai 使用 |
| `crossterm` / `unicode-width` | 同 | `tui.docs.eval.ts` 的终端录制若移植需要 |
| `pulldown-cmark` / `similar` / `globset` | 同 | 文档审计、TUI 探针 |
| `rquickjs` / `oxc` | — | 本单元不直接使用（属于 agent 单元） |
| `rusqlite` | `rusqlite`（bundled） | test-workers 的 SqliteStorage |
| `clap` | `clap`（derive） | `kiana-env` 只有 `serve --token`，也可手写以精确控制 usage/退出码 |
| `async-trait`、`futures`、`bytes` | 同 | 异步接口 |
| `indexmap` | `indexmap` | 需要保序 Map（env、driveCwds） |
| `regex` | `regex` | 注意 JS 正则方言（lookbehind、backreference 不可用） |
| `sha2` + `hex` | 同 | 哈希与十六进制 |
| `base64` | 同 | Windows 上传与 `-EncodedCommand`（UTF-16LE） |
| `nix` | `nix`（unix） | pty、信号、poll、statfs |

### 6.2 Node 语义差异（必须桥接）

1. **路径**：JS 的 `path.posix`/`path.win32` 与 Rust 的 `Path` 语义不同（`..` 归一化、盘符相对路径、尾随分隔符、`//` 折叠）。必须移植一份 `posix.rs`/`win32.rs`，逐函数对齐 `resolve`、`join`、`basename`、`dirname`、`normalize`。
2. **错误码**：Node 用 `err.code`（如 `ENOENT`）与 `err.syscall`、`err.path`；daemon 用 `Failure` 生成这些字段。Rust 必须把 `io::Error::raw_os_error()` 转成 libuv 名称（§2.2.9/§2.2.11），并生成与 libuv 相同的 `message`。
3. **`fs.rm` 目录语义**：Node 默认 `recursive: false` 时 rm 目录报 `ERR_FS_EISDIR`，且错误 message 特殊（§2.2.5）；`force` 吞 ENOENT。Rust `remove_dir_all` 的其它错误文本不用于 message（用 daemon 自己生成的 libuv 文本）。
4. **`fs.mkdir` 递归**：目标已存在且是目录 → 成功（Node 行为）；已存在但是文件 → `EEXIST`。
5. **`fs.mkdtemp`**：Node 在 POSIX 用 `mkdtemp` 要求 `XXXXXX` 后缀，并把 `prefix` 里的 `XXXXXX` 替换；Windows 用随机 6 位字符。Rust 需逐平台实现，且错误路径文案要包含 `{prefix}XXXXXX`。
6. **`fs.writeFile` 的 `flag: "w"`**：会截断并创建；`append` 用 `"a"`；两者都是 0o666 & umask。
7. **字符串索引**：JS 的 `string.length`、`slice`、`substring` 按 UTF-16 码元；Rust 按字节/`char`。所有涉及“切割文本”的地方（行扫描的 `firstLineBytes`、watch 的排序、report 的 `toFixed`、`padStart`）都要按 JS 语义处理。特别是 `sort_like_javascript` 需按 UTF-16 码元序。
8. **JSON 键顺序**：`JSON.stringify` 保留插入顺序；`serde_json` 默认用 BTreeMap 排序，**必须** `preserve_order`（IndexMap）。
9. **数字格式化**：`Number.prototype.toFixed` 与 Rust `format!("{:.1}")` 在舍入上一致（都是“就近偶数”？实际上 JS 用十进制表示转换，Rust 用 round-half-to-even；差异极少但需测试）；`toPrecision(15)` 必须模拟（用 `format!("{:.15e}", x)` 后转回十进制）。`String(number)` 对整数的输出与 `{}` 一致，但对 `1e21` 以上 JS 用指数形式（报告里不出现）。
10. **Date/ISO**：`toISOString()` 固定 `YYYY-MM-DDTHH:mm:ss.sssZ`；Rust 用 `time`/`chrono` 格式化后替换 `:`。
11. **正则**：`constrainSampling` 的 JSON schema 与 `escapeRegExp` 简单；`tui.docs.eval.ts` 的 `/\d+(?:\.\d+)?%\/272k(?: \(auto\))?/` 在 Rust `regex` 中可直接编译（无 lookbehind）。
12. **timer unref**：Node 的 `unref()` 让进程可退出；Rust 的 `tokio::time::timeout` 不阻塞进程退出。需要显式取消。
13. **进程组**：`setsid` + `kill(-pid)`；Rust `Command::new` 需 `pre_exec(setsid)`。Windows 用 Job Object。
14. **`fs.readFile` 编码路径**：Node 对 `size === 0` 用 `StringDecoder` 循环；对已知大小先读 512 KiB 再流式解码；第一块直接 `toString("utf8")`（不经过 decoder），以保证与一次性 `toString` 相同。Rust 必须完全照抄这个“first chunk special case”。
15. **`Uint8Array` vs `Buffer`**：`readBinaryFile` 返回 `Buffer`（Node），reader 返回 `Uint8Array`；两者在 Rust 都是 `Vec<u8>`/`Bytes`，但对 `clone` 语义有影响（必须复制，见 `detaches retained writes` 一致性用例）。
16. **`process.platform` / `process.arch`**：Node 名（`win32`/`darwin`/`linux`/`android`、`x64`/`arm64`/`ia32`）与 Rust 的 `std::env::consts` 不同，需要映射。
17. **事件循环顺序**：客户端 `#send` 里“先 send 再检查 aborted”的顺序、`#dispatch` 里“先 remove pending 再 resolve”的顺序，都会影响竞态测试（例如 `test/remote.test.ts` 的 20 次立即取消）。Rust 需用同样的顺序（在同一同步块内完成）。
18. **`AbortSignal`**：Rust 用 `tokio_util::sync::CancellationToken` 或自定义 `AbortSignal`（`Arc<AtomicBool>` + `Notify`）；注意 `abort()` 后可多次监听只触发一次。
19. **`AbortSignal.abort()`（静态）**：用于 conformance 的 `abortedContext()`（已中止的信号）。

### 6.3 WHATWG UTF-8 解码

- 使用 `encoding_rs::UTF_8.new_decoder_with_bom_removal()`；**只移除流开头的 BOM**，中间 BOM 原样保留；不嗅探 UTF-16（`FF FE` 会产生替换符）。
- `decode_to_string` 的第三个参数 `last` 必须只在流结束时传 `true`，否则不完整序列会被过早替换（§2.2.13 单测 3）。
- `LineScanner` 的 BOM 处理是独立的：只看文件前 3 字节，若为 `EF BB BF` 则跳过（在 `feed` 中跨块缓存）。

### 6.4 watch 后端差异

- `notify` 8.x 在 Linux 用 inotify、macOS 用 FSEvents、Windows 用 ReadDirectoryChangesW。macOS 的 FSEvents 有 500 ms 合并窗口，需额外 settle。
- 快照 diff 是“最终真相”：即使 native 事件丢失，也可通过重扫发现（因此 `need_rescan` 是关键路径）。
- `statfs` 在 macOS 上需 `statfs64`/`getfsstat`；Windows 无对应实现 → 恒 native。
- overflow 语义：当目录数超预算、快照过大、或从 native 切到 polling 时必须发 `{overflow:true}` 而不是逐个路径。

### 6.5 PTY 差异

- `openpty` 用 `libc::openpty`；`TIOCSCTTY` 需在子进程 `setsid()` 后由子进程调用（`pre_exec`）。
- 前台进程组名：Linux/Android 读 `/proc/{pid}/comm`；macOS `libc::proc_name`。Rust 侧需同样的平台分支。
- 输出不做 UTF-8 解码（原始字节），与客户端 `onData` 一致。

### 6.6 evals 的 Node 依赖

- `vitest`/`vitest-evals`：Rust 侧无法直接复用。建议**保留 Node 侧评测运行时**（即 evals 容器仍用 Node），Rust 只移植 `plan`/`report`/`docker`/`cli` 的纯逻辑与格式化（这些是可以用 Rust 测试的）。若必须全 Rust，则需要重写一个兼容的 JSON reporter 读取器（`readReportWorkspace`/`readVitestJsonReportFile` 的 schema 见 §4.3）。
- `styleText`/`process.stdout.hasColors()`：见 §3.5。
- `Docker`：Rust 用 `std::process::Command` 调 `docker`，参数顺序完全一致。

### 6.7 Cloudflare Workers 差异

- workerd 没有 `nativeTimer`（`timers.ts` 返回 `undefined`）；Rust 侧若用 wasm/Workers，同样要在 `cfg(target_arch = "wasm32")` 下返回 `None`。
- `runInDurableObject`/`evictDurableObject`/`runDurableObjectAlarm` 是测试运行时能力，Rust 侧可用 `worker` crate 或保留 TS 测试（推荐保留）。
- SQLite 用 `rusqlite`（bundled）；`openDurableObjectSqliteStorage` 的 schema 迁移必须逐版本一致（v1→v2 加 `sleeping` 状态与 5 个索引）。

---

## 7. 测试清单

### 7.1 `packages/env/test`

| 文件 | 覆盖内容 |
|---|---|
| `daemon.ts` | 解析 daemon 路径：`KIANA_ENV_DAEMON` ?? `resolve(import.meta.dirname, "../daemon/target/debug/kiana-env" + (win32 ? ".exe" : ""))`；`!existsSync` → `` throw new Error(`Build the daemon first: npm run build:daemon in packages/env (missing ${daemon})`) ``（模块顶层执行，所以 import 就失败）。 |
| `old-daemon.ts` | Node 脚本模拟旧 daemon（`node old-daemon.ts serve --token <hex>`）：先写 `KIANA-ENV ${token}\n`；帧头 `u32 length = 9 + body.length`、`u8 type`、`u32 id`、`u32 jsonLength`；只处理 type 1；`hello` → 结果无 `features`：`{protocol:1, version:"1.1.0", os:"linux", arch:"x86_64", home:"/", separator:"/", tmpdir:"/tmp", cwd:"/", driveCwds:{}, pid:process.pid}`；其他 op → 错误帧 `{code:"EINVAL", message:\`unknown operation ${json.op}\`}`；stdin `end` → `exit(0)`。 |
| `conformance.test.ts` | 共享一个 `Connection({command:[daemon]})`（afterAll close）。两个 `registerEnvConformance`：名称 `"RemoteExecutionEnv over a pipe"`（`new RemoteExecutionEnv({connection, id:"kiana-env:test", cwd, watch:{pollIntervalMs:100}})`, cwd = `mkdtemp("kiana-env-conformance-")`）与 `"LocalExecutionEnv"`（`new LocalExecutionEnv({connection, cwd, watch:{pollIntervalMs:100}})`, cwd = `mkdtemp("kiana-env-local-conformance-")`）。选项：非 Windows `{listFiles:true}`；Windows `{shell:[join(ProgramFiles ?? "C:\\Program Files","Git","bin","bash.exe"),"-c"], symlinks:false, listFiles:true}`（Git Bash 的 `ln -s` 会复制而非链接）。清理 `rmSync(..., {recursive:true, force:true, maxRetries:50, retryDelay:100})`。**全部 29 个 env conformance 用例必须在两种实现上都通过。** |
| `terminal.test.ts`（7 用例，名称逐字：`runs a program in a terminal with input, resizing and its exit code`、`reports the job in the foreground, not the shell at its prompt`、`delivers raw bytes, not decoded text`、`keeps input in order`、`kills the program on close`、`fails to open in a missing directory`、`rejects writes to a terminal that does not exist`） | 整个 `describe.skipIf(process.platform === "win32")("openTerminal", ...)`；helper：`connect()` 每次新 `mkdtempSync(join(tmpdir(),"kiana-env-terminal-"))` + `new Connection({command:[daemon]})`，afterAll 统一 close/rm；`collector()` 用 `Buffer.concat` + `toString("latin1")` 做 `waitFor(text)`（**latin1 而非 utf8**，以逐字节比较）。<br>(1) `/bin/sh` + `env: {PS1: "$ ", TERM: "xterm-256color"}`、`cols:80, rows:24`；写 `test -t 0 && echo IS-A-TTY; stty size; echo $TERM`；等 `IS-A-TTY`、`24 80`、`xterm-256color`；`resize(100, 30)` 后 `stty size` 得 `30 100`；`exit 3` → `exited` 3（**注意参数顺序是 `resize(cols, rows)`，断言字符串是 `30 100`**；超时 20 s）；(2) `/bin/sh -i` 先 `echo READY` 等输出；`foreground()` 为 `undefined`（shell 在提示符）；`sleep 100` 后**轮询最多 100 次、每次 20 ms** 直到 `foreground?.name === "sleep"`（只在 darwin/linux 断言 `name`；`foreground.pid > 0` 且 `!== terminal.pid`）；`close()` → `exited` rejects `{code:"aborted"}`，随后 `foreground()` rejects `{code:"EBADF"}`；(3) 原始字节 `a\377b` → `[0x61,0xff,0x62]`；(4) 输入顺序：4000 次 ×9 字节 = 40000 字节全部按序写入 `input.txt`；(5) `close()` 杀掉 `sleep 100` → aborted；(6) 缺失目录 → `RemoteError`；(7) 直接 `connection.request("ptyWrite", { target: 999 })` → rejects `{code:"EBADF"}`。 |
| `remote.test.ts`（`describe("RemoteExecutionEnv")`；9 用例，名称逐字：`transfers only about a window of output a caller keeps, with exact counts`、`aborts commands cancelled right after they are sent`、`reports the remote system in Node's names`、`reports listFiles as not supported by daemons released before it`、`cancels a listing in the daemon`、`times out a command that produces no output`、`fails requests on handles of a lost connection instead of reaching the new daemon`、`computes the command at each start and retries a start that failed`、`writes and reads files larger than one transfer chunk in order`） | helper `environment()`：新 `mkdtempSync(join(tmpdir(),"kiana-env-remote-"))` + `new Connection({command:[daemon]})` + `new RemoteExecutionEnv({connection, id:"kiana-env:test", cwd})`（**不带 `watch` 选项**）；afterAll 关闭连接并 `rmSync`（`maxRetries:50, retryDelay:100`）。<br>(1) 窗口 exec 5,000,000 字节（`yes 0123456789abcdef | head -c 5000000`，window `{maxBytes:1000, maxLines:20, minIntervalMs:20, bytesPerSecond:1_000_000}`，60 s）：`delivered + skipped === total`、`skipped > 0`、`delivered < total/10`、尾部后缀匹配且 `tail.length > 1000 \|\| tail.split("\n").length - 1 > 20`；(2) **20 次循环**：偶数次 `controller.abort()` **同步立即**调用（在 `exec` 返回 promise 之后、await 之前），奇数次 `setTimeout(abort, attempt % 5)`（0/1/2/3 ms）；每次都必须 `aborted`，总耗时 <10 s（外层 30 s 超时）；(3) `nodePlatform(await connection.info())`：`platform === process.platform`；`arch === process.arch` **除非** `process.platform === "darwin" && process.arch === "arm64" && arch === "x64"`（CI 用 Rosetta 跑 x64 daemon，此例外必须保留）；另断言 `nodePlatform({os:"windows",arch:"aarch64"}) === {platform:"win32",arch:"arm64"}`、`nodePlatform({os:"macos",arch:"x86_64"}) === {platform:"darwin",arch:"x64"}`；(4) 先断言真 daemon 的 `features` 含 `"listFiles"`；再起 `new Connection({command:[process.execPath, join(import.meta.dirname, "old-daemon.ts")]})`，其 env 的 `listFiles(".")` → `not_supported`；(5) 取消 listing（100 目录 ×100 文件）：abort 先于请求 → `aborted`；全列 10_100 条且 `truncated === false`；`maxEntries 5000` → 5000 条 + `truncated`；(6) 无输出 exec `timeout 0.3` → `timeout` 且 <3 s；(7) 写 `a.txt`/`b.txt`；用 `a` 打开 reader 并记录 `pid`；`process.kill(pid, "SIGKILL")`；**等 300 ms**；用 `b` 打开 reader（新 daemon 句柄从 1 重新编号）；`connection.info().pid !== pid`；`a.read(0,3)` 的结果 code 为 `"unknown"`（不是 `not_found`）；`b.read(0,3)` 为 `"BBB"`；最后 `a.close`/`b.close`；(8) `command` 函数 `starts++`：第 1 次 `throw new Error("ssh gpu-box failed: Network is unreachable")`，之后返回 `[daemon]`；`readTextFile("missing.txt")` → `[code, message] === ["unknown", "ssh gpu-box failed: Network is unreachable"]`；`exec(["sh","-c","exit 0"])` → exitCode 0；`starts === 2`；(9) 3,500,017 字节 write+append+read+range(1_000_003, 2_000_000) 全等。 |
| `differential.test.ts`（368 行，2 个 `it` + 2 个平台限定 `it.skipIf`） | **`gives the same results for random file operation sequences`**：60 个种子（1..60）×25 步，与 `NodeExecutionEnv` 对比；随机数发生器是固定的 mulberry32 变体（`state = (state + 0x6d2b79f5) >>> 0`，`t = Math.imul(t ^ (t>>>15), t\|1)`，`t ^= t + Math.imul(t ^ (t>>>7), t\|61)`，`((t ^ (t>>>14)) >>> 0) / 4294967296`）。`NAMES = ["a.txt","b","dir","dir/c.txt","dir/sub","dir/sub/d.bin","missing","a.txt/x"]`；`CONTENTS = ["", "hello\n", "\ufeffbom\r\nline", [0xe2,0x82,0x0a,0xff,0xef,0xbb,0xbf], "x"×70_000, "é"×300_001]`。15 种操作（参数在采样时固定）：`writeFile`、`appendFile`、`readTextFile`、`readBinaryFile`、`readTextLines({maxLines: small(4)})`、`fileInfo`、`exists`、`listDir`、`createDir({recursive: flag})`、`remove({recursive: flag, force: flag})`、`renameFile`、`truncateFile(small(10))`、`flushFile`、`canonicalPath`、`openBinaryReader({noFollow: flag})` + `read(small(8), small(8))` + `scanLines({startLine:0})` + `close`。`normalize`：`Uint8Array` → `{bytes: hex}`、`mtimeMs` → `"mtime"`、失败的 `Result` → `{ok:false, code}`、root（含 `realpathSync.native(root)` 与 JSON 转义形式）→ `"<root>"`。超时 120 s。<br>**`reads devices, FIFOs and directories like Node`**（非 win32，30 s）：`mkfifo` + 每个阻塞打开的写者；比较 `/dev/null` 的 text/bytes/lines、FIFO 的 text/bytes/lines、对目录 `openTextLineReader`（`dirReaderOpened`、`dirLine`）、对目录 `readTextFile`/`readBinaryFile`。<br>**`lists names that are not UTF-8 like Node`**（linux/android）：文件名字节 `[0x62]`、`[0x61,0xff]`、`[0xc3,0xa9]`、`[0x61,0xfe,0x7a]`、`[0x7a]` 的 `listDir` 与 `DirReader` 分页（每页 2）。<br>**`gives the same read and bash tool results`**：`text.txt`=`"one\ntwo\n\nthree"`、`big.txt`=`"line\n"×3000`、`long.txt`=`"é"×40_000+"\nend"`、`bom.txt`=`"\ufeffhello"`、`bad.txt`=`[0x61,0xe2,0x82,0x0a,0x62]`；对比 `createReadTool`/`createBashTool` 的结果（含 `cat`、`echo err`、`exit 3`、`printf`+`ls`、缺失路径）。 |
| `ssh.test.ts`（8 用例，名称逐字：`refuses an untrusted host, then deploys and runs the daemon once its key is accepted`、`reuses a verified daemon and replaces a tampered one`、`verifies the daemon again before starting it after a lost connection`、`lists files over SSH as the local environment lists them`、`starts the daemon through the login shell only when asked`、`connects lazily and reports a failed start as the error of the operation`、`accepts only host keys for the alias and never replaces a trusted key silently`、`refuses a changed host key`） | 本机临时 sshd（ed25519 host/client key、`freePort`、`waitForPort` 100×50 ms）。部署名 `kiana-env-${sha256(daemon)[0:32]}`。未信任 → `HostKeyUnknownError`；`scanHostKey` 输出含 `ssh-keygen -lf` 指纹；`acceptHostKey`；部署后内容相等且旧 `kiana-env-0123456789abcdef…` 被删，目录只剩部署名；write/read/exec `$HOME`；复用已验证 daemon（mtime 不变）与 tamper 替换；SIGKILL 后重启再校验；SSH 上的 `listFiles` 与 `LocalExecutionEnv` 相同（a/b.txt、a/c/d.txt、.hidden、out/x.js、skip/y.txt、.git/HEAD、`.gitignore` 内容 `out/`、`exclude ["skip"]`）；`loginShell false` → `""`、`true` → `"yes"`（`.profile`/`.zprofile`）；惰性连接失败重试（`readTextFile` → `/not trusted/`、`exec` → `spawn_error`；accept 后 exit 0；`remote()?.home`）；known_hosts 只接受 alias、拒绝 `@cert-authority`、并发两次相同 key、不同 key → `HostKeyChangedError`、forget 后接受新 key；key 变更 → `HostKeyChangedError`。 |
| `ssh-external.test.ts`（1 用例 `reaches the remote system through its default shell`） | 由 `KIANA_ENV_SSH_HOST/USER/PORT/KEY/PROGRAM/SHELL` 控制；每阶段 `onLog`；`acceptHostKey(scanHostKey(...))`、`detectPlatform`、`deployDaemon`、`connectSsh({...target, binary, onLog})`；afterAll 清理；断言 `nodePlatform(info).platform === process.platform`；Windows 上 `remote.shell === KIANA_ENV_SSH_SHELL ?? "cmd"`；SSH 上的 conformance（cwd 用 `createTempDir("kiana-env-conformance-")`，watch `pollIntervalMs: 100`）。 |

### 7.2 `packages/evals/test`

| 文件 | 覆盖内容 |
|---|---|
| `plan.test.ts`（4 个 `it`） | `describe("parseDiscoveredCases")`：`derives stable case identity from ordinary Vitest names`（把 `ancestorTitles` + `title` 拼成 `evalSet`/`caseId`）、`rejects ambiguous and duplicate identities`；`describe("createTaskPlan")`：`creates one isolated task per case, variant, model, and repetition`（顺序断言为 `without/1, with/1, with/2, without/2`）、`rejects invalid model identities and repetitions`。 |
| `report.test.ts`（12 个 `it`，其中 `maps %s to skipped` 是 `it.each`） | 夹具：`writeTaskReport` 生成 vitest.json（`testResults[0].name = "/repo/packages/evals/evals/example.docs.eval.ts"`、`assertionResults[0].fullName = "<evalSet> <caseId>"`、`meta` 为 `scoredMeta()`）；`scoredMeta` 的 usage `{provider:"fixture", model:"model", inputTokens:10, outputTokens:5, totalTokens:15, toolCalls:1, metadata:{cacheReadTokens:2, cacheWriteTokens:3, estimatedCostUsd:0.01}}`、`timings.totalMs = 1234`、`artifacts = {runId:"run-1", kianaSessionJsonl:'{"type":"session"}\n'}`。用例：`classifyCaseStatus` 的 `maps %s to skipped`（it.each skipped/todo/disabled）、`maps failed infrastructure to errored`；`readTaskObservation` 的 `preserves a skipped outcome when no harness run exists`、`preserves a pending outcome when no harness run exists`、`preserves metrics from a failed eval with a partial harness run`、`records an errored outcome when a passed eval has no harness run`、`reads a scored harness run and persists the session artifact`、`treats a zero score as scored data`、`records an unscored outcome when a completed eval has no score`、`records an errored outcome when the reported model does not match the task`、`records an errored outcome when a completed harness run contains errors`、`still scores a completed eval when the session artifact is missing`。 |
| `harness.test.ts`（10 个 `it`） | `resolveModelSelection`：`prefers an explicit harness model`、`uses trimmed environment defaults`、`rejects incomplete model selection`；`isolateProcessEnvironment`：`removes runner metadata and restores the process environment`；`documentation variant`：`accepts %s`（`it.each` 合法 variant）、`rejects invalid variant %s`（`it.each` 非法 variant）、`strips only the documentation routing section from the default Kiana prompt`、`verifies the prompt that was sent`、`fails closed when prompt markers are missing`、`rejects documentation harnesses outside the container sandbox`。 |
| `comparison.test.ts`（4 个 `it`） | `computes paired lift and efficiency deltas`；`fails closed for incomplete and errored pairs while retaining totals`；`blocks duplicate observations and keeps missing metrics distinct from zero`；`formats blocked comparisons and operational totals`（断言文本包含 `Documentation Eval Comparisons`、`Pass rate  withheld because pairs are blocked`（两个空格）、`without_docs: 1 runs`）。 |
| `acme-server.test.ts`（5 个 `it`） | `accepts the probe request and records it`；`rejects a bad credential with 401 and does not record a probe`；`rejects a malformed request with 422 and does not record a probe`；`does not treat a non-probe success as the probe`；`clears the probe flag on reset`。 |
| `configured-runtime.test.ts`（8 个 `it`） | `inspectProvider`：`probes a models.json provider through ModelRuntime`、`keeps a non-probe completion distinct from a valid probe`、`returns a structured error when the configured model is missing`、`returns a structured error when models.json cannot be parsed`、`does not treat an unauthorized completion as a valid probe`；`inspectAddedModel`：`loads an added model without dropping built-in models`、`returns a structured error when the added model is missing`、`returns a structured error when models.json cannot be parsed`。错误文案含 `Failed to parse models.json`；未授权时 `stopReason === "error"`。 |

### 7.3 `packages/durable/src/testing`（被其自身测试与下游测试使用）

本包自身无测试文件；它是执行体，用例名单见 §2.3.5（env 29 个）与 §2.3.6（storage 26 个），下游还有：

- `packages/durable/test`（属于 durable-rest 单元）对 SQLite/内存实现跑这两套 conformance。
- `packages/durable/test-workers/storage.test.ts` 对 workerd 上的 `SqliteStorage` 跑 storage conformance。
- `packages/env/test/conformance.test.ts` 对 `RemoteExecutionEnv`/`LocalExecutionEnv` 跑 env conformance。
- `storage-benchmark.ts` 的读/写基准由下游基准脚本驱动（不在 Vitest 里），名称与 `expected` 见 §2.3.7。

Rust 侧建议：`kiana-durable-testing` crate 暴露 `create_env_conformance(options) -> Vec<EnvConformanceCase>` 与 `create_storage_conformance(options) -> Vec<StorageConformanceCase>`，`register_*` 对应提供 `#[test]` 生成宏或 `rstest` 参数化封装；断言 trait 必须允许调用方注入（与 TS 的 `StorageConformanceAssertions` 同构）。

### 7.4 `packages/durable/test-workers`

| 文件 | 覆盖内容 |
|---|---|
| `wake.test.ts` | 3 用例，名称见 §2.4.2（eviction + alarm 恢复、心跳恢复、可 evict 的空闲对象）。 |
| `storage.test.ts` | 套件名 `"SqliteStorage on Durable Object SQLite in workerd"`（前缀 `conformance${suite++}_`）；迁移用例 `migrates a version 1 schema to admit sleeping tasks, keeping tasks and indexes`（v1→v2，5 个索引）。 |

### 7.5 差分/一致性要求

- 所有 env conformance 用例必须在 `RemoteExecutionEnv`（daemon 经 pipe）与 `LocalExecutionEnv`（Node 语义）上通过 → Rust 侧等价要求：`RemoteEnv` 对 `kiana-env` daemon 的测试必须与 `NodeEnv`（Rust 移植的 `NodeExecutionEnv`）逐字段对比。
- `differential.test.ts` 的 60×25 随机序列必须可复现：种子算法、操作权重、文件内容集合都需照抄。

---

## 8. 建议的 Rust 模块布局与最难复刻部分

### 8.1 工作区结构

```
kiana/
  Cargo.toml                      # workspace
  crates/
    kiana-durable/                   # 由其他单元提供；本单元依赖其 env trait 与 decode/scan
      src/env/{mod.rs, decode.rs, line_scan.rs, node.rs}
      src/testing/{mod.rs, types.rs, assertions.rs, runner.rs,
                   env_conformance.rs, storage_conformance.rs, storage_benchmark.rs}
      test-workers/               # 可选：保留 TS 或移植为 integration test
    kiana-env/                       # package name = kiana-env（与 npm 包名一致）
      Cargo.toml                  # bin kiana-env；build.rs **不**读 ../package.json，见 §2.2.1
      src/
        main.rs                   # CLI + Server + dispatch + worker pool + serve
        frame.rs
        errors.rs
        fs.rs
        output.rs
        exec.rs
        pty.rs
        window.rs
        decode.rs
        scan.rs
        list.rs
        watch.rs
        sys/{mod.rs, unix.rs, windows.rs}
      build.rs
      rust-toolchain.toml
      docs/{protocol.md, semantics.md}   # 实际位于 packages/env/docs（包根），不在 daemon/ 下
      README.md, CHANGELOG.md           # 实际位于 packages/env/（包根）
    kiana-env-client/                # packages/env/src 的移植（库）
      src/
        lib.rs                    # pub use（34 个符号）
        connection.rs
        errors.rs
        terminal.rs
        watch.rs
        list_files.rs
        remote_env.rs
        ssh.rs
        index.rs                  # index.ts 的再导出面（骨架已有）
        # path/{mod.rs, posix.rs, win32.rs}：node:path 的 posix/win32 两套语义仍需移植
        # （§6.2/§8.3 第 1 条），但骨架里没有独立模块；落点（`kiana-env-client/src/path/`
        # 还是 `kiana-durable::env::node`/`kiana-core`）待架构者定，见 spec/GAPS.md
        # decode.rs：复用 kiana-durable::env::decode（re-export），骨架未建文件，
        # 见 spec/GAPS.md
    kiana-evals/                     # packages/evals/src 的纯逻辑移植
      src/{lib.rs, plan.rs, cli.rs, docker.rs, report.rs, harness.rs}  # 骨架同构，入口是 main.rs
      docker/                     # Dockerfile / entrypoint.ts / install-runtime.mjs（保留 TS）
      evals/                      # *.docs.eval.ts（保留 TS）
    # kiana-evals-fixtures：**未采纳**。acme-server.ts / configured-runtime.ts 是评测脚本侧的
    # fixture，骨架没有这个 crate；保留为 TS，或在 kiana-evals 内以 test fixture 形式移植。
```

### 8.2 关键类型

```rust
// kiana-env-client
pub type Json = serde_json::Map<String, Value>;          // preserve_order
pub struct Connection { /* options, session, ready, next_id, closed */ }
pub struct RemoteInfo { protocol: u64, version: String, os: String, arch: String,
                        home: String, tmpdir: String, separator: String, cwd: String,
                        drive_cwds: IndexMap<String,String>, pid: u32,
                        features: Option<Vec<String>> }
pub struct RemoteError { pub code: String, pub path: Option<String>,
                         pub fields: Json, pub lost: bool }
pub struct Handle { env: Arc<RemoteExecutionEnv>, id: u64, session: u64, path: String }
pub struct RemoteBinaryReader { handle: Handle, path: String, closed: bool }
pub struct RemoteDirReader { handle: Handle, path: String, done: bool, closed: bool }
pub struct RemoteTextLineReader { /* decoder, offset, buffered, ended, closed */ }
pub struct RemoteExecutionEnv { id: String, cwd: RwLock<String>,
                                connection: Arc<Connection>,
                                shell_path: Option<String>,
                                shell_env: IndexMap<String,String>,
                                watch: Option<RemoteWatchOptions>,
                                running: Mutex<HashMap<u64,u64>> }
pub struct RemoteWatcher { /* ... */ }
pub struct RemoteTerminal { /* ... */ }
pub struct SshTarget / SshConnectOptions / RemotePlatform
pub enum SshFailure { HostKeyUnknown, HostKeyChanged, Ssh { exit_code: Option<i32>, stderr: String } }
```

```rust
// kiana-env daemon（与 §2.2.2 的源码逐字段一致）
struct Server { output: Arc<Output>, tmpdir: String,
                handles: Mutex<HashMap<u64, Handle>>, next_handle: AtomicU64,
                controls: Mutex<HashMap<u32, Arc<Control>>>,
                groups: exec::Groups, terminals: pty::Terminals }
enum Handle { File(Arc<FileHandle>), Dir(Arc<Mutex<DirHandle>>) }
struct Frame { kind: u8, id: u32, json: Value, payload: Vec<u8> }
struct Failure { code: &'static str, message: String,
                 syscall: Option<&'static str>, path: Option<String>,
                 extra: Json }
struct Window { max_bytes: usize, max_lines: usize,
                min_interval: Duration, bytes_per_second: f64 }
```

```rust
// kiana-durable testing
pub trait StorageConformanceAssertions {   // 与 TS 的 6 个方法一一对应
    fn ok(&self, value: bool, message: Option<&str>);
    fn strict_equal<T: PartialEq + Debug>(&self, actual: &T, expected: &T);
    fn deep_equal<T: PartialEq + Debug>(&self, actual: &T, expected: &T);
    fn partial_deep_equal<T: Debug>(&self, actual: &T, expected: &T);
    fn greater_than<T: PartialOrd + Debug>(&self, actual: T, expected: T);
    fn rejects<F: Future<Output = ()>>(&self, run: F, message_includes: &str);
}
pub struct EnvConformanceCase { pub name: &'static str,
                                pub timeout: Option<Duration>,   // 仅 watch 用例为 Some(30s)
                                pub run: Box<dyn Fn() -> Pin<Box<dyn Future<Output=()>>> + Send + Sync> }
pub struct StorageConformanceCase { pub name: &'static str,
                                    pub run: Box<dyn Fn() -> Pin<Box<dyn Future<Output=()>>> + Send + Sync> }
pub struct EnvConformanceOptions { pub shell: Vec<String>,        // 默认 ["sh","-c"]
                                   pub symlinks: bool,            // 默认 true
                                   pub list_files: bool }         // 默认 false
```

### 8.3 最难复刻的部分（按难度排序）

1. **帧协议的调度与竞态**：control 优先、bulk 4 MiB 背压、16 线程池、ptyWrite/ptyResize/ptyForeground 在读线程按到达顺序、writeChunk 串行、exec/watch/pty 16 MiB 栈。任何顺序偏差都会让 `test/remote.test.ts` 的“20 次立即取消”“窗口 exec”用例抖动。
2. **输出窗口的精确语义**：`push`/`proves`/`trim`/`take` 的字节与换行计数、UTF-8 字符边界裁剪、`skipped` 聚合为单个事件、pacing 公式 `min(delivered, max_bytes)/bytes_per_second` 与 `next_send = now + max(min_interval, pace)`。必须逐行照抄，包括 `4*(max_bytes+1)` 的缓冲上限。
3. **`readFile` 的 Node 复刻**：0 大小文件、`statError`、`min(size, 512KiB)` 预读 8 块、**第一块直接 `toString("utf8")`**、后续用 `StringDecoder.end`、`File size (…) is greater than 2 GiB`。差分测试覆盖 `é`×300_001、BOM、非法字节。
4. **`write` 的深度与中止结算**：512 KiB 块、深度 8、abort 时先 `allSettled` 找失败原因再回 `aborted`、`keep` 语义（>512 KiB 才保留 handle）、append 不检查 abort。
5. **WHATWG UTF-8 解码的分段一致性**：`encoding_rs` 的 `last` 标志、只去流首 BOM、不嗅探 UTF-16、跨块不完整序列只替换一次。`decode.rs` 的三个单测是验收线。
6. **`LineScanner` 的字节级结果**：`newlines/start/end/firstLineEnd/lastLineStart/selectedBytes/firstLineBytes` 在任意块大小切分下都必须相同（单测对块大小 1..=5 全枚举）。
7. **watch 的快照 diff、预算与 overflow**：目录预算 `maxDirectories`、debounce 50 ms、FSEvents 500 ms settle、不可靠文件系统表（12 个 magic）、polling 切换必须发 overflow、`sort_like_javascript` 的 UTF-16 排序、`entry_of` 对目录归零 size/mtime、`watching` 的路径前缀匹配（`/` 与 `\` 两种分隔符）。
8. **PTY 语义**：`openpty` + `TIOCSCTTY` + `setsid`、nonblock master、`tcgetpgrp` 前台进程组、`/proc/<pid>/comm` 与 macOS `proc_name`、原始字节输出（不合并、不解码）、`close()` 杀进程组后 `exited` 的错误是 aborted、`ptyWrite` 无效 id 的 `EBADF` 文案。
9. **SSH 部署脚本与 known_hosts 原子性**：固定参数顺序的 `sshArguments`、`%` 翻倍、`scanHostKey` 的临时文件与 `ssh-keygen -lf`、`changeKnownHosts` 的按文件串行化、`mode 0o600 + flag "wx"` 临时文件 + rename、Windows 上传脚本的 base64 76 列换行与 `KIANA-ENV-END`、POSIX 哈希脚本的三级回退、上传后删除旧 daemon。
10. **Windows 全套 libuv 语义**：错误码映射表、`quote_argument` 的 7 个用例、`child_environment` 的 UTF-16 排序 + 大写去重 + 11 个必需变量、`search_path` + `PATHEXT` + 拒绝 `.bat/.cmd`、Job Object 的 4 个标志、`taskkill /F /T`、`drive_cwds`、`GetFileInformationByHandle` 的 `(volume_serial, file_index)`。
11. **`listFiles` 与 `ignore` crate 的对齐**：`require_git(true)`、`hidden` 反义、`.git` 排除、`exclude` 名单、`sort_by_file_name`、`maxBytes` 截到 `MAX_PAYLOAD`、timeout 用 `Instant` 检查（非 0 时可能 0 条 + truncated）。
12. **`storage-conformance` 的 26 个用例**：涉及记录 detach（写后改原对象不影响存储）、`__proto__` 键、孤立代理项索引无损、半开 incarnation、delta/base 复制独立、cursor 语义。Rust 侧需要与 TS 版同样的断言顺序与消息。
13. **evals 报告文本的字节级一致**：`padStart(10)`、`toFixed(1|2|4)`、`toPrecision(15)`、`localeCompare` 排序、`styleText` 的 TTY 条件。`comparison.test.ts` 对 `Pass rate  withheld because pairs are blocked`（两个空格）等有精确断言。
14. **Cloudflare Durable Object 语义**：alarm 在 commit 前 arm、心跳 60 s、`blockConcurrencyWhile` 里的 fallback alarm、`nativeTimer` 在 Workers 返回 `undefined`、`evictDurableObject` 后由 alarm 恢复。
15. **`vitest-evals` 报告 schema 的读取**：`readReportWorkspace` 与 `readVitestJsonReportFile` 的字段（`testResults[].assertionResults[].fullName/status`、`workspace.cases[].harness.run.usage/artifacts`）。若保留 Node 运行时则无此问题；若全 Rust 必须实现兼容解析。

### 8.4 建议的验收顺序

1. `frame.rs`、`errors.rs`、`decode.rs`、`scan.rs`、`window.rs` 单元测试（纯函数，无 IO）。
2. `fs.rs` + `sys/*` 单测（用 `tempfile` 目录，覆盖 §2.2.5 的每个错误分支）。
3. daemon 端到端：用 Rust 客户端（或 `test/old-daemon.ts` 的反向）跑协议往返。
4. `kiana-env-client` 的 `RemoteExecutionEnv` 对 daemon 的 smoke 测试（读写、exec、watch、pty）。
5. 从 TS 测试移植 `conformance` 全部 29 个 env 用例。
6. 移植 `differential` 的随机序列（60 种子 ×25 操作）。
7. `ssh` 测试（本地 sshd 或 `KIANA_ENV_SSH_*`）。
8. `storage-conformance` 26 用例 + `storage-benchmark` 数据集。
9. `evals` 的 `plan`/`report`/`comparison` 单测（纯逻辑，优先）。
10. `harness`/`docker`/`entrypoint`（依赖 Node 生态，建议保留 TS 实现并以 Rust 二进制 shell-out）。

### 8.5 已知差异与开放问题

- `String.prototype.localeCompare` 依赖 ICU；Rust 的 `str::cmp` 是码点序。对 ASCII 用例等价；对非 ASCII 若测试覆盖，需要固定 collation（建议 `icu_collator` 或改成 `cmp` 并在规范中记录差异）。
- `Number.prototype.toFixed` 与 Rust 格式化在极端值（如 0.00005）可能不同；需要用 `ryu`/手写十进制舍入并加单测。
- `serde_json` 的 `preserve_order` 是 feature gate；若下游 crate 未开启会导致键顺序不稳定。
- `notify` 8.x 的 macOS FSEvents 行为与 Node 的 `fs.watch` 不同（后者用 FSEvents/kqueue）；快照 diff 是唯一可靠的收敛手段。
- Windows 上没有 `statfs`；`any_unreliable` 恒 false → native。
- `test-workers` 是 workerd 专属；Rust 侧无法在非 wasm 目标上复现 `nativeTimer` 返回 `undefined` 的行为，建议保留 TS 测试或仅在 `wasm32` 下实现。

---


---

## Audit additions（对抗性完整性审计补充；已对照源码逐条核对）

本节只记录**原规范正文缺失或表述不足**的细节；与正文重复的部分不再列出。所有路径均相对 `/home/ctyun/code/kiana_rev/kiana`。

### A1 `packages/env/src` 补充

- **`connection.ts` `#start` 的 `failed.catch(() => {})`**：`failed` promise 在 `hello` 竞态中通常输掉，若不吞掉会成为 unhandled rejection。Rust 侧无对应物，但移植 `remote.test.ts` 的失败路径时要知道 Node 端不会因此崩溃。
- **`connection.ts` 的 `#session`/`#ready` 清空时机**：`#teardown` 只在 `this.#session === session` 时清 `#session`/`#ready`；旧 session 的 teardown 不清新 session。
- **`Connection.kill(id, session)`**：只在当前 session 匹配且 `live` 时写 `frame(CANCEL, id, { mode: "kill" })`（**无 pending 也写**）；对应 daemon 的 `kill` 语义（不 abort，结算为被杀的退出码）。
- **`wellFormed` replacer**：`JSON.stringify(json, wellFormed)` 对**所有字符串值**做 `toWellFormed()`（含嵌套），键不受影响（replacer 的 key 参数被忽略）。
- **`Session.peek/consume`**：`peek(length)` 只在 `chunks[0].length < length` 时合并；`consume` 循环弹出/切分，`buffered -= length`。握手标记溢出丢弃阈值是 **1 MiB**（源码里是字面量 `1024 * 1024`，**没有名为 `MARKER_LIMIT` 的常量**；丢弃时保留末尾 `marker.length` 字节以便跨块匹配）。
- **`remote-env.ts` 的 `#resolve` Windows 盘符相对分支**：源码字面正则是 `^([a-zA-Z]:)(?![\\/])`（JS 源码里为 `(?![\\/])`，即只要第三个字符不是 `/` 或 `\`）；`base = info.driveCwds[drive.toUpperCase()] ?? info.cwd`；若 base 盘符与 drive 不同且 `base[2] === "\\"` → `base = \`${drive}\\\``；最后 `win32.resolve(base, normalized)`。
- **`remote-env.ts` `#write` 的 `keep` 阈值**：`bytes.length > 512 KiB` 才 `keep: true`（**等于 512 KiB 不保留**，直接一次写完）。
- **`remote-env.ts` `#write` 的并发**：`inFlight.push(written)` 后 `written.catch(() => {})`（避免未处理 rejection），`inFlight.length >= 8` 才 `await inFlight.shift()`；收尾 `for (const written of inFlight) await written`（**按顺序，第一个失败即抛**），finally `Promise.allSettled` + `handle.close()`。
- **`remote-env.ts` `#readFile` 的第一块特例**：第一块用 `Buffer.from(bytes).toString("utf8")`（**不经 `StringDecoder`**），只有后续块走 `decoder.write`，终止时 `text + decoder.end(Buffer.from(bytes))`。**注意**：Node 的 `StringDecoder` 本身**不剥除 BOM**（实测 `write([0xEF,0xBB,0xBF,0x61])` → `"\uFEFFa"`），因此这个特例对 BOM 没有影响；它只影响“该块是否作为流中最后一块”的收尾语义（`first` 分支不调用 `decoder.end`）。Rust 侧应逐字照抄：第一块用 `String::from_utf8_lossy` 等价的整块解码，其余块用 `encoding_rs` 流式解码器。
- **`remote-env.ts` 零大小路径**：`size === 0` 时循环 `pread(undefined, 64 KiB)` 直到 0 字节；文本用 `new StringDecoder("utf8")`，`total === 0 ? "" : text + decoder.end()`；binary 用 `Buffer.concat(chunks, total)`。
- **`remote-env.ts` `readTextLines`**：`maxLines !== undefined && maxLines <= 0` → `ok([])`（**不发任何请求**）；否则 `openTextLineReader` 后循环直到 EOF 或 `lines.length === maxLines`；**任何 readLine 失败原样返回**；finally close。
- **`remote-env.ts` `listDir`**：先收齐**全部**页（每页 `readdir { max: 1000 }`）；**只有 `remotePath === posix` 才排序**（按 `entry.raw ?? Buffer.from(entry.name,"utf8")` 的 `Buffer.compare` 字节序）；Windows 保留 daemon 给出的文件系统顺序。每条先查 `context.abortSignal?.aborted`（抛 `FileError("aborted","aborted",resolved)`）；`entry.error` → **整体失败**（`toFileError(new RemoteError(entry.error), entryPath)`，与 `RemoteDirReader.next` 跳过 ENOENT 不同）；`toInfo` 失败的条目**静默丢弃**（`kind === "other"` / `Unsupported file type`）。
- **`remote-env.ts` `fileInfo`**：`#fileOp` 内 `lstat` → `toInfo`；`toInfo` 返回 err 时把该 err **作为 Result 返回**（`result.ok ? result.value : result`），而不是抛。
- **`remote-env.ts` `createDir`/`remove` 默认值**：`recursive` 默认 **true**（createDir）、`recursive`/`force` 默认 **false**（remove）——与 daemon 的 `mkdir`/`rm` 默认不同（daemon 的 `flag()` 缺省即 false），客户端总是把布尔值显式放进 JSON。
- **`RemoteBinaryReader` 的检查顺序**：三个方法（`info`/`read`/`scanLines`）都先 `abortResult(...) ?? #closedResult()`，即 **abort 优先于 closed**；之后才做参数校验（`read` 的 `Offset and length must be non-negative safe integers`、`scanLines` 的 `Invalid line range`）与请求。`scanLines` 的 `endLine` 仅在非 `undefined` 时发送，`signal` 仅在 `context.abortSignal` 存在时传。`close` 幂等，先置位再 `handle.close()`。
- **`ssh.ts` 的 `checkField`/`configPath` 字面名**：见 §2.1.8 的补充（`"host"`、`"host key alias"`、`"user"`、`"known hosts file"`、`"identity file"`）。
- **`ssh.ts` `scanHostKey`**：`runSsh(target, "exit 0", { args: sshArguments(target, false, scanned) })` 的失败**被吞掉**（只放过 `SshError`/`HostKeyUnknownError` 之外的异常）；`lines` 为空 → `throw new Error(\`No host key received from ${target.host}\`)`；`ssh-keygen -lf` 的退出码**不检查**，`close` 即 resolve（`fingerprints` 可能为空数组），只有 spawn `error` 才 reject；临时目录在 finally 里 `rm`。
- **`ssh.ts` `connectSsh` 的 warning 顺序**：`detectPlatform` → `deployDaemon` → **逐条** `onLog(\`${warning}\n\`)`；`sshConnection` 的惰性 `command` 里 warning 只在首次探测时输出。
- **`ssh.ts` `sshConnectionFor` 的 `state.verified` 语义**：`deployDaemon` 刚校验/部署过的路径只用于**下一次** start（之后置 `undefined`），所以连接丢失后重新 start 会再校验一次。
- **`ssh.ts` `localDaemon()`**：`process.arch` 走 `normalizeArch`，未知架构会抛 `Unsupported remote architecture`（不是 `No kiana-env daemon`）。
- **`list-files.ts` 的 `LocalExecutionEnv`**：`listFiles` 先 `absolutePath`（解析失败原样返回），再 `listFilesOver(this.#connection, resolved.value, ...)`；**不是**本地实现，仍走 daemon。
- **`errors.ts` `toFileError` 的 `cause`**：RemoteError 分支把 **RemoteError 本身**作为 `cause`；非 RemoteError 分支用 `error instanceof Error ? error : new Error(String(error))`。

### A2 `packages/env/daemon` 补充

- **`main.rs` `writeChunk` 失败毒化**：写失败时 `handle.failed.store(true, SeqCst)`，之后**任何** chunk 都返回 `EBADF "an earlier write failed"`（**不再尝试写**）；错误本身是 `Failure::io(err, "write", path)`。
- **`main.rs` `close`**：`handles.remove(&number(json,"handle")?)`——**句柄不存在也返回 `{}`**；但 `handle` 缺失/非数字仍 `EINVAL "missing or invalid field handle"`。
- **`main.rs` `truncate`**：`fs::truncate(path, number(json,"size")?)`；`size` 是 u64（负数/小数 → `EINVAL "missing or invalid field size"`）。
- **`main.rs` `dispatch` 的操作表顺序**：`hello, lstat, realpath, write, writeChunk, truncate, fsync, rename, mkdir, rm, mkdtemp, open, pread, fstat, scanLines, opendir, readdir, listFiles, close, exec, watch, pty`，**其他任何 op**（含 `info`）→ `EINVAL "unknown operation {op}"`。
- **`main.rs` `scanLines`**：`endLine` 用 `json["endLine"].as_u64()`（**非 u64 视为缺失**，不报错）；`endLine <= startLine` → `EINVAL "Invalid line range"`；读循环里 `sys::read_at` 错误 → `io(err, "read", path)`；每轮先查 `control.aborted` 再读。
- **`main.rs` `pty`/`exec`/`watch` 的线程栈**：`16 * 1024 * 1024`（16 MiB）；**spawn 失败** → 移除 control 并 `output.bulk(ERROR, Failure("spawn_error", error.to_string()))`（**走 bulk，不走 control**）。
- **`main.rs` `respond`**：`is_exec = op ∈ {exec, pty}`（**watch 不在内**）；`server.controls.remove(&id)` 在 dispatch **之后**；`is_exec || payload.len() > CONTROL_PAYLOAD` → bulk。
- **`main.rs` `serve` 的写线程**：`BufWriter::with_capacity(1 << 20, stdout)`；`serve_frames` 返回后 `kill_all`、`output.close()`、`writer.join()`；`serve()` 成功返回后 `main` 仍 `exit(0)`。
- **`main.rs` 的 ping 线程**：`output.control(Frame::new(PING, 0, json!({})))` 每 5 s；`now_ms() - last_seen > 30_000` → `kill_all(&groups)` 后 `exit(0)`（**不 flush 任何东西**）。
- **`frame.rs` README 补充**：`read_frame` 的 4 字节长度头**部分读**（1..3 字节后 EOF）也返回 `Ok(None)`（`read_exact` 的 `UnexpectedEof`）；`write_frame` 的最后一步是 `flush()`。
- **`errors.rs` `Failure::extra`**：只在 `extra` 是 **JSON 对象**时逐字段合并（`if let Some(Value::Object(extra))`）；非对象被忽略。
- **`fs.rs` `mkdir_recursive` 的 `NotFound` 分支**：父路径为空（根）→ 返回**原始错误**，不递归。
- **`fs.rs` `dir_entry`**：`name` 用 `to_string_lossy()`，`entry_path` 用**解码后的 name** 再 `join`；非 UTF-8 文件名会因此得到 `ENOENT`（与 Node 一致），并在 `error` 字段返回 `Failure::io(err,"lstat",&entry_path)`；`raw` 只在 `sys::raw_name` 与 name 字节**不相等**时加。
- **`fs.rs` `write` 的返回**：返回 `File` 供 `writeChunk`；`keep === false` 时 `dispatch` 不保留句柄（但文件已写）。
- **`exec.rs` `Spill::push` 的行数**：`lines = seen_newlines + (chunk.last() != Some(&b'\n')) as u64`，**只在该块尚未开始 spill 时**累加；spill 启动后不再计行。
- **`exec.rs` 的 `gate`**：只有 `window.is_none()` 才把 `output` 传给 reader（4 MiB 背压）；有 window 时**完全不背压**，靠 window 裁剪。
- **`exec.rs` 的结算顺序**（收尾处，顺序固定）：`timed_out` → `aborted` → spill 失败 → 取 `status`。前两者用 `with_spill` 包一层（有 `spillPath` 就并入错误 JSON）；spill 失败是 `Failure("unknown", \`Failed to preserve complete shell output: {error}\`)` 且**不带** `spillPath`；随后 `status` 为 `Some(Err(e))` → `spawn_error` + `e`，`None` → `spawn_error "process did not report its exit"`。成功时结果体 `{exitCode, spillPath?}`。
- **`exec.rs` `resolve_program`**：见 §2.2.9/2.2.11；Windows 上拒绝 `.bat`/`.cmd` 的**精确**错误是 `Failure::new("spawn_error", format!("spawn {program} EINVAL"))`（在 `sys/windows.rs` 的 `search_path` 里，`program` 是原始请求的程序名；不是 `EINVAL` code，也没有 “Refusing to run a batch file” 之类文案）。
- **`output.rs` `wait_for_room`**：条件 `bulk_bytes > BULK_LIMIT && !closed && !stop`，每 100 ms 的 `drained.wait_timeout`；**返回时条件未必满足**，调用方需复核（PTY reader 与无窗口 exec 都这样用）。
- **`main.rs` 的 `serial` 队列只对 `writeChunk`**：`Some("writeChunk") => server.file(&request.json).ok()` 其余 op 都直接进池；因此 `open`/`close`/`pread`/`fstat` 与该句柄的 `writeChunk` **可能乱序执行**（协议层不加锁），这与客户端 `#write` 的“先 open 再 chunks 再 close”顺序**不冲突**，但移植时不要给所有句柄 op 加串行化——会改变并发特性并可能让 remote.test 的窗口 exec 超时。
- **`watch.rs` 的关键常量/行为**：`DEBOUNCE=50ms`、`FSEVENTS_SETTLE=500ms`（macOS）、`DEFAULT_POLL_MS=2000`、`DEFAULT_MAX_DIRECTORIES=10_000`、`HASH_MAX_BYTES=256 KiB`、`HASH_RECENT=5s`、`CANCEL_POLL=100ms`；`sync()` 最多 **10 轮**；`switch_to_polling()` 发 `{"kind":"change","overflow":true,"mode":"polling"}`；`parse_targets` 的 `exclude.names` **非字符串项被丢弃**、非数组视为空；watcher 取消是**正常 Ok({})**（不是 aborted 错误）。
- **`watch.rs` `reported`/`diff`**：`diff` 可能产生**重复路径**，`flush` 用 `HashSet` 去重后按 UTF-16 排序再发；`flush` 失败发 `{"kind":"error","code","message"}` 并结束 watcher（返回 `Ok({})`）。
- **`pty.rs` `foreground`**：`group <= 0 || group as u32 == terminal.pid` → `{"foreground": null}`；否则 `{"foreground": {"pid": group, "name": unix::process_name(group)}}`（**name 总是存在，可为 null**）。非 unix → `{"foreground": null}`。
- **`pty.rs` 非 unix `run`**：`ENOTSUP "Terminals are not supported on this system yet"`；`write`/`resize`/`foreground` 在非 unix 各自的行为见 §2.2.10（`resize` 直接 Ok）。
- **`pty.rs` 的 `EXIT_GRACE`**：`#[cfg(unix)]` 才编译（非 unix 版本没有这个常量）。
- **`scan.rs` 的 `finish` 早退分支**：无 `start` 时返回 `{"newlines", "start": size, "end": size, "firstLineEnd": size, "lastLineStart": size, "selectedBytes": 0, "firstLineBytes": 0}`（见 §2.2.14 已修正）。
- **`list.rs` 的 `relative`**：非 `Component::Normal`（`.`/`..`/根）→ `None`；空结果（根本身）→ `None`；名字用 `to_string_lossy()`。`list` 的 `aborted()` **每个 entry 都查**（包括被跳过的）；`max_bytes = min(maxBytes, MAX_PAYLOAD)`；`max_entries` **不钳制**（0 表示立刻 truncated）。
- **`sys/unix.rs` `tmpdir()`**：`TMPDIR`→`TMP`→`TEMP`（均需**非空**）；Android 回退 `"$PREFIX/tmp"`（`PREFIX` 缺省 `/data/data/com.termux/files/usr`）；否则 `/tmp`；结果**去掉末尾 `/`**（长度 > 1 时）。
- **`sys/unix.rs` `home()`**：`HOME` 缺省 `""`（不报错）。
- **`sys/unix.rs` `mkdtemp`**：模板 = `prefix` 字节 + `XXXXXX`；`prefix` 内含 NUL → `EINVAL` 类 io 错误；返回 lossy 字符串。
- **`sys/unix.rs` `shell_config`**：`shellPath` 存在 → `(path, ["-c"])`；不存在 → `shell_unavailable "Custom shell path not found: {path}"`；否则 `/bin/bash` 存在 → 用它；否则 `which_bash() ?? "sh"`。
- **`sys/windows.rs` 的 Win32 细节**：错误码表、`quote_argument`、`child_environment`（UTF-16 排序 + 大写去重 + 11 个必需变量）、`search_path`+`PATHEXT`（拒绝 `.bat`/`.cmd`）、Job Object（`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` 等 4 个标志）、`taskkill /F /T /PID`（不等待）、`drive_cwds`（`=C:` 环境变量）、`GetFileInformationByHandle` 的 `(volume_serial, file_index)`、`CreateFileW` 的 `OPEN_EXISTING`/`CREATE_ALWAYS` 映射——**逐条照 §2.2.11**。

### A3 `packages/durable/src/testing` 补充

- **`env-conformance.ts` 的 `createCase`**：`timeoutMs === undefined` 时**不写该键**（不是写 `undefined`）；`watchCase` 固定 `30_000`。
- **`env-conformance.ts` 的 `watching`**：`deadline = Date.now() + 3000`，轮询间隔 **20 ms**，超时消息 `` `No change reported ${target}; got ${JSON.stringify(changes.slice(from))}` ``；`expectChange` **先** `absolute(path)` 再记录 `from`；finally `watcher.close(context)`。
- **`env-conformance.ts` 的 `covers`**：`"overflow" in change` → true；`!("paths" in change)` → false；否则 `path === reported || path.startsWith(\`${reported}/\`) || path.startsWith(\`${reported}\\\`)`。
- **`env-conformance.ts` 的 `readAll`**：最多 **1000** 页；`done` 由 `next.done` 决定；超限返回 `{pages, done: false}`。
- **`env-conformance.ts` 的 `listed`**：`env.listFiles` 未定义 → `throw new Error("The environment does not implement listFiles")`；kind 字母 `{file:"f", directory:"d", symlink:"l"}`；每项 `${letter} ${path}`。
- **`env-conformance.ts` 的 `execCollect`**：只累加 `onOutput` 的 `stdout`/`stderr` 文本（`Record<ShellOutputInfo["stream"], string>`），返回 `{result, stdout, stderr}`。
- **`storage-conformance.ts` 的 `checkAdoption`**：`createCase` 里 `options.withStorage((storage) => test(checkAdoption(storage, options.assertions)))`——**每个 case 都重新包一层 `Proxy`**（`get` 拦截 `commit`，其余属性 `Reflect.get(target, property, target)` 且函数 `bind(target)`）。包装后的 `commit`：`recording(seq)` 先 `assertions.ok(!settled, "Storage.commit() adopted after it settled")` 再记录并转调真正的 `adopt`；提交 reject 时 `settled = true` 且 `deepEqual(adopted, [])`；成功时 `settled = true` 且 `deepEqual(adopted, [seq])`（**恰好一次、且在 resolve 之前**）。
- **`storage-conformance.ts` 的 26 个用例名**：已在 §2.3.6 逐字列出（审计再次核对无遗漏）。
- **`storage-benchmark.ts` 的 `storageBenchmarkPrimaryRecordCount`**：`1 + entryCount + taskCount + documentCount + REPLAY_TAILS.length + 1 + FORK_DEPTH * (1 + ENTRIES_PER_FORK)`（root conversation 1 + 每条 replay 文档 1 + historical 文档 1 + 每个 fork 1 conversation + 32 entries）。
- **`storage-benchmark.ts` 的 `task(id, index)`**：`statuses = ["pending","running","terminal"] as const`，`status = statuses[index % 3]`；公共字段 `{id, conversationId: ROOT_CONVERSATION_ID, kind: index % 4 === 0 ? "benchmark.filtered" : "benchmark.other", version: 1, input: {index}, background: index % 5 === 0, abortRequested: index % 7 === 0}`；`terminal` 用 `state: {status, outcome: {status:"completed", result:{index}}}`，其余用 `state: {status, checkpoint: {index, payload: "x".repeat(64)}}`（**两种 state 互斥，不会同时出现**）。`filteredTaskCount = Math.min(50, Math.ceil(taskCount / 60))`。
- **`storage-benchmark.ts` 的 seed 顺序**：root conversation → entries（每 100 条一次 commit，`index === 0` 带 `head` 并记 `firstEntryId`）→ tasks（每 100 条）→ documents（每 100 条，`exactDocumentId` 被**覆盖为最后一个**，`exactDocumentKey = key-(documentCount-1)`）→ 4 个 replay 文档（一次 commit）→ **1024 次单独 commit**（每次包含所有 `tail >= count` 的 delta，`count` 从 1 到 `REPLAY_TAILS.at(-1) = 1024`）→ historical 文档（128 次 delta → **一次 base 覆盖** → 128 次 delta；`ancientAt` = 第一段最后一次 delta commit 的 Seq，`recentAt` = 第二段最后一次 delta commit 的 Seq）→ 8 层 fork（每层 1 conversation + 32 entries，`parentAt` 取该层最后一个 entry id）。
- **`storage-benchmark.ts` 的 write 基准**：`seedStorageWriteBenchmark` 先 root conversation，再一次 commit 100 条 `benchmark.baseline` entry；三个基准的 `expected` 分别是 1/100/4。
- **`runner.ts` 的两个 `register*`**：`registerStorageConformance` 的 `runner.it(name, run)` **不传 timeout**；`registerEnvConformance` 的 `runner.it(name, run, timeoutMs)` **传**（可能为 `undefined`）；两者的 `options` 只透传 `shell`/`symlinks`/`listFiles`。

### A4 `packages/durable/test-workers` 补充

- **`wake.test.ts`**：见 §2.4.2 已补全（`eventually` 200×10 ms、`runDurableObjectAlarm === true`、`advance` 跨 eviction、`wakes.at(-1)`、`getAlarm() === null`、`hanging.promise`、`submission.wait`、`nativeTimer(...) === undefined`、2 s evict race）。
- **`storage.test.ts`**：`registerStorageConformance` 的 provider 在 `runInDurableObject(fresh(), ...)` 内用 `openDurableObjectSqliteStorage(state.storage, { prefix: \`conformance${suite++}_\` })`；迁移用例用 `createSqliteSchema("legacy_")`、`createSqliteMigrations(schema).slice(0, 1)` 先落到 v1，再 `openDurableObjectSqliteStorage(state.storage, { prefix: "legacy_" })` 触发 v1→v2；5 个索引名断言为 `tasksByAbortRequested`/`tasksByBackground`/`tasksByConversation`/`tasksByKind`/`tasksByStatus`。
- **`cloudflare.d.ts`**：`env` 在 **`cloudflare:workers`**（见 §2.4.4 已修正）；`cloudflare:test` 只有 `runInDurableObject`/`runDurableObjectAlarm`/`evictDurableObject`；`DurableObject` 本体由 `packages/codemode/src/cloudflare-modules.d.ts` 声明（`ctx` 无类型）。
- **`vitest.config.ts`**：`root: source(".")`；`resolve.conditions: ["source"]`；`ssr.resolve.conditions: ["source"]`；三个 alias 分别指向 `../src/index.ts`、`../src/testing/index.ts`、`../src/storage/sqlite/cloudflare.ts`；`test.include: ["*.test.ts"]`。
- **`wrangler.jsonc`**：`name:"kiana-durable-test-workers"`、`main:"./worker.ts"`、`compatibility_date:"2026-10-01"`、`compatibility_flags:["nodejs_compat"]`、`durable_objects.bindings[0] = {name:"SESSIONS", class_name:"TestSession"}`、`migrations[0] = {tag:"v1", new_sqlite_classes:["TestSession"]}`。

### A5 `packages/evals` 补充

- **`report.ts` 的 `readTaskObservation` 顺序**：`skipped`/`pending` 在 `workspace.cases` 检查**之前**返回；`persistSession` 在**模型一致性检查之前**调用（模型不匹配也会写出 `session.jsonl`）。**另**：`loaded` 的 `.catch(() => undefined)` 让**任一**读取失败都变成 errored（不会用半份数据）；`actualModel = run.usage.provider && run.usage.model ? \`${provider}/${model}\` : undefined`——**两者任一为空串都会得到 `undefined`**，随后因 `undefined !== task.model` 判为 errored。
- **`report.ts` 的 `persistSession`**：`caseResult.harness?.run?.artifacts?.[KIANA_SESSION_SNAPSHOT_ARTIFACT]` 非字符串 → 直接返回；目录 `<artifact>/<variant>/sessions/<sha256(JSON.stringify([evalSet,caseId,variant,model,runNumber]))>`，`mkdir 0o700`、文件 `session.jsonl` 0o600。
- **`report.ts` 的 `classifyCaseStatus`**：`failed → errored`；`skipped|todo|disabled → skipped`；`pending → pending`；`passed` 及其他 → `undefined`（`undefined` 时继续走 harness 检查）。
- **`report.ts` 的 `erroredObservation`**：只返回 identity + `outcome:"errored"`（无 metrics）。
- **`report.ts` 的 `formatEvalComparisonReport`**：没有 comparison 时返回 `""`；`report.txt` 由调用方补 `\n`；`Pass rate  withheld because pairs are blocked`（**两个空格**）与 `Pass rate  unavailable` 是精确文案；`pairedMetric` 的 `label.padStart(10)`、`toFixed(1)`；成本行 `(meanDelta >= 0 ? "+" : "-") + "$" + abs(meanDelta).toFixed(4)`。
- **`harness.ts` 的 `applyIsolatedEnvironment`**：先删除**所有** `KIANA_EVAL_*` 变量（记录原值），再设置 `HOME`/`USERPROFILE`/`KIANA_CODING_AGENT_DIR`（若原值未记录则先记录）；restore 时原值 `undefined` 则删除。
- **`harness.ts` 的 `resolveSandboxIdentity`**：两个都未设 → `undefined`；只设一个 → `Set both KIANA_EVAL_SANDBOX_UID and KIANA_EVAL_SANDBOX_GID, or neither.`
- **`harness.ts` 的 `runKianaCodingAgent` 顺序**：`startedAt` → `signal?.throwIfAborted()` → `resolveModelSelection` → `getAgentDir` → `resolveSandboxIdentity` → `mkdtemp` → workspace/home/agentDir → 注册 transform 扩展工厂 → **`applyIsolatedEnvironment`** → 读 `auth.json`/`ModelRuntime.create` → mkdir/seedWorkspace → 取 model/auth → 沙箱身份下删 `auth.json` 并隐藏 `auth.source` 环境变量 → `createAgentSessionServices` → `SessionManager.create` → `setArtifact("runId", ...)` → `createAgentSessionFromServices` → 检查意外扩展 → `enterToolSandbox` → steps。
- **`harness.ts` 的 cleanup 顺序**：读 session 文件（缺失 → `cleanupErrors.push(new Error("Kiana eval produced no session file."))`）→ `session?.dispose()` → `rm(root, {recursive:true, force:true})` → `restoreEnvironment()` → 恢复隐藏的凭据环境变量。错误聚合成 `AggregateError([runError, ...cleanupErrors], "Agent run failed and cleanup also failed.")` 或 `AggregateError(cleanupErrors, "Agent cleanup failed.")`。
- **`harness.ts` 的 `promptAgent`**：函数是 `async function promptAgent(session: AgentSession, input: string, signal: AbortSignal | undefined): Promise<string>`；首行 `signal?.throwIfAborted()`；只扫描 `session.messages.slice(previousMessageCount)`（`previousMessageCount` 在 `prompt` 前记录）；无 assistant → `Agent run completed without an assistant message.`；`stopReason !== "stop" && !== "toolUse"` → `assistant.errorMessage ?? \`Agent run ended with unexpected stop reason: ${stopReason}.\``；输出取 `session.getLastAssistantText()`，`!output && stopReason === "stop"` → `Agent run produced no assistant text.`；否则返回 `output ?? ""`（`toolUse` 时可为空串）。
- **`harness.ts` 的 `createKianaDocumentationEvalHarness`**：`process.env.KIANA_EVAL_CONTAINER !== "1" || !resolveSandboxIdentity()` → `Documentation evals must run in the isolated container sandbox.`。
- **`evals/acme-server.ts` 的 `readJson`**：`for await` 累加 `chunk.toString()`（**无大小上限**）后 `JSON.parse`；调用方 catch → `400 "Invalid JSON"`。
- **`evals/acme-server.ts` 的错误响应**：`content-type: text/plain`、`response.end(message)`（**无换行**）。
- **`evals/custom-provider.docs.eval.ts` / `openai-provider.docs.eval.ts` 的 setup 差异**：stream 版是 `beforeAll(() => { process.env.ACME_STREAM_API_KEY = STREAM_API_KEY; return server.start(); })`、`afterAll(() => { delete process.env.ACME_STREAM_API_KEY; return server.stop(); })`；openai 版的 `beforeAll` **只** `server.start()`（不设任何环境变量，key 只在 `options.env` 里），`afterAll` 只 `server.stop()`。两版都有 `beforeEach(() => server.reset())`。`judgeThreshold: null` 表示由 strict judge 决定，不设通过阈值。
- **`evals/models.docs.eval.ts`**：`existingModelsPreserved: true`；prompt 里要求“不要创建 project-local 配置”。
- **`evals/extensions.docs.eval.ts`**：`run([prompt, {type:"reload"}, prompt])` —— **两次 prompt 中间 reload**；judge 是 `StructuredOutputJudge` + `ToolCallJudge({expectedTools:[{name,arguments}]})`。
- **`docker/entrypoint.ts`**：`sandboxUid`/`sandboxGid` 在模块顶层 `parseId`（值须为 `>= 1` 的安全整数，否则 `` `${name} must be a positive integer.` ``）；`assertRootOnly` 断言 `uid === 0` 且 `(mode & 0o077) === 0`（否则 `` `Evaluator source must be readable only by root: ${path}` ``）；`assertSandboxCannotRead` 按 `statSync(path).isDirectory()` 选 `readdirSync`/`readFileSync`，用 `spawnSync(process.execPath, ["--eval", `require("node:fs").${method}(${JSON.stringify(path)})`], {uid,gid,stdio:"ignore"})`，`status === 0` → `` `The model-facing user can read evaluator source: ${path}` ``；`evalFiles = vitestArgs.filter(a => a.endsWith(".eval.ts"))`（`vitestArgs` 已剔除 `--discover`），空 → `The container runner requires an explicit eval file.`，并对每个 evalFile 再做 `assertRootOnly` + `assertSandboxCannotRead`。
- **`docker/install-runtime.mjs`**：`outputDirectory = process.argv[2]`；`!outputDirectory || process.argv.length !== 3` → `` throw new Error("Usage: node packages/evals/docker/install-runtime.mjs <output-directory>") ``；`produceArtifactSet({build:false, outDir: outputDirectory/artifacts, repoRoot, source: null})`；`evaluatorDependencies` = evals `devDependencies` 中**不属于** artifact 集合的项；`installConsumer({artifactSet, directory: outputDirectory/install, packageNames: [codingAgentName]})`；`npm install --ignore-scripts --omit=dev --no-audit --no-fund --no-save <name@version...>`（cwd 为 install 目录，`stdio: "inherit"`）；对非 coding-agent 的 artifact 包删除 `docs`/`examples`/`src`/`test`/`tests` 目录与 `/^(?:readme|changelog)(?:\..+)?$/i` 文件；随后把 `package.json`+`vitest.base.ts` 复制到 `<out>/root`、把 `packages/evals/{package.json,vitest.evals.config.ts}` 与 `{src,evals}` 目录、`docker/entrypoint.ts` 复制到 `<out>/root/packages/evals/`。

### A6 跨文件/枚举类补充

- **`packages/env/src` 导出计数**：`connection.ts` 9、`errors.ts` 2、`list-files.ts` 3、`remote-env.ts` 3（含 `RemoteInfo` 再导出）、`ssh.ts` 16、`terminal.ts` 4、`watch.ts` 2 = **39 去重**；`index.ts` 再导出 **34**，未再导出的 5 个是 `Json`、`abortResult`、`toFileError`、`listFilesOver`、`remotePath`。
- **`packages/evals` 导出计数**：`plan.ts` 6、`report.ts` 15、`docker.ts` 6、`harness.ts` **12 个去重名字 / 16 行 `export` 声明**（`createKianaCodingAgentHarness` 与 `createKianaDocumentationEvalHarness` 各有 2 个重载 + 1 个实现，各多出 2 行）、`cli.ts` 0；合计 **39 个去重名字**，与 §5.2 及开头的 121 口径一致（无需按声明行重算）。`evals/acme-server.ts` 11、`configured-runtime.ts` 6。
- **`packages/durable/src/testing` 导出计数**：`assertions.ts` 2、`env-conformance.ts` 1、`runner.ts` 3、`storage-benchmark.ts` 11、`storage-conformance.ts` 1、`types.ts` 8 = **26 去重**；`index.ts` 再导出 26。
- **`packages/env/test` 文件**：`conformance.test.ts`、`daemon.ts`、`differential.test.ts`、`old-daemon.ts`、`remote.test.ts`、`ssh.test.ts`、`ssh-external.test.ts`、`terminal.test.ts`（**无 vitest setup 文件**）。
- **`packages/evals/test` 文件**：`acme-server.test.ts`、`comparison.test.ts`、`configured-runtime.test.ts`、`harness.test.ts`、`plan.test.ts`、`report.test.ts`。
- **`packages/evals/evals` 文件**：`acme-server.ts`、`configured-runtime.ts`、`smoke.eval.ts`、`custom-provider.docs.eval.ts`、`documentation-audit.eval.ts`、`extensions.docs.eval.ts`、`models.docs.eval.ts`、`openai-provider.docs.eval.ts`、`tui.docs.eval.ts`。
- **环境变量补充**：`ACME_STREAM_API_KEY`（custom-provider eval 的 `beforeAll` 设置并被 prompt 引用）、`ACME_API_KEY`（仅作为 `options.env` 的键，由测试写入子进程环境，**不是宿主环境变量**）、`KIANA_OFFLINE`（`tui.docs.eval.ts` 临时设为 `1`）。
- **daemon 环境变量读取点**：`sys/unix.rs::tmpdir`（`TMPDIR`/`TMP`/`TEMP`/`PREFIX`）、`home`（`HOME`）、`pty.rs`（`SHELL`）、`sys/windows.rs`（`ProgramFiles`/`ProgramFiles(x86)`/`SystemRoot`/`PATHEXT`/`PATH`/`USERPROFILE` 与所有环境变量的 UTF-16 排序）；`main.rs` 不读环境变量（除了 `env!("KIANA_ENV_VERSION")` 编译期常量）。
- **`packages/env` 包元数据**：`@earendil-works/kiana-env` version `1.4.0`；`docs/protocol.md` 151 行、`docs/semantics.md` 68 行、`README.md` 66 行、`CHANGELOG.md` 26 行。`daemon/Cargo.lock` 已签入（55 个 `[[package]]`，`version = 4`，660 行）；`rust-toolchain.toml` 为 `channel = "1.96.0"`、`components = ["rustfmt", "clippy"]`。
- **`packages/evals` 包元数据**：`@earendil-works/kiana-evals` 1.4.0、`private: true`；scripts 逐字（`clean`/`eval`/`eval:host`/`eval:docs`/`test`）；`vitest.evals.config.ts` 的 `docs`/`host` 两个 project 都 `sequence.concurrent: false`、`testTimeout`/`hookTimeout` 300_000。

## 附录 A：`packages/env/docs/protocol.md` 的调度条款（逐字要点）

- 16 个 worker 线程处理文件操作；`exec`/`watch`/`pty` 在独立 16 MiB 栈线程。
- `ptyWrite`/`ptyResize`/`ptyForeground` 在读线程内联处理，保证与到达顺序一致。
- 同一 handle 的 `writeChunk` 串行执行。
- control 队列优先于 bulk；payload > 64 KiB 的响应进入 bulk；未发送 bulk 超过 4 MiB 时暂停生产。
- 5 s ping；30 s 无输入则 daemon 杀死所有子进程并以 0 退出。

## 附录 B：`packages/env/docs/semantics.md` 的“不复用标准库”清单（逐字要点）

- recursive mkdir：已存在目录视为成功。
- rm：非 recursive 删目录 → `ERR_FS_EISDIR` + `Path is a directory: rm returned EISDIR (is a directory) {path}`。
- mkdtemp：POSIX 用 `path.join(tmpdir, prefix ?? "tmp-")` 后交给 `mkdtemp`；Windows 用随机 6 位。
- tmpdir：`TMPDIR` → `TMP` → `TEMP`；Android 默认 `$PREFIX/tmp`。
- listDir：POSIX 按原始字节序排序。
- `openBinaryReader`：`noFollow` 用 `O_NOFOLLOW`。
- `read*`：用 `open("r")`，因此目录报 `EISDIR`、FIFO 报 `NOT_REGULAR`。
- `listFiles` 与 `readdir` 的差异：前者走 gitignore/ignore 规则并输出扁平列表，后者只列一层。
- `readBinaryFile` 返回 `Buffer`；`BinaryReader::read` 返回 `Uint8Array`。
- Windows：错误码表、`quote_argument`、`search_path`、Job Object、`taskkill`、`drive_cwds` 等全部按 libuv。
