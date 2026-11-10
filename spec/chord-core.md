# chord-core 移植规格（TypeScript → Rust）

本文件是 `@earendil-works/chord` 包核心单元 `chord-core` 的逐项移植规格。目标：用 Rust 重写后**功能与用户交互完全一致**——同样的导出 API（对应语义）、同样的错误码与错误消息、同样的线上协议帧、同样的进程锁格式、同样的默认值与超时、同样的可见行为顺序（发布队列、通知顺序、重载各阶段）。

约束：

- TypeScript 源码树 `/home/ctyun/code/kiana_rev/kiana` 是**只读参考**，不得修改。
- Rust 工作区：`/home/ctyun/code/kiana_rev/kiana`。
- 本单元源文件范围：`packages/chord/src/{contract,object,connection,host,reload,remote,manager}.ts`，外加 `docs/**`、`README.md`、相关测试。同包内的 `wire.ts`、`link.ts`、`schema.ts`、`json.ts`、`publish.ts`、`registry.ts`、`cell.ts`、`cluster.ts`、`loader.ts`、`websocket.ts`、`context/`、`delta/` 属于其它单元（见 §5 跨单元接口），但其行为在本单元接口面中被引用；规格中给出签名与关键语义，便于联调。

阅读方式：实现者应同时读 TS 源码；本文档的作用是 (a) 完整核对清单（checklist），(b) Rust 设计决策。

---

## 1. 概述

### 1.1 目的

Chord 是一个"类型化宿主运行时"：应用由**对象（object）**组成，对象分布在多个进程（server / worker / 浏览器）中，可**热重载**而不中断。核心概念：

| 术语 | 含义 |
| --- | --- |
| facet | 一个插件在某个宿主上的代码；通过 `defineFacet` 声明它 serve 哪些对象类型 |
| object type | 名字 + 契约。`remote`（可跨宿主寻址，含 input、variant 的 state members、method args/result 的 schema）或 `local`（仅本宿主，成员可以是函数与原生对象，但必须在契约里列出：`method`、`opaque` 或 state schema） |
| factory | `(input, principal, self) => implementation`，**必须同步** |
| object | 类型的一个实例；身份 = `(type, canonicalInput, principalKey)`；拥有 cells、resources、references、listeners、tasks、registrations；跨 reload 存活 |
| implementation | 工厂一次运行返回的东西（方法 + state 成员；remote 类型还有 `variant`）；按 code version 不可变 |
| reference | `get` 返回的晚绑定"声明"，不是对象本身；对象在有人引用时存活 |
| principal | 应用自定义身份；链路上被盖章；宿主内代码以 host principal 行事 |
| root | 一次短的调用树；`{ rootId, codeVersion }` |
| code version | 一次提交的实现/注册集合（server 发放 id；宿主最多保留 current + previous） |
| code holder | 长期执行体（task 循环、浏览器），它们发起的每次调用都开新 root |

### 1.2 包与入口

- npm 包：`@earendil-works/chord` v1.4.0，`"type": "module"`，Node `>=22.19.0`。
- 依赖：`typebox@1.3.27`（schema）、`@earendil-works/kiana-telemetry@1.4.0`（仅 `Context.telemetry`；boundary 测试强制它是唯一的 Kiana 依赖）。devDeps：`ws@8.21.0`、`vitest@4.1.12`、`@types/ws@8.18.2`、`shx@0.3.4`。
- `exports` 映射（Rust 需对应 crate feature / module）：`.`, `./context`, `./output`, `./delta`, `./node`, `./telemetry-sqlite`, `./sqlite`, `./sqlite/node`, `./sqlite/cloudflare`, `./package.json`。
- 本单元涉及两个入口：
  - `src/index.ts`（浏览器安全）：契约（`local`, `remote`, `method`, `opaque`, `hostNamed`, `hostFamily`, `bind`, `coverage` 及契约类型）、`ChordError`/`isChordError`/`forbidden`/`invalid`/`ChordErrorCode`（**`CHORD_ERROR_CODES` 数组本身不在 index.ts 再导出**，只在 `errors.ts`/`wire.ts` 内部使用）、host（`createHost`, `defineFacet`, `Facet`, `FacetApi`, `FacetDefinition`, `FacetLoader`, `Host`, `HostOptions`, `LoadOptions`, `PluginRef`, `RootOptions`, `ConnectOptions`, `PrincipalMatch`, `PrincipalPattern`, `Route`）、`defineRegistry`/`Registry`/`RegistryTable`/`RegistryView`、`ReloadOptions`/`ReloadReport`、link 的 `memoryLinkPair`、`ipcLink`、`webSocketLink`、`PACK_THRESHOLD`、`inflateMessage`、`acceptWebSockets`、`PROTOCOL_VERSION`/`Frame`、`copyJson`/`isJsonValue` 等、`replicatedState` 与其状态类型、watch 类型（`Watch`/`NativeWatch`/`Watchable`/`NativeWatchable`/`WatchEnd`/`WatchHandle`/`NativeWatchHandle`）、`Draft`。**不在 index.ts**：`CHORD_ERROR_CODES`、`MemberDescription`、`MemoryLink`/`IpcLink`/`WebSocketLink` 类、`isFrame`/`toWireError`/`fromWireError`。
  - `src/node.ts`（Node only）：`nodeLoader`、`createHostManager`、`forkDriver`、`childControl`、`processLock`，以及 `ControlMessage`、`Driver`、`Exit`、`ForkDriverOptions`、`HostManager`、`HostManagerOptions`、`Instance`、`Liveness`。
- **Chord 是库，没有 CLI 参数、斜杠命令、按键绑定、TUI 文本、ANSI 渲染**。本单元"用户可见面"= 导出 API 的行为与错误、错误码/错误消息文本、日志前缀、线上帧、环境变量 `CHORD2_LOCK`、锁 socket/pipe 地址、进程管理状态枚举（见 §3）。

### 1.3 集成位置与使用方式

```
app 代码 ── serve / facet / reload ──► HostImpl（host.ts）
                                     ├── ObjectImpl（object.ts）: cells(cell.ts)、resources、refs、listeners、tasks
                                     ├── Connection（connection.ts）: 每链路一个；消费远程引用（RemoteBinding/RemoteRef, remote.ts）
                                     │                              provider 端 serve hold；relay
                                     ├── Reload（reload.ts）+ Cluster（cluster.ts）
                                     └── Link（link.ts: memory / IPC / WebSocket）
```

- `createHost({ self, name?, ... })` 创建 `HostImpl`；`host.serve(type, factory, options)` 或 `host.install(facet)` / `host.load(plugins)` 注册。
- `host.get` / `host.hold` / `ctx.get` / factory 里的 `self.get` 获取引用。
- `host.root(fn, {principal?, signal?, codeVersion?})` 开一个 root。
- `host.accept(link, { principal, kind?, name?, ... })` 接受子链路；`host.connect(link, { principal })` 连接父链路；`host.route(prefix, route)` 配置 server 端到 worker 的路由。
- `host.reload(facets, options)` 热重载；当存在子 host 或指定 `remote` 时走集群重载。
- Node 端 `createHostManager({ driver })` 管理子进程；子进程入口用 `childControl()` 握手。

---

## 2. 逐文件 API 与行为

### 2.1 `src/errors.ts`（被本单元广泛使用；delta/cell/connection 均依赖）

```ts
export const CHORD_ERROR_CODES = ["aborted","contract_mismatch","forbidden","gone","invalid","not_in_variant","not_ready","released","superseded","unavailable","upgrade_conflict","upgrade_required"] as const;
export type ChordErrorCode = (typeof CHORD_ERROR_CODES)[number];
export type ChordErrorReason = string;
const RETRYABLE = new Set(["superseded","upgrade_conflict"]);
export class ChordError extends Error {
  readonly code: ChordErrorCode; readonly reason: string|undefined; readonly retryable: boolean; readonly details: unknown;
  constructor(code, message, options?: { cause?: unknown; details?: unknown; reason?: string });
}
export const isChordError = (error: unknown, code?: ChordErrorCode): error is ChordError;
export const forbidden = (message = "forbidden"): ChordError;
export const invalid = (reason, message, details?): ChordError;             // code "invalid"
export const asChordError = (error: unknown, context: string): ChordError;  // 非 ChordError → unavailable "<context>: <message>", cause 保留
export async function runAll(steps: readonly (() => unknown)[], message: string): Promise<void>;
```

行为要点（逐字）：

- `ChordError.message`（即 `Error.message`）= `` `${code}${reason ? `/${reason}` : ""}: ${message}` ``；`name = "ChordError"`；`retryable = RETRYABLE.has(code)`；`details` = options.details。**注意** 构造时传入的 `message` 参数本身不含前缀；前缀是 message 的一部分（串上后其它代码会再次剥前缀，见 `fromWireError`）。
- `isChordError(e, code?)`：`instanceof ChordError && (code === undefined || e.code === code)`。Rust 用 `enum ChordError { code, reason, message, details, cause }` + `is(code)`。
- `forbidden()` 默认消息 `"forbidden"`，最终 message 为 `"forbidden: forbidden"`。
- `runAll`：顺序 await 每个 step（step 可以返回 promise），全部跑完后：0 个失败 → 正常返回；恰好 1 个 → 抛出该错误（原样）；>1 → `AggregateError(errors, message)`（Rust：自定义 `Aggregate` 错误包装多条，Display 与 Node 的 `AggregateError` 近似；测试只断言单错路径与聚合存在）。

### 2.2 `src/contract.ts`（440 行）

契约是运行时值。导出的运行时常量与函数：

#### 2.2.1 `method`

```ts
export function method(): MethodSpec<undefined, undefined>;
export function method<A extends TSchema>(args: A): MethodSpec<A, undefined>;
export function method<A extends TSchema | undefined, R extends TSchema>(args: A, result: R): MethodSpec<A, R>;
```

实现：`{ [MEMBER]: "method", args, result }`，`MEMBER = Symbol("chord.member")`。语义：`method()` 无参数无返回值；`method(args)` 有参数、无返回值（void）；`method(args, result)` 有参数有返回值；`method(undefined, result)` 无参数有返回值。`args === undefined` 与 `result === undefined` 在描述里表现为**字段缺省**，线上帧同样缺省（call 帧无 `args`，result 帧无 `value`）。

#### 2.2.2 `opaque`

```ts
export const opaque = <T = unknown>(id: string): OpaqueSpec<T> => {
  if (!/^[^@\s]+@\d+$/.test(id)) throw new TypeError(`opaque id must be name@version: ${id}`);
  return { [MEMBER]: "opaque", id };
};
```

正则：`^[^@\s]+@\d+$`（JS 正则，`\d` 为 ASCII 数字，`\s` 为 JS 空白集；Rust 用 `^[^@\s]+@[0-9]+$`，注意 `\w` 不涉及）。

#### 2.2.3 `hostNamed` / `hostFamily`

```ts
export function hostNamed(name: string): HostNamed;      // 无效: TypeError("hostNamed needs a host name")   (name 非 string 或 "")
export function hostFamily<const K extends string>(prefix: string, key: K): HostFamily<K>;
// TypeError("hostFamily needs a host-name prefix")  /  TypeError("hostFamily needs an input field name")
export type Placement = HostNamed | HostFamily;          // 纯数据，进入描述（与 fingerprint）
```

#### 2.2.4 `resolveHost` / `placedOn` / `placementText`

```ts
export function resolveHost(typeName: string, placement: Placement, input: unknown): string;
export const placedOn = (placement: Placement, hostName: string): boolean;
export const placementText = (placement: Placement): string;
```

- `resolveHost`：`"name" in placement` → `placement.name`；否则读 `input[placement.key]`（仅当 `input !== null && typeof input === "object"`），若不是非空 string → `invalid("placement", `${typeName}: input field ${placement.key} names no host`)`（code invalid，reason placement，message 为后者）；否则 `prefix + value`。
- `placedOn`：named → `placement.name === hostName`；family → `hostName.startsWith(prefix) && hostName.length > prefix.length`（**必须比 prefix 长**，即 `session:` 不能 serve 在 `session:` 本身）。
- `placementText`：named → `name`；family → `` `${prefix}<${key}>` ``。

#### 2.2.5 常量与 schema

```ts
export const LOCAL_VARIANT = "";                                   // local 契约的唯一 variant 名（空串）
export const RESERVED_MEMBERS: ReadonlySet<string> = new Set(["variant","status","ready","shape","error","onShape","onStatus","release","then","contract"]);
export const DescriptionSchema = Type.Object({
  name: Type.String({ minLength: 1 }),
  scope: Type.Union([Type.Literal("remote"), Type.Literal("local")]),
  version: Type.Integer(),
  input: SchemaObject,        // Type.Unsafe<TSchema>(Type.Object({}))：任意对象
  host: Type.Optional(PlacementSchema),
  variants: Type.Record(Type.String(), Type.Record(Type.String(), MemberDescriptionSchema)),
});
export type MemberDescription =
  | { kind: "method"; args?: TSchema; result?: TSchema }
  | { kind: "state"; schema: TSchema }
  | { kind: "opaque"; id: string };
```

`PlacementSchema` = union of `{name: string(minLength 1)}`（`additionalProperties:false`）和 `{prefix: string(minLength 1), key: string(minLength 1)}`（`additionalProperties:false`）。`MemberDescriptionSchema` 三个分支（method/state/opaque）。

`describeMember(name, member, scope)`：
- `RESERVED_MEMBERS.has(name)` → `TypeError("member name is reserved: " + name)`。
- brand `"method"` → `{kind:"method", ...(args===undefined?{}:{args}), ...(result===undefined?{}:{result})}`。
- brand `"opaque"`：若 scope === "remote" → `TypeError("remote contracts cannot expose opaque members: " + name)`；否则 `{kind:"opaque", id}`。
- 其它情况（TSchema）→ `{kind:"state", schema: member}`（**不检查它是否真是 schema**）。

`finish`：`fingerprint = hash64(canonicalJson(description))`（见 §5 schema 单元；JS 字符串 UTF-16 code unit 上做两路 FNV-1a）。

`checkHeader(name, version)`：`!name` → `TypeError("contract name is empty")`；`!Number.isInteger(version) || version < 1` → `` TypeError(`contract version must be a positive integer`) ``（注意没有 name 插值）。

#### 2.2.6 `remote(name, def)`

```ts
export function remote<const D extends RemoteDef>(name: string, def: D & { readonly host: PlacementFor<Static<D["input"]>> }): RemoteContract<D>;
```

校验顺序与消息：

1. `checkHeader(name, def.version)`。
2. `placement = def.host`；若 `undefined` 或 `!Value.Check(PlacementSchema, placement)` → `` TypeError(`remote contract ${name} needs \`host\`: hostNamed(…) or hostFamily(…)`) ``。
3. 复制 placement（JSON 副本）：`"name" in placement ? {name} : {prefix, key}`（多余字段被丢弃）。
4. `Object.keys(def.variants).length === 0` → `` TypeError(`remote contract ${name} has no variants`) ``。
5. 对每个 variant（按 `Object.entries` 顺序）：先 state 成员（`variant.state ?? {}`，顺序同插入顺序），再 methods（`variant.methods ?? {}`）：
   - method 名与 state 名冲突（`key in members`）→ `` TypeError(`member ${key} is both state and method in ${name}.${kind}`) ``。
   - `members[key] = describeMember(key, spec, "remote")`；若 `members[key].kind !== "method"` → `` TypeError(`methods.${key} is not a method()`) ``（即 methods 里放了非 method 值）。
6. `description = { name, scope:"remote", version, input: def.input, host, variants }`；返回 `{ scope:"remote", name, version, input, def, description, fingerprint }`。

注意 variant 的**键顺序**保留 JS 对象插入顺序（字符串键按首次插入，数字样式的键会提前——见 §5 的 JS 键序差异）；state 成员先于 method 成员。

#### 2.2.7 `local(name, def)`

同 header 检查。`members` 按 `def.members ?? {}` 顺序 describeMember(scope "local")（允许 opaque）。description = `{name, scope:"local", version, input, variants: { [LOCAL_VARIANT]: members }}`（即 `{"": {...}}`）。无 `host` 字段。

#### 2.2.8 `contractFromDescription(description)`

```ts
const { name, scope, version, input, host } = description;
if (typeof name !== "string" || (scope !== "remote" && scope !== "local") || !Number.isInteger(version) || (scope === "remote") !== (host !== undefined))
  throw new ChordError("invalid", "malformed contract token", { reason: "token" });
const base = { name, version, input, description, fingerprint: hash64(canonicalJson(description)) };
return scope === "remote" ? { ...base, scope, def: { version, input, host, variants: {} } } : { ...base, scope, def: { version, input } };
```

要点：remote 必须有 host，local 必须没有；`def.variants` 为空（description 才是真源）；fingerprint 重新算（对收到的 description 做 canonicalJson）。

#### 2.2.9 `coverage(provider, consumer)`（"provider 是否覆盖 consumer"）

返回 `undefined`（覆盖）或原因字符串（**逐字**）：

```
provider.name !== consumer.name || provider.scope !== consumer.scope → "different type"
!samePlacement(provider, consumer)                               → "host"
!fits(consumer.input, provider.input)                            → "input"
对 consumer.variants 每个 [kind, consumerMembers]（JS 键序）:
  provider.variants[kind] === undefined                          → `variant ${kind} missing`
  对每个 [name, wanted]:
    offered === undefined || offered.kind !== wanted.kind        → `${kind}.${name} missing or of another kind`
    method vs method:
      (wanted.args===undefined) !== (offered.args===undefined)   → `${kind}.${name} arguments`
      wanted.args!==undefined && offered.args!==undefined && !fits(wanted.args, offered.args) → `${kind}.${name} arguments`
      wanted.result!==undefined && (offered.result===undefined || !fits(offered.result, wanted.result)) → `${kind}.${name} result`
    state vs state: !fits(offered.schema, wanted.schema)          → `${kind}.${name} state`
    opaque vs opaque 且 offered.id !== wanted.id                  → `${kind}.${name} opaque ${offered.id} != ${wanted.id}`
```

- `samePlacement(a,b)`：`canonicalJson(a.host ?? null) === canonicalJson(b.host ?? null)`。
- 语义：方法参数**逆变**（provider 要接受 consumer 能发的一切 → `fits(consumer.args, provider.args)`），结果**协变**（`fits(provider.result, consumer.result)`）；consumer 不要求结果（`result===undefined`）则接受任何 provider 结果；state 值 `fits(provider.schema, consumer.schema)`。
- 额外：consumer 的 members 必须是 provider 的子集（多出来的 provider 成员被隐藏）。
- 注意 `coverage` 不看 `version`（调用方先比 version）。

#### 2.2.10 `bind(provider, accepts, consumer)` → `Adapter | undefined`

```ts
if (provider.fingerprint === consumer.fingerprint) return undefined;
if (!samePlacement(provider.description, consumer.description))
  throw new ChordError("contract_mismatch", `${provider.name} lives on ${placementOf(provider.description)}, the consumer expects ${placementOf(consumer.description)}`);
if (provider.version === consumer.version) {
  const reason = coverage(provider.description, consumer.description);
  if (reason === undefined) return undefined;
  throw new ChordError("contract_mismatch", `${provider.name}@${provider.version} does not cover the consumer's description (${reason}); forgotten version bump?`);
}
const adapter = accepts?.[consumer.version];
if (adapter === undefined)
  throw new ChordError("contract_mismatch", `${provider.name}: provider version ${provider.version}, consumer version ${consumer.version}, no accepts adapter`);
return adapter;
```

- `placementOf`: `description.host === undefined ? "(local)" : placementText(description.host)`。
- `Adapter = (current: Record<string, unknown>) => Record<string, unknown>`；`Accepts = Readonly<Record<number, Adapter>>`（键是 consumer 的 contract version，JS 数字键）。
- 注意：**移动了 host 的类型对消费者而言是另一个类型**——即使有 accepts 也不桥接。
- 相同 version 但 fingerprint 不同 → 用 coverage 判定是否 "forgotten bump"。

#### 2.2.11 `takesNoArgs(contract, name)`

遍历 `contract.description.variants` 的所有成员集合（JS 值顺序），第一个 `kind==="method"` 的成员决定：返回 `member.args === undefined`。找不到返回 false（即按"有参数"处理——调用形式 `ref.name(args, ctx?)`）。

#### 2.2.12 静态 TS 类型（Rust 不直接需要，但影响 API 形态与运行时工具函数）

`MethodSpec<A,R>`, `OpaqueSpec<T>`, `VariantDef`, `HostNamed`, `HostFamily<K>`, `Placement`, `PlacementFor<I>`, `RemoteDef`, `LocalMember`, `LocalDef`, `RemoteContract<D>`, `LocalContract<D>`, `AnyContract`, `Description`, `MemberDescription`, `Adapter`, `Accepts`, `MethodImplementation<M,P>`, `Call<M>`, `ImplementationOf<C,P>`, `Ref<C>`, `InputOf<C>`。Rust 侧对应泛型 trait 设计见 §8。

### 2.3 `src/object.ts`（1932 行）

对象运行时：服务注册、资源、声明集合、Ctx、引用（本地代理）、对象激活/运行时/调用/生命周期/释放。

#### 2.3.1 服务与 lifetime

```ts
export type ServeOptions = ReferencedServeOptions | HostServeOptions;
export interface ReferencedServeOptions { lifetime?: "referenced"; linger?: number; accepts?: Accepts }
export interface HostServeOptions { lifetime: "host"; linger?: undefined; accepts?: Accepts }
export type ServeOptionsFor<C> = ...;       // 仅类型
export type Lifetime = "referenced" | "host";
export type Factory<C, P = unknown> = (input: InputOf<C>, principal: P, self: Self<P>) => ImplementationOf<C, P>;
export interface Service { readonly contract: AnyContract; readonly factory: AnyFactory; readonly options: ServeOptions; readonly facet: string }
export const makeService = (contract, factory, options, facet): Service;
export const hostLifetime = (service): boolean => service.options.lifetime === "host";
```

`checkLifetime(contract, options)`（serve 时）：

1. `lifetime = options.lifetime ?? "referenced"`；不是 `"referenced"`/`"host"` → `` TypeError(`${contract.name}: unknown lifetime ${String(lifetime)}`) ``。
2. 非 host → 返回。
3. `contract.scope !== "local"` → `` TypeError(`${contract.name}: lifetime "host" needs a local type (remote types are referenced)`) ``。
4. `options.linger !== undefined` → `` TypeError(`${contract.name}: an object of lifetime "host" does not linger (it lives as long as the host)`) ``。
5. `normalize(contract.input, {}, `${contract.name} input`)` 抛错 → `` TypeError(`${contract.name}: lifetime "host" needs an input that accepts {} (the object's one address)`) ``。

#### 2.3.2 `StateOptions` / `StateInit` / `Self`

```ts
export interface StateOptions<T> { version?: number; migrate?: (old: unknown, from: number) => T }
export interface StateInit<T, P = unknown> { init: (ctx: Ctx<P>) => T | Promise<T> }

export interface Self<P = unknown> {
  state<S extends TSchema>(name: string, schema: S, initial: Static<S> | StateInit<Static<S>, P>, options?: StateOptions<Static<S>>): Cell<Static<S>>;
  get<C extends AnyContract>(type: C, input: InputOf<C>, options?: GetOptions<P>): Ref<C>;
  resource<T>(name: string, declaration: ResourceDeclaration<T>): Resource<T>;
  on<S>(name: string, source: S, handler: (event: EventOf<S>, ctx: Ctx<P>) => void | Promise<void>): void;
  on<S, E>(name: string, source: S, select: (value: ValueOf<S>) => Subscribable<E>, handler: (event: E, ctx: Ctx<P>) => void | Promise<void>): void;
  task(fn: (ctx: Ctx<P>) => Promise<void>): void;
  derive<const S extends Sources, T>(sources: S, fn: (...values: SourceValues<S>) => T): Readable<T>;
  register<V>(registry: Registry<V>, entries: Readonly<Record<string, V>>): void;
  expose<T>(resource: Resource<T>): Exposed<T>;
}
```

#### 2.3.3 资源

```ts
class ExposedImpl { readonly resource: ResourceImpl }                 // self.expose 的运行时形态

export class ResourceImpl<T = unknown> implements Resource<T> {
  readonly key: string; spec: ResourceDeclaration<T>; readonly deps: readonly (RefCore | ResourceImpl)[];
  state: "closed" | "opening" | "open" | "unavailable" = "closed";
  constructor(key, spec, deps);
  get value(): T; get exclusive(): boolean;
  async open(signal: AbortSignal, order: ResourceImpl[]): Promise<void>;
  async close(order: ResourceImpl[]): Promise<void>;
  markUnavailable(): void;
  adopt(spec: ResourceDeclaration<T>): void;
}

class ResourceFacade<T> implements Resource<T> { get key; get value }  // host.inFactory > 0 时 value 抛 not_ready(`resource ${key} (inside a factory)`)
```

`ResourceImpl.value`：`state === "unavailable"` → `ChordError("unavailable", `resource ${key} could not be restored`)`；`state !== "open"` → `notReady(`resource ${key}`)`（即 `not_ready: resource ${key} is not ready`）；否则 `#value`。

`open(signal, order)`：
- `state !== "closed"` → 直接返回（幂等；`opening`/`open`/`unavailable` 都不动）。
- `state = "opening"`；`value = await spec.open({ signal })`；抛错 → `state="closed"`，抛原错。
- `signal.aborted` → `state="closed"`；`await spec.close?.(value)`；抛 `ChordError("aborted", `resource ${key} opened after abort`)`（**message 里的 key 是资源名**，Rust 注意这是 code aborted、无 reason）。
- 否则 `#value=value; state="open"; order.push(this)`（openOrder 记录打开顺序）。

`close(order)`：`state !== "open"` → 返回；`state="closed"`；从 order 中 splice 掉；取 `#value`，置空；`await spec.close?.(value)`（错误上抛）。

`markUnavailable`：`state="unavailable"; #value=undefined`。

`adopt(spec)`：仅替换 `this.spec`（reload 保留的资源在 commit 时换用新声明的 close；旧声明（及它闭包住的旧代码）不再被引用）。

`resourceOf(value)`：`value instanceof ResourceImpl` 或 facade（WeakMap `facades`）。`facade` 在 `self.resource` 返回时注册。

#### 2.3.4 声明集合 `Decls` 与 `Run` / `Installed` / `HostCore`

```ts
export class Decls {
  readonly cells = new Map<string, Cell<unknown>>();
  readonly cellSchemas = new Map<string, TSchema>();
  readonly cellVersions = new Map<string, number>();
  readonly cellMigrations = new Map<string, (old: unknown, from: number) => unknown>();
  readonly refs = new Map<string, RefCore>();                  // 键 = address
  readonly resources = new Map<string, ResourceImpl>();
  readonly adopted = new Map<ResourceImpl, ResourceDeclaration<unknown>>();
  readonly listeners = new Map<string, ListenerDecl>();
  readonly tasks = new Map<string, TaskFn>();                  // 键 "#0","#1",…
  readonly derives = new Map<string, Derived<unknown>>();      // 键 "#0","#1",…
  readonly registrations = new Map<string, Registration>();    // 键 `${registry.name}\u0000${key}`
}

export interface Run {
  readonly implementation: Record<string, unknown>;
  readonly decls: Decls;
  readonly created: { refs: RefCore[]; resources: ResourceImpl[]; derives: Derived<unknown>[]; inits: (readonly [Cell<unknown>, InitFn])[] };
}
export interface Installed { codeVersion: number; service: Service; implementation: Record<string, unknown>; decls: Decls }

export interface HostCore {
  name: string; dev: boolean; principal: unknown; inFactory: number; failed: unknown;
  report(error: unknown): void;
  currentCodeVersion(): number; hasCodeVersion(codeVersion: number): boolean;
  retainedCodeVersions(): Iterable<readonly [number, ReadonlyMap<string, Service>]>;
  pendingCodeVersion(): number | undefined;
  service(typeName: string): Service | undefined;
  acquire(type, input, options, owner, from?, candidate?): RefCore;
  addressOf(type, input, principal, candidate?): string;
  root<T>(fn, options?: { codeVersion?: number; signal?: AbortSignal }): Promise<T>;
  touch(rootId, object): void; forget(object): void; disposed(object): void;
  taskStopDeadline: number; readyTimeout: number;
  checkRegistrations(object, decls): void; registriesChanged(including?): void;
}
```

`isStateInit(value)`：`value !== null && typeof === "object" && typeof value.init === "function"`（JSON 值不会有函数，因此带函数 `init` 的对象就是初始化器）。

#### 2.3.5 `CtxImpl`

```ts
export class CtxImpl implements Ctx {
  host: HostCore; signal: AbortSignal; rootId: string; codeVersion: number; principal: unknown;
  readonly holder: boolean;                       // code holder(task)：每次调用都开新 root
  get closed(): boolean;
  check(host: HostCore | RemoteHost): void;
  constructor(host, { signals: readonly AbortSignal[]; rootId; codeVersion; principal; holder? });
  async get<C>(type, input, options?): Promise<Ref<C>>;
  child(object: ObjectImpl): CtxImpl;
  close(): void;
}
```

- `check(host)`：
  - `this.host !== host` → `invalid("foreign_ctx", "ctx belongs to another host")`。
  - `#closed` → `ChordError("aborted", "ctx expired: its call already settled", { reason: "expired" })`。
  - `signal.aborted`：若 `signal.reason instanceof ChordError && reason.code === "superseded"` → `ChordError("superseded", reason.message, { cause: reason })`；否则 `ChordError("aborted", "ctx aborted", { cause: signal.reason })`。
- 构造：`linkedSignal(fields.signals)` 生成 signal；记录 release。
- `get(type, input, options)`：`check(this.host)`；`ref = host.acquire(type, input, { principal: this.principal, ...options }, "call")`（`options.principal` 覆盖 ctx principal——注意展开顺序：`{principal: ctx.principal, ...options}`）；`#refs.push(ref)`；`await awaitReady(ref, [this.signal, #closing.signal], host.readyTimeout)`；返回 proxy。**失败时只释放该引用**（awaitReady 内部 release）。
- `child(object)`：`new CtxImpl(host, { signals: [this.signal, object.callAbort.signal], rootId: this.rootId, codeVersion: this.codeVersion, principal: host.principal })`（对象内代码以 host principal 行事；不含 holder）。
- `close()`：`#closed = true`；`#closing.abort(ChordError("aborted","ctx expired: its call already settled",{reason:"expired"}))`；`#release()`（解除对上游 signal 的监听）；释放所有 `#refs`（`ref.release()`，清空数组）。

#### 2.3.6 `linkedSignal` / `awaitReady` / `raceSignal`

```ts
export function linkedSignal(sources: readonly AbortSignal[]): { signal: AbortSignal; release(): void };
export async function awaitReady(ref: RefCore, signals: readonly AbortSignal[], ms: number): Promise<void>;
export async function raceSignal<T>(promise: Promise<T>, signal: AbortSignal): Promise<T>;
```

- `linkedSignal`：自建 AbortController；按顺序遇第一个已 aborted 的 source 即以 `source.reason` abort 并停止；否则为每个 source 添加一次性 `abort` 监听（转发 reason 后 release）。`release()` 移除全部监听。**不用 `AbortSignal.any`**：Node 会让长寿命 source 的 dependant 在仍有监听时保持可达，导致每次调用/任务的 ctx 把其闭包活到 source 结束。
- `awaitReady`：若 `ms` 有限则 setTimeout(ms) → 内部 AbortController abort 一个 `ChordError("unavailable", `${ref.address} was not ready after ${ms} ms`, { reason: "ready_timeout" })`，timer `.unref()`（不阻止进程退出）。`linked = linkedSignal([...signals, timeout.signal])`；`await raceSignal(ref.ready, linked.signal)`；catch 时 **`ref.release()`**，若 `linked.signal.reason` 是 reason 为 `ready_timeout` 的 ChordError 则抛它，否则抛原错；finally `clearTimeout(timer); linked.release()`。
- `raceSignal(promise, signal)`：已 aborted → 抛 `ChordError("aborted","call aborted",{cause: signal.reason})`；否则 `Promise.race([promise, aborted])`，aborted 也在 abort 时以同消息拒绝；finally 移除监听。**注意**：`raceSignal` 从不取消原 promise（只是不再等）。

#### 2.3.7 引用基础

```ts
export type RefOwner = "object" | "call" | "host" | "external" | "connection" | "client";
export interface RefCore { address; contract; status; ready; proxy; object: ObjectImpl|undefined; release(): void }
export const REF_BASE: ReadonlySet<PropertyKey> = new Set(["contract","status","ready","variant","shape","error","onShape","onStatus","release"]);
export type StatusListener = (status: RefStatus, error: unknown) => void;
export function notifyStatus(listeners, status, error, report): void;  // enqueue 一批通知；listener 抛错 → report
export const refsByProxy = new WeakMap<object, RefCore>();
export const refOf = (value: unknown): RefCore | undefined;
```

#### 2.3.8 `MemberReadable`（本地引用的 state 成员）

```ts
class MemberReadable implements Readable<unknown> {
  get value(): unknown;
  get [HYDRATION](): Hydration | undefined;
  subscribe(listener: (value: unknown) => void): () => void;
  events(listener: (event: StateEvent<unknown>) => void): () => void;
  bind(): void;      // 重新绑定到当前 source
  stop(): void;      // 清空监听并解绑
}
```

行为（逐条）：

- `value`：引用 `released`/`gone` → `ref.terminalError()`；`status !== "live"` → `notReady(`${address}.${name}`)`；`currentView()` 为 undefined → 同 not_ready；`#current()` 为 undefined → `ChordError("not_in_variant", `${address}: ${name} is not a state member of variant ${variantOf(view) ?? "(local)"}`)`；否则 `source.value`。
- `[HYDRATION]`：引用 live → `hydrationOf(current)`；否则返回 `{hydrated:false, whenHydrated: async () => { await ref.ready; await hydrationOf(current)?.whenHydrated(); }}`。
- `subscribe`：加入 `#values`；若 `#readable()` 有值：记录 `#seen = {value: source.value}` 并同步（`#safe`）调 listener 一次；`bind()`；返回移除函数。
- `events`：加入 `#events`；若可读：`#seen = {value}`，同步发 `{type:"snap", value}`；否则加入 `#unseen`（等可读时补 snap）；`bind()`；返回移除（同时从 unseen 删）。
- `bind()`：`source = (#values.size + #events.size) > 0 ? #current() : undefined`；相同则返回；否则解除上游，订阅 `source.events(...)`：
  - 事件序列：`first` 标志；若**第一个**事件且 `#unseen` 为空且 `#seen !== undefined` 且 `event.value === #seen.value`（`===` 引用相等，不是 deepEqual）→ 跳过（不重复通知首个快照）。
  - 记录 `#seen = {value: event.value}`；`translated = first ? {type:"reset", value: event.value} : event`。
  - 快照 listeners/events/unseen 集合，清空 unseen；**`enqueue`**（发布队列）后再分发：event listener 中未见的得到 `{type:"snap", value}`，其余得到 translated（首个是 reset，之后原样）；value listener 得到 `event.value`。分发时 listener 已被移除则跳过；listener 抛错 → `host.report`。
- `stop()`：清空 values/events/unseen，`bind()`（解绑上游）。

#### 2.3.9 `RefImpl`（本地引用 + Proxy）

```ts
export class RefImpl {
  readonly object: ObjectImpl; readonly contract: AnyContract; readonly owner: RefOwner; readonly address: string;
  readonly proxy: object; readonly ready: Promise<void>; status: RefStatus; error: unknown;
  readonly shapeListeners = new Set<(shape: Shape) => void>();
  readonly goneListeners = new Set<(error: unknown) => void>();       // 内部：link 转发 gone
  readonly readables = new Map<string, MemberReadable>();
  readonly bindings = new WeakMap<Service, Adapter | undefined>();
  readonly views = new WeakMap<Record<string, unknown>, Record<string, unknown>>();
  readonly callers = new Map<string, (...given: unknown[]) => Promise<unknown>>();
  constructor(object, contract, owner);
  get variant(); get shape();
  onShape(listener): () => void; onStatus(listener): () => void;
  release(): void; live(): void; gone(error): void; shapeChanged(): void; terminalError(): unknown;
  viewOf(installed: Installed): Record<string, unknown>;
  currentView(): Record<string, unknown> | undefined;
  describe(variant, member): MemberDescription | undefined;
  inVariant(view, name): boolean;
  member(name): unknown;
  caller(name): (...given: unknown[]) => Promise<unknown>;
}
```

- 构造：`status = object.status === "live" ? "live" : "loading"`；`ready = object.ready.then(() => { if (status==="gone") throw error; if (status==="released") throw ChordError("released", `reference ${address} released`); })`；`ready.catch(noop)`。
- **Proxy 行为（Rust 无 Proxy，需显式成员表 + 动态成员访问器）**：
  - `get(target, property)`：
    - `REF_BASE.has(property)`：反射取 target 属性，函数则 `bind(target)`（保证 `this`）；返回。
    - 非 string 或 `property === "then"` → `undefined`（使引用不被当作 thenable；Symbol 属性一律 undefined）。
    - 否则 `target.member(property)`。
  - `has(target, property)`：`REF_BASE.has(property) || target.member(String(property)) !== undefined`。
  - `refsByProxy.set(proxy, this)`。
- `variant`：`currentView()` 存在则 `variantOf(view)`，否则 undefined。
- `shape`：仅当 `status === "live"` 取 `object.currentInstalled()`，经 `#safeView` 得 view；无则 undefined；否则 `{variant: variantOf(view), members: Object.keys(view).filter(k => k !== "variant"), contract: installed.service.contract.fingerprint}`。**注意 members 是 view 的键顺序，不含 "variant"**。
- `onShape` / `onStatus`：加入集合，返回删除函数。
- `release()`：已 released/gone → 返回；`status="released"`；停止所有 readables；`#statusChanged()`（异步排队通知）；`object.release(this)`。
- `live()`：`status === "loading"` 才 `status="live"`；`#statusChanged()`；`shapeChanged()`。
- `gone(error)`：已 released/gone → 返回；`status="gone"; this.error=error`；停止 readables；`#statusChanged()`；**同步**调用所有 `goneListeners`（link 用它发 `gone` 帧）。
- `shapeChanged()`：所有 readables `bind()`；取 `shape`；undefined 则返回；对快照的 `shapeListeners` **enqueue** 后逐个调用（抛错 report）。
- `terminalError()`：`status === "gone" ? error : ChordError("released", `reference ${address} released`)`。
- `viewOf(installed)`：用 `WeakMap<Service, Adapter|undefined>` 缓存 bind 结果；`bind(installed.service.contract, installed.service.options.accepts, this.contract)`；无 adapter 返回 `installed.implementation`；有 adapter 时按 implementation 缓存 `adapter(implementation)`。
- `currentView()`：`status !== "live"` → undefined；无 installed → undefined；否则 `#safeView`（bind 抛错 → report 并返回 undefined）。
- `describe(variant, member)`：`variant !== undefined` → `variants[variant]?.[member]`；否则遍历所有 variant 的值，第一个含 member 的返回其描述；都没有 → undefined。
- `inVariant(view, name)`：本地契约永远 true；remote 契约 → `describe(variantOf(view), name) !== undefined`。
- `member(name)`：
  - `declared = describe(undefined, name)`；`view = currentView()`；`value = view?.[name]`。
  - `declared?.kind === "method" || (declared === undefined && typeof value === "function")` → `caller(name)`。
  - `declared?.kind === "state" || (declared === undefined && (isSource(value) || isReadableLike(value)))` → 惰性创建/复用 `MemberReadable`。
  - `view === undefined || !inVariant(view, name)` → undefined。
  - 否则 `value instanceof ExposedImpl ? value.resource.value : value`（opaque/expose 成员解包为资源值）。
- `caller(name)`（惰性缓存）：`noArgs = takesNoArgs(contract, name)`；
  - 取参：`const [args, ctx] = (noArgs ? [undefined, given[0]] : given)`。
  - `host.inFactory > 0` → **同步抛** `ChordError("not_ready", `${address}.${name} called inside a factory`)`。
  - `ctx !== undefined && !(ctx instanceof CtxImpl)` → `Promise.reject(invalid("foreign_ctx","ctx must come from a Chord host"))`。
  - `ctx === undefined || ctx.holder`：`ctx?.check(host)`（抛 → reject）；`host.root((root) => object.call(this, name, args, root), { signal: ctx?.signal })`（每次调用新 root，取消随 holder 的 signal）。
  - 否则 `object.call(this, name, args, ctx)`。

#### 2.3.10 `variantOf`

```ts
export const variantOf = (implementation) => typeof implementation.variant === "string" ? implementation.variant : undefined;
```

#### 2.3.11 对象状态与内部结构

```ts
export type ObjectStatus = "activating" | "live" | "failed" | "disposing" | "disposed";
interface Gate { readonly grandfathered: Set<string>; readonly queue: {key,event}[]; drained(): void }
interface Subscription { readonly origin: unknown; readonly select: ListenerDecl["select"]; unsubscribe: () => void }
export interface StagedListeners { activate(): void; discard(): void }
export interface ObjectIdentity { address; typeName; input; principal; hostLifetime: boolean }
const sameSelect = (a,b) => a === b || (a !== undefined && b !== undefined && String(a) === String(b));
```

`sameSelect`：同一函数对象，或**函数源码文本相同**（每次工厂运行的内联箭头都是新对象，但源码相同）；捕获值不同的 select 检测不到（已知限制）。

`ObjectImpl` 字段：

```ts
host: HostCore; address; typeName; input; principal; hostLifetime;
status: ObjectStatus = "activating";
ready: Promise<void> = Promise.resolve();
service: Service | undefined;
minCodeVersion = 0;                 // 被 gate 升级后 = 新 code version；更老 ctx 的调用/写入 -> upgrade_conflict
decls = new Decls();
pending = new Set<Decls>();         // 尚未 install/discard 的工厂运行（激活或 reload 候选）
implementations = new Map<number, Installed>();
refs = new Set<RefImpl>();
leases = new Map<string, number>(); // rootId -> codeVersion
openOrder: ResourceImpl[] = [];
callAbort = new AbortController();
activationAbort = new AbortController();
gate: Gate | undefined;
deferredRuntime = false;            // 仅由运行中的 reload pending code version serve：listeners/tasks/registrations 在 commit 才启动
#subscriptions = new Map<string, Subscription>();
#invocations = new Set<Promise<unknown>>();
#tasks = new Map<string, {controller: AbortController; done: Promise<void>}>();
#stopped = new Set<Promise<void>>();
#taskRuns = 0;                      // 每次 start/stop 任务递增；延迟启动若已不是最新则丢弃
#linger: Timeout | undefined;
#disposal: Promise<void> | undefined;
```

#### 2.3.12 `start` / `#activate`（激活事务）

`start(after)`：
- `ready = #activate(after).then(onOk, onErr)`：
  - 成功：所有 `refs` `live()`；`#maybeIdle()`。
  - 失败：若 status 仍是 activating → `"failed"`；`host.forget(this)`；`error = asChordError(cause, `activating ${address}`)`（ChordError 保 code；其它 → `unavailable` + 前缀）；所有 refs `gone(error)`；**抛出** error（让 ready 拒绝）。
- `ready.catch(noop)`。

`#activate(after)` 顺序（失败路径见下）：
1. `await after?.catch(noop)`（等待该地址前一次 dispose 完成）。
2. `signal = activationAbort.signal`；`abandoned = () => ChordError("aborted", `${address} abandoned`)`；`signal.aborted` → 抛 abandoned()。
3. `service = host.service(typeName)`；undefined → `ChordError("unavailable", `${typeName} is not served`, { reason: "unknown_type" })`。
4. `run = this.run(service, undefined)`（同步跑工厂）。
5. `await this.open(run.created.resources, run.created.refs, signal)`；aborted 检查。
6. `await this.waitHydrated(run)`；aborted 检查。
7. `await this.initCells(run, signal)`；aborted 检查。
8. `host.checkRegistrations(this, run.decls)`（可能抛 registry_conflict）。
9. `this.decls = run.decls`；对 `host.retainedCodeVersions()` 中 **service 相同**（`services.get(typeName) === service`）的每个 code version 写 `implementations.set(retained, {codeVersion: retained, service, implementation: run.implementation, decls: run.decls})`。
10. 若 `!implementations.has(host.currentCodeVersion())` 且（pending 未定义或未含此 implementation）→ `ChordError("superseded", `${address}: code changed during activation`)`。
11. `run.created.derives` 逐个 `attach()`（计算首值并订阅源）；`refreshSchemas()`。
12. `deferredRuntime = !implementations.has(host.currentCodeVersion())`（只服务于 pending code version 的新对象：listeners/tasks/registrations 等到 commit）。
13. 若 !deferredRuntime：若有 registrations → `host.registriesChanged(this)`；`startRuntime()`。
14. 再次检查 `signal.aborted` → abandoned（listener 启动时可能已同步释放最后一个引用：**不复活**）。
15. `status = "live"`。

失败 catch：
- `callAbort.abort(error)`；`stopListeners()`；`stopTasks()`；
- `registered = decls.registrations.size > 0 && !deferredRuntime`；`implementations.clear()`；`decls = new Decls()`；若 registered → `host.registriesChanged()`（移除自己的注册）。
- `await Promise.allSettled([...invocations, ...stopped])`；
- `await unwind(run).catch(report)`；
- 抛 error。finally：`pending.delete(run.decls)`。

#### 2.3.13 `run(service, previous)`（记录模式）

- 新建 `SelfImpl(this, previous)`；`discard(run)` = 从 pending 删除 + 异步 unwind（catch report）。
- `pending.add(self.run(undefined).decls)`（先注册声明集，让 cycle 检查能看到）。
- `host.inFactory++`；同步 `implementation = service.factory(input, principal, self)`；抛错 → `inFactory--`、`discard(self.run(undefined))`、重抛。`inFactory--`。
- `run = self.run(implementation)`；若 `implementation instanceof Promise` → `invalid("implementation", `${typeName}: factories must be synchronous`)`（discard 后抛）；`validateImplementation(service, run.implementation)`（同上 discard 后抛）。
- 返回 run；声明集留在 pending 直到调用方 install 或 discard。

#### 2.3.14 `open(resources, refs, signal)`

- `openOne(resource)`：memo（Map）；先 `Promise.all(resource.deps.map(dep => dep instanceof ResourceImpl ? (resources.includes(dep) ? openOne(dep) : undefined) : dep.ready))`（资源依赖递归、引用依赖 await ready），再 `resource.open(signal, openOrder)`。
- `ready = withinReadyTimeout(Promise.all(refs.map(r => r.ready)), () => refs.filter(r => r.status === "loading").map(r => r.address))`。
- `Promise.allSettled([...resources.map(openOne), ready])`；取第一个 rejected 抛其 reason（**所有都跑完再失败**）。

#### 2.3.15 `waitHydrated(run)`

- `exposed = Object.entries(run.implementation ?? {})`；`pending = unhydrated([...run.created.derives, ...exposed.map(([,v]) => v)])`；空则返回。
- `withinReadyTimeout(Promise.all(pending.map(h => h.whenHydrated())), () => [exposed 中 hydrationOf(value)?.hydrated === false 的 `${address}.${name}`, ...derives 中 hydrationOf 存在的 `derive ${key}`])`。

#### 2.3.16 `withinReadyTimeout(work, waiting)`

- `ms = host.readyTimeout`；非有限 → 直接 await work。
- setTimeout(ms)：`list = waiting()`；抛 `ChordError("unavailable", `${address} was not ready after ${ms} ms${list.length === 0 ? "" : `, waiting on ${list.join(", ")}`} (a reference cycle across hosts?)`, { reason: "ready_timeout" })`；timer `.unref()`。
- `Promise.race([work, timeout])`；finally clearTimeout。

#### 2.3.17 `initCells(run, signal)`

`run.created.inits` 为空 → 返回。否则 `Promise.allSettled(inits.map(async ([cell, init]) => { const value = await host.root((ctx) => init(ctx), { signal }); if (!signal.aborted) cell.initialize(value); }))`；第一个 rejected 抛其 reason。**每个 init 在自己的 root 中，以 host principal**；并行；abort 后不写入。

#### 2.3.18 `unwind(run)`

- 所有 created derives `detach()`。
- resources 按 openOrder 逆序排序（`openOrder.indexOf(b) - openOrder.indexOf(a)`；不在 order 中的 index=-1）。
- `runAll([...resources.map(r => () => r.close(openOrder)), ...run.created.refs.map(r => () => r.release())], `${address}: cleanup failed`)`。

#### 2.3.19 `currentInstalled` / `refreshSchemas` / `allDecls`

- `currentInstalled()`：`implementations.get(host.currentCodeVersion()) ?? [...implementations.values()].at(-1)`。
- `refreshSchemas()`：汇集所有 implementations 的 `memberSchemas(service, implementation)` → `Map<Readable, TSchema[]>`（同一 source 的多 schema 连接）；对 `allDecls()` 中每个 decls 的所有 cells 与 derives 设置 `source.schemas`（无则 `[]`）。dev 模式下每次写入用它校验。
- `allDecls()`：`new Set([decls, ...pending, ...implementations 的 decls])`（Set 迭代顺序：插入序）。

#### 2.3.20 监听器

`#origin(decl)` = `resourceOf(decl.source) ?? decl.source`。

`#listenSource(key, decl)`：`resource = resourceOf(decl.source)`；`value = resource === undefined ? decl.source : resource.value`；`source = decl.select === undefined ? value : decl.select(value)`；若 `typeof source?.subscribe !== "function"` → `invalid("implementation", `listener ${key}: source has no subscribe()`)`。

`stageListeners(decls)`：
- 遍历 `decls.listeners`（键序）：`origin`；`previous = #subscriptions.get(key)`；若 previous 存在且 `previous.origin === origin`（引用相等）且 `sameSelect(previous.select, decl.select)` → 标记 kept，**不重订阅**（对只允许单订阅者的源是关键），继续。
- 否则取 source：`entry = {origin, select, unsubscribe: noop, buffer: []}`；若 source 是 Cell/Derived（`isSource`）或 readable-like → `buffer = undefined`，`subscribe = () => { unsubscribe = source.subscribe(event => #deliver(key, event)) }`（**等 activate 时才订阅**：state 在 commit 时给当前值，而不是候选值）；否则立即 `unsubscribe = source.subscribe(event => buffer ? buffer.push(event) : #deliver(key,event))`。
- 任一步抛错 → 全部 fresh unsubscribe 后重抛（不留下半订阅）。
- 返回：
  - `activate()`：删除所有未 kept 的旧订阅；对每个 fresh：写入 `#subscriptions`，`subscribe?.()`，先把 buffer 中事件按序 `#deliver`。
  - `discard()`：全部 fresh unsubscribe。

`startRuntime()`：`stageListeners(this.decls).activate(); startTasks();`

`stopListeners()`：全部 unsubscribe，清空 map。

#### 2.3.21 任务

`startTasks()`：
- `run = ++#taskRuns`；`start = () => { for key of decls.tasks.keys(): #startTask(key, host.currentCodeVersion()) }`。
- `#stopped.size === 0` → 立即 start。
- 否则：`deadline = host.taskStopDeadline`；`late` promise（setTimeout deadline → resolve(true)）；`stopped = Promise.allSettled([...#stopped]).then(() => false)`；`Promise.race(...).then(missed => { clearTimeout; if (run !== #taskRuns || status !== "live") return; if (missed) host.report(ChordError("aborted", `${address}: aborted tasks still running after ${deadline} ms; starting their replacements anyway`)); start(); })`。

`#startTask(key, codeVersion)`：
- `fn = decls.tasks.get(key)`；无则返回。
- `controller = new AbortController()`；`ctx = new CtxImpl(host, { signals: [controller.signal, callAbort.signal], rootId: `task:${address}:${key}`, codeVersion, principal: host.principal, holder: true })`。
- 执行：`await fn(ctx)`；抛错且 `!ctx.signal.aborted` → `host.report(error)`；finally `ctx.close()`，若当前 `#tasks.get(key).controller === controller` 删除。
- 记入 `#tasks`。

`stopTasks()`：`#taskRuns++`；每个任务 `controller.abort(ChordError("aborted","task stopped"))`，把 done 加入 `#stopped`，done 完成时移除；清空 `#tasks`。

#### 2.3.22 事件投递与 gate

`#deliver(key, event)`：
```
enqueue(() => {
  if (gate !== undefined) { gate.queue.push({key, event}); return; }
  const decl = decls.listeners.get(key);
  if (decl === undefined || (status !== "live" && status !== "activating")) return;
  const invocation = host.root((root) => {
    lease(root);
    const child = root.child(this);
    return #invoke(async () => { try { await decl.fn(event, child); } finally { child.close(); } });
  });
  invocation.catch(error => host.report(error));
});
```

- 每个 listener 事件在**新 root** 中运行，**lease** 该对象（gate 会等它），计入 `#invocations`（disposal 等它），强制 dispose 会取消它（child signal 含 object.callAbort）。
- gate 打开时 `openGate()`：`gate = undefined`，按队列顺序重新 `#deliver`。

#### 2.3.23 准入与租约

`admit(ctx)`（调用或写入前）：
1. `host.failed !== undefined` → `ChordError("unavailable", `${host.name} failed and admits no work`, { reason: "host_failed" })`。
2. `!host.hasCodeVersion(ctx.codeVersion)` → `ChordError("superseded", `code version ${ctx.codeVersion} is retired`)`。
3. `ctx.codeVersion < minCodeVersion` → `ChordError("upgrade_conflict", `${address} was upgraded in code version ${minCodeVersion}`)`。
4. `gate !== undefined && !gate.grandfathered.has(ctx.rootId)` → `ChordError("upgrade_conflict", `${address} is being upgraded`)`。

`lease(ctx)`：若 rootId 未记录 → `leases.set(rootId, ctx.codeVersion)`；`host.touch(rootId, this)`。

`guard = (ctx)`（cell 写入前）：
1. `host.inFactory > 0` → `invalid("read_only", "cells cannot be written while a factory runs")`。
2. `ctx instanceof CtxImpl` → `ctx.check(host)`（会抛 foreign_ctx/expired/aborted）。
3. `ctx.signal.aborted` → `ChordError("aborted","write with an aborted ctx")`。
4. `status === "disposed"` → `ChordError("gone", `${address} is disposed`)`。
5. `ctx.rootId.startsWith("task:")` → **直接 return（不 admit：task 写入不看 gate/升级）**。
6. `admit(ctx)`。

#### 2.3.24 `call(ref, name, args, ctx)`

顺序（本单元最关键的调用路径）：
1. `ctx.check(host)`。
2. `ref.status` 为 released/gone → 抛 `ref.terminalError()`。
3. `ref.status === "loading"` → `await raceSignal(ref.ready, ctx.signal)`。
4. `ref.status !== "live" || this.status !== "live"` → 若 ref live → `ChordError("gone", `${address} is ${this.status}`)`；否则 `ref.terminalError()`。
5. `admit(ctx)`。
6. `installed = implementations.get(ctx.codeVersion)`；无 → `ChordError("superseded", `${address} has no implementation for code version ${ctx.codeVersion}`)`。
7. `view = ref.viewOf(installed)`；`variant = variantOf(view)`；`wanted = ref.describe(contract.scope === "remote" ? variant : LOCAL_VARIANT, name)`；`fn = view[name]`。
8. 若 `typeof fn !== "function"` 或（remote 且 `wanted?.kind !== "method"`）→ `ChordError("not_in_variant", `${address}: ${name} is not a method of variant ${variant ?? "(local)"}`)`。
9. `direct = view === installed.implementation`；`schema = direct ? installed.service.contract.description.variants[variant ?? LOCAL_VARIANT]?.[name] : wanted`；`method = schema?.kind === "method" ? schema : undefined`；`validated = contract.scope === "remote" ? method : undefined`（**本地调用不逐次校验**）。
10. `takesArgs = method === undefined || method.args !== undefined`；`returns = method === undefined || method.result !== undefined`。
11. 若 takesArgs：
    - `own = remote && wanted?.kind === "method" ? wanted.args : undefined`；
    - `sent = own === undefined ? args : Value.Clean(own, Value.Clone(args))`（按 consumer 自己知道的 schema 剥离）；
    - `input = validated?.args ? Value.Clean(validated.args, Value.Clone(sent)) : sent`；
    - `validated?.args` 存在 → `assertValid("args", `${address}.${name} args`, validated.args, input)`（invalid/args）。
12. `lease(ctx)`；`child = ctx.child(this)`；`result = await #invoke(() => takesArgs ? fn(input, child) : fn(child))`；finally `child.close()`。
13. `!returns` → 返回 undefined（丢弃实现返回值）。
14. `validated?.result` 存在 → `assertValid("result", `${address}.${name} result`, validated.result, result)`（invalid/result）；返回 result。

`#invoke(fn)`：立即执行（async 包装）并加入 `#invocations`；settle 时移除，若空 → `#maybeIdle()`。返回同一 promise。

#### 2.3.25 生命周期

`rootEnded(rootId)`：`leases.delete`；若 gate 存在且 grandfathered 删除成功且变空 → `gate.drained()`。

`addRef(ref)`：加入 refs；清除并置空 `#linger` 定时器。

`release(ref)`：`refs.delete(ref)` 成功 → `#maybeIdle()`。

`#maybeIdle()`：
- `refs.size > 0 || #invocations.size > 0 || #linger !== undefined` → 返回。
- `status === "activating"` → `#background(dispose(false))`（被抛弃的激活一结束就释放）。
- `status !== "live"` → 返回。
- `linger = service?.options.linger ?? 0`；<= 0 → `#background(dispose(false))`。
- 否则 setTimeout(linger)（unref）：超时后若仍无 refs/invocations 且 live → `#background(dispose(false))`。

`dispose(forced, reason?)`：`#disposal ??= #dispose(forced, reason)`；返回同一 promise（幂等）。

`#dispose(forced, reason)`：
1. `wasActivating = status === "activating"`；`status = "disposing"`；`host.forget(this)`；清 linger。
2. `error = reason ?? ChordError("gone", `${address} was disposed`)`。
3. 若 forced：所有 refs `gone(error)`；`callAbort.abort(error)`（取消在飞调用）。
4. `activationAbort.abort(error)`；若 wasActivating → `await ready.catch(noop)`。
5. 再次所有 refs `gone(error)`（**注意：非 forced 也会 gone**——对象因无人引用而释放时，残留引用（无？）会 gone）。
6. `decls = allDecls()`；`stopTasks()`；`stopListeners()`；`await Promise.allSettled([...invocations])`（等所有在飞调用/监听器结束）。
7. `registered = 任一 decls 集有 registrations && !deferredRuntime`；`implementations.clear()`；`this.decls = new Decls()`；若 registered → `host.registriesChanged()`。
8. 收集所有 decls 集的 derives（detach）与 refs（去重 Set）。
9. `runAll([() => Promise.allSettled([...stopped]), ...openOrder.reverse().map(r => () => r.close(openOrder)), ...refs.map(r => () => r.release())], `${address}: disposal failed`)`。
10. finally `status = "disposed"; host.disposed(this)`。

#### 2.3.26 `isReadableLike` / `memberSchemas` / `validateImplementation`

```ts
const isReadableLike = (value) => value !== null && typeof value === "object" && typeof value.events === "function" && typeof value.subscribe === "function";

export function memberSchemas(service, implementation): Map<Readable, TSchema[]>;
```
`memberSchemas`：仅 remote 契约；取 `contract.description.variants[variantOf(implementation) ?? ""] ?? {}`；对每个成员：若 `kind === "state"` 且 `implementation[name]` 是 Cell/Derived（`isSource`）→ 加入 map（同一 source 多 schema 连接）。**注意**：转发来的 remote 成员（readable-like 但非 Source）不获得 provider schema（它们用自己的 snapshot 校验）。

`validateImplementation(service, implementation)`（`invalid("implementation", ...)`，消息前缀 `${contract.name}: `）：
- 非对象/为 null → `factory must return an object`。
- remote：`implementation.variant` 必须是 string 且是已声明 variant → 否则 `unknown variant ${String(variant)}`；对 implementation 的每个键：`key !== "variant" && members[key] === undefined` → `${key} is not in variant ${String(variant)}`。
- local：`members = description.variants[""] ?? {}`。
- 对契约列出的每个成员：
  - method 且 `typeof value !== "function"` → `${name} must be a method`。
  - state 且 `!isSource(value) && !isReadableLike(value)` → `${name} must be a cell, a derivation or a held object's state member`。
  - opaque 且 `value === undefined` → `${name} is missing`。
- 注意：**remote 契约的实现不能有契约外的键**；local 可以有任意额外键（`ImplementationOf` 里是 `Record<string, unknown>`）。

#### 2.3.27 `SelfImpl`（工厂的 self）

```ts
class SelfImpl implements Self {
  constructor(object: ObjectImpl, previous: Decls | undefined);
  run(implementation: unknown): Run;
  #unique(map, kind, name): void;      // name 非 string 或 "" -> TypeError(`${kind} needs a name`)
                                       // 已存在 -> invalid("duplicate", `${typeName}: duplicate ${kind} name ${name}`)
  state(name, schema, initial, options = {});
  get(type, input, options?);
  resource(name, declaration);
  on(name, source, ...rest);
  task(fn);
  derive(sources, fn);
  expose(resource);
  register(registry, entries);
}
```

- `state`：`#unique(cells, "state", name)`；`version = options.version ?? 1`；非正整数 → `TypeError(`state ${name}: version must be a positive integer`)`；`init = isStateInit(initial) ? initial.init : undefined`；`cell = previous?.cells.get(name)`；
  - 不存在 → 新建 `Cell(name, schema, init === undefined ? {value: initial} : undefined, object.guard, {dev: host.dev, report: host.report})`；有 init 时记入 `created.inits`。
  - 存在（reload 保留）→ 复用旧 cell（initial 忽略，init 不跑）。
  - 写 decls：cells/cellSchemas/cellVersions；`options.migrate` 存在 → cellMigrations。
- `get`：`candidate = previous !== undefined`；`address = host.addressOf(type, input, options?.principal ?? host.principal, candidate)`。
  - `decls.refs.get(address)` 已存在：fingerprint 不同 → `invalid("duplicate", `${typeName}: ${type.name} held twice with different contracts`)`；否则返回同一个 proxy。
  - 否则若 `previous?.refs` 中同 address 且 fingerprint 相同且状态不是 gone/released → 复用（写入 decls.refs，不加入 created）。
  - 否则 `host.acquire(type, input, options, "object", this.#object, candidate)`；写入 decls；`created.refs.push`。
- `resource`：`#unique(resources, "resource", name)`；`Number.isInteger(declaration.version)` 否则 `TypeError(`resource ${name}: version must be an integer`)`；deps 解析：`refOf(dep) ?? resourceOf(dep)`，无 → `TypeError(`resource ${name}: deps must be references or resources of this object`)`。
  - kept 条件：旧资源存在，且 `spec.version` 相同，`exclusive` 相同（`declaration.exclusive === true` 对比 `existing.exclusive`），deps 长度和逐项引用相同。
  - kept → 复用旧 ResourceImpl 并 `decls.adopted.set(existing, declaration)`；否则 `new ResourceImpl(...)` 加入 `created.resources`。
  - 返回 `new ResourceFacade(resource, host)`。
- `on(name, source, ...rest)`：`#unique(listeners,"listener",name)`；`[select, fn] = rest.length === 1 ? [undefined, rest[0]] : [rest[0], rest[1]]`；fn 非函数 → `TypeError(`listener ${name}: the handler must be a function`)`；select 非 undefined 且非函数 → `TypeError(`listener ${name}: select must be a function`)`；记入。
- `task(fn)`：非函数 → `TypeError("a task must be a function")`；key = `#${tasks.size}`（按声明序号）。
- `derive(sources, fn)`：非函数 → `TypeError("a derivation needs a function")`；key = `#${derives.size}`；新建 `Derived(key, sources, fn, {dev, report})`；加入 created 和 decls。
- `expose(resource)`：`resolved = resourceOf(resource)`；undefined 或不在本 run 的 decls.resources 中 → `TypeError("expose takes a resource declared by this object")`；返回 `ExposedImpl`。
- `register(registry, entries)`：非 hostLifetime → `invalid("not_host_lifetime", `${typeName}: only objects of lifetime "host" may register`)`；每个 entry：`id = `${registry.name}\u0000${key}``；`#unique(registrations, "register", id)`；记 `{registry, key, value}`。

#### 2.3.28 `notReady`

```ts
export const notReady = (what: string): ChordError => new ChordError("not_ready", `${what} is not ready`);
```
（定义在 cell.ts，object.ts 引用。）

### 2.4 `src/connection.ts`（1327 行）

一条链路在本宿主上的一端：远程引用的**消费者**、peer 的 hold 的**提供者**、以及**中继（relay）**。

#### 2.4.1 选项与身份常量

```ts
export interface LinkOptions {
  readonly pingInterval?: number;        // 默认 15000 ms
  readonly pingTimeout?: number;         // 默认 45000 ms；超时未收到任何帧 -> link.close("ping timeout")
  readonly window?: number;              // 默认 256*1024（未确认帧 JSON 文本字节）
  readonly pendingCap?: number;          // 默认 window * 4（等待中的 ops 字节）
  readonly onProtocolMismatch?: (versions: { readonly local: number; readonly peer: number | undefined }) => void;
}

const DEFAULT_WINDOW = 256 * 1024;
const PENDING_WINDOWS = 4;

export interface AcceptOptions<P> extends LinkOptions {
  readonly principal: P;
  readonly kind?: "host" | "client";     // 默认 "client"；由接受方决定，永不信任 peer 的 hello
  readonly name?: string;                // child host 的名字：hello 必须匹配，且可按此名寻址
  restamp?(principal: P): P | undefined; // invalidatePrincipal 时：返回新 principal 保留链路，undefined 关闭
  readonly maxCalls?: number;            // 该链路上 peer 可同时在飞的调用数（默认无限制）
}

export interface Peer { readonly kind: "host" | "client"; readonly name: string | undefined }
```

`originId`：接受 client 链路时由 host 生成 `${incarnation}:${++originSeq}`（见 host.accept）。

#### 2.4.2 内部数据类型

```ts
type StateFrame = Extract<Frame, { t: "state" }>;
type Overflow = { readonly resend: () => void } | { readonly resync: () => void };
type Slot =
  | { kind: "unit"; hold: number; flush: () => void }
  | { kind: "state"; hold: number; m: string; base: StateFrame | undefined; ops: (readonly unknown[])[]; size: number; lazy: boolean; overflow: Overflow };

interface Feed { unsubscribe: () => void; encoder: Encoder; buffer: StateEvent<unknown>[] | undefined }
interface ServedLocal {
  kind: "local"; id: number; ref: RefImpl; principal: unknown; origin: string | undefined;
  wantsState: boolean; again: boolean; unitSent: boolean; feeds: Map<string, Feed>;
}
type Served = ServedLocal | { kind: "relay"; relay: RelayRef };
type Pending = { resolve(value: unknown): void; reject(error: unknown): void };
```

#### 2.4.3 `RelayRef`

```ts
export class RelayRef {
  readonly origin: Connection; readonly frame: Extract<Frame, { t: "hold" }>;
  readonly principal: unknown; readonly originTag: string | undefined;
  readonly generation: number;
  down: Connection | undefined; downId = 0; closed = false; stateWanted: boolean;
  urgent: Set<string> | undefined;      // 首个 unit 的成员，越过 origin 的窗口
  #shaped = false;
  readonly awaiting = new Set<string>(); // 丢弃过 ops 的成员：直到 reset 到达前忽略 downstream ops
  readonly calls = new Map<number, RelayCall>();
  constructor(origin, frame, principal, originTag);
  get originId(): number;               // frame.id
  get client(): boolean;                // originTag !== undefined
  get current(): boolean;               // !closed && origin.open && origin.generation === generation
  ready(): Promise<Connection>;         // 已 closed -> reject ChordError("gone","relayed hold closed")；down 存在 -> resolve；否则等待
  open(down: Connection): void;
  wantState(wanted: boolean): void;
  resync(m: string): void;
  shape(frame): void;
  state(frame: StateFrame): void;
  close(error, options: { releaseDown: boolean; notify: boolean }): void;
}
```

- 构造：`stateWanted = frame.state !== false`；`generation = origin.generation`（授权代次；撤销会递增）。
- `open(down)`：已 closed 返回；`downId = down.nextId()`；`down.relayedRefs.set(downId, this)`；发送 `{t:"hold", id: downId, at, token, input, principal, ...(originTag ? {origin: originTag} : {}), ...(stateWanted ? {} : {state:false}), ...(frame.again === true ? {again:true} : {})}`；唤醒所有 waiter。
- `wantState(wanted)`：closed 或相同 → 返回；`stateWanted = wanted`；若关闭 → `origin.dropHoldState(originId)`、`awaiting.clear()`；`down?.send({t: wanted ? "sub" : "unsub", hold: downId})`。
- `resync(m)`：`awaiting.add(m)`；`down?.send({t:"resync", hold: downId, m})`。
- `shape(frame)`：首次 shape 且 `frame.again !== true`（指 origin 的 hold 帧 `again`）→ `urgent = new Set(frame.state)`；转发 `{...frame, id: originId}`（立即发送，不走窗口）。
- `state(frame)`：若 `awaiting.has(m)`：ops → 丢弃；否则（snap/reset/error）移除 m。`out = {...frame, hold: originId}`；若 `urgent.delete(m)` → 立即 `origin.send(out)`；否则 `origin.sendState(out, { resync: () => this.resync(m) })`。
- `close(error, {releaseDown, notify})`：已 closed 返回；`closed = true`；所有 calls `cancel(error, releaseDown)`；`origin.dropHoldState(originId)`；若 origin.served 中该 id 是 relay → 删除；若 down 存在：删除 `down.relayedRefs[downId]`，`releaseDown` 时 `down.send({t:"release", id: downId})`；唤醒所有 waiter 以 error 拒绝；`notify` 时 `origin.send({t:"gone", id: originId, error: toWireError(error)})`。

#### 2.4.4 `RelayCall`

```ts
export class RelayCall {
  constructor(relay, originId, downId, pending);
  finish(error, value?): void;     // 只结算一次；删除 relay.calls[originId] 与 relay.down.relayedCalls[downId]
  cancel(error, forward: boolean): void; // forward 时 down.send({t:"cancel", id: downId})；finish(error)
}
```

#### 2.4.5 `Connection` 字段

```ts
export class Connection implements RemoteChannel {
  host: HostImpl; link: Link; role: "upstream" | "downstream"; peer: Peer;
  originId: string | undefined;
  principal: unknown;                       // 上游：connect 给的 principal（可被转发 principal 覆盖每帧）；下游：accept 的 principal
  restamp: ((principal) => unknown | undefined) | undefined;
  generation = 0;                           // 撤销/重盖时递增
  peerCodeVersion: number | undefined;
  synced: boolean;                          // 下游 host peer 在本宿主 current code version 上才可用
  fencedCodeVersions = new Set<number>();   // 子 host 落后时 announce 的 code version（本宿主从未运行过）
  #admitted = new Map<string, number>();    // rootId -> 被接纳的 code version
  #parentName: string | undefined;
  #syncWaiters = new Set<Pending>();
  #nextId = 1; #lastSeen = Date.now(); #pinger;
  #maxCalls: number; #window: number; #pendingCap: number;
  #peerAcks: boolean | undefined; #sent = 0; #acked = 0; #unacked: number[] = []; #unackedBytes = 0;
  #pending = new Map<string, Slot>(); #pendingBytes = 0;
  #received = 0; #ackTimer; #onProtocolMismatch; #mismatched = false;
  remotes = new Map<number, RemoteBinding>();     // 本宿主发出的 hold
  calls = new Map<number, Pending>();             // 本宿主发出的 call
  served = new Map<number, Served>();             // peer 对本宿主发的 hold
  incoming = new Map<number, AbortController>();  // peer 对本宿主发的 call
  relayedRefs = new Map<number, RelayRef>();      // 本宿主作为 relay 下游时使用的 id
  relayedCalls = new Map<number, RelayCall>();
  votes = new Map<number, Pending>();             // 等待子 peer 的 reload 投票
}
```

构造 `(host, link, config: { role; principal; peer; originId?; restamp?; maxCalls? } & LinkOptions)`：
- `synced = !(role === "downstream" && peer.kind === "host")`；`#window = config.window ?? DEFAULT_WINDOW`；`#pendingCap = config.pendingCap ?? window*4`；`#maxCalls = config.maxCalls ?? Infinity`。
- `link.onClose(reason => #closed(reason))`；`link.receive(frame => #receive(frame))`。
- **立即**发送 hello：`{ t:"hello", protocol: PROTOCOL_VERSION, name: host.name, kind: host.kind, codeVersion: host.current.id, acks: true }`。
- 若 `link.closed`（构造时已关闭，onClose 立刻触发过）→ 不再启动 pinger。
- pinger：`setInterval(pingInterval)`（默认 15000），每次若 `Date.now() - lastSeen > pingTimeout`（45000）→ `link.close("ping timeout")`，否则 `send({t:"ping", n: ++n})`；`unref()`。

#### 2.4.6 发送与流量控制

`send(frame)`：`size = link.send(frame) ?? 0`；若 `#peerAcks === false` → 返回（不计）；否则 `#sent++`，`#unacked.push(size)`，`#unackedBytes += size`。

`sendState(frame, overflow)`：
```
key = stateKey(hold, m)                       // `s${hold}\u0000${m}`
slot = #pending.get(key)
if (slot === undefined && !#full) { send(frame); return }
if (slot?.kind === "unit") return             // 一个等待中的 unit 已覆盖
if (slot === undefined) { 新建 state slot(lazy=false, base=undefined, ops=[], size=0)；#pending.set(key, slot) }
if (frame.as !== "ops") {                     // snap / reset / error 替换
  #pendingBytes -= slot.size; slot.base = frame; slot.ops = []; slot.size = 0; slot.lazy = false; return
}
if (slot.lazy) return                         // flush 时构建的 reset 已覆盖这些 ops
size = JSON.stringify(frame.ops).length
if (#pendingBytes + size > #pendingCap) #makeRoom(size, slot)
if (slot.lazy || #pending.get(key) !== slot) return
for (op of frame.ops ?? []) slot.ops.push(op)
slot.size += size; #pendingBytes += size
```

`#makeRoom(size, current)`：遍历 `#pending.values()`（插入序）：若 `#pendingBytes + size <= #pendingCap` 返回；跳过非 state、current、size 为 0 的；`#drop(slot)`。最后若仍超 → `#drop(current)`。

`#drop(slot)`：`#pendingBytes -= slot.size; base=undefined; ops=[]; size=0`；若 `overflow` 有 `resend` → `slot.lazy = true`（provider 在 flush 时重发当前值）；否则 `#pending.delete(key)` 并 `overflow.resync()`（relay 向下游请求 reset）。

`#resetLater(hold, m, resend)`：若已有 unit slot → 返回；无 slot 则新建 `{lazy:true, overflow:{resend}}`；有 slot 则清空并置 lazy；然后 `#flush()`。

`#queueUnit(hold, flush)`：`key = unitKey(hold)`（`u${hold}`）；无则加入 unit slot；`#flush()`。

`dropState(hold, m)`：若 slot 是 state → 减字节、删除。
`dropHoldState(hold)`：删除该 hold 的所有 slot（state 的减字节）。

`get waiting(): { slots, bytes }`（测试/诊断）。
`get #full()`：`#peerAcks === true && #unackedBytes >= #window`。

`#onAck(n)`：若 `#peerAcks !== true` 或 `n < #acked` 或 `n > #sent` → `#violation(`ack ${n} outside ${#acked}..${#sent}`)`；否则从 `#unacked` 头部 `splice(0, n - #acked)` 并逐项减字节；`#acked = n`；`#flush()`。

`#flush()`：按 `#pending` 插入序：`#full` → 停止；删除该 slot；unit → `slot.flush()`；state → 减字节；lazy → 有 resend 则 `resend()`（无则什么都不发，relay 已 resync）；否则先发 base（若有）再发 `{t:"state", hold, m, as:"ops", ops}`（若有 ops）。

`#scheduleAck()`：若 `#peerAcks !== true || #ackTimer !== undefined || !open` → 返回；`setTimeout(0)` 后 `link.send({t:"ack", n: #received})`；unref。**每个突发一次 ack**。

`nextId()`：`#nextId++`。
`whenSynced()`：`!open` → reject `unavailable(`link to ${name ?? "peer"} closed`)`；`synced` → resolve；否则加入 `#syncWaiters`。
`markSynced(codeVersion)`：`peerCodeVersion = codeVersion; synced = true`；唤醒全部 waiter。

`revoke(next)`：`generation++`；`error = ChordError("gone","principal invalidated")`；所有 relay served `close(error, {releaseDown:true, notify:true})`；`next === undefined` → `link.close("principal invalidated")`；否则 `principal = next`。

`#protocolMismatch(peer, close = true)`：若已 `#mismatched` 返回；置 true；`onProtocolMismatch?.({local: PROTOCOL_VERSION, peer})`（抛错 → host.report）；`close` 时 `link.close(`protocol-version ${PROTOCOL_VERSION}`)`。

`#violation(reason)`：`host.report(ChordError("invalid", `link to ${name ?? "peer"}: ${reason}`, { reason: "protocol" }))`；`link.close(`protocol error: ${reason}`)`。

#### 2.4.7 消费端（RemoteChannel）

`hold(binding, again)`：`id = nextId()`；`remotes.set(id, binding)`；`binding.bound(this, id)`；发送：
```
{ t:"hold", id, at: binding.at, token: binding.contract.description, input: binding.input,
  ...(principal === undefined ? {} : {principal}), ...(binding.stateOn ? {} : {state:false}), ...(again ? {again:true} : {}) }
```
返回 id。注意 `binding.stateOn` 在 `bound` 里被设置为 `wantsState`。

`release(id)`：`remotes.delete(id)`；`send({t:"release", id})`。

`wantState(hold, wanted)`：仅当 `remotes.has(hold)` → `send({t: wanted ? "sub":"unsub", hold})`。

`call(hold, member, args, root, signal)`：`id = nextId()`；new Promise：
- `signal.aborted` → reject `ChordError("aborted","call aborted")`（**无 cause**）。
- onAbort：删除 calls[id]；`send({t:"cancel", id})`；reject `ChordError("aborted","call aborted",{cause: signal.reason})`。
- `calls.set(id, {resolve, reject})`，两者都会移除 abort 监听。
- `carried = root !== undefined && host.kind === "host"`（client 永不携带 root/code version；server 为 client 起 root）；`carried` 时 `host.rootSent(root.rootId, this)`。
- `send({t:"call", id, hold, member, args, ...(carried ? {root} : {})})`。

#### 2.4.8 接收与分发

`#receive(frame)`：
- `#mismatched` → 返回（另一协议版本之后什么都不处理）。
- `#lastSeen = Date.now()`。
- `!isFrame(frame)` → `#violation("malformed frame")`。
- `ack` → `#onAck(n)` 并返回（不计数、不 ack）。
- `#received++`（**在 dispatch 之前**，所以抛错的帧也计数，ack 仍会推进）；`try { #dispatch(frame) } catch (e) { host.report(e) }`；`#scheduleAck()`。

`#dispatch` 全表：
| t | 处理 |
| --- | --- |
| `hello` | `#onHello` |
| `ping` | `send({t:"pong", n})` |
| `pong`/`bye`/`ack` | 忽略（bye 由 Link 层消费） |
| `hold` | `#onHold` |
| `release` | `#dropServed(id, true)` |
| `call` | `#onCall` |
| `cancel` | `incoming.get(id)?.abort(ChordError("aborted","cancelled by caller"))` |
| `sub`/`unsub` | `#onWantState(hold, t === "sub")` |
| `resync` | `#onResync(hold, m)` |
| `shape`/`gone` | `#onHoldReply` |
| `result` | `#onResult` |
| `state` | `#onState` |
| `root_end` | `#admitted.delete(rootId)`；`host.foreignRootEnded(rootId, this)` |
| `invalidate`/`prepare`/`commit`/`abort`/`retire` | 仅当 `role === "upstream"`，否则 `#violation(`${t} from a child`)`；`invalidate` → `host.invalidatePrincipal(pattern).catch(report)`；`retire` → `host.allowRetire(codeVersion)`；其余 → `host.onReloadFrame(this, frame)` |
| `vote` | 仅当 `role === "downstream" && peer.kind === "host"`，否则 `#violation("vote from a non-participant")`；取 `votes.get(reload)` 并删除；无 error → resolve(report)；有 → reject(fromWireError(error)) |

`#onHello(frame)`：
- `(frame.protocol ?? 1) !== PROTOCOL_VERSION` → `#protocolMismatch(frame.protocol ?? 1)`；返回。
- `#peerAcks === undefined` → `#peerAcks = frame.acks === true`；false 时清空 unacked 计数。
- upstream：`frame.kind !== "host"` → `#violation("parent is not a host")`；`#parentName ??= frame.name`；返回。
- downstream：`frame.kind !== peer.kind || (peer.name !== undefined && frame.name !== peer.name)` → `#violation(`hello ${frame.kind} ${frame.name} does not match the accepted ${peer.kind} ${peer.name ?? ""}`)`；返回。
- `peer.kind === "host"` → `host.peerHello(this, frame.codeVersion)`。

`#principalOf(frame)`：downstream → `this.principal`（接受方盖章，忽略帧里的）；upstream → `frame.principal ?? this.principal`（只有父链路可转发 principal）。

`#originOf(frame)`：downstream → `this.originId`（client 链路本宿主自己的 id）；upstream → `frame.origin`。

#### 2.4.9 hold 的服务端（本地对象）

`#onHold(frame)`：
- `served.has(frame.id)` → `#violation(`duplicate hold id ${frame.id}`)`。
- `principal = #principalOf(frame)`；`origin = #originOf(frame)`；`client = origin !== undefined`。
- `frame.at !== host.name`：
  - upstream → `send({t:"gone", id, error: toWireError(unavailable(`${host.name} does not relay`))})`；返回。
  - 否则建立 `RelayRef(this, frame, principal, origin)`，`served.set(id, {kind:"relay", relay})`，`host.relay(relay).catch(e => relay.close(e, {releaseDown:true, notify:true}))`。
- 否则本地：
  - `token = contractFromDescription(frame.token)`；`token.scope !== "remote"` → `invalid("token", `${token.name} is local; only remote types cross links`)`。
  - `resolveHost(token.name, token.description.host, frame.input) !== host.name` → `invalid("placement", `${token.name} with this input does not live on ${host.name}`)`。
  - `acquired = host.acquire(token, frame.input, { principal }, client ? "client" : "connection")`；非 RefImpl → `unavailable("not a local reference")`。
  - 失败 → `send({t:"gone", id, error: toWireError(client ? clientError(error) : error)})`（client：contract_mismatch → **upgrade_required**，消息保留原 message，cause 保留）。
  - 构造 `ServedLocal {wantsState: frame.state !== false, again: frame.again === true, unitSent:false, feeds:new Map()}`；`served.set`；`ref.onShape(() => #unit(served))`；`ref.goneListeners.add(error => { if (served.get(id) !== served) return; #dropServed(id, false); send({t:"gone", id, error: toWireError(error)}) })`；若 `ref.status === "live"` → `#unit(served)`。

`#stateMembers(ref, shape)`：`shape.members.filter(name => ref.describe(shape.variant, name)?.kind === "state")`。

`#unit(served)`：`served` 仍是当前 → `shape = ref.shape`；undefined 返回；`members = wantsState ? #stateMembers : []`；对 `served.feeds` 中不在 members 的：unsubscribe、删除 feed、`dropState`；`urgent = served.feeds.size === 0 && !(served.again && !served.unitSent)`（首个 unit 且不是 re-hold → 立即发；re-hold 的首个 unit 也走窗口）；urgent 则 `#sendUnit`，否则 `#queueUnit(id, () => #sendUnit(served))`。

`#sendUnit(served)`：`#pending.delete(unitKey(id))`；仍是当前且有 shape；`unitSent = true`；`members = wantsState ? #stateMembers : []`；`listed = members.filter(name => !feeds.has(name))`；对每个 listed `#attach(served, name)` 得 `{frame, feed}`（不可读的跳过）；发送 `{t:"shape", id, ...(variant===undefined?{}:{variant}), members: shape.members, contract: shape.contract, state: snaps.map(f => f.m)}`；逐个发送 snap 帧；对每个 attached `#replay(served, name, feed)`。

`#attach(served, name, as = "snap")`：`readable = ref.member(name)`；非对象或无 `events` 方法 → undefined。`feed = {unsubscribe: noop, encoder: encoder(), buffer: []}`；订阅 `readable.events(event => { if (first === undefined) first = event; else if (feed.buffer !== undefined) feed.buffer.push(event); else #feedEvent(...) })`；`served.feeds.set(name, feed)`；返回 `{frame: #snapshot(served, name, as, first?.value), feed}`。

`#replay`：把 buffer 置 empty（`feed.buffer = undefined`）并按序 `#feedEvent`。

`#snapshot(served, name, as, value)`：`validateSnapshot(ref, name, value)` 抛错 → `{t:"state", hold, m, as:"error", error: toWireError(error)}`；否则 `{t:"state", hold, m, as, ...(value === undefined ? {} : {value})}`。**undefined 的 snap/reset 不带 value 字段**。

`#feedEvent(served, name, feed, event)`：仍是当前 feed；ops → `sendState({t:"state",hold,m,as:"ops",ops: feed.encoder.encode(event.ops)}, {resend: () => #resend(served,name)})`；否则 `feed.encoder = encoder()`（reset 重置编码器字典）并 `sendState(#snapshot(served,name,"reset",event.value), overflow)`。

`#resend(served, name)`：卸载 feed 后 `#attach(served,name,"reset")`，立即发该帧并 replay。

`#onWantState(hold, wanted)`：relay → `relay.wantState(wanted)`；本地：相同 → 返回；`wantsState = wanted`；wanted → `#unit(served)`；否则所有 feed unsubscribe、clear、`dropHoldState(hold)`。

`#onResync(hold, m)`：relay → `relay.resync(m)`；本地：无该 feed → 返回；`#resetLater(hold, m, () => #resend(served, m))`。

`#dropServed(id, release)`：无 → 返回；删除 served；relay → `close(ChordError("released","relayed hold released"), {releaseDown: release, notify:false})`；本地：所有 feed unsubscribe、clear、`dropHoldState(id)`；`release` 时 `ref.release()`（引用归 connection 所有）。

#### 2.4.10 调用（provider 端）

`#onCall(frame)`：
- `incoming.has(frame.id)` → `#violation(`duplicate call id ${frame.id}`)`。
- `incoming.size >= #maxCalls` → 立即回 `{t:"result", id, error: toWireError(ChordError("unavailable", `Too many calls in progress on this connection (${#maxCalls}); wait for earlier ones to finish`, { reason: "too_many_calls" }))}`（链路保持）。
- `served = served.get(frame.hold)` 无 → 回 `{t:"result", id, error: toWireError(ChordError("gone","unknown hold"))}`。
- `controller = new AbortController()`；`incoming.set(id, controller)`。
- `root = peer.kind === "host" ? #admitRoot(frame.root) : undefined`（client 的 call 不带 root：本宿主起 root）。
- `principal = served.kind === "local" ? served.principal : served.relay.principal`；`options = {principal, signal: controller.signal}`。
- `run = served.kind === "local" ? (ctx) => served.ref.object.call(served.ref, frame.member, unpack(frame.args), ctx) : (ctx) => relayCall(served.relay, frame, {rootId: ctx.rootId, codeVersion: ctx.codeVersion}, ctx.signal, host)`。
- `result = root === undefined ? host.root(run, options) : host.joinRoot(root.rootId, root.codeVersion, run, {...options, from: this})`。
- `result.then(value => send({t:"result",id,value}), error => send({t:"result",id,error:toWireError(error)})).finally(() => incoming.delete(id))`。

`#admitRoot(root)`：root 为 undefined、或 role 不是 downstream、或 `!fencedCodeVersions.has(root.codeVersion)` → 原样返回。若 `host.hasCodeVersion(root.codeVersion)` → 原样。否则查 `#admitted`：无则取 `host.current.id` 并记录；返回 `{rootId, codeVersion: admitted}`。（子 host 落后被 fenced 时，其旧 code version 的 root 在本宿主当前 code version 下被接纳；一次决定，后续沿用；该 version 被 retire 后 → superseded。）

#### 2.4.11 hold 回复 / result / state（consumer 端）

`#onHoldReply(frame)`：先查 `relayedRefs.get(id)` → `gone` → `relay.close(fromWireError(error), {releaseDown:false, notify:true})`；`shape` → `relay.shape(frame)`。否则查 `remotes.get(id)`：`gone` → 删除并 `binding.gone(fromWireError(error))`；`shape` → `binding.shaped({variant, members, contract}, frame.state)`。

`#onResult(frame)`：先查 `relayedCalls.get(id)` → `finish(error === undefined ? undefined : fromWireError(error), value)`。否则 `pending = calls.get(id)`，删除；有 error → reject(fromWireError)；否则 `unpack(frame.value)`（抛错 → reject 该错）并 resolve。

`#onState(frame)`：先查 `relayedRefs.get(hold)` → `relay.state(frame)`。否则 `binding = remotes.get(hold)`；无则返回；`frame.value` 非 Packed → `binding.state(m, frame)`；是 Packed → `unpack`（抛错 → `binding.state(m, {as:"error", error: toWireError(error)})`）后 `binding.state(m, {...frame, value})`。

#### 2.4.12 关闭

`#closed(reason)`：
- `reason` 匹配 `/^protocol-version (\d+)$/` 且数字 != PROTOCOL_VERSION → `#protocolMismatch(n, false)`（对端先发现；不再次关闭）。
- 清 pinger、ackTimer、`#pending`、`#pendingBytes`。
- `error = unavailable(`link to ${name ?? "peer"} closed: ${reason}`)`。
- 作为 origin：所有 served `#dropServed(id, true)`；所有 incoming `abort(error)` 并清空；所有 calls `reject(error)`；votes reject；syncWaiters reject。
- 作为下游 relay：所有 relayedRefs `relay.close(error, {releaseDown:false, notify:true})`；清 relayedRefs/relayedCalls。
- 所有 remotes：role === "upstream" → `binding.detached()`（可能重连，保留值）；否则 `binding.gone(error)`。
- `host.connectionClosed(this)`。

#### 2.4.13 辅助函数

- `stateKey(hold, m) = `s${hold}\u0000${m}``；`unitKey(hold) = `u${hold}``。
- `relayCall(relay, frame, root, signal, host)`：
  - `abortError = () => ChordError("aborted","call cancelled",{cause: signal.reason})`；已 aborted → 抛。
  - `aborted` promise（reject abortError）+ `.catch(noop)`。
  - `down = await Promise.race([relay.ready(), aborted])`；`relay.closed` → `ChordError("gone","relayed hold closed")`。
  - 返回 new Promise：`new RelayCall(relay, frame.id, down.nextId(), {resolve, reject})`；abort 时 `call.cancel(abortError(), true)`；`host.rootSent(root.rootId, down)`；`down.send({t:"call", id: call.downId, hold: relay.downId, member: frame.member, args: frame.args, root})`。
  - finally 移除两个 abort 监听。
- `validateSnapshot(ref, member, value)`：`value === undefined` → 返回；无 currentInstalled → 返回；`view = ref.currentView()`；`variant = view === undefined ? variantOf(installed.implementation) : variantOf(view)`；description 选择：若 view 存在且不是直接实现（经过 adapter）→ `ref.contract.description.variants[variant ?? ""]?.[member]`（consumer 契约）；否则 → `installed.service.contract.description.variants[variantOf(installed.implementation) ?? ""]?.[member]`（provider 契约）。若 kind === "state" → `assertValid("state", `${ref.address}.${member}`, description.schema, value)`。
- `clientError(error)`：ChordError 且 code contract_mismatch → `new ChordError("upgrade_required", error.message, { cause: error })`；否则原样。

### 2.5 `src/host.ts`（1303 行）

#### 2.5.1 Facet 与选项

```ts
export interface Serve<P = unknown> { <C extends AnyContract>(type: C, factory: Factory<C, P>, options?: ServeOptionsFor<C>): void }
export interface FacetApi<P = unknown> {
  readonly serve: Serve<P>;
  readonly name: string;
  readonly root: <T>(fn: (ctx: Ctx<P>) => T | Promise<T>, options?: RootOptions<P>) => Promise<T>;
  readonly registry: <V>(registry: Registry<V>) => RegistryView<V>;
  readonly signal: AbortSignal;
}
export interface FacetDefinition<P = unknown> { readonly uses?: readonly AnyContract[]; setup(api: FacetApi<P>): void }
export interface Facet<P = unknown> extends FacetDefinition<P> { readonly name: string }
export const defineFacet = <P = unknown>(
  setup: (api: FacetApi<P>) => void,
  options: { readonly uses?: readonly AnyContract[] } = {},
): FacetDefinition<P> => ({ setup, uses: options.uses });
export type FacetLoader = (modules: readonly (string | URL)[]) => Promise<Facet[]>;
export type PluginRef<P = unknown> = Facet<P> | string | URL;
```

`isFacet(ref)`：`typeof ref === "object" && !(ref instanceof URL)`。

`FacetApi.signal`：该次 facet 运行被替换/移除/reload 中止/host 关闭时 abort（消息见 facetRunChanged）。

#### 2.5.2 HostOptions（默认值全部逐字）

```ts
export interface HostOptions<P = unknown> {
  readonly name?: string;                   // 默认 "host"
  readonly self: NoInfer<P>;                // 必需；缺失 -> TypeError("createHost needs `self`: the principal of the host's own code")
  readonly principalKey?: (principal: P) => string;   // 默认 canonicalJson
  readonly dev?: boolean;                   // 默认 false
  readonly onError?: (error: unknown) => void;        // 默认 error => console.error("chord:", error)
  readonly taskStopDeadline?: number;       // 默认 5000
  readonly readyTimeout?: number;           // 默认 60000
  readonly linger?: { readonly types?: Readonly<Record<string, number>>; readonly cap?: number };  // cap 默认 4，types 默认 {}
  readonly loader?: FacetLoader;
  readonly kind?: "host" | "client";        // 默认 "host"
  readonly upstream?: { readonly link: Link } & ConnectOptions<P>;
}
export interface ConnectOptions<P> extends LinkOptions { readonly principal: P }
export interface Route<P> {
  authorize(principal: P, hostName: string): boolean | Promise<boolean>;
  resolve(hostName: string): Promise<Link>;
  readonly principal: (hostName: string) => P;
}
export type PrincipalPattern<P> = P extends object ? { readonly [K in keyof P]?: P[K] } : Readonly<Record<string, unknown>>;
export type PrincipalMatch<P> = ((principal: P) => boolean) | PrincipalPattern<P>;
export interface RootOptions<P> { principal?: P; signal?: AbortSignal; codeVersion?: number }
export interface LoadOptions { readonly awaitHostObjects?: boolean }   // 默认 true
```

#### 2.5.3 `Host<P>` 接口（用户可见 API）

```ts
export interface Host<P = unknown> {
  readonly name: string;
  readonly codeVersion: number;
  readonly previousCodeVersion: number | undefined;
  readonly serve: Serve<P>;
  install(facet: Facet<P>, options?: LoadOptions): Promise<void>;
  load(plugins: readonly PluginRef<P>[], options?: LoadOptions): Promise<void>;
  ready(): Promise<void>;
  get<C>(type: C, input: InputOf<C>, options?: GetOptions<P> & { signal?: AbortSignal }): Promise<Ref<C>>;
  hold<C>(type: C, input: InputOf<C>, options?: GetOptions<P>): Ref<C>;
  root<T>(fn: (ctx: Ctx<P>) => T | Promise<T>, options?: RootOptions<P>): Promise<T>;
  registry<V>(registry: Registry<V>): RegistryView<V>;
  invalidatePrincipal(match: PrincipalMatch<P>): Promise<void>;
  accept(link: Link, options: AcceptOptions<P>): void;
  connect(link: Link, options: ConnectOptions<P>): void;
  route(prefix: string, route: Route<P>): void;
  reload(plugins: readonly PluginRef<P>[], options?: ReloadOptions): Promise<ReloadReport>;
  close(): Promise<void>;
}
export interface CodeVersionState {
  readonly id: number;
  readonly services: Map<string, Service>;
  tables: ReadonlyMap<string, RegistryTable<unknown>>;
  readonly roots: Map<string, number>;       // 运行中的 root -> 在本宿主上运行的调用数
  readonly abort: AbortController;           // 强制 retirement 时中止该 code version 的所有 root
}
const EMPTY_TABLE: RegistryTable<never> = new Map<string, never>();   // 类型参数 <string, never>，不是裸 new Map()
```

`HostImpl implements Host, HostCore` 关键字段：`name`, `dev`, `principal`, `inFactory = 0`, `failed: unknown`, `objects: Map<address, ObjectImpl>`, `codeVersions: Map<id, CodeVersionState>`, `facets: Map<name, Facet>`, `hostObjects: Map<typeName, RefCore>`, `kind`, `connections: Set<Connection>`, **`upstream: Connection | undefined`（公开字段，可读写；`connect()` 赋值、`connectionClosed` 置 undefined）**, `bindings: Map<bindingKey, RemoteBinding>`, `#lingering: Map<string, RemoteBinding>`, `#lingerTypes`, `#lingerCap`, `#routes: Map<prefix, Route>`, `#childReloads: ChildReloads`, `#foreign: Map<rootId, Set<Connection>>`, `#local: Set<rootId>`, `#incarnation = Math.random().toString(36).slice(2, 8)`（**至多 6 个 base36 字符**：`Math.random()` 的尾数位数不定，如 `0.5` 得到 `"i"`——见 A3，协议不校验长度）, `#rootLinks: Map<rootId, Set<Connection>>`, `taskStopDeadline`, `readyTimeout`, `#principalKey`, `#selfKey`, `#retirementWatchers`, `#deferredHellos: Map<Connection, number>`, `#reloadWaiters: (() => void)[]`, `#facetRuns: Map<name, AbortController>`, `#onError`, `#loader`, `#dying: Map<address, ObjectImpl>`, `#touches: Map<rootId, Set<ObjectImpl>>`, `#order: Map<registryName, Map<key, number>>`, `#subscribers: Map<registryName, Set<listener>>`；还有（声明的公开字段）`current`, `previous`, `pending`, `reloading`, `gating: Set<string> | undefined`, `remoteModules: Record<string, string[]>`, `remoteRemoved`, `codeVersion` getter 等。

构造顺序：`name = options.name ?? "host"`；`dev = options.dev ?? false`；`"self" in options` 检查（`TypeError`）；`principal = options.self`；`#principalKey = options.principalKey ?? canonicalJson`；`#selfKey = #principalKey(self)`；`#onError`；`#loader`；`taskStopDeadline ?? 5000`；`readyTimeout ?? 60000`；`#lingerTypes ?? {}`；`#lingerCap ?? 4`；`#codeVersionSeq = 1`（**字段初值即 1**）、`#rootSeq = 0`、`#originSeq = 0`、`#retireAllowed = 0`；`current = {id: #codeVersionSeq(=1), services:new Map(), tables:new Map(), roots:new Map(), abort:new AbortController()}`；`codeVersions.set(1, current)`（**初始 code version 是 1，不是 0**）；`kind = options.kind ?? "host"`；有 upstream → `connect(upstream.link, upstream)`。

#### 2.5.4 方法逐个

- `isSelf(principal)`：**公开方法**（不是私有）：`principal === self || #principalKey(principal) === #selfKey`。
- `get codeVersion()`：`current.id`；`get previousCodeVersion()`：`previous?.id`。
- `nextCodeVersionId(given?)`：`given !== undefined && given <= current.id` → `throw new Error(`code version ${given} is not newer than ${current.id}`)`；`#codeVersionSeq = given ?? #codeVersionSeq + 1`；返回。
- `report(error)`：`#onError(error)`（onError 自身抛错会外泄——不捕获）。
- `serve`：`reloading` → `throw new Error("host.serve during a reload; serve from a facet passed to reload()")`；`serveInto(current.services, makeService(type, factory, options, ""))`；`options.lifetime === "host"` → `startHostObject`。
- `install(facet, options={})`：
  - `reloading` → `Error("host.install during a reload")`；`facets.has(facet.name)` → `Error(`facet ${facet.name} is already installed; use reload()`)`。
  - `run = new AbortController()`；`facet.setup(facetApi(serve-wrapper, run.signal))`，wrapper 为每个 serve 创建 service 并 `serveInto(current.services, service)`，加入 `added`；setup 抛错 → `run.abort(error)` 并重抛。
  - `facets.set`；`facetRunChanged(name, run)`；`refs = added.filter(hostLifetime).map(startHostObject)`。
  - `options.awaitHostObjects === false` → `Promise.resolve()`；否则 `#hostObjectsReady(refs)`。
- `load(plugins, options={})`：`facets = await resolve(plugins)`；`await Promise.all(facets.map(f => install(f, options)))`（**install 之间无 await**：host object 工厂可以消费后续 facet serve 的类型；每个 install 内部同步执行 setup）。
- `ready()`：`#hostObjectsReady(hostObjects.values())`（**调用时刻的快照**，不 acquire 新引用，不是持续健康检查）。
- `#hostObjectsReady(refs)`：`Promise.all([...refs].map(r => r.ready)).then(() => {})`；`ready.catch(() => {})`（忽略返回值时也经 onError 报告，不产生 unhandled rejection）；返回 ready。
- `resolve(plugins)`：`modules = plugins.filter(非 facet)`；有 modules 且无 loader → `Error("loading modules needs createHost({ loader }), e.g. nodeLoader from @earendil-works/chord/node")`；`loaded = modules.length > 0 ? await loader(modules) : []`；**按原顺序**映射：facet 原样，字符串/URL 依次从 loaded 取（`loaded.shift()`）。
- `facetApi(serve, signal)`：`{serve, name: this.name, root: (fn, options) => this.root(fn, options), registry: r => this.registry(r), signal}`。
- `facetRunChanged(name, run?)`：取旧 run；undefined → 删除，否则设置；`replaced !== run` → `replaced?.abort(ChordError("gone", `facet ${name} was replaced or removed`))`。
- `serveInto(services, service)`：`services.has(name)` → `Error(`${name} is already served`)`；`placement = description.host`；存在且 `!placedOn(placement, this.name)` → `TypeError(`${name} is declared for ${placementText(placement)}, but ${this.name} serves it`)`；`services.set`。
- `startHostObject(service)`：`ref = acquire(service.contract, {}, undefined, "host")`；`hostObjects.set(name, ref)`；`ref.ready.catch(report)`；返回。
- `currentCodeVersion()`、`hasCodeVersion(id)`、`pendingCodeVersion()`。
- `*retainedCodeVersions()`：按 Map 插入序 yield 每个 `[id, state.services]`，再 yield pending（若有）。
- `service(typeName)`：`current.services.get ?? pending?.services.get`（reload 中添加的类型从 pending 提供）。
- `addressOf(type, input, principal, candidate=false)`：`#elsewhere` → 远程 key；否则 `#bindingService(name, candidate)` 无 → `unknownType(name, this.name)`；`#locate(...).address`。
- `#elsewhere(type, input)`：仅 remote 且有 host；`normalized = normalize(type.input, input, `${type.name} input`)`；`at = resolveHost(name, placement, normalized)`；`at === this.name` → undefined；否则 `{at, input: normalized}`。
- `#locate(service, input, principal)`：`normalized = normalize(schema, input, `${name} input`)`；`identity = canonicalIdentity(schema, normalized)`；hostLifetime：若 `identity !== canonicalIdentity(schema, normalize(schema, {}, `${name} input`))` → `invalid("input", `${name} has lifetime "host": its one object has the empty input`)`；`principal = this.principal`；返回 `{address: `${name}\u0000${identity}\u0000${#principalPart(principal)}`, normalized, principal}`。
- `#principalPart(principal)`：`isSelf(principal) ? "host" : `p:${#principalKey(principal)}``。
- `#remoteKey(type, normalized, principal)`：`@${type.name}\u0000${canonicalIdentity(type.input, normalized)}\u0000${#principalPart(principal)}\u0000${type.fingerprint}`（**无 host 名**：类型与输入决定它住哪）。
- `acquire(type, input, options, owner, from?, candidate=false)`：
  - `#elsewhere` → `#acquireRemote`。
  - `service = #bindingService(name, candidate)`；无 → `unknownType`。
  - `bind(service.contract, service.options.accepts, type)`（不匹配 → contract_mismatch）。
  - `{address, normalized, principal} = #locate(service, input, options?.principal ?? this.principal)`。
  - `placement = service.contract.description.host`；存在且 `resolveHost(...) !== this.name` → `invalid("placement", `${type.name} with this input lives on ${resolveHost(...)}, not ${this.name}`)`（provider 自己的 placement 决定，绝不按请求说的地方）。
  - `object = objects.get(address)`；若 `from !== undefined && object !== undefined && (object === from || reaches(object, from))` → `invalid("cycle", `${from.address} -> ${address} would create a ref cycle`)`。
  - 若 object 不存在：`gating?.has(type.name)` → `ChordError("upgrade_conflict", `${type.name} is being upgraded; retry`, { reason: "gate" })`；`dying = #dying.get(address)`，若已 disposed 则删除记录；`object = new ObjectImpl(this, {address, typeName: type.name, input: normalized, principal, hostLifetime: hostLifetime(service)})`；`objects.set`；`object.start(dying?.status === "disposing" ? dying.dispose(false) : undefined)`。
  - `ref = new RefImpl(object, type, owner)`；`object.addRef(ref)`；返回 ref。
- `forget(object)`：若 objects 中正是它 → 删除；`status === "disposing"` → `#dying.set(address, object)`。
- `disposed(object)`：若 `#dying` 中正是它 → 删除。
- `touch(rootId, object)`：`#touches` 集合加入。
- `root(fn, options={})`：`rootId = `${name}#${#incarnation}:${++#rootSeq}``；`#local.add`；`joinRoot(rootId, options.codeVersion ?? current.id, fn, options)`。
- `joinRoot(rootId, codeVersionId, fn, options {principal?, signal?, from?})`：
  - `codeVersion = codeVersions.get(id)`；无 → `#local.delete(rootId)`，抛 `ChordError("superseded", `code version ${codeVersionId} is not retained by ${this.name}`)`。
  - `options.from !== undefined && !#local.has(rootId)` → 把 from 加入 `#foreign[rootId]` 集合。
  - `codeVersion.roots.set(rootId, (get ?? 0) + 1)`。
  - `ctx = new CtxImpl(this, { signals: options.signal === undefined ? [codeVersion.abort.signal] : [options.signal, codeVersion.abort.signal], rootId, codeVersion: id, principal: options.principal ?? this.principal })`。
  - `try { return await fn(ctx) } finally { ctx.close(); count = roots.get(rootId)-1; count > 0 ? set : (roots.delete; if (!#foreign.has(rootId)) endRoot(rootId); maybeRetire(); #retirementChanged()) }`。
- `rootSent(rootId, connection)`：`#rootLinks[rootId].add(connection)`。
- `endRoot(rootId)`：`#local.delete`；`#foreign.delete`；`#touches[rootId]` 中每个 object `rootEnded(rootId)`；删除 touches；`#rootLinks[rootId]` 中每个 connection 发 `{t:"root_end", rootId}`；删除 rootLinks。
- `foreignRootEnded(rootId, from)`：`links = #foreign.get`；无或 `!links.delete(from) || links.size > 0` → 返回；若任一 codeVersion.roots 还有该 rootId（调用在飞）→ `#foreign.delete(rootId)`（租约等调用结束）；否则 `endRoot(rootId)`。
- `maybeRetire()`：`retirable()` → `retire()`。
- `retirable()`：previous 无或 roots 非空 → false；`kind === "client" || upstream === undefined || previous.id <= #retireAllowed`。
- `allowRetire(codeVersion)`：`#retireAllowed = max(...)`；`maybeRetire()`；`#retirementChanged()`。
- `watchRetirement(watcher)`：加入/删除。
- `#retirementChanged()`：调用所有 watcher（快照）。
- `announceRetired(connection)`：`retired = (previous ?? current).id - 1`；`> 0` → 发 `{t:"retire", codeVersion: retired}`。**调用点有 3 处**（详见 A7）：`peerHello` 的已同步分支、`cluster.reconcileChild` 的已同步分支、以及 `reconcileChild` 发 `prepare` **之前**。
- `retire()`：无 previous 返回；`previous = undefined`；`codeVersions.delete(previous.id)`；对所有 objects `retireImplementation(object, id)`；所有 downstream host 连接发 `{t:"retire", codeVersion}`；`#retirementChanged()`。
- `retireImplementation(object, codeVersion)`：`installed = implementations.get`；删除；`installed === undefined || installed.decls === object.decls` → 返回；`kept = object.allDecls()`；`refs/resources/derives` 为所有保留声明中的集合；`installed.decls.derives` 中不在 derives 的 detach；`closing = installed.decls.resources 中不在 resources 的，按 openOrder 逆序`；`releasing = installed.decls.refs 中不在 refs 的`；`object.refreshSchemas()`；`runAll([...closing.map(r => () => r.close(openOrder)), ...releasing.map(r => () => r.release())], `${address}: retiring code version ${codeVersion} failed`).catch(report)`。
- `registry(registry)`：`table(ctx?)`：`ctx === undefined ? current : codeVersions.get(ctx.codeVersion)`；无 → `ChordError("superseded", `code version ${ctx?.codeVersion} is retired`)`；返回 `tables.get(name) ?? EMPTY_TABLE`。view = `{table, get: (key, ctx) => table(ctx).get(key), subscribe(listener)}`；subscribe：加入 `#subscribers[name]`；**立即同步**以当前表与 current.id 调用一次；返回删除函数。
- `buildTables(entries)`：`byRegistry: Map<registryName, Map<key, {value, object}>>`；按 entries 顺序；冲突：`existing !== undefined && existing.object !== object && registry.conflict === "error"` → `invalid("registry_conflict", `${registry.name}: ${key} registered by ${existing.object.address} and ${object.address}`)`；`entries.set(key, {value, object})`（`"last"` 时后注册覆盖）；`#order` 记录 key 首次出现序号。最后每个 registry 按 order 排序构造 `new Map(sorted)`。
- `#bindingService(typeName, candidate)`：`(candidate ? pending?.services.get : undefined) ?? service(typeName)`。
- `liveDecls(candidates=false)`：objects 中 `status === "live" && (candidates || !deferredRuntime)` → `[object, object.decls]`。
- `checkRegistrations(object, decls)`：registrations 为空返回；`buildTables([...liveDecls().filter(([o]) => o !== object), [object, decls]])`（冲突抛错）。
- `registriesChanged(including?)`：`entries = liveDecls().filter(o => o !== including)`；including 存在 → push `[including, including.decls]`；`installTables(current, buildTables(entries))`。
- `fail(error)`：`failed ??= error`；`report(error)`。
- `installTables(codeVersion, tables, all=false)`：保存旧表；`codeVersion.tables = tables`；非 current → 返回；对每个 `#subscribers` 项：`next = tables.get(name) ?? EMPTY_TABLE`；`!all && sameTable(before.get(name) ?? EMPTY_TABLE, next)` → 跳过；**enqueue** 后逐个回调（`(next, codeVersion.id)`），抛错 report。
- `get(type, input, options?)`：`ref = acquire(type, input, options, "external")`；`await awaitReady(ref, options?.signal ? [options.signal] : [], readyTimeout)`；返回 proxy。
- `hold(type, input, options?)`：`acquire(..., "external").proxy`（同步）。
- `invalidatePrincipal(match)`：
  - `matches = typeof match === "function" ? match : p => matchesPattern(match, p)`；`reason = ChordError("gone","principal invalidated")`。
  - 对每个 downstream 且 `matches(connection.principal)` 的连接：`connection.revoke(connection.restamp?.(connection.principal))`（重新盖章或关闭；授权代次立即提升，relay 关闭）。
  - 若 match 是 pattern（非函数）：对所有 downstream host 连接发 `{t:"invalidate", pattern: match}`（传播）。
  - `matching = objects 中 !isSelf(principal) && matches(principal)`；`await disposeHoldersFirst(matching, reason, "invalidatePrincipal: disposal failed")`。
- `accept(link, options)`：`kind = options.kind ?? "client"`；`kind === "host" && options.name === undefined` → `TypeError("accepting a host link needs its name")`；`#addConnection(link, {...options, role:"downstream", principal, peer: {kind, name: kind === "host" ? options.name : undefined}, ...(kind === "client" ? {originId: `${#incarnation}:${++#originSeq}`} : {})})`。
- `connect(link, options)`：缺少 `principal` 键 → `TypeError("connect needs `principal`: what the parent's own requests act as")`；`upstream?.open` → 旧链路 `close("replaced")`；新建 upstream connection（`peer: {kind:"host", name: undefined}`）；`upstream = connection`；**遍历 bindings**：`hold === undefined && !ended` → `connection.hold(binding, binding.applied)`（重连后重 hold，`again` 表示保留值）。
- `#addConnection(link, config)`：`new Connection(...)`；仅 `connection.open` 时加入 `connections`（已在构造中关闭的链路不保留——否则会永远路由到它）。
- `route(prefix, route)`：`typeof route.principal !== "function"` → `TypeError(`route ${prefix} needs \`principal\`: what the routed hosts act as when they call back`)`；`#routes.set`。
- `peerHello(connection, codeVersion)`：
  - `codeVersion > current.id` → `report(ChordError("invalid", `${connection.name} runs code version ${codeVersion}, ahead of ${current.id}`))`（无 reason）；`connection.link.close("code version ahead of parent")`；返回。
  - `reloading` → `connection.peerCodeVersion = codeVersion`；落后则 `fencedCodeVersions.add(codeVersion)`；`#deferredHellos.set(connection, codeVersion)`；返回（reload 结束时决定）。
  - `codeVersion === current.id` → `markSynced`；`announceRetired`。
  - 否则：`peerCodeVersion = codeVersion`；`fencedCodeVersions.add`；`reconcileChild(this, connection).catch(e => { report(e); connection.link.close("reconciliation failed") })`（reconcileChild 见 cluster 单元）。
- `reloadEnded()`：`reloading = false`；处理 deferred hellos（仍 open 的重新 peerHello）；解析 reloadWaiters；`maybeRetire()`。
- `whenReloadEnded()`：非 reloading → resolved；否则加入 waiters。
- `connectionClosed(connection)`：从 connections 删除；删除 deferredHellos；对 `#foreign` 中该连接：`foreignRootEnded(rootId, connection)`；所有 `#rootLinks` 删除该连接；若它是 upstream：`upstream = undefined`；`#childReloads.parentLost()`。
- `bindingIdle(binding)`：`ms = Object.hasOwn(#lingerTypes, contract.name) ? #lingerTypes[name] : 0`；`ms <= 0 || binding.ended || !binding.applied || #lingerCap <= 0` → `binding.close()`；否则 `lingerTimer = setTimeout(() => binding.close(), ms)`（unref）；`#lingering.set(key, binding)`；然后遍历 lingering（插入序）若 size > cap → `oldest.close()`（最少最近释放的先走）。**注意 `!binding.applied` 这一条**：从未拿到过 shape 的绑定（对象从未 live）**永不 linger**，直接关闭。复活路径的顺序是 `#acquireRemote` 里先 `#lingering.delete(key)`、再 `binding.addView(ref)`（`addView` 只清 `lingerTimer`），所以复活后该 binding **不再**占 cap 名额；`binding.close()` 最后经 `host.forgetBinding(this)` 同时从 `bindings` 与 `#lingering` 删除（仅当是同一对象）。Rust 侧用 `IndexMap::shift_remove`/`remove` 均可，但必须保持"插入序 = 淘汰序"。
- `forgetBinding(binding)`：从 bindings 与 lingering 中按 key 删除（仅当同一对象）。
- `#childNamed(name)`：connections 中 `open && role === "downstream" && peer.kind === "host" && peer.name === name` 的第一个（**只认配置名，不认 peer 自称**）。
- `#acquireRemote(type, at, input, options, owner)`：`principal = options?.principal === undefined || isSelf(options.principal) ? undefined : options.principal`；`key = #remoteKey(type, input, principal ?? this.principal)`；`binding = bindings.get(key)`；`fresh = binding === undefined || binding.ended`；fresh → 新建 `RemoteBinding(this, {key, contract: type, at, input, principal})` 并 set；`ref = new RemoteRef(binding, owner, options?.state !== false)`；`#lingering.delete(key)`；`binding.addView(ref)`；fresh → `#hold(binding)`；返回 ref。
- `#hold(binding)`：
  - `bindWhenSynced(connectionPromise)`：resolve 时若 `!binding.ended && binding.hold === undefined` → `target.hold(binding, false)`；reject → `binding.gone(error)`。
  - `direct = #childNamed(binding.at)`；
  - `upstream !== undefined` → `upstream.hold(binding, false)`；
  - 否则 `direct !== undefined` → `bindWhenSynced(direct.whenSynced().then(() => direct))`；
  - 否则 `route = #routeFor(binding.at)`；无 → `binding.gone(ChordError("unavailable", `no route to ${binding.at}`))`；有 → `bindWhenSynced(#resolve(route, at, binding.principal ?? this.principal))`。
  - **注意**：upstream 存在时直接 hold（即使有 direct child）；不做 whenSynced（上游 connection 立即用；若未 synced 则……按 TS 原样实现）。
- `#routeFor(hostName)`：`#routes` 中 prefix 匹配且最长者（**严格更长才替换**，即最早注册的同长度胜出）。
- `#resolve(route, hostName, principal, still = () => true)`：
  - `refuse = () => ChordError("forbidden", `${hostName} refused for this principal`)`；`revoked = () => ChordError("gone", "principal invalidated while resolving")`。
  - `await route.authorize(principal, hostName)` false → refuse；`!still()` → revoked；`link = await route.resolve(hostName)`；`!still()` → revoked；再 authorize 一次；`!still()` → revoked。
  - 找 `connections` 中 `link === link` 的连接；无则 `#addConnection(link, {role:"downstream", principal: route.principal(hostName), peer:{kind:"host", name: hostName}})`；`await connection.whenSynced()`；`!still()` → revoked；返回。
- `relay(relay)`：`at = relay.frame.at`；`placement = contractFromDescription(relay.frame.token).description.host`；`placement === undefined || resolveHost(token.name, placement, frame.input) !== at` → `invalid("placement", `${token.name} with this input does not live on ${at}`)`；`route = #routeFor(at)`；无 → `#childNamed(at) === undefined ? ChordError("unavailable", `no route to ${at}`) : ChordError("forbidden", `no route authorizes ${at}`)`；`down = await #resolve(route, at, relay.principal, () => relay.current)`；`!relay.current` → 返回；`relay.open(down)`。
- `reload(plugins, options={})`：`kind !== "client" && upstream !== undefined` → reject `ChordError("unavailable", "reloads are coordinated by the server", { reason: "not_coordinator" })`；`children = 存在 downstream host 连接`；`children || options.remote !== undefined || options.remoteRemove !== undefined` → `runClusterReload(...)`，否则 `runReload(...)`。
- `onReloadFrame(connection, frame)`：`#childReloads.handle(connection, frame)`。
- `rememberRemoteModules(remote, remove)`：prefix 为两者键的并集（Set 插入序）：`modules = remote?.[prefix] ?? []`，`removed = remove?.[prefix] ?? []`；`remoteModules[prefix] = [...new Set([...旧, ...modules])].filter(m => !removed.includes(m))`；`remoteRemoved[prefix] = [...new Set([...旧, ...removed])].filter(m => !modules.includes(m))`。
- `peekNextCodeVersion()`：`#codeVersionSeq + 1`。
- `#retireAllowed`（`#retireAllowed = 0`）：`allowRetire(codeVersion)` 取 `max`；`retirable()` 要求 `previous.id <= #retireAllowed`（client 或没有 upstream 时不看它）。
- 其余字段：`inFactory = 0`（factory 深度，`#caller`/ref 调用据此拒绝）、`failed: unknown`（`fail()` 记录的第一个错误，`ObjectImpl.guard` 检查）、`remoteModules: Record<string, string[]> = {}`、`remoteRemoved: Record<string, string[]> = {}`、`#originSeq = 0`。
- `close()`：
  - 所有 lingering `binding.close()`；`hostObjects.clear()`；所有 `#facetRuns` `facetRunChanged(name, undefined)`（abort）。
  - `reason = ChordError("gone", `host ${name} closed`)`；`try { await disposeHoldersFirst([...objects.values()], reason, "host close: disposal failed") } finally { 所有 connections link.close("host closed") }`（**对象先于链路**）。
- `disposeHoldersFirst(objects, reason, message)`（模块级）：
  - `remaining = Set(objects)`；`held(object)` = 所有 decls 集中 `ref.object !== undefined` 的目标集合；`refs = Map(object -> held)`。
  - 循环：`heldByRemaining` = remaining 中对象所持有的所有目标；`wave = remaining 中不被任何剩余对象持有的`；空（环）→ wave = 全部 remaining；从 remaining 删除 wave；`Promise.allSettled(wave.map(o => o.dispose(true, reason)))`；收集错误。
  - 结束：1 个错误抛出；>1 → AggregateError(message)。

#### 2.5.5 模块级辅助

- `sameTable(a, b)`：size 相同；键顺序逐一相同且值引用相同（`keys[index] === key && a.get(key) === value`）。
- `matchesPattern(pattern, principal)`：principal 非 null 对象；pattern 每项 `deepEqual(principal[key], value)`。
- `unknownType(type, host)`：`ChordError("unavailable", `${type} is not served by ${host}`, { reason: "unknown_type" })`。
- `reaches(from, to)`：DFS（栈）沿对象拥有的引用（decls.refs 中 `ref.object`），有环保护，命中 to → true。

### 2.6 `src/reload.ts`（723 行）

#### 2.6.1 选项与报告

```ts
export interface ReloadOptions {
  readonly remove?: readonly string[];
  readonly gateDeadline?: number;      // 默认 5000
  readonly retireDeadline?: number;    // 默认 5000
  readonly remote?: Readonly<Record<string, readonly string[]>>;
  readonly remoteRemove?: Readonly<Record<string, readonly string[]>>;
  readonly voteDeadline?: number;      // server：默认 gate + retire + 5000
}
export interface ReloadReport {
  readonly codeVersion: number; readonly previous: number;
  readonly changed: readonly string[]; readonly removed: readonly string[];
  readonly prepared: readonly string[]; readonly gated: readonly string[];
  readonly children?: Readonly<Record<string, ReloadReport>>;
}
interface Candidate {
  object; service; run; reasons: string[]; atGate: ResourceImpl[]; replaced: ResourceImpl[];
  migrated: Map<Cell, unknown>; derived: Map<unknown, unknown>; staged: StagedListeners | undefined;
}
```

#### 2.6.2 辅助函数

- `dryRun(candidate)`：`exposed = memberSchemas(service, run.implementation)`；对每个 `[cell, value] of migrated`：对 `exposed.get(cell) ?? []` 每个 schema `assertValid("state", `${address}: migrated state ${cell.key}`, schema, value)`。`values = new Map(migrated)`；`derives = [...run.decls.derives].sort(([,a],[,b]) => a.seq - b.seq)`（按创建序号）；对每个：`value = derived.compute(source => values.has(source) ? values.get(source) : candidateValue(source))`；对 exposed.get(derived) 每个 schema `assertValid("state", `${address}: derivation ${name}`, schema, value)`；`values.set(derived, value)`。`candidate.derived = values`。
- `candidateValue(source)`：`source.value`；抛 not_ready / not_in_variant → `undefined`；其它抛。
- `reloadError(error)`：`cause = asChordError(error, "reload")`；若 `cause.message` 去掉前缀（`replace(/^[^:]*: /, "")`）后以 `"reload aborted"` 开头 → 返回 cause；否则 `new ChordError(cause.code, `reload aborted: ${cause.message}`, {cause, reason: cause.reason, details: cause.details})`。
- `runReload(host, plugins, options)`：`reload = await beginReload(...)`；`try { await reload.prepare(); return reload.commit() } finally { endReload(host) }`。
- `beginReload(host, plugins, options, codeVersion?)`：`host.reloading` → `ChordError("unavailable", "a reload is already running", { reason: "reload_running" })`；`host.reloading = true`；`try { return new Reload(host, await host.resolve(plugins), options, codeVersion) } catch (e) { host.reloadEnded(); throw reloadError(e) }`。
- `endReload(host)`：`host.reloadEnded()`。
- `ABORTED_ROOTS_GRACE = 1000`。
- `until(host, ready, ms)`：ready 立即 true；否则 `setTimeout(ms)` → false；`host.watchRetirement` 每次检查，true 时 clear+stop+resolve(true)。

#### 2.6.3 `Reload` 类

字段：`host`, `facets`, `options`, `codeVersion`（server 发放的 id；本地 reload 为 undefined → 自分配），`candidates = Map<ObjectImpl, Candidate>`, `abort = AbortController`, `closed: [ObjectImpl, ResourceImpl][]`, `services = Map`, `changed = Set`, `removed = Set`, `tables = Map`, `gated: Candidate[]`, `rolledBack = false`, `deferred = Map<ObjectImpl, StagedListeners>`, `facetRuns = Map<string, AbortController>`。

`prepare()`（全部阶段，失败即 rollback + reloadError）：
1. `await waitForRetirement()`。
2. `load()`。
3. `checkProviders()`。
4. `await prepareAll()`。
5. `host.gating = this.changed`。
6. `await withDeadline(this.prepareAll(), "activations in flight did not finish")`（第二次 prepareAll 到不动点：期间新激活的对象也准备）。
7. `checkRefs()`。
8. `tables = host.buildTables(entries())`。
9. `await gate()`；`#alive()`；`#stage()`。

`summary()`（child 的 vote 内容）：`live = candidates 中 object.status === "live"`；`{codeVersion: host.pending?.id ?? host.current.id, previous: host.current.id, changed, removed, prepared: live.map(address), gated: gated.map(address)}`。

`#alive()`：`rolledBack` → `ChordError("aborted", "reload aborted while preparing", { reason: "aborted" })`。

`#stage()`：对每个 candidates 中 `object.status === "live"` 的 → `candidate.staged = object.stageListeners(candidate.run.decls)`；对所有 `host.objects` 中 `status === "live" && deferredRuntime` → `deferred.set(object, object.stageListeners(object.decls))`。

`waitForRetirement()`：
- `previous = host.previous`；无 → 返回；`deadline = options.retireDeadline ?? 5000`；`gone = () => host.previous !== previous`。
- `await until(host, () => gone() || host.retirable(), deadline)` 为 false 且 !gone → `previous.abort.abort(ChordError("superseded", `code version ${previous.id} was retired`))`；`await until(host, () => gone() || previous.roots.size === 0, Math.min(deadline, ABORTED_ROOTS_GRACE))`。
- `!gone()` → `host.retire()`。

`load()`：
- `replaced = Set([...facets.map(name), ...(options.remove ?? [])])`；`current = host.current.services`；`services = new Map([...current].filter(([, s]) => !replaced.has(s.facet)))`。
- 对每个 facet（**facets 数组顺序**）：`run = new AbortController()`；`facetRuns.set(name, run)`；`facet.setup(host.facetApi(wrapper, run.signal))`，wrapper 为 `(type, factory, options={}) => host.serveInto(this.services, makeService(type, factory, options, facet.name))`。
- `for ([name, service] of services) if (current.get(name) !== service) changed.add(name)`；`for (name of current.keys()) if (!services.has(name)) removed.add(name)`。
- `host.pending = {id: host.nextCodeVersionId(this.codeVersion), services, tables: new Map(), roots: new Map(), abort: new AbortController()}`。

`withDeadline(promise, what)`：`deadline = gateDeadline ?? 5000`；超时 → `ChordError("aborted", `${what} within ${deadline} ms`, { reason: "deadline" })`；race。

`checkProviders()`：
- 对每个 `changed`：`next = services.get(name).contract`；`old = current.services.get(name)?.contract`；`old === undefined || old.version !== next.version` → continue；`coverage(next.description, old.description)` 有 reason → `ChordError("contract_mismatch", `${name}@${next.version} changed incompatibly without a version bump (${reason})`)`。（**新 provider 必须覆盖旧 provider**。）
- `facets = new Map(host.facets)`；本次 facets 覆盖上；`options.remove` 删除。对每个 facet 的 `uses ?? []`：`service = services.get(token.name)`；存在且 `changed.has(token.name)` → `bind(service.contract, service.options.accepts, token)`（不匹配抛 contract_mismatch）。

`prepareAll()`（不动点循环）：
```
for (;;) {
  todo = host.objects 中 changed.has(typeName) && !candidates.has(object) && object.service !== services.get(typeName) && (status === "live" || "activating")
  if (todo.length === 0) return
  await Promise.allSettled(todo.map(o => o.ready))
  #alive()
  for (object of todo) if (object.status === "live" && object.service !== services.get(object.typeName)) { await prepareObject(object); #alive() }
}
```
（第二轮循环会重新扫描，因为 activations 在 prepare 期间会新增对象。）

`prepareObject(object)`：
- `service = services.get(typeName)`；`run = object.run(service, object.decls)`（**记录模式**：已有 cells 可读、写入抛 read_only、listeners/tasks/registrations 记录但不启动、引用记录；非 exclusive 的新资源在此打开，candidate 所有——abort 时关闭）。
- `candidate` 初始化；`candidates.set`。
- 对 run.decls.cells 中 `object.decls.cells.get(name) === cell`（**保留下来的**，不是新 cell）：`from = object.decls.cellVersions.get(name) ?? 1`；`to = run.decls.cellVersions.get(name) ?? 1`；`to < from` → `invalid("state", `${address}: version of state ${name} lowered from ${from} to ${to}`)`；`to > from` → `reasons.push(`state ${name} version ${from} -> ${to}`)`；否则若 `!deepEqual(cell.schema, run.decls.cellSchemas.get(name))` → `invalid("state", `${address}: schema of state ${name} changed without a version bump`)`。
- `deferred = Set`：对 run.created.resources：`resource.exclusive || resource.deps.some(dep => deferred.has(dep))` → 加入。
- 对 deferred 中每个：`atGate.push(resource)`；`old = object.decls.resources.get(resource.key)`；`resource.exclusive && old?.exclusive && old.state === "open"` → `replaced.push(old)`。
- `deferred.size > 0` → `reasons.push("exclusive resource replaced")`。
- `await object.open(非 deferred 的 created.resources, run.created.refs, this.abort.signal)`；`await object.waitHydrated(run)`。
- 若 `reasons.length === 0`：`await object.initCells(run, this.abort.signal)`；`dryRun(candidate)`。（被 gate 的对象在 gate 阶段做。）

`checkRefs()`：
- `dropped = Set`：对每个 candidate，`object.decls.refs` 中不在 `run.decls.refs` 的引用。
- 对所有 `host.objects`：`service = services.get(typeName)`；无或 `!changed.has(typeName)` → continue；对每个 `object.refs`：若 `ref === host.hostObjects.get(typeName)` → 跳过（host 自己的 bootstrap 引用在 commit 跟随 provider；不是旧代码消费者）；若 `!dropped.has(ref) && ref.owner !== "client"` → `bind(service.contract, service.options.accepts, ref.contract)`（不匹配 → contract_mismatch；**浏览器引用不阻塞 reload**）。

`entries()`：`host.liveDecls(true).filter(([o]) => !removed.has(o.typeName)).map(([object, decls]) => [object, candidates.get(object)?.run.decls ?? decls])`。

`gate()`：
- `gated = candidates 中 reasons.length > 0 && object.status === "live"`。
- `drained = gated.map(({object}) => new Promise(resolve => { grandfathered = new Set(object.leases.keys()); object.gate = {grandfathered, queue: [], drained: resolve}; object.stopTasks(); if (grandfathered.size === 0) resolve(); }))`。
- `deadline = gateDeadline ?? 5000`；race（全部 drained vs 超时）。
- 超时 → 收集所有 `object.gate?.grandfathered` 中 rootId → `ChordError("aborted", `gate deadline (${deadline} ms) missed; blocking roots: ${blocking.join(", ")}`, { reason: "gate_deadline", details: {blocking} })`。
- `#alive()`；对每个 gated（顺序）：对 `candidate.replaced`：`closed.push([object, old])`（先登记，rollback 必须能恢复）；`await old.close(object.openOrder)`；`await object.open(candidate.atGate, [], this.abort.signal)`；`#alive()`；`await object.initCells(candidate.run, this.abort.signal)`；`#alive()`。
- 然后对所有 gated `migrate(candidate)`。

`migrate(candidate)`：
- 对 `run.decls.cells` 中 `object.decls.cells.get(name) === cell`（保留 cells）：`schema = run.decls.cellSchemas.get(name)`；`from = object.decls.cellVersions.get(name) ?? 1`；`fn = run.decls.cellMigrations.get(name)`；
  - 若 `(run.decls.cellVersions.get(name) ?? 1) > from && fn !== undefined` → `value = fn(structuredClone(cell.value), from)`；
  - 否则若 `!deepEqual(cell.schema, schema)` → `value = cell.value`（版本未升但 schema 深度相等？——此处是 schema 变化路径；由于 prepare 已在无版本升级且 schema 不同时拒绝，这里实际对应"版本升了但无 migrate"或其它情况）；
  - 否则 continue（无迁移）。
  - `assertValid("state", `${address}: migrated state ${name}`, schema, value)`；`candidate.migrated.set(cell, value)`。
- `dryRun(candidate)`。

`rollback()`（幂等，`rolledBack` 标记）：
1. `abort.abort(ChordError("aborted","reload aborted"))`；所有 facetRuns abort 同错误。
2. 所有 candidate.staged?.discard()；所有 deferred staged discard。
3. 每个 candidate：`object.pending.delete(run.decls)`；`await object.unwind(run).catch(report)`。
4. `closed` 中每个 `[object, resource]`：`resource.state !== "closed"` → continue；`await resource.open(new AbortController().signal, object.openOrder).catch(error => { resource.markUnavailable(); report(ChordError("unavailable", `${object.address}: resource ${resource.key} could not be restored`, { cause: error })) })`（**换回的旧 exclusive 资源若打不开 → 标记 unavailable，绝不静默关闭**）。
5. 对每个 gated：若 `object.gate === undefined` continue；`object.openGate()`；`object.startTasks()`。
6. `reason = ChordError("gone", "reload aborted; type not served")`；对 `host.objects` 中 `!host.current.services.has(typeName)` 的对象 → `object.dispose(true, reason).catch(report)`（本次 reload 会新增的类型失去代码）。
7. `host.pending = undefined`；`host.gating = undefined`。

`commit(beforeStart?)`（同步一步）：
```
try {
  return holdPublications(() => {
    const report = #publish();
    beforeStart?.();                       // 协调者在此排队 commit 帧（在任何新代码启动前）
    for (const start of #starts.splice(0)) start();
    return report;
  });
} catch (error) { host.fail(error); throw reloadError(error); }
```

`#publish()`（全部同步）：
1. `previous = host.current`；`next = host.pending`；`host.pending = undefined`；`codeVersions.set(next.id, next)`；`host.previous = previous`；`host.current = next`。
2. `prepared = []`；对 `host.objects` 快照：removed → continue；candidate 无 → 若有 `implementations.get(previous.id)` 则复制为 `{...installed, codeVersion: next.id}`；candidate 有且 `status === "live"` → `install(candidate, next.id)`，`prepared.push(address)`。
3. 对 candidates 中 `status !== "live"` 的：`staged?.discard()`；`pending.delete(run.decls)`；`unwind(run).catch(report)`。
4. 对 `deferred`（本次新增类型的 live 对象）：`deferredRuntime = false`；`staged.activate()`；`#starts.push(() => object.startTasks())`。
5. `host.installTables(next, this.tables, true)`。
6. `reason = ChordError("gone","type no longer served")`；removed 类型的对象 `dispose(true, reason).catch(report)`；`host.hostObjects.delete(name)`。
7. 本次 facets 写入 `host.facets`；`options.remove` 删除。
8. 对 `facetRuns` 每项 `#starts.push(() => host.facetRunChanged(name, run))`；对 remove 的每个 `#starts.push(() => host.facetRunChanged(name, undefined))`。
9. `host.gating = undefined`。
10. 对 `services` 每项：
    - `hostLifetime(service) && changed.has(name) && host.hostObjects.get(name)?.object?.status !== "live"` → `#starts.push(() => host.startHostObject(service))`。
    - `!hostLifetime(service) && pinned !== undefined` → `host.hostObjects.delete(name)`；`#starts.push(() => pinned.release())`。
11. 返回 `{codeVersion: next.id, previous: previous.id, changed: [...changed], removed: [...removed], prepared, gated: gated.map(address)}`。

`install(candidate, codeVersion)`：
1. `gated = reasons.length > 0`；`pending.delete(run.decls)`；`object.stopTasks()`。
2. gated：旧 derivations 中不在新集合的 detach（旧实现不能再跑；旧 derivation 会按迁移值计算）。
3. `object.decls = run.decls`；`object.service = service`；对 `run.decls.adopted` 每个 `resource.adopt(declaration)`。
4. `implementations.set(codeVersion, {codeVersion, service, implementation, decls})`。
5. gated：`minCodeVersion = codeVersion`；对其它旧 implementations → `host.retireImplementation(object, old)`（**旧代码绝不能跑在迁移后的状态上**）。
6. `refreshSchemas()`；对每个 `[name, cell] of run.decls.cells`：`schema = cellSchemas.get(name)`；有 migrated → `cell.reset(value, schema)`；否则 `cell.schema = schema`。
7. `created = new Set(run.created.derives)`；created 逐个 `attach(candidate.derived)`；其余 `refresh()`。
8. `candidate.staged?.activate()`；`#starts.push(() => object.startTasks())`；`object.openGate()`。
9. host 自己的引用重绑：`pinned = hostObjects.get(typeName)`；若 `pinned?.object === object && pinned.contract.fingerprint !== service.contract.fingerprint` → `replacement = new RefImpl(object, service.contract, "host")`；**先 acquire 再 release**（先 addRef 防止对象被释放重开）：`object.addRef(replacement)`；`hostObjects.set`；`pinned.release()`。
10. 对 `object.refs` 快照：client 引用 → `bind(...)` 失败则 `ref.gone(ChordError("upgrade_required", `${typeName} changed; reload the web facets`, {cause: error}))` 并 `object.release(ref)`，continue；否则 `ref.shapeChanged()`。

### 2.7 `src/remote.ts`（658 行）

#### 2.7.1 类型

```ts
export interface StateIn { readonly as: "snap"|"reset"|"ops"|"error"; readonly value?: unknown; readonly ops?: readonly unknown[]; readonly error?: WireError }
export interface RemoteChannel {
  hold(binding: RemoteBinding, again: boolean): number;
  release(id: number): void;
  call(hold: number, member: string, args: unknown, root: RootRef | undefined, signal: AbortSignal): Promise<unknown>;
  wantState(hold: number, wanted: boolean): void;
}
export interface RemoteHost {
  readonly inFactory: number; report(error: unknown): void;
  root<T>(fn: (ctx: Ctx) => T | Promise<T>, options?: { signal?: AbortSignal }): Promise<T>;
  bindingIdle(binding): void; forgetBinding(binding): void;
}
```

#### 2.7.2 `RemoteBinding`

字段：`host`, `key`, `contract`, `at`, `input`, `principal`（转发的 principal，宿主自身 principal 时为 undefined）, `status: "loading"|"live"|"ended"`, `error`, `shape: Shape|undefined`, `hydrated = false`, `hold: {channel, id}|undefined`, `stateOn = false`, `views: Set<RemoteRef>`, `members: Map<string, {value, decoder}>`, `lingerTimer`, `#listeners: Map<string, Set<ViewReadable>>`, `#pending: {shape, missing:Set, values:Map} | undefined`, `#live`（deferred）, `#applied = false`, `#hydrationWaiters: Set<Settle>`。

- getters：`ended`（status === "ended"）；`stateViews`（wantsState 的 view 数）；`wantsState`（无 view → stateOn；否则 stateViews > 0）；`applied`。
- `addView(view)`：加入；清 lingerTimer；`#syncState()`；`view.update()`。
- `removeView(view)`：删除失败返回；从 listeners 中删除属于该 view 的 readable；views 为空 → `host.bindingIdle(this)`；否则 `#syncState()`。
- `#syncState()`：ended 或 views 为空返回；`wanted = stateViews > 0`；相同返回；`stateOn = wanted`；若变为 false：`hydrated = false`；`members.clear()`；`#pending?.missing.clear()`；若 hold 存在 → `hold.channel.wantState(hold.id, wanted)`；所有 views `update()`。
- `listen/unlisten`（维护 `#listeners` 集合）。
- `inVariant(name)`：`shape?.variant` 非 undefined 且 consumer 契约中该 variant 的该成员是 state。
- `read(name)`：ended → terminalError；`!hydrated` → `ChordError("not_ready", `${key}.${name} is not hydrated yet`)`；`!inVariant` → `ChordError("not_in_variant", `${key}: ${name} is not a state member of variant ${shape?.variant}`)`；否则 `members.get(name)?.value`。
- `whenHydrated()`：hydrated → resolve；ended → reject terminalError；否则加入 waiters。
- `readable(name)`：`status !== "ended" && hydrated && inVariant(name)`。
- `terminalError()`：`error ?? ChordError("released", `reference ${key} released`)`。
- `bound(channel, id)`：`hold = {channel, id}`；`stateOn = wantsState`。
- `shaped(shape, listed)`：
  - ended 返回；`previous = #pending`；`missing = new Set(listed)`；previous 存在 → 并入其 missing（**relay 可能重排：等两批都快照**）。
  - `wanted = new Set(#stateMembers(shape))`；对 missing 中不 wanted 或 `!stateOn` 的删除。
  - `#pending = {shape, missing, values: previous?.values ?? new Map()}`；missing 空 → `#apply()`。
- `state(name, frame)`：
  - error：`fromWireError`；若 `!hydrated && stateOn` → `gone(error); close()`（首次快照前没人能读：直接失败而不是永远等）；否则 `host.report(error)`。
  - ops：`target = pending?.values.get(name) ?? members.get(name)`；无 → 返回；`ops = target.decoder.decode(frame.ops ?? [])`；`target.value = applyImmutable(target.value, ops)`；若 target 就是 members 里的 → `#deliver(name, {type:"ops", ops, value})`。
  - snap/reset：`fresh = {value: frame.value, decoder: decoder()}`；若 pending 存在且（missing 有 name 或 pending.values 有 name）→ `pending.values.set`；missing.delete；missing 空 → `#apply()`；返回。否则 `!inVariant(name)` → 返回；`members.set(name, fresh)`；`#deliver(name, {type:"reset", value})`。
- `gone(error)`：ended 返回；`status="ended"; error`；`hold=undefined; #pending=undefined`；`#live.reject(error)`；`#settleHydration(error)`；所有 views `end(error)`；清空 listeners；`host.forgetBinding(this)`。
- `detached()`：ended 返回；`hold=undefined; #pending=undefined`；若 status live → `status="loading"`；`#live` 换新 deferred；所有 views `update()`。
- `close()`：清 linger；有 hold → `channel.release(id)`；`hold=undefined`；若非 ended → `status="ended"`，`error = ChordError("released", `reference ${key} released`)`，`#live.reject`，`#settleHydration(error)`，views `end(error)`；`#pending=undefined`；清 listeners；`host.forgetBinding`。
- `call(name, args, ctx)`（循环）：
  1. ended → 抛 terminalError。
  2. `status !== "live" || hold === undefined` → `await raceSignal(#live.promise, ctx.signal)`。
  3. 再检查 hold/status，不满足 → continue。
  4. `variant = shape?.variant`；`description = variant === undefined ? undefined : contract.description.variants[variant]?.[name]`；非 method → `ChordError("not_in_variant", `${key}: ${name} is not a method of variant ${variant}`)`。
  5. `sent = description.args === undefined ? undefined : Value.Clean(description.args, Value.Clone(args))`（**只发 consumer 自己 schema 知道的内容**，additive 规则前提）。
  6. `return hold.channel.call(hold.id, name, sent, {rootId: ctx.rootId, codeVersion: ctx.codeVersion}, ctx.signal)`。
- `#stateMembers(shape)`：consumer 契约中该 variant 的 state 成员，按 shape.members 顺序过滤。
- `#apply()`：取 pending；清空；`shape = pending.shape`；`wanted = #stateMembers`；members 中不在 wanted 的删除；pending.values 中 wanted 的写入；`hydrated = stateOn && 所有 wanted 都有 value`；hydrated → `#settleHydration()`；`status="live"`；`#applied=true`；`#live.resolve()`；所有 views `update()`；对 pending.values 中 wanted 的 `#deliver(name, {type:"reset", value})`；所有 views（快照）`shapeChanged()`。
- `#deliver(name, event)`：对 `#listeners.get(name)` 快照逐个 `readable.deliver(event)`。

#### 2.7.3 `ViewReadable`

- `value`：view ended → terminalError；否则 `binding.read(name)`。
- `[HYDRATION]`：`binding.hydrated` → undefined；否则 `{hydrated:false, whenHydrated: () => binding.whenHydrated()}`。
- `subscribe(listener)`：加入 values；`#attach()`；若 `#readable()` 同步发 `members.get(name)?.value`；返回移除（values+events 都空 → unlisten）。
- `events(listener)`：加入 events；attach；若可读同步发 `{type:"snap", value}`；否则加入 unseen；返回移除（unseen + events）。
- `deliver(event)`：对 events 快照：若 unseen 中（删除）→ 仍发原事件？——代码：`if (!this.#unseen.delete(listener)) this.#safe(() => listener(event)); else if (event.type !== "ops") this.#safe(() => listener({type:"snap", value: event.value}))`。即 unseen 的监听者收到 snap（ops 被转换成 snap），其余原样；然后 values 监听者收到 `event.value`。
- `stop()`：清空三集合，`binding.unlisten`。

#### 2.7.4 `RemoteRef`

- 字段：`object = undefined`, `binding`, `owner`, `wantsState`, `proxy`, `ready`（deferred promise）, `status: RefStatus = "loading"`, `error`, `shapeListeners`, `#statusListeners`, `#readables`, `#callers`, `#settleReady`。
- Proxy：与 RefImpl 相同的 `get` 陷阱（REF_BASE 原方法 + 绑定 `this`；非 string 或 `"then"` → undefined；否则 `member(name)`）与 `refsByProxy.set`。**注意**：`RemoteRef` 的 Proxy **没有** `has` 陷阱（RefImpl 有）——`"x" in remoteRef` 走默认语义（false），这是一个真实的类型/行为差异，Rust 的 handle 无需模拟。
- getters：`contract`（binding.contract）、`address`（binding.key）、`ended`（released/gone）、`variant`（binding.shape?.variant）、`shape`。
- `onShape/onStatus`：集合。
- `terminalError()`：gone → error；否则 `ChordError("released", `reference ${address} released`)`。
- `release()`：ended 返回；`status="released"`；`#notify()`；reject ready（released 错误）；停止所有 readables；`binding.removeView(this)`。
- `update()`：ended 返回；`live = binding.status === "live" && binding.hold !== undefined && (!wantsState || binding.hydrated)`；`next = live ? "live" : "loading"`；相同返回；设置；`#notify()`；live → ready resolve。
- `end(error)`：ended 返回；`released = error instanceof ChordError && error.code === "released"`；`status = released ? "released" : "gone"`；非 released → `error` 记录；`#notify()`；ready reject；停止 readables。
- `shapeChanged()`：shape 存在且 !ended → 对 shapeListeners 快照**同步**调用（抛错 report）。**注意**：远程引用这里是同步，本地 RefImpl 是 enqueue——差异需保持。
- `member(name)`：在所有 variant 成员里找第一个该名字的描述；method → `#caller`；state → 复用/新建 ViewReadable；否则 undefined。
- `#caller(name)`：noArgs 取参；`host.inFactory > 0` → 同步抛 `ChordError("not_ready", `${address}.${name} called inside a factory`)`；非 CtxImpl → reject `invalid("foreign_ctx","ctx must come from a Chord host")`；`ctx?.check(host)` 抛 → reject；`ctx === undefined || ctx.holder` → `host.root(root => #call(name, args, root), {signal: ctx?.signal})`；否则 `#call(name, args, ctx)`。
- `#call(name, args, ctx)`：ended → reject terminalError；`binding.call(name, args, ctx)`。

### 2.8 `src/manager.ts`（443 行，Node only）

#### 2.8.1 控制消息与类型

```ts
export type ControlMessage =
  | { type: "init"; value: unknown }
  | { type: "ready" }
  | { type: "fatal"; message: string; reason?: string }
  | { type: "shutdown" }
  | { type: "status"; activeCalls?: number; viewers?: number; durableTasks?: number };
export interface Liveness { booting: boolean; activeCalls: number; viewers: number; durableTasks: number; idleFor: number }
export interface Exit { code: number | null; signal: string | null }
export interface Instance { pid: number; link: IpcLink; exited: Promise<Exit>; send(m: ControlMessage): void; killTree(): void; onControl(l): void }
export interface Driver { spawn(): Instance }
export interface ForkDriverOptions {
  entry: string; args?: readonly string[]; env?: Readonly<Record<string, string>>; execArgv?: readonly string[];
  init?: unknown; lock?: string;
  onOutput?: (stream: "stdout"|"stderr", chunk: Uint8Array) => void;
  onMessage?: (message: unknown) => void;
}
export interface HostManagerOptions {
  driver: Driver; keepAlive?: (liveness: Liveness) => boolean; checkInterval?: number;
  grace?: number;        // 默认 5000
  readyTimeout?: number; // 默认 30000
  onExit?: (exit: Exit) => void;
}
export interface HostManager {
  readonly status: "stopped"|"starting"|"running"|"stopping";
  readonly instance: Instance | undefined; readonly liveness: Liveness;
  start(): Promise<Instance>; stop(): Promise<void>; replace(): Promise<Instance>;
}
```

控制消息在 IPC 上包为 `{ chord2control: message }`；Chord 帧包为 `{ chord2: frame }`（见 link 单元）。

#### 2.8.2 `processLock(name)` → `Promise<() => Promise<void>>`

- `key = sha256(name).hex.slice(0, 32)`。
- `file = platform !== "win32" && platform !== "linux" && platform !== "android"`（需要 socket 文件的平台）。
- `address`：win32 → `\\.\pipe\chord2-${key}`；file → `await lockSocketPath(`chord2-${key}.sock`)`；否则（Linux/Android）→ `\0chord2-${key}`（**抽象 unix socket，名字以 NUL 开头**）。
- `server = net.createServer(socket => socket.end())`；循环尝试 `server.listen(address)`：
  - 非 `EADDRINUSE` 错误 → 抛出。
  - `!file || (await accepting(address))` → `ChordError("unavailable", `lock ${name} is held by another process`, { reason: "locked" })`。
  - 否则（file 平台且无人 accept：陈旧文件）→ `rm(address, {force:true})` 后重试。
- file 平台：注册 `process.once("exit", removeFile)`（unlinkSync，吞错）。
- 返回 release 函数：file 平台移除 exit 监听；`server.close(err => err ? reject(err) : resolve())`。
- `MAX_SOCKET_PATH_BYTES = 103`；`lockSocketPath(file)`：`preferred = join(tmpdir(), file)`；`Buffer.byteLength(preferred) <= 103` → preferred；否则 `uid = process.getuid?.() ?? 0`；`directory = `/tmp/chord2-${uid}``；`mkdir(directory, {mode: 0o700, recursive: true})`；`lstat`；若非目录 / uid 不匹配 / `(mode & 0o077) !== 0` → `ChordError("unavailable", `lock directory ${directory} must be a directory only its owner can access`)`；返回 `join(directory, file)`。
- `accepting(address)`：`connect`；connect → destroy，true；error → `code !== "ECONNREFUSED" && code !== "ENOENT"`（其它错误视为有人持有）。
- `LOCK_ENV = "CHORD2_LOCK"`；`ignoreSendError = () => {}`。

#### 2.8.3 `forkDriver(options)` → `Driver`

`spawn()`：
- 环境：`{...process.env}`，**删除 CHORD2_LOCK**（继承来的锁不是子进程的），`Object.assign(env, options.env)`，`options.lock !== undefined` → `env.CHORD2_LOCK = options.lock`。
- `fork(entry, args, { env, execArgv, detached: true, stdio: ["ignore", onOutput ? "pipe" : "inherit", onOutput ? "pipe" : "inherit", "ipc"] })`（**detached：子进程自成进程组**）。
- onOutput：[stdout, stderr] 上监听 data：先回调 `options.onOutput(stream, chunk)`，再镜像写到父进程对应 stream。
- `pid = child.pid ?? -1`；`killGroup = () => process.kill(-pid, "SIGKILL")`（吞错）。
- `exited`：`child.once("exit", (code, signal) => { killGroup(); resolve({code, signal}) })`（**无论怎么退出，杀掉整组**）。
- 消息：`child.on("message", message => { options.onMessage?.(message); control = message?.chord2control; control 存在 → 通知监听者 })`。
- `send(message)`：`child.connected` 时 `child.send({chord2control: message}, ignoreSendError)`。
- `options.init !== undefined` → 立即 `send({type:"init", value: init})`。
- 返回 `{pid, link: ipcLink(child), exited, send, killTree: () => { try { process.kill(-pid,"SIGKILL") } catch { child.kill("SIGKILL") } }, onControl: l => listeners.add(l)}`。

#### 2.8.4 `createHostManager(options)` → `HostManager`

内部：`status`, `instance`, `queue: Promise`（串行化）, `report = {activeCalls:0, viewers:0, durableTasks:0}`, `idleSince`, `booting`, `timer`。

- `serialized(step)`：`next = queue.then(step, step)`；`queue = next.catch(() => {})`；返回 next（**start/stop/replace 串行**）。
- `liveness()`：`{booting, ...report, idleFor: idleSince === undefined ? 0 : Date.now() - idleSince}`。
- `doStop()`：
  - 无 instance → `status="stopped"; booting=false`。
  - `status="stopping"`；清 interval；`current.send({type:"shutdown"})`；race（`exited` vs `setTimeout(grace ?? 5000)`）；未优雅退出 → `current.killTree()`；`await current.exited`；若 instance 还是它 → undefined；`status="stopped"`。
- `doStart()`：
  - 已有 instance 且 status running → 返回它。
  - `status="starting"; booting=true; report` 归零。
  - `driver.spawn()`（抛错 → `status="stopped"; booting=false`，重抛）；`instance = current`。
  - `current.exited.then(exit => { 若 instance 还是它 → undefined、status="stopped"、清 interval；options.onExit?.(exit) })`。
  - 等待 ready/fatal/超时（`new Promise`）：
    - timeout（`readyTimeout ?? 30000`）→ reject `ChordError("unavailable","child not ready in time")`。
    - `exited` → reject `ChordError("unavailable", `child exited before ready (${exit.code ?? exit.signal})`)`。
    - 控制消息：`ready` → resolve；`fatal` → reject `ChordError("unavailable", `child failed: ${message.message}`, { reason: message.reason })`；`status` → 更新 report（缺省 0）；若 `activeCalls + viewers + durableTasks > 0` → `idleSince = undefined`；否则 `idleSince ??= Date.now()`（**idle 时钟从 busy → idle 转变开始，而不是最后一次 busy 报告**）。
    - `.catch(async error => { current.killTree(); await current.exited; throw error })`。
  - `booting=false`；若总活动为 0 → `idleSince ??= Date.now()`；`status="running"`。
  - `keepAlive` 存在：`setInterval(checkInterval ?? 1000)`：`status === "running" && !keepAlive(liveness())` → `serialized(doStop)`；`unref()`。
  - 返回 current。
- 返回对象：`status`/`instance`/`liveness` getters；`start: () => serialized(doStart)`；`stop: () => serialized(doStop)`；`replace: () => serialized(async () => { await doStop(); return doStart() })`。

#### 2.8.5 `childControl(channel = process)`

- `send(message)`：`channel.connected` → `channel.send({chord2control: message}, undefined, undefined, ignoreSendError)`。
- `init` promise：收到 `{type:"init"}` 以 value resolve。
- `channel.on("message")`：`{type:"shutdown"}` → 所有 shutdown handler（同步调用，不 await）。
- `fatal(error)`：`{type:"fatal", message: error instanceof Error ? error.message : String(error), ...(error instanceof ChordError && error.reason !== undefined ? {reason} : {})}`。
- 返回 `{ init, lock, ready, fatal, report, onShutdown }`：
  - `lock()`：`name = process.env.CHORD2_LOCK`；undefined → 直接返回；`try { await processLock(name) } catch (e) { fatal(e); throw e }`。
  - `ready()`：`send({type:"ready"})`。
  - `report(status)`：`send({type:"status", ...status})`。
  - `onShutdown(handler)`：加入集合。

## 3. 用户可见面（逐字）

本单元**没有 CLI 参数、斜杠命令、按键绑定、TUI 布局或 ANSI 渲染**（Chord 是库）。其"用户可见面"是：导出 API、错误码与错误消息、日志输出、环境变量、锁地址、进程管理状态、链路关闭原因。全部逐字列出如下。

### 3.1 导出符号（`@earendil-works/chord` 入口 `src/index.ts`，与本单元相关部分）

| 名字 | 种类 | 来源 |
| --- | --- | --- |
| `CHORD_ERROR_CODES` | 值（12 项数组，顺序固定） | errors.ts |
| `ChordError`, `isChordError`, `forbidden`, `invalid` | 值/类 | errors.ts |
| `method`, `opaque`, `hostNamed`, `hostFamily`, `remote`, `local`, `bind`, `coverage` | 值 | contract.ts |
| `RESERVED_MEMBERS`, `LOCAL_VARIANT`, `DescriptionSchema` | 值 | contract.ts（**注意**：`RESERVED_MEMBERS`/`LOCAL_VARIANT`/`DescriptionSchema` 未在 index.ts 再导出，属包内）
| `createHost`, `defineFacet` | 值 | host.ts |
| `memoryLinkPair`、`ipcLink`、`webSocketLink`、`PACK_THRESHOLD`、`inflateMessage` | 值 | link.ts（**注意**：`MemoryLink`/`IpcLink`/`WebSocketLink` 三个**类**以及 `Packed`/`unpack`/`BaseLink` 只在 `link.ts` 内导出，**未**在 index.ts 再导出；包使用者拿到的是工厂函数返回值） |
| `PROTOCOL_VERSION`, `isFrame`, `toWireError`, `fromWireError` | 值 | wire.ts（**只有 `PROTOCOL_VERSION` 在 index.ts 再导出**；`isFrame`/`toWireError`/`fromWireError` 只在 wire.ts 内导出）
| `acceptWebSockets` | 值 | websocket.ts |
| `defineRegistry` | 值 | registry.ts |
| `copyJson`, `isJsonValue` | 值 | json.ts |
| `replicatedState`, `Watch`, `NativeWatch` | 值 | state.ts / watch.ts |
| 类型：`Accepts`, `Adapter`, `AnyContract`, `Description`, `HostFamily`, `HostNamed`, `ImplementationOf`, `InputOf`, `LocalContract`, `Placement`, `PlacementFor`, `Ref`, `RemoteContract`, `EventOf`, `Resource`, `ResourceDeclaration`, `Subscribable`, `ValueOf`, `Draft`, `ChordErrorCode`, `ConnectOptions`, `PrincipalMatch`, `PrincipalPattern`, `Route`, `Facet`, `FacetApi`, `FacetDefinition`, `FacetLoader`, `Host`, `HostOptions`, `LoadOptions`, `PluginRef`, `RootOptions`, `CopyJsonOptions`, `JsonRepresentation`, `JsonValue`, `IpcChannel`, `Link`, `WebSocketLike`, `WebSocketLinkOptions`, `Factory`, `HostServeOptions`, `Lifetime`, `ReferencedServeOptions`, `Self`, `ServeOptions`, `ServeOptionsFor`, `StateInit`, `StateOptions`, `Registry`, `RegistryTable`, `RegistryView`, `ReloadOptions`, `ReloadReport`, `Ctx`, `GetOptions`, `Readable`, `RefStatus`, `Shape`, `StateEvent`, `Cell`, `AcceptOptions`, `LinkOptions`, `Context`, `ContextKey`, `Frame`, `NativeWatchable`, `Watchable`, `WatchEnd`, `WatchHandle`, `NativeWatchHandle`, `WebSocketAcceptOptions`, `WebSocketServerLike`, `AttachedReplicatedState`, `MutableReplicatedState`, `ReplicatedState`, `ReplicatedStateDelivery`, `ReplicatedStateSource`, `ReplicatedStateSourceAttachment`, `ReplicatedStateSourceFrame`, `ReplicatedStateSourceOptions` | 类型 | 各文件（**`MemberDescription` 例外**：它在 contract.ts 导出，但 index.ts **没有**再导出，包使用者拿不到） |

`@earendil-works/chord/node`（`src/node.ts`）：`nodeLoader`、`ControlMessage`、`childControl`、`createHostManager`、`Driver`、`Exit`、`ForkDriverOptions`、`forkDriver`、`HostManager`、`HostManagerOptions`、`Instance`、`Liveness`、`processLock`。

### 3.2 错误码（`docs/guide.md` 表格逐字）

| Code | Meaning |
| --- | --- |
| `forbidden` | The factory or a method refused this principal |
| `invalid` | Input, arguments or a result failed validation |
| `not_ready` | A declared resource or reference was used inside a factory, or state read before its reference's first hydration |
| `not_in_variant` | The member doesn't exist in the variant you have |
| `released` | The reference was released |
| `gone` | The object was forced away while referenced. Call `get` again. |
| `contract_mismatch` | The provider's contract doesn't cover the consumer's, and no adapter exists |
| `upgrade_conflict` | An old root reached an object migrated meanwhile. Retry as a new root. |
| `superseded` | The root's code version is no longer available on this host. Retry. |
| `upgrade_required` | A browser's contract is outdated. Reload its web facets. |
| `unavailable` | The target host can't be reached or started; reason `ready_timeout`: a reference was not ready within `readyTimeout` |
| `aborted` | The call's `ctx` was cancelled |

可重试集合（`retryable`）恰为：`superseded`, `upgrade_conflict`。

### 3.3 错误消息格式

`ChordError.message`（即 `Error.message`，也就是打印/序列化时看到的东西）逐字为：

```
`${code}${reason ? `/${reason}` : ""}: ${message}`
```

例：`invalid/args: t.chat.send: /by Expected number`、`unavailable/ready_timeout: t.x@1 was not ready after 60000 ms`、`forbidden: forbidden`。

`name === "ChordError"`。HTTP/IPC 传输时用 `WireError = { code, message, reason? }`；跨链路后接收侧用 `fromWireError` **剥掉**消息前缀：`message.replace(/^[a-z_]+(\/[a-z_]+)?: /, "")`（只剥一次；若消息体本身以形如 `xxx: ` 开头也只剥一层）。

### 3.4 本单元产生的可见错误消息（逐字，按文件分组）

**完整性口径**：本小节按文件列出**所有**会被用户看到（抛出、作为 `WireError` 过链、`onError` 上报、或作为 abort 原因）的 `message` 文本；§2 中已逐行引用过的模板在此不重复展开（`${...}` 为原样插值）。TypeBox/`assertValid` 产生的 `Expected …` 片段属于 schema 单元，见 §2.1。此前版本曾遗漏若干条，已在本版补齐并标注。

通用/契约（见 §2.2，不重复）。

对象与激活（`object.ts`）：

```
resource ${key} could not be restored                                   (unavailable)
resource ${key} opened after abort                                      (aborted)
ctx expired: its call already settled                                   (aborted, reason expired)
ctx aborted                                                             (aborted, cause=signal.reason)
${address} was not ready after ${ms} ms                                 (unavailable, reason ready_timeout)
call aborted                                                            (aborted)
reference ${address} released                                           (released)
not_ready: `${address}.${name} called inside a factory`
${address} abandoned                                                    (aborted)
${typeName} is not served                                               (unavailable, reason unknown_type)
${address}: code changed during activation                              (superseded)
task stopped                                                            (aborted)
${host.name} failed and admits no work                                  (unavailable, reason host_failed)
code version ${ctx.codeVersion} is retired                              (superseded)
${address} is being upgraded                                            (upgrade_conflict)
write with an aborted ctx                                               (aborted)
${address} is disposed                                                  (gone)
${address} is ${status}                                                 (gone；status 为 activating/live/failed/disposing)
${address} was disposed                                                 (gone)
${address} was not ready after ${ms} ms, waiting on ${list.join(", ")} (a reference cycle across hosts?)   (unavailable, reason ready_timeout；list 为等待中的成员/引用名，非空时追加)
${address} was upgraded in code version ${minCodeVersion}               (upgrade_conflict)
${address} has no implementation for code version ${ctx.codeVersion}    (superseded)
${address}: ${name} is not a method of variant ${variant ?? "(local)"}  (not_in_variant)
${address}: ${name} is not a state member of variant ${variantOf(view) ?? "(local)"}   (not_in_variant)
${address}: aborted tasks still running after ${deadline} ms; starting their replacements anyway   (aborted；只经 onError 上报)
${address}: cleanup failed                                              (runAll 的聚合标签；资源 close 与 created refs release)
${address}: disposal failed                                             (runAll 的聚合标签)
${address}: retiring code version ${codeVersion} failed                 (runAll 的聚合标签；retireImplementation)
${address}.${name} called inside a factory                              (not_ready；RefImpl 方法代理)
resource ${key} (inside a factory)                                      (not_ready；ResourceFacade.value)
${typeName}: factories must be synchronous                              (invalid, reason implementation)
${typeName}: unknown lifetime ${String(lifetime)}                       (TypeError)
${typeName}: lifetime "host" needs a local type (remote types are referenced)   (TypeError)
${typeName}: an object of lifetime "host" does not linger (it lives as long as the host)   (TypeError)
${typeName}: lifetime "host" needs an input that accepts {} (the object's one address)     (TypeError)
${typeName}: ${type.name} held twice with different contracts           (invalid, reason duplicate)
${typeName}: duplicate ${kind} name ${name}                             (invalid, reason duplicate；kind 只可能是 state/resource/listener/register——derivation 与 task 用位置键 #0,#1…，没有名字)
${typeName}: only objects of lifetime "host" may register               (invalid, reason not_host_lifetime)
${contract.name}: ${message}                                            (validateImplementation 的统一前缀，见下)
factory must return an object                                           (invalid, reason implementation，经上面的前缀)
unknown variant ${String(variant)}                                      (invalid, implementation)
${key} is not in variant ${String(variant)}                             (invalid, implementation)
${name} must be a method                                                (invalid, implementation)
${name} must be a cell, a derivation or a held object's state member     (invalid, implementation)
${name} is missing                                                      (invalid, implementation；opaque 成员)
listener ${key}: source has no subscribe()                              (invalid, implementation)
cells cannot be written while a factory runs                            (invalid, read_only)
state ${name}: version must be a positive integer                       (TypeError)
resource ${name}: version must be an integer                            (TypeError)
resource ${name}: deps must be references or resources of this object    (TypeError)
listener ${name}: the handler must be a function                        (TypeError)
listener ${name}: select must be a function                             (TypeError)
a task must be a function                                               (TypeError)
a derivation needs a function                                           (TypeError)
expose takes a resource declared by this object                         (TypeError)
${kind} needs a name                                                    (TypeError；kind ∈ state/resource/listener/register，name 非 string 或 "")
```

host（`host.ts`）：

```
createHost needs `self`: the principal of the host's own code              (TypeError，非 ChordError)
code version ${given} is not newer than ${current.id}                      (Error)
host.serve during a reload; serve from a facet passed to reload()          (Error)
host.install during a reload                                              (Error)
facet ${name} is already installed; use reload()                          (Error)
loading modules needs createHost({ loader }), e.g. nodeLoader from @earendil-works/chord/node   (Error)
facet ${name} was replaced or removed                                     (gone，作为 facet run 的 abort 原因)
${contract.name} is already served                                        (Error)
${contract.name} is declared for ${placementText(placement)}, but ${this.name} serves it        (TypeError)
${name} has lifetime "host": its one object has the empty input           (invalid, reason input)
${type.name} with this input lives on ${resolveHost(...)}, not ${this.name}                     (invalid, reason placement)
${from.address} -> ${address} would create a ref cycle                    (invalid, reason cycle)
${type.name} is being upgraded; retry                                     (upgrade_conflict, reason gate)
code version ${codeVersionId} is not retained by ${this.name}             (superseded)
code version ${ctx?.codeVersion} is retired                               (superseded)
${registry.name}: ${key} registered by ${existing.object.address} and ${object.address}          (invalid, reason registry_conflict)
principal invalidated                                                     (gone)
accepting a host link needs its name                                      (TypeError)
connect needs `principal`: what the parent's own requests act as          (TypeError)
route ${prefix} needs `principal`: what the routed hosts act as when they call back              (TypeError)
${connection.name} runs code version ${codeVersion}, ahead of ${current.id}  (invalid；随后 link.close("code version ahead of parent"))
no route to ${binding.at}                                                 (unavailable)
${hostName} refused for this principal                                    (forbidden)
principal invalidated while resolving                                     (gone)
${token.name} with this input does not live on ${at}                      (invalid, reason placement)
no route authorizes ${at}                                                 (forbidden)
reloads are coordinated by the server                                     (unavailable, reason not_coordinator)
host ${this.name} closed                                                  (gone)
${type} is not served by ${host}                                          (unavailable, reason unknown_type)
```

（`connection.ts` 的 4 条 hold 相关消息——`${this.name} does not relay`、`${token.name} is local; only remote types cross links`、`not a local reference`、`${token.name} with this input does not live on ${this.host.name}`——归在下面的 connection 段，不在 host 段。）

**host 补充（先前遗漏）**：

```
${contract.name}: unknown lifetime ${String(lifetime)}                     (TypeError；checkLifetime，实际由 makeService 在 serve 时抛)
${contract.name}: lifetime "host" needs a local type (remote types are referenced)   (TypeError)
${contract.name}: an object of lifetime "host" does not linger (it lives as long as the host)   (TypeError)
${contract.name}: lifetime "host" needs an input that accepts {} (the object's one address)     (TypeError)
${connection.name} runs code version ${codeVersion}, ahead of ${current.id}                      (invalid；同时 report 与 close)
```

注：`checkLifetime` 的 4 条 TypeError 也出现在 object.ts 的导出路径上（`makeService`），但文件归属与消息内容见 §2.3.1；此处只是提醒 Rust 侧它们同样算"用户可见"。

重载（`reload.ts`）：

```
a reload is already running                                               (unavailable, reason reload_running)
reload aborted while preparing                                            (aborted, reason aborted)
code version ${previous.id} was retired                                   (superseded，作为 previous code version abort 原因)
${what} within ${deadline} ms                                             (aborted, reason deadline)
reload aborted                                                            (aborted，rollback 时 abort 原因)
${object.address}: resource ${resource.key} could not be restored         (unavailable)
reload aborted; type not served                                           (gone)
type no longer served                                                     (gone)
${name}@${next.version} changed incompatibly without a version bump (${reason})     (contract_mismatch)
gate deadline (${deadline} ms) missed; blocking roots: ${blocking.join(", ")}       (aborted, reason gate_deadline, details {blocking})
${address}: version of state ${name} lowered from ${from} to ${to}        (invalid, reason state)
${address}: schema of state ${name} changed without a version bump        (invalid, reason state)
${address}: migrated state ${name}: ...                                   (invalid, reason state；`...` = assertValid 的 summary，见 §5.1)
${address}: derivation ${name}: ...                                       (invalid, reason state；同上)
reload aborted: ${cause.message}                                          (reloadError 包装；若已是则不再包)
${object.typeName} changed; reload the web facets                         (upgrade_required，用于 client 引用)
```

**reload 补充（先前遗漏的细节）**：

- `${address}: migrated state ${name}` 与 `${address}: derivation ${name}` 是 `assertValid` 的 `what` 前缀，**不是**完整消息；实际文本为 `${what}: ${summary}`，summary 为前 3 条校验错误的 `instancePath || "/"` + 空格 + message，以 `; ` 连接（见 §5.1 的 `assertValid`）。
- `reloadError(error)`（逐字语义）：`cause = asChordError(error, "reload")`；若 `cause.message.replace(/^[^:]*: /, "")` 以 `reload aborted` 开头（即**先剥掉可能的一层 `code[/reason]: ` 前缀再判断**）则**直接返回 cause**（幂等）；否则返回 `new ChordError(cause.code, `reload aborted: ${cause.message}`, { cause, reason: cause.reason, details: cause.details })`——code/reason/details **原样继承**。Rust 侧必须保留这个剥前缀再判断的细节。
- 迁移函数的 schema 校验走 `memberSchemas(service, run.implementation)` 对 `exposed.get(cell)` 的每个 schema，dev 模式下还会按**远程** schema 再校验一次（review 测试）。

remote（`remote.ts`）：

```
${key}.${name} is not hydrated yet                                        (not_ready)
${key}: ${name} is not a state member of variant ${shape?.variant}        (not_in_variant)
reference ${key} released                                                 (released)
${key}: ${name} is not a method of variant ${variant}                     (not_in_variant)
${address}.${name} called inside a factory                               (not_ready)
ctx belongs to another host                                              (invalid, reason foreign_ctx；CtxImpl.check 用在其它 host 的 ctx 上)
ctx must come from a Chord host                                          (invalid, reason foreign_ctx；RemoteRef.#caller 收到非 CtxImpl 的 ctx)
```

connection（`connection.ts`）：

```
relayed hold closed                                                      (gone)
principal invalidated                                                    (gone)
link to ${name ?? "peer"}: ${reason}                                     (invalid, reason protocol；同时 link.close(`protocol error: ${reason}`))
call aborted                                                             (aborted)
cancelled by caller                                                      (aborted，作为 cancel 的 abort 原因)
relayed hold released                                                    (released)
unknown hold                                                             (gone)
call cancelled                                                           (aborted)
link to ${name ?? "peer"} closed: ${reason}                              (unavailable；链路关闭时所有挂起调用的拒绝值)
```

**connection 补充（先前遗漏）：协议违规文本（全部经 `#violation` → `invalid("protocol", `link to ${name ?? "peer"}: ${reason}`)` 上报，并 `link.close(`protocol error: ${reason}`)`）**：

```
malformed frame                                                          (isFrame 失败)
duplicate hold id ${id}
duplicate call id ${id}
hello ${frame.kind} ${frame.name} does not match the accepted ${peer.kind} ${peer.name ?? ""}
parent is not a host                                                     (upstream hello 的 kind !== "host")
${frame.t} from a child                                                  (控制帧来自非 upstream)
vote from a non-participant                                              (vote 来自非 downstream host 连接)
ack ${n} outside ${this.#acked}..${this.#sent}                           (ack 越界或倒退)
Too many calls in progress on this connection (${maxCalls}); wait for earlier ones to finish   (unavailable, reason too_many_calls；以 result 帧回，链路保持)
```

**connection 补充（其它可见文本）**：

```
relayed hold closed                                                      (gone；RelayRef.call 与 relayCall 两处各产生一次，消息相同)
link to ${name ?? "peer"} closed                                         (unavailable；#call 时链路尚未 open，无 ": ${reason}" 后缀)
not a local reference                                                    (unavailable；#onHold 的 acquire 返回非 RefImpl，回 gone 帧)
${this.host.name} does not relay                                         (unavailable；upstream 链路收到非本宿主的 hold.at，回 gone 帧)
${token.name} is local; only remote types cross links                    (invalid, reason token；#onHold，回 gone 帧)
${token.name} with this input does not live on ${this.host.name}         (invalid, reason placement；#onHold，回 gone 帧)
```

manager（`manager.ts`）：

```
lock ${name} is held by another process                                  (unavailable, reason locked)
lock directory ${directory} must be a directory only its owner can access (unavailable)
child not ready in time                                                  (unavailable)
child exited before ready (${exit.code ?? exit.signal})                  (unavailable)
child failed: ${message.message}                                         (unavailable, reason=message.reason)
```

`too_many_calls` 的完整值（`AcceptOptions.maxCalls`，默认无限制）：拒绝一条调用时发送 `result` 帧 `{ t:"result", id, error: { code:"unavailable", message:`Too many calls in progress on this connection (${maxCalls}); wait for earlier ones to finish`, reason:"too_many_calls" } }`；链路保持打开。

### 3.5 日志输出

- 默认 `onError`：`error => console.error("chord:", error)`。即**前缀固定为 `chord:`** 后接一个空格与错误对象（Node 打印 `ChordError: invalid/args: ...` 及栈）。
- 框架内部所有"不应中断流程"的失败（监听器抛错、dispose 失败、facet 被替换时 abort 之后的二次错误、协议违规上报等）都走 `host.report(error)` → `#onError`。`onError` 自身抛错不捕获（会外泄到调用点）。
- `async` 的 `ready` promise 若调用方忽略返回值：框架内部已 `catch(() => {})`，不会产生 unhandled rejection，但失败仍会经 `report` 输出。

### 3.6 环境变量

| 变量 | 读取方 | 语义 |
| --- | --- | --- |
| `CHORD2_LOCK` | `childControl().lock()`（子进程）、`forkDriver`（父进程写入） | 锁名。子进程启动后调用 `lock()` 时若存在则取同名内核锁；不存在则什么都不做。父进程 fork 时**先删除继承来的同名变量**，再合并 `options.env`，最后（若给了 `options.lock`）写入自己的值。 |
| `TMPDIR`（及 `TMP`/`TEMP`，经 `os.tmpdir()`） | `processLock` 的 `lockSocketPath`（非 win32/linux/android 平台） | **不是 Chord 自己的变量**，但决定锁 socket 路径：`join(tmpdir(), \`chord2-${key}.sock\`)` 超过 103 字节时回退到 `/tmp/chord2-<uid>`。`manager.test.ts` 专门把 `TMPDIR` 设成很深的目录来覆盖回退分支——Rust 侧必须按同样的环境变量解析顺序。 |

本单元**只**读取以上变量；`FIXTURE_MODE`/`GRANDCHILD_FILE`/`HOST_NAME` 只出现在测试夹具 `test/fixtures/worker.ts`（不是库行为），`process.env.CHORD2_LOCK` 的直接设置在 `manager.test.ts` 里用于测试"子进程不继承父的锁"。

### 3.7 锁地址（逐字格式）

- `key = sha256(name)` 的十六进制小写、取**前 32 个字符**。
- win32：命名管道 `\\.\pipe\chord2-${key}`。
- Linux / Android：**抽象 unix socket**，地址 `\0chord2-${key}`（首字节 NUL；不落文件系统）。
- 其它平台（macOS/BSD 等，`file === true`）：socket 文件 `chord2-${key}.sock`，路径 `join(tmpdir(), file)`；若该路径字节长度 > `MAX_SOCKET_PATH_BYTES = 103`，改用 `/tmp/chord2-${uid}/chord2-${key}.sock`（目录 `0700`，且必须是属主为该 uid、无 group/other 权限的目录）。
- 冲突：`EADDRINUSE` 且（非 file 平台，或 `accepting(address)` 返回 true 表示有人 accept）→ `unavailable/locked`；file 平台且无人 accept（陈旧文件）→ `rm(address, {force:true})` 后重试。file 平台在进程 `exit` 时 unlink。

### 3.8 进程管理状态与生命周期事件

- `HostManager.status`：`"stopped" | "starting" | "running" | "stopping"`。
- `Liveness = { booting: boolean; activeCalls: number; viewers: number; durableTasks: number; idleFor: number }`；`idleFor` 毫秒，从**busy → idle 转变**起算，从未 busy 过则从启动成功起算，正在 busy 时为 0。
- `start()` 解析条件：收到 `{type:"ready"}`；拒绝：`{type:"fatal", message, reason?}`、超时（`readyTimeout ?? 30000`）、子进程提前退出。
- `stop()`：发 `{type:"shutdown"}`，`grace ?? 5000` ms 内未退出则 `killTree()`，然后等 `exited`。
- `replace()`：stop 后 start，**等退出后才启动下一个**。
- 子进程侧 `childControl()`：`init`（父进程立即发送的 `{type:"init", value}`）、`lock()`、`ready()`、`fatal(error)`、`report({activeCalls?, viewers?, durableTasks?})`、`onShutdown(handler)`。

### 3.9 链路关闭原因（`Link.close(reason)` / `onClose(reason)`，逐字）

| 原因 | 产生方 |
| --- | --- |
| `"closed"` | `BaseLink.close()` 默认原因；`onClose` 注册在已关闭链路上时立即以 `"closed"` 回调 |
| `"peer closed"` | 收到对端 `bye` 且无 reason 时 |
| `"protocol error"` | 解帧失败（IPC/memory 的 JSON 解析、WebSocket 解包失败等） |
| `"protocol error: ${reason}"` | `Connection.#violation`（malformed frame、duplicate hold id、hello 与 accept 决定不符、`vote from a non-participant`、`${frame.t} from a child`、`parent is not a host` 等） |
| `"protocol-version ${PROTOCOL_VERSION}"` | 协议版本不符时本端主动关闭（`PROTOCOL_VERSION = 4`） |
| `"ping timeout"` | `pingTimeout`（默认 45000 ms）内未收到任何帧 |
| `"principal invalidated"` | `Connection.revoke(undefined)`（`invalidatePrincipal` 未提供 restamp） |
| `"host closed"` | `HostImpl.close()` 关闭所有链路 |
| `"replaced"` | `connect()` 替换旧 upstream 链路 |
| `"unauthorized"` | `acceptWebSockets` 的 `principal(request)` 返回 `undefined` |
| `"authentication failed"` | `acceptWebSockets` 的 `principal(request)` 拒绝 |
| `"socket closed"` | WebSocket 的 `close` 事件 |
| `"ipc disconnected"` | IPC channel `disconnect` 事件 |
| `"ipc send failed: ${code ?? message}"` | IPC 写回调收到错误（EPIPE 等） |
| `"code version ahead of parent"` | 子 host 的 hello 声明的 code version 大于本 host 当前版本 |
| `"reconciliation failed"` | `reconcileChild` 失败 |
| `inflateMessage` 等抛错 | 见下 |

**解包/解帧的 TypeError（全部以 `close("protocol error")` 收尾，或由调用方转成 `protocol error`）**：

```
TypeError("inflateMessage needs zlib")                  (inflateMessage：本端无 zlib)
TypeError("packed payload without zlib")                (unpack：收到 Packed 但本端无 zlib)
TypeError("packed frames are not accepted")             (WebSocketLink acceptPacked=false 时收到二进制)
TypeError("unknown message type")                       (WebSocket 的 data 既非 string 也非 ArrayBuffer/TypedArray)
TypeError("bad packed payload")                         (z 字段不是 [field, base64]，或 field 不是 args/value)
TypeError("bad envelope")                               (二进制消息的 envelope JSON 不是对象)
TypeError("not a frame")                                (parseFrame：JSON 不是对象或 t 不是 string)
```

（`TypeError("a task must be a function")`、`TypeError("a derivation needs a function")`、`TypeError("expose takes a resource declared by this object")` 属于 object.ts 的 `self` API，不是链路错误——先前版本把它们错列在此表。）

## 4. 外部格式（逐字）

### 4.1 对象地址 / 远程 key / root id（跨进程可见）

```
object address  = `${type.name}\u0000${canonicalIdentity(input.schema, normalizedInput)}\u0000${principalPart}`
principalPart   = isSelf(principal) ? "host" : `p:${principalKey(principal)}`
remote key      = `@${type.name}\u0000${canonicalIdentity(type.input, normalizedInput)}\u0000${principalPart}\u0000${type.fingerprint}`
rootId          = `${host.name}#${incarnation}:${seq}`          // 本地 root；incarnation = Math.random().toString(36).slice(2, 8)
task rootId     = `task:${object.address}:${key}`
originId        = `${incarnation}:${seq}`                       // client 连接的 origin（由 server 分配）
```

- `canonicalIdentity(schema, value)`：对规范化值做 canonical JSON，并**省略等于 schema 默认值的字段**（逐层；数组按 items 递归）。
- `fingerprint = hash64(canonicalJson(description))`：两路 FNV-1a（`a` 初值 `0x811c9dc5`，`b = 0x01000193 ^ 0x5bd1e995`；逐 UTF-16 code unit：`a = imul(a ^ code, 0x01000193)`，`b = imul(b ^ code, 0x5bd1e995) ^ (b >>> 13)`），输出 `(a>>>0).toString(16).padStart(8,"0") + (b>>>0).toString(16).padStart(8,"0")` → **16 个小写十六进制字符**。Rust 必须逐位仿真 `Math.imul`（i32 乘法回绕）与 `>>> 13`（逻辑右移），并且输入是 UTF-16 code unit 序列（非 UTF-8 字节）。
- `canonicalJson`：对象键**排序**、丢弃值为 `undefined` 的键、非对象/数组走 `JSON.stringify`（`undefined` → `"null"`）；字符串按 JSON 转义。数字使用 JS `JSON.stringify` 的表示（Rust 需与 JS 一致：最短往返、`-0` → `0`、无指数形式的差异等）。

### 4.2 线上协议帧（`PROTOCOL_VERSION = 4`）

一帧一个 JSON 对象，字段名极短；所有帧用 `isFrame`（TypeBox `Value.Check`）校验，失败即 `#violation("malformed frame")` → 关闭链路 `protocol error: malformed frame`。方向约定：**id 由创建 hold/call/subscription 的一侧选择**，每种帧只有一个方向，所以 id 不冲突。

消费方 → 提供方：`hold`, `release`, `call`, `cancel`, `sub`, `unsub`, `resync`
提供方 → 消费方：`shape`, `gone`, `result`, `state`
双向：`hello`, `ping`, `pong`, `bye`, `root_end`, `ack`
父 → 子：`invalidate`, `prepare`, `commit`, `abort`, `retire`
子 → 父：`vote`

公共子 schema：

```
Id     = Integer{minimum:1}
Name   = String{minLength:1}
Reload = Integer{minimum:1}
List<T>= Immutable(Array(T))
RootRef        = { rootId: Name, codeVersion: Integer{minimum:1} }
WireError      = { code: Enum(CHORD_ERROR_CODES), message: String, reason?: String }
WireReloadOptions = { remove?: List<String>, gateDeadline?: Number, retireDeadline?: Number }
WireReloadReport  = { codeVersion: Integer, previous: Integer, changed: List<String>, removed: List<String>, prepared: List<String>, gated: List<String> }
```

逐帧字段（`?` = 可选）：

```
{t:"hello", protocol?: Integer{min:1}, name: Name, kind: "host"|"client", codeVersion: Integer{min:0}, acks?: Boolean}
{t:"ack", n: Integer{min:1}}                                    // 已处理的（除 ack 外）帧计数，从链路开始算
{t:"hold", id: Id, at: Name, token: DescriptionSchema, input: Unknown, principal?: Unknown,
         origin?: Name, state?: false, again?: true}
{t:"release", id: Id}
{t:"shape", id: Id, variant?: String, members: List<String>, contract: String, state: List<Name>}
{t:"gone", id: Id, error: WireError}
{t:"call", id: Id, hold: Id, member: Name, args?: Unknown, root?: RootRef}
{t:"result", id: Id, value?: Unknown, error?: WireError}
{t:"cancel", id: Id}
{t:"sub", hold: Id}
{t:"unsub", hold: Id}
{t:"resync", hold: Id, m: Name}
{t:"state", hold: Id, m: Name, as: "snap"|"reset"|"ops"|"error", value?: Unknown,
         ops?: List<List<Unknown>>, error?: WireError}
{t:"ping", n: Integer}
{t:"pong", n: Integer}
{t:"bye", reason?: String}                                       // 由 Link 自己消费
{t:"root_end", rootId: Name}
{t:"invalidate", pattern: Record<String, Unknown>}
{t:"prepare", reload: Reload, codeVersion: Integer{min:1}, plugins: List<String>, options: WireReloadOptions}
{t:"vote", reload: Reload, error?: WireError, report?: WireReloadReport}
{t:"commit", reload: Reload}
{t:"retire", codeVersion: Integer{min:1}}
{t:"abort", reload: Reload}
```

语义要点（必须保持）：

- `hello.protocol` 缺省视为 **1**；与本端 `PROTOCOL_VERSION` 不等即 `#protocolMismatch`（见 §3.9），并上报 `onProtocolMismatch({local, peer})`（peer 为 `undefined` 表示对方未声明）。
- `hello.acks` 决定是否启用流控：**两端都 announce 才启用**；在收到 hello 之前发出的帧也计入 `#unacked`，若对方不说 acks 则清空这些计数。
- `hold.at` 是消费方按 token 的 `host` 与 `input` 解析出的目标 host 名；provider 若发现自己不是该地址，且本连接是 **upstream（父链路）** → 回 `{t:"gone", error: unavailable("${this.host.name} does not relay")}`；否则创建 relay。
- `hold.principal` **只在来自父链路的帧上被采纳**（下游链路一律用 `accept` 时给定的 principal）。
- `hold.origin` 同理：下游链路用本连接自己的 `originId`（server 分配，不管帧里写什么），父链路用帧中转发值；`origin` 存在表示这是 client 的 hold（不是 reload 参与者）。
- `hold.state: false` = 仅方法引用，直到 `sub` 才有状态；`again: true` = 重连后的重 hold（消费方已有值，首个 unit 不优先）。
- `state` 中 `snap`/`reset` 若 `value` 缺省则值为 `undefined`；`ops` 是 delta 编码后的操作数组（每个 op 由接收侧 decoder 逐条校验；只有 dev 模式校验值本身）。
- `root_end` 只在 root 来过的链路上被采纳（`#admitted` 集合 + `HostImpl.foreignRootEnded`）。
- 控制帧（`invalidate`/`prepare`/`commit`/`abort`/`retire`）**只接受来自父链路**（`role === "upstream"`），否则 `#violation(`${frame.t} from a child`)`。`vote` 只接受来自 `downstream` 且 `peer.kind === "host"` 的连接，否则 `#violation("vote from a non-participant")`。
- `release` 对未知 id 是 no-op；`cancel` 对未知 id 是 no-op；`sub`/`unsub` 对未知 hold 是 no-op；`resync` 对未知 hold 是 no-op。
- 协议违规还包含：`malformed frame`、`duplicate hold id ${id}`、`duplicate call id ${id}`、`hello ${kind} ${name} does not match the accepted ${peer.kind} ${peer.name ?? ""}`、`parent is not a host`、`ack ${n} outside ${acked}..${sent}`（ack 数越界或倒退）。

### 4.3 载荷打包（跨链路透明优化）

- 触发条件：payload 字段的 `JSON.stringify` 长度 **> `PACK_THRESHOLD = 64 * 1024`** 字符（**严格大于**；等于阈值不打包），且发送端启用压缩（`WebSocketLink.compress` 默认 **false**；`acceptWebSockets` 服务端侧设为 **true**；memory/IPC 恒为 true）且本端有 zlib。
- **zlib 是可选依赖**：`src/link.ts:115-117` 通过 `` (globalThis as {...}).process?.getBuiltinModule?.("node:zlib") `` 探测，**不** `import "node:zlib"`（type-only import）。探测失败（浏览器）则：发送端永不打包；接收端收到 Packed 时 `unpack` 抛 `TypeError("packed payload without zlib")`，WebSocket 路径改用 `DecompressionStream` 异步解压。Rust 侧没有这一分支，但必须在移植说明里保留"通信双方不一定都能压/解"的语义（Rust 端恒可解，故对端压缩总是安全）。
- 压缩算法：**raw deflate，level 1**（`zlib.deflateRawSync(json, { level: 1 })`）；解压 `inflateRawSync`。
- 只有两个字段会被打包：`call.args`、`result.value`/`state.value`（`payloadField`）。
- 对象传输（memory / IPC）：帧去掉该字段，改为 `z: [field, base64(bytes)]`，其中 `field ∈ {"args","value"}`，base64 为**标准字母表 + padding**。
- WebSocket：打包帧是**二进制消息**，布局为
  `[u32 大端：envelope JSON 字节长度][envelope JSON（含 "z": field，不含 payload）][deflate 后的 payload 字节]`；非打包帧是**文本**消息（正常情况下也是把 payload JSON 直接拼进 envelope，避免二次编码：`${head.slice(0,-1)},${JSON.stringify(field)}:${json}}`）。
- 收到打包帧但本端无 zlib（浏览器）：用 `DecompressionStream("deflate-raw")` 异步解压；解码期间后续帧**排队等待**（`deliverLater`），保证顺序；解压失败 → `close("protocol error")`。
- `acceptPacked: false`（服务端对不可信客户端）时收到二进制帧 → `TypeError("packed frames are not accepted")` → `close("protocol error")`。
- 中继（relay）只转发不读 payload：`Packed` 对象原样透传，避免解压/再压。
- **`inflateMessage(data)`（逐字实现）**：先 `splitMessage` 取 `{envelope, field, bytes}`，再把 envelope `JSON.stringify`，然后
  `` `${head.slice(0, -1)},${JSON.stringify(field)}:${zlib.inflateRawSync(bytes).toString("utf8")}}` `` —— 即把解压后的 payload **文本原样拼进** envelope JSON 的末尾并补 `}`，**不重新解析/校验 payload**（保留原始数字/转义表示）。它用于"只中继文本帧的传输"（TUI launcher 喂其沙箱）。无 zlib 时抛 `TypeError("inflateMessage needs zlib")`；`splitMessage` 的 `TypeError` 会原样上抛（`bad envelope`/`bad packed payload`）。
- `splitMessage` 的细节：`length = DataView(...).getUint32(0)`（**大端**，默认）；envelope 从 `data.subarray(4, 4+length)` 解码；payload 用 `data.slice(4+length)`（**拷贝**，不是视图）；envelope 里 `z` 字段必须存在且为 string，否则 `TypeError("bad packed payload")`。
- 对象传输的 `z` 字段校验（`unpackObject`）：`"z" in message` 且**必须是数组**、`z[0] ∈ {"args","value"}`、`z[1]` 为 string，否则 `TypeError("bad packed payload")`；base64 用 `Buffer.from(z[1], "base64")` 解码（**宽松**：Node 忽略非法字符，不抛错）。

### 4.3.1 `Link` 接口与三种传输（此前只在 §2.4 提到 `link.receive/onClose`，接口本身未逐字给出）

```ts
export interface Link {
  readonly closed: boolean;
  send(frame: Frame): number | undefined;   // 返回"按线上编码后的尺寸"；不报告尺寸的链路返回 undefined，因而永不被窗口化
  receive(handler: (frame: Frame) => void): void;   // 单接收者；先到的帧排队
  onClose(handler: (reason: string) => void): void;
  close(reason?: string): void;             // 两端关闭：对端收到 bye
}
```

`BaseLink`（抽象基类，未导出）的共享行为（Rust 侧等价 trait/默认实现必须一致）：

- `#queue: Frame[]`：`receive(handler)` 之前到达的帧入队；设置 handler 时按序同步 flush。
- `#decoding: Promise<void> | undefined`：异步解压（浏览器）期间**后续帧排队**；`deliverLater(promise)` 串成一条 `then` 链，解压失败 → `close("protocol error")`，链尾在同一 promise 上清空 `#decoding`。
- `#deliverNow(frame)`：已关闭则丢弃；`bye` → `ended(frame.reason ?? "peer closed")`（`bye` **不会**到达上层 handler）；无 handler → 入队。
- `ended(reason)`：幂等；置 `closed`；**清空 receive 队列**；`shutdown()`（transport 清理）；在 `finally` 里逐个调 close handler 后清空集合。即 close handler 抛错也不会漏掉其它 handler。
- `onClose(handler)`：**已关闭时立即同步以 `"closed"` 回调**（注意：不是因为对端 `bye` 而关闭的 `"peer closed"`——已关闭后注册一律得到 `"closed"`）。
- `close(reason = "closed")`：已关闭 no-op；先尝试 `transmit({t:"bye", reason})`（**传输可能已死，异常被吞**），再 `ended(reason)`。因此对端看到 `bye.reason`，本端 close handler 拿到的是 `reason` 本身。
- `send(frame)`：已关闭返回 `undefined`；否则 `transmit(frame)`（实现方负责打包）。

三种 transport 的实现差异（全部逐字核对自 `src/link.ts`）：

| 传输 | `transmit` 返回值 | 打包 | 关闭/异常 |
| --- | --- | --- | --- |
| `MemoryLink` | `data.length`（打包后 JSON 的**字符数**） | `packObject(frame, true)`，恒压缩 | `sever()`：**模拟断网**——两个方向静默丢帧且**不触发 close**（`#severed` 置位，`transmit` 返回 undefined、投递跳过）；`shutdown()` 把 `peer?.ended("peer closed")` 串到 `#pending` 链尾 |
| `IpcLink` | `undefined`（不参与流控） | `packObject(frame, true)`，恒压缩 | channel `disconnect` → `ended("ipc disconnected")`；写回调报错 → `ended(`ipc send failed: ${code ?? message}`)`；`transmit` 仅在 `channel.connected` 时发，**不检查返回值**；`shutdown` 解绑两个监听 |
| `WebSocketLink` | 文本：`data.length`；二进制：`data.byteLength` | `compress` 默认 **false**；`acceptPacked` 默认 **true** | `open` 事件前 `#outbox` 缓冲（`transmit` 仍返回尺寸），open 时按序 flush；`close` 事件 → `ended("socket closed")`；`shutdown`：清空 outbox，`readyState <= OPEN` 时 `socket.close(1000)`；构造时若 socket 支持则设 `binaryType = "arraybuffer"` |

`MemoryLink` 的 FIFO 细节（可观察）：`transmit` 先 `JSON.stringify(packObject(frame, true))`（**JSON 往返**，顺带拒绝 JSON 无法表达的值如 bigint），然后把"解析并投递"追加到**每方向一条** `#pending` promise 链——因此投递总是异步且保序，`send` 返回 `data.length`；`MemoryLink.peer` 是公开字段。

`IpcLink`/`ipcLink`：`ipcLink = (channel: IpcChannel) => new IpcLink(channel)`。`IpcChannel` 是结构化接口（`connected`、带可选 callback 的 `send`、`on/off("message"|"disconnect")`），Node 的 `process` 与 `ChildProcess` 都满足。

`WebSocketLink`/`webSocketLink`：`webSocketLink(socket, options?) => new WebSocketLink(socket, options)`。`WebSocketLike` 只要求 `readyState`、`send(string|Uint8Array)`、`close(code?, reason?)`、`addEventListener("message", e => e.data)` 与 `addEventListener("open"|"close"|"error", () => void)`——浏览器 `WebSocket`、Node 全局 `WebSocket`、`ws` 的 socket 都满足，**生产代码不依赖 `ws`**。

`Packed`/`unpack`：`Packed { readonly bytes: Uint8Array }`；`unpack(value)` 非 Packed 原样返回，否则无 zlib 抛 `TypeError("packed payload without zlib")`、有则 `JSON.parse(inflateRawSync(bytes).toString("utf8"))`。

### 4.4 IPC 信封

- Chord 帧：`{ chord2: <frame> }`（frame 可能已被 `packObject` 打包）。
- 控制消息：`{ chord2control: <ControlMessage> }`，`ControlMessage` 为
  `{type:"init", value}` | `{type:"ready"}` | `{type:"fatal", message, reason?}` | `{type:"shutdown"}` | `{type:"status", activeCalls?, viewers?, durableTasks?}`。
- IPC 链路**不报告尺寸**（`transmit` 返回 `undefined`），因此不参与流控（窗口永不填满）。
- IPC 的**入站**处理：只处理带 `chord2` 字段的消息，其它消息（控制消息、应用消息）**静默忽略**（不报错）；`chord2` 值经 `unpackObject` 解包，任何异常 → `close("protocol error")`。
- IPC 的**出站**处理：`channel.connected === false` 时**静默丢弃**（不抛错、不关闭链路——`disconnect` 事件随后会关）；每个 `send` 都挂 `#onSent` 回调以捕获 EPIPE。
- `childControl` 的 `fatal`：`{type:"fatal", message: error instanceof Error ? error.message : String(error), ...(ChordError && reason !== undefined ? {reason} : {})}`——即**message 是原始 message，不含 `code/reason: ` 前缀**。
- `childControl` 的其它细节：`send` 前检查 `channel.connected`（父进程已死则丢弃，靠随后的 `disconnect` 发现）；`init` 是**永远 pending** 的 promise，直到收到 `{type:"init"}`；`shutdown` 的 handler 可以返回 promise，但**不会被 await**（`void handler()`，多个 handler 并发）；`lock()` 在 `CHORD2_LOCK` 未设置时直接 resolve。

### 4.5 流控状态（链路内，不可见但影响帧顺序）

- 默认 `window = 256 * 1024` 字节，`pendingCap = 4 * window` 字节。
- 只有 `state` 帧会等待窗口（`call`/`result` 不等待，可越过后面的 state 帧；同一订阅的帧保持顺序）。
- 新引用的**第一个 unit 不等待**（`get` 不会排在别的流后面）。
- `ack` 为累计帧数（不含 ack 自身），收到 ack 后从队首弹出对应数量并归还字节。
- 每个 (hold, member) 一个等待槽：后来者合并（coalesce）；`ops` 超过槽上限则替换为当前值的 `reset`；总等待量超过 `pendingCap` 时，最老的等待流被替换（provider 发 reset；relay 向下游请求 `resync`）。
- 引用被 release 时丢弃它的等待槽。

### 4.6 重载报告

`ReloadReport`（本地/`host.reload()` 返回值）：

```
{ codeVersion: number, previous: number, changed: string[], removed: string[],
  prepared: string[], gated: string[], children?: Record<string, ReloadReport> }
```

`children` 仅在集群重载时存在（键为子 host 的配置名）。`vote` 帧中的 `report` 是同一结构**去掉 `children`**。

`ReloadOptions`：`{ remove?: string[], gateDeadline?: number /*5000*/, retireDeadline?: number /*5000*/, remote?: Record<string, string[]>, remoteRemove?: Record<string, string[]>, voteDeadline?: number /*gate+retire+5000*/ }`。

## 5. 跨单元接口（本单元消费/暴露的确切类型）

本单元的 7 个文件**不实现**下列模块，但直接依赖其语义。Rust 侧须把它们作为同一 crate 的兄弟模块（或独立 crate）先实现，接口按此表对齐。

### 5.1 消费（本单元 import 的符号）

| 单元 | 符号 | 本单元用它的地方 | 关键语义 |
| --- | --- | --- | --- |
| `schema` | `canonicalJson(value) -> string` | host（principalKey 默认、identity）、contract | 键排序、丢 undefined、数组递归 |
| | `hash64(text) -> string` | contract 指纹 | 见 §4.1（FNV-1a 双路，UTF-16 code unit） |
| | `deepEqual(a,b) -> bool` | host `matchesPattern`、reload（cell schema 比较） | `===` 或 canonicalJson 相等 |
| | `assertValid(reason, what, schema, value)` | connection（hold 输入/snapshot）、object、reload | 失败抛 `invalid(reason, `${what}: ${summary}`)`，summary 为前 3 条 `instancePath || "/"` + 空格 + message，用 `; ` 连接 |
| | `normalize(schema, value, what) -> unknown` | host `#locate`、object 方法参数 | `Clean(Default(Clone(value ?? {})))` 后 `assertValid("input", ...)`；返回新值 |
| | `canonicalIdentity(schema, value) -> string` | host 地址/远程 key | canonical JSON 且省略等于默认值的字段 |
| | `fits(sub, sup) -> bool` | contract 覆盖判定 | schema 子类型检查（见 review 测试 #11） |
| `json` | `copyJson(value, {omitUndefinedProperties?}) -> JsonValue` | 参数/结果拷贝（经 delta `Value.Clone` 等价路径） | 严格 JSON；拒绝非有限数、循环、稀疏数组、非普通原型 |
| `errors` | 见 §2.1 | 全部 | — |
| `publish` | `enqueue(task)`、`holdPublications(fn)` | object 通知、host 注册表通知、reload commit | **进程级** FIFO；`holdPublications` 期间入队但不派发，结束后一次性按序 drain；drain 期间重入的 enqueue 直接排到队尾（不递归）；task 抛错必须自行 report（drain 不捕获） |
| `registry` | `Registry<V> {name, conflict: "error"|"last"}`, `RegistryTable<V> = ReadonlyMap<string,V>`, `RegistryView<V>` | host | `table(ctx?)` 按 `ctx.codeVersion`；无 ctx 用 current；`subscribe` 立即同步回调一次 |
| `cell` | `Cell<T>`（`value`/`schema`/`version`/`init`/`change`/`replace`/`reset`/`subscribe`/`events`）、`Derived<T>`（`attach`/`detach`/`compute`/`refresh`）、`HYDRATION`、`Hydration`、`isSource`、`notReady(what)`、`sharesStructure` | object、remote | cell 写入需合法 Ctx；factory/recording 模式下写入抛 `invalid/read_only`；Derived 同步计算，依赖变更后按 seq 顺序重算 |
| `delta` | `Op`、`WireOp`、`Encoder`/`encoder()`、`Decoder`/`decoder()`、`apply`、`applyImmutable`、`isBase` | connection（编码 ops）、remote（解码/应用）、object（`self.state` 的 diff） | op 词汇：`["r", value]`、`["s", path, value]`、`["d", path]`、`["a", path, text]`、`["t", path, count]`、`["i", path, index, items]`、`["p", path, index, count, items]`、`["m", path, order]`；`isBase` = 首 op 为 `r`；codec 会 interning 路径并省略相邻重复路径；接收侧 decoder 逐条校验，遇到 base 重置字典 |
| `declare` | `Subscribable<E>`、`ResourceDeclaration<T>`、`Resource<T>`、`ListenerDecl`、`ValueOf`、`EventOf` | object `Self` | — |
| `types` | `GetOptions`、`Ctx`、`StateEvent`、`Readable`、`RefStatus`、`Shape` | object/remote/connection | `Shape = { contract: string; variant: string|undefined; members: string[] }` |
| `link` | `Link`、`Packed`、`unpack`、`PACK_THRESHOLD`、`MemoryLink`（含 `peer`/`sever()`）、`IpcLink`/`ipcLink`、`WebSocketLink`/`webSocketLink`、`WebSocketLike`、`WebSocketLinkOptions`、`IpcChannel` | connection、manager、host | 见 §4.3/§4.3.1/§4.4；`BaseLink` 是 `link.ts` 内私有抽象类，三种 transport 都继承它 |
| `wire` | `Frame`、`isFrame`、`PROTOCOL_VERSION`、`toWireError`、`fromWireError`、`RootRef`、`WireError`、`WireReloadOptions`、`WireReloadReport` | connection、host、reload、remote | 见 §4.2 |
| `cluster` | `runClusterReload(host, plugins, options) -> Promise<ReloadReport>`、`reconcileChild(host, connection)`、`ChildReloads` | host.reload / peerHello / onReloadFrame | `runClusterReload` 由 server 调用：`synced = downstream && peer.kind==="host" && open && synced` 的连接；发 `prepare`；任何失败/超时 → 向所有参与者发 `abort`、`rollback`、抛首个失败；否则 `reload.commit(beforeStart)` 里发 `commit` 并更新 `peerCodeVersion`，最后 `rememberRemoteModules` 并汇总 `children` |
| `loader` | `nodeLoader: FacetLoader` | host.resolve（通过 `HostOptions.loader` 注入） | 每次加载生成新 `?chord2-reload=<n>`；default 导出必须是 facet，否则 `TypeError(`${name}: default export must be a facet (defineFacet)`)` |
| `websocket` | `acceptWebSockets(server, host, options)` | Node 宿主 | 每连接先 `webSocketLink(socket, {compress:true, acceptPacked:false})`，再异步认证；`undefined` → `close("unauthorized")`；拒绝 → `close("authentication failed")` |

### 5.2 暴露（其它单元从本单元 import）

| 符号 | 使用者 | 说明 |
| --- | --- | --- |
| `HostImpl` 类（含 `peerHello`、`connectionClosed`、`announceRetired`、`allowRetire`、`onReloadFrame`、`relay`、`foreignRootEnded`、`rootSent`、`report`、`rememberRemoteModules`、`remoteModules`、`remoteRemoved`、`peekNextCodeVersion`、`current`/`previous`/`pending`/`reloading`/`gating`、`whenReloadEnded`、`endRoot`、`retire`、`retirable`、`watchRetirement`、`facetApi`、`serveInto`、`nextCodeVersionId`、`buildTables`、`liveDecls`、`installTables`、`retireImplementation`、`fail`、`objects`、`codeVersions`、`bindings`、`connections`、`hostObjects`、`facets`、`#childReloads`、`#defer`…） | cluster.ts、websocket.ts、loader.ts、connection.ts、reload.ts、object.ts | `HostCore` 接口（object.ts 定义）是 `ObjectImpl` 需要的宿主视图，见 §2.3.4 |
| `Connection`（含 `votes: Map<number,{resolve,reject}>`、`send`、`name`、`role`、`peer`、`open`、`synced`、`peerCodeVersion`、`whenSynced`、`markSynced`、`revoke`、`hold`、`release`、`call`、`wantState`、`nextId`、`fencedCodeVersions`/`#admitRoot`、`restamp`、`principal`、`originId`） | cluster.ts、host.ts、remote.ts | 实现 `RemoteChannel` |
| `Reload` 类（`prepare`/`commit`/`rollback`/`waitForRetirement`/`summary`）、`beginReload`、`endReload`、`runReload`、`reloadError`、`ReloadOptions`、`ReloadReport` | cluster.ts、host.ts、index.ts | — |
| `ObjectImpl`（`start`、`run`、`open`、`waitHydrated`、`initCells`、`unwind`、`decls`、`service`、`status`、`gate`、`leases`、`refs`、`openOrder`、`stageListeners`、`startRuntime`、`startTasks`、`stopTasks`、`openGate`、`admit`、`guard`、`call`、`rootEnded`、`addRef`、`release`、`dispose`、`refreshSchemas`、`allDecls`、`currentInstalled`、`implementations`、`pending`、`deferredRuntime`、`callAbort`、`address`、`typeName`、`input`、`principal`、`hostLifetime`） | host.ts、reload.ts、connection.ts、remote.ts | — |
| `RefImpl`、`RefCore`、`RefOwner`、`StatusListener`、`REF_BASE`、`refsByProxy`、`refOf`、`notifyStatus`、`variantOf`、`memberSchemas`、`awaitReady`、`raceSignal`、`linkedSignal`、`CtxImpl`、`makeService`、`hostLifetime`、`ResourceImpl`、`Decls`、`Run`、`Installed`、`HostCore`、`Service`、`ObjectStatus`、`StagedListeners`、`ObjectIdentity` | host/connection/reload/remote/wire | **注意（常见误列）**：`MemberReadable`、`Gate`、`Subscription`、`ResourceFacade`、`facades`、`resourceOf`、`isReadableLike`、`validateImplementation` 都是 object.ts 的**私有**声明（无 `export`）；本单元之外无法 import。`describeMember` 同理是 contract.ts 的私有函数。`StagedListeners`/`ObjectIdentity`/`RefOwner`/`StatusListener` 才是 `export` 的 |
| `RemoteBinding`、`RemoteRef`、`RemoteChannel`、`RemoteHost`、`StateIn` | connection.ts、host.ts | — |
| `processLock`、`forkDriver`、`createHostManager`、`childControl`、`ControlMessage`、`Liveness`、`Exit`、`Instance`、`Driver`、`ForkDriverOptions`、`HostManager`、`HostManagerOptions` | node 入口、应用 | 仅 Node |
| `contract` 全部导出（含 `LOCAL_VARIANT`、`RESERVED_MEMBERS`、`DescriptionSchema`、`MemberDescription`（type）、`contractFromDescription`、`resolveHost`、`placedOn`、`placementText`、`takesNoArgs`、`bind`、`coverage`；**`describeMember` 与 `MemberDescriptionSchema` 都是私有声明，不导出**） | connection/reload/remote/wire | — |

### 5.3 不变量（跨单元共享，必须一致）

1. `ChordError.message` 自带 `code[/reason]: ` 前缀；`WireError.message` 也带；`fromWireError` 剥一层。
2. `StateEvent` 三种：`{type:"snap", value}`、`{type:"ops", ops, value}`、`{type:"reset", value}`；`ops` 是 **Op 数组**（已解码，非 wire 形式）。
3. `RefStatus = "loading" | "live" | "released" | "gone"`。
4. 通知一律经 `publish.enqueue`（进程级），除显式注明为"同步"的两处：`RemoteRef.shapeChanged` 对 shape 监听器是**同步**调用；`registry.view.subscribe` 首次回调是**同步**。
5. 所有 `Map` 的顺序即插入顺序（Rust `IndexMap`），它在多处是可观察行为：`codeVersions`、`objects`、`bindings`、`connections`、`registries` 的 key 顺序、`lingering` 的淘汰顺序、`facetRuns`、`deferred`、`prepared`/`gated` 报告数组、`closing`（逆 openOrder）。
6. `taskStopDeadline` 默认 5000，`readyTimeout` 默认 60000，`gateDeadline`/`retireDeadline` 默认 5000，`voteDeadline` 默认 `gate+retire+5000`，manager `grace` 默认 5000、`readyTimeout` 默认 30000、`checkInterval` 默认 1000，连接 `pingInterval` 15000、`pingTimeout` 45000、`window` 256 KiB、`pendingCap` 4 windows，`linger.cap` 4，`PACK_THRESHOLD` 64 KiB，`MAX_SOCKET_PATH_BYTES` 103，`ABORTED_ROOTS_GRACE` 1000，reconcile vote deadline 30000。

### 5.4 导出符号统计（供交叉核对）

7 个范围文件的 `export` 声明共 **124** 行（`grep -cE '^export '`）：`contract.ts` 41、`object.ts` 35、`host.ts` 18、`manager.ts` 12、`reload.ts` 7、`connection.ts` 6、`remote.ts` 5。其中**运行期**导出行（`export const/function/async function/class/abstract class/let`）共 **52** 行：contract 19、object 16、reload 5、manager 4、connection 3、host 3、remote 2；其余 72 行为纯类型（`export type/interface/declare`），Rust 侧对应为 trait/struct/enum 或直接略去。

**计数口径注意**：contract.ts 的 19 行运行期导出里有 3 行是同一 `method` 的**重载签名**（`src/contract.ts:31-34`：`method()`、`method(args)`、`method(args,result)` + 实现），按**不同符号名**去重后 contract.ts 只有 **16** 个运行期符号、7 文件合计 **49** 个（`method`, `opaque`, `hostNamed`, `hostFamily`, `resolveHost`, `placedOn`, `placementText`, `LOCAL_VARIANT`, `DescriptionSchema`, `RESERVED_MEMBERS`, `remote`, `local`, `contractFromDescription`, `coverage`, `bind`, `takesNoArgs` = 16；object 16；host 3；manager 4；reload 5；connection 3；remote 2）。另：`DescriptionSchema`/`RESERVED_MEMBERS`/`LOCAL_VARIANT` 是运行期值但未进 index.ts。

## 6. npm 依赖 → Rust crate 映射

`package.json` 的**运行时依赖只有两个**：`typebox@1.3.27`、`@earendil-works/kiana-telemetry@1.4.0`。其余均为 Node 内建模块或结构化接口。

| npm / Node | 用途（本单元） | 建议 Rust 替代 | 必须桥接的行为差异 |
| --- | --- | --- | --- |
| `typebox`（`Type.*`、`Value.Check/Errors/Clean/Default/Clone`、`Static`） | 契约 schema 的构造与校验 | `serde_json::Value` 承载 schema + 自研 JSON Schema 子集校验器（`jsonschema` crate 可作为底座，但需支持 TypeBox 的 `default`、`additionalProperties`、`anyOf`、`not`、`pattern`、`format` 及 `fits` 子类型判定） | **最关键**：`Value.Clean`（删未知字段）、`Value.Default`（填默认值）、`Value.Clone` 的精确语义；`Value.Errors` 的 `instancePath`/`message` 文本会进入用户可见错误消息，必须复刻 TypeBox 的措辞（如 `Expected number`）；`fits` 必须与 TS 版本给出相同真假（review 测试 #11 用 `Value.Check` 反例校验） |
| `node:zlib`（`deflateRawSync`/`inflateRawSync`） | 载荷打包 | `flate2`（`Compression::fast()` = level 1，`DeflateEncoder` raw） | deflate 输出字节**不要求**逐字节相同（对端解压即可），但 raw 格式、level 1、阈值判定（按 JS 字符串长度）必须一致；浏览器路径用 `DecompressionStream("deflate-raw")` 等价实现 |
| `node:crypto` `createHash("sha256")` | 进程锁 key | `sha2`（`Sha256`） | hex 小写取前 32 字符 |
| `node:net` `createServer`/`connect` | 进程锁（抽象 socket / 命名管道 / socket 文件） | `tokio::net::UnixListener`（Linux 抽象地址：`SocketAddr::from_abstract_name`，需要 `socket2`/`interprocess`）、Windows 用 `interprocess` 的 named pipe；macOS 用文件 socket | Linux/Android 必须用**抽象命名空间**（不落盘）；Windows 管道名逐字 `\\.\pipe\chord2-<key>`；其他平台 `sun_path` 103 字节限制与 `/tmp/chord2-<uid>` 0700 回退 |
| `node:child_process` `fork` | 子 host 进程 | `tokio::process::Command`（或 `std::process`）+ 自建 IPC | 需要 `detached` 等价（POSIX `setsid`/进程组），`process.kill(-pid, "SIGKILL")` = 杀整个进程组（`killpg`）；IPC 需提供 `connected`/`send`/`on("message")`/`on("disconnect")` 的等价通道（可用 stdio 上的长度前缀 JSON，但要保持 §4.4 的 `{chord2}`/`{chord2control}` 信封语义与 EPIPE 处理） |
| `node:module` `registerHooks` + 动态 `import()` | `nodeLoader` 热重载 facet 模块 | Rust 无直接等价 | 需自建"模块→facet"的插件加载机制（或 FFI 到 JS 运行时）。这是**最难复刻**的一处；可先用"注册表 + 重新构造 facet 定义"的静态方案，但 `nodeLoader` 的可见语义（每次 reload 得到新模块副本、`?chord2-reload=n` 传播到同目录子导入、default 导出校验、facet 名 = URL）需在 Rust 插件模型里找到对应物 |
| `ws@8.21.0`（devDependency，测试用） | WebSocket 服务器 | `tokio-tungstenite` / `axum` ws | 只在测试中出现；`WebSocketLike` 是结构化接口，生产代码不依赖 `ws` |
| `vitest@4.1.12` | 测试 | `cargo test` + `tokio::test` | 需要 `until()` 轮询式断言的等价实现（tests 里到处用 `until(check, attempts=200)`，每 2 ms 轮询） |
| `@earendil-works/kiana-telemetry@1.4.0` | 仅 `Context.telemetry`（`context/` 单元） | 空实现 / `tracing` | 本单元不直接使用；boundary 测试要求它仍是唯一 Kiana 依赖 |
| Node 全局 `WebSocket`/`Blob`/`DecompressionStream` | 浏览器侧解包 | `flate2` + `tokio-tungstenite` | — |
| `structuredClone`（reload migrate） | 深拷贝旧值再交给 `migrate` | `serde_json::Value::clone()`（JSON 值语义等价；注意 JS 里 `undefined`/非 JSON 值不会出现在 cell 中） | `structuredClone` 对纯 JSON 值 == 深拷贝 |
| `AbortController`/`AbortSignal` | ctx、取消、facet 生命周期 | `tokio_util::sync::CancellationToken` + 原因携带 | 必须能携带 `ChordError` 作为"原因"，且 `ctx.check()` 依据原因区分 `superseded` 与 `aborted`（见 §2.3） |
| `WeakMap`/`WeakRef`（`refsByProxy`） | proxy → ref 映射 | `HashMap<ProxyId, Weak<RefCore>>` 或直接以句柄代替 JS Proxy | Rust 没有 JS Proxy；本单元 API 面的 "proxy 晚绑定" 需改为显式 handle + `member(name)` 分发（见 §8） |
| `queueMicrotask`/`Promise` 调度 | 各处 await 顺序 | `tokio` 单线程 runtime 可近似，但**微任务顺序**不同 | 凡是可观察的 await 顺序（`Promise.allSettled` 的遍历顺序、`Promise.race`、`await` 之间插入的同步代码）必须逐处核对 |
| `node:fs` `unlinkSync` | 进程锁 socket 文件在 `process.once("exit")` 时删除（`manager.ts:112`） | `std::fs::remove_file` + `libc::atexit`/`Drop` guard | 必须在**进程退出**时删（不只是正常的 release 路径）；Rust 用 `atexit` 或全局 drop guard，且吞掉 ENOENT |
| `node:fs/promises` `lstat`/`mkdir`/`rm` | `lockSocketPath` 的 `/tmp/chord2-<uid>` 目录校验（`mkdir({mode:0o700,recursive:true})`、`lstat` 检查 `isDirectory`/`uid`/`mode & 0o077 === 0`）、陈旧 socket 文件 `rm({force:true})` | `std::fs::metadata`/`create_dir_all` + `std::os::unix::fs::PermissionsExt`、`remove_file`（忽略 NotFound） | `mode & 0o077 !== 0` → **拒绝使用**（不是纠正 chmod）；uid 必须等于 `getuid()`；无 `getuid` 时按 0（Windows 不走该分支） |
| `node:os` `tmpdir` | `lockSocketPath` 的首选目录 | `std::env::temp_dir()` | 与 Node 的 `TMPDIR`/`TMP`/`TEMP` 解析顺序保持一致（测试用深 TMPDIR 触发回退） |
| `node:path` `join` | 同上 | `Path::join` | — |
| `node:url` `pathToFileURL` | `loader.ts` 把模块路径转成 `file:` URL（`toUrl`） | `url::Url::from_file_path` | **相对路径**必须按当前工作目录解析成绝对路径（`pathToFileURL` 的行为），且已经是 `file:` 的字符串原样保留；`URL` 实例克隆而非复用 |
| `Buffer`（`Buffer.from(bytes).toString("base64")`、`Buffer.from(z[1], "base64")`、`Buffer.byteLength`） | 对象传输的 `z` 字段、`MAX_SOCKET_PATH_BYTES` 判定 | `base64` crate（标准字母表 + padding；解码用**宽松**模式，忽略非法字符） | 编码必须**标准字母表 + `=` padding**（非 URL-safe）；`Buffer.byteLength(string)` 是 **UTF-8 字节数**（判断 sun_path 长度用），不是 JS 字符数——与 `PACK_THRESHOLD` 的字符数口径**不同** |
| `TextEncoder`/`TextDecoder`（`src/link.ts:178-179`） | WebSocket 二进制 envelope 的 UTF-8 编解码 | `str::as_bytes()` / `String::from_utf8` | `TextDecoder` 默认**非 fatal**：非法 UTF-8 会被替换成 U+FFFD 而不是抛错（随后 JSON.parse 多半失败 → `protocol error`）；Rust 若用严格解码会得到不同错误路径 |
| `globalThis.process?.getBuiltinModule?.("node:zlib")`（`src/link.ts:115-117`） | **可选** zlib 探测：让同一份代码在浏览器跑 | 不需要（Rust 恒有 flate2）；但需保留"对端可能不压缩/可能只发文本"的兼容分支 | 见 §4.3；TS 侧无 zlib 时**发送端永不打包**、接收端仅 WebSocket 走 `DecompressionStream` |
| `Math.random().toString(36).slice(2, 8)`（`host.ts` 的 `#incarnation`） | rootId/originId 的 base36 随机串（**至多 6 字符**） | 需 `rand` 生成 base36（0-9a-z）字符；`rand::distributions::Alphanumeric` 含大写、不适用 | 只要求"短、进程内足够随机、不含 `#`/`:`"；不要求与 TS 相同值。**必须知道** `Math.random()` 的尾串位数不定：`0.5.toString(36)` → `"0.i"`（`slice(2,8)` = `"i"`，**1 个字符**），`0.1.toString(36)` → `"0.3lllllllllm"`（`slice(2,8)` = `"3lllll"`，6 个）。因此 TS 的 incarnation **常常短于 6 位**；`RootRef.rootId` 的 schema 只要求 `minLength: 1`，长度不校验。Rust 侧固定 6 位更安全，但与 TS 的分布不同（记录在案） |
| `setTimeout(...).unref?.()`（`host.bindingIdle`、`awaitReady`、manager keepAlive） | 定时器不得阻止进程退出 | `tokio::time::sleep` + 可取消；或 `std::thread` 的 detach | Rust 与 Node 的事件循环模型不同：**没有"unref"概念**；等价语义是"该定时器不应让宿主进程无法退出"——在 tokio 下天然满足（runtime 退出即丢弃），但要避免用阻塞线程 |
| `FinalizationRegistry`（`test/generations.test.ts` 用，非生产代码） | 验证旧代码 generation 可回收 | 不适用 | 测试层面的"无泄漏"断言不能直接移植；Rust 侧用 `Weak`/`Arc::strong_count` 写等价断言（见 §7.1 `generations.test.ts`） |

## 7. 测试清单

`packages/chord/test/` 下 `*.test.ts` **共 53 个**（其中顶层 48 个，子目录 5 个：`delta-tracker/retention.test.ts`、`delta-tracker/tracker.test.ts`、`sqlite/cloudflare.test.ts`、`sqlite/migrations.test.ts`、`sqlite/node.test.ts`）；另有非 `*.test.ts` 的基准/夹具：`delta-traversal.bench.ts`、`delta-benchmark/`（`benchmark.ts`、`benchmark.worker.ts`、`conversation-view-benchmark.ts`、`conversation-view-benchmark.worker.ts`、`memory-benchmark.ts`、`memory-benchmark.worker.ts`）、`fixtures/`（`counting.ts`、`echo.ts`、`loader.ts`、`worker.ts`）。按单元归属：本单元直接相关的是 host/object/connection/reload/remote/manager 的行为测试；其余属其它单元，但其中的跨单元断言（尤其请求/评审/集群/成本）同时约束本单元。下表列出每个文件断言的核心行为（`it` 名称即断言意图，括号内为原文件**行数**；此前版本中标注为 "K" 的若干条目实为**字节**大小，已在下表按行数更正）。

### 7.1 本单元核心

| 文件 | 断言的可见行为 |
| --- | --- |
| `host.test.ts`（739） | 激活→live→晚绑定成员→release 后 dispose；并发 get 合并；linger 后复活；激活被放弃后 settle 时 dispose；激活失败传播给依赖者并在下次 get 重试；factory 抛错时 unwind 声明、async factory 被拒；重复声明名被拒；hold 环检测；资源按逆序关闭 + `runAll` 聚合错误；调用结束释放 ctx.get 的 hold。声明与方法：每种 kind 名字独立、重名 `invalid/duplicate`；无参方法以 ctx 调用、void 方法解析为 undefined；state 监听器启动时拿当前值、reload 换掉派生后再次拿当前值。lifetime：`"host"` 的类型在编译期与运行期都被拒；同一对象被所有 get 命中、另一 input 为 invalid。principal/variant/释放：按 principalKey 分对象、variant 强制；provider 边界校验远程 args/result；强制释放使引用 gone 并 abort 在飞调用；state 成员是晚绑定 readable（值、snap/reset/ops 事件）。注册表：仅 `"host"` 生命周期的对象可注册、表按注册顺序；冲突被拒。组合：引用 state 成员可作为监听/派生源；`ctx.get` 在入口以 root principal、在对象内以 host；factory 内调用引用抛 not_ready；factory 的 ChordError 保留 code 传给依赖者 |
| `reload.test.ts`（738） | 兼容重载：无 gate、pinned roots 走旧代码、状态保留、previous 退休；prepare 以 recording 模式运行（写入抛错，factory 失败则一切不变）；cell schema 改了没升 version 被拒；state 升版：gate、`migrate(old, fromVersion)` 进新 schema、向 watcher 发 reset、旧 root `upgrade_conflict`；版本按 cell 独立（只迁移升级的那个，降级被拒）；gate：迟到的旧 root 拿 `upgrade_conflict`、grandfathered root 重新进入、然后 commit；gate 超时：reload 中止、报告 blocking roots、什么都不变；exclusive 资源在 gate 替换、非 exclusive 在 prepare 打开并在退休时关闭；两版本规则：下一次 reload 退休上一版、其 roots 拿 `superseded`；强制退休先 abort 遗留 roots 再关闭只有旧代码用的资源；保留资源用**最新声明**的 close；task 无视 abort 时最多拖 `taskStopDeadline`（并上报）；facet 的 signal 在 run 被替换/移除、reload 中止、host 关闭时 abort；重载时检查契约（忘记 bump、live consumer、accepts adapter）；监听器重绑到新代码、task 重启；注册表按 code version 保留；reload 可同时新增类型与持有它的消费者；gate 拒绝 changed 类型的新对象、commit 后它们拿到新代码 |
| `review.test.ts`（686） | commit：监听器订阅失败会让 prepare 失败且 code version 与所有对象不变；迁移值按**新**远程 schema 校验（dev 模式）；commit 中途仍失败则 host **终结性失败**（不出现半个 code version 服务）；commit 期间重入观察者看到完整新版本。lease/调用/激活：运行中的监听器持 lease，gate 等它结束再迁移；dispose 等运行中的方法结束再关资源（released 与 forced 都是）；finalize 时发现注册表冲突则完全 unwind 输家；启动监听器期间释放最后一个 hold 不会让对象复活。prepare 隔离与契约：prepare 重跑也不能碰 retained 资源（含已 open 的）；reload 新增类型的对象在 commit 才启动监听器与 task；破坏性的 provider 变更与其消费者在同一次 reload 里完成；exclusive 资源 close 失败后 rollback 时恢复，恢复不了则 mark unavailable；退休先关被丢弃的资源再释放被丢弃的 hold；`fits` 对未建模关键字无假阳性；参数先按消费者所知裁剪、再按 provider 所知裁剪。ctx 竞争：aborted task ctx 不能发起新调用；过期 ctx 与 foreign ctx 被拒；远程引用拒绝 foreign ctx 与非 Chord 造的 ctx 且不发起调用 |
| `requests.test.ts`（509） | 10 条应用级请求：held 对象的状态成员可作为本对象状态成员返回；facet 通过 `FacetApi.root`/`FacetApi.registry` 访问自己的 host；资源与 select 不变时监听跨 reload 保持订阅；select 变化则重订阅；`replace` 对新 revision 做结构共享 diff，无关值整体替换；连到 server 之后的子 host 的 roots 被按 server 的 code version 接纳；`self.expose` 一个由资源支撑的 opaque 成员，live 后可读；集群重载报告每个子进程准备了什么；held 远程引用无监听器也保持状态，后来的订阅者看到当前值；引用报告状态变化（live、重连时 loading、gone、released）；host 关闭先 dispose 持有者再 dispose 被持有者，最后才关链路 |
| `bindings.test.ts`（459） | 共享绑定流量：N 个引用一个对象 → 一次 hold、每成员一次 snapshot、每变化一帧、一次 release；held 引用无监听器仍订阅、ready 后立刻可读。linger：linger 中的绑定保持 hold 与状态、期间取新引用零链路成本；最多 `cap` 个 linger，最少最近释放的先走。身份：规范化后相等的 input 与相等 principal key 共享，其它 principal 与 fingerprint 不共享。跨链路水合：null/空/undefined 值也水合；无 state 的 variant 直接 ready；快照失败则 reject；首次水合前读抛 not_ready。仅方法引用（`state:false`）：单独不传状态，完整引用加入后启动一次，release 后停止。client hold 的 origin：relay 的 hold 带 client 连接 id，server 自己的 hold 不带；client 发送的 origin 被忽略，server 盖自己的章。协议版本 4：v3 对端被拒；shape 不带 code version |
| `link.test.ts`（631） | 子调 server（server 盖子 principal 章）；state 成员先 snapshot 再 delta ops，解码后等于 provider 值；server 按名访问已连接子；拒绝 local 类型与未知 host；relay browser→server→worker（worker 看到 browser 的盖章 principal）；忽略来自 client 与子 host 的 payload principal；route 返回已关闭链路时不会毒化 host 名；authorize 在 resolve 前与后各一次；browser 断开只释放自己的 hold、worker 链路保持；无 pong 的死链路被关闭、其上所有 hold 被释放；跨链路校验契约 token（host 得 `contract_mismatch`、browser 得 `upgrade_required`）；`invalidatePrincipal` 重新盖章并传播到 worker；重连后状态订阅者拿 reset、调用恢复；无参方法不带 args/value 并解析为结果或 undefined；协议版本：无版本的老客户端被告知并断开、期间什么都不服务；client 学到 server 的版本后停止；同版本正常通信、普通 close 不算 mismatch；打包：大 args/results/snapshots 经 relay 压缩传输并完好到达；只有超过阈值的载荷被压缩；IPC：对端退出后写失败（EPIPE）结束链路而不是抛未处理错误 |
| `flow.test.ts`（301） | 流控：窗口满时扣住 state、同时照常回 call、最终合并成一帧；ops 超过槽位后替换为当前值；被释放引用等待中的东西丢弃；relay 的 state 为 origin 合并、溢出时向下游要 reset；对端 hello 未 announce acks 则不窗口化也不 ack；ack 了从未发出的帧则关闭链路。链路调用上限：超过 `maxCalls` 立即以 `too_many_calls` 回 `unavailable`，链路保持 |
| `flow-units.test.ts`（271） | 大流被满窗口挡住时新 get 立刻解析、call 照常回；大于窗口的 snapshot 整体到达、后续变化等 ack；不报告尺寸的链路（IPC）永不扣住 state；等待的 ops 受每链路 cap 限制，每个流最终各得一次 reset；relay 拓扑：每个 origin hold 对应一个下游 hold，release/close/revoke 后不留残留；一个卡住的 tab 溢出拿 reset，另一个 tab 仍收到每次变化 |
| `variants.test.ts`（168） | 新 variant 的成员随 shape 到达；不在 variant 内的成员读时抛 `not_in_variant`；每个展示的 shape 在展示瞬间完整可读（`onShape` 里逐个成员读不抛）；改 variant 只发新成员那一个 snapshot；链路丢失后重 hold：最后值仍可读、发一个 fresh unit、期间加入的引用等这个 unit；重 hold 帧 `again: true`、两个引用只有一次 hold、每成员一个 snapshot；旧 hold 随链路释放，server 重建对象（cell 从初值开始） |
| `provider-cost.test.ts`（83） | **此前遗漏，直接约束 connection.ts 的 fan-out 成本模型**。用 `vi.mock` 包装 `delta/index.ts` 的 `encoder()` 统计 `encoders`/`encodes`。1 个 server 对象 + 4 个 `kind:"client"` 子 host（同一 principal，命中同一对象），每个客户端用 `CountingLink` 包住 memory link；对 `items` 做**一次** `change()`：断言 `encodes` 恰好增加 **K=4**（**每个 feed 各编码一次**，不是一个变化全局编码一次、也不是每客户端各编码两次）、`encoders` 不增加（**一次变化不新建流**）；每条链路上新增的 state 帧**恰好 1 个**，且 4 条链路的 `ops` JSON 完全相同（"every link carries the same ops"）；4 个客户端最终 `items.value === ["x"]`。Rust 侧若要合并编码（共享一次 diff 结果）会违反"每 feed 一个 encoder"的状态性（encoder 字典按 feed 累积），**必须保持每 feed 独立编码** |
| `state-init.test.ts`（232） | **此前遗漏，约束 `self.state(..., { init })` 的生命周期**：init 在资源 open **之后**、对象 live **之前**运行，且外界看不到中间值（listener 与 task 首见即为 init 结果，日志严格为 `["open","init","listener a,b","task a,b"]`）；init 期间发起的调用等到 live 后用加载值返回；live 前引用的成员读到 `undefined`，首个订阅值就是加载值；factory 内读 init cell → `not_ready` 导致 hold 失败；init 抛错或值非法 → 激活失败、已开资源被关闭、下次 get 重试；init 在 root 中运行（可用 `ctx.get` 调其它对象，以 host principal）；reload：保留的 cell **不重跑 init**，新增的 cell 在 **prepare**（非 gate）阶段跑 init；新增 cell 的 init 抛错则整个 reload 被拒、旧代码继续；带 gate 的 reload 中新增 cell 的 init 在 **gate** 阶段运行（`report.gated.length === 1`） |
| `state-value.test.ts`（37） | **此前遗漏，属 delta 单元但被本单元 `applyImmutable` 语义间接依赖**：`track()` 对无别名的可变 JSON 根**就地接管**且**不冻结**（`Object.isFrozen === false`）；`beginChange()`/`prepare()` 的写时复制共享未变分支（`retained` 引用相等、`changed` 为新对象且未冻结），`adopt` 后旧 base 保持快照不变；同一对象被 push 两次会在 `prepare` 中**去别名**（两个独立副本） |

其余与 `object.ts`/`host.ts` 行为强相关：
| 文件 | 断言 |
| --- | --- |
| `awaiting.test.ts`（101） | 被 abort 的 get 只释放自己的引用、绑定留给其它人；ctx.get 在调用 settle 时仍等待则 reject aborted 并释放引用；get 超 `readyTimeout` 得 `ready_timeout` 并释放 hold；ctx.get 解析为已水合、调用结束时释放该引用 |
| `readiness.test.ts`（262） | `install`/`load`/`get` 等到资源激活与注册完成，lazy 对象保持 lazy；急切失败以激活错误 reject；load 先装完所有 facet 再等 eager 消费者；缺失 eager 依赖会 reject load 而不只是 onError；`ready` 覆盖直接的 host serve，不覆盖之后的 serve；可在子进程被父接纳后再急切就绪 |
| `hydration.test.ts`（195） | 转发/派生远程成员的本地对象带值 live；factory 内读成员抛 not_ready，live 后是普通值；子进程的 host 对象持有父对象时在父接纳后 live；跨 host 引用环在 readyTimeout 时失败并带 cycle 提示 |
| `host-objects-reload.test.ts`（345） | host 对 host 对象的引用在 commit 随契约 bump 换 token、不重开对象或资源；失败则保留旧引用；外部旧版本消费者仍拒绝无 accepts 的升级，给了 adapter 就保留；对象自己持有的旧版本引用仍会阻止升级，除非同一次 reload 里替换了消费者；忘记 bump 的契约即使唯一消费者是 host 自己的引用也被拒；集群契约升级在全员 prepare 后才换 server 与 worker 的 token；reload 改变类型 lifetime 时创建 host 对象或放掉它 |
| `generations.test.ts`（176） | 跨 reload 持有的引用只让当前 generation 存活；带 state、保留资源、监听器、task、注册的 `"host"` 对象同样；调用与 task 加到 ctx.signal 的 abort 监听器不超出它们的生命周期；已 dispose 的对象在释放后不被保留 |
| `placement.test.ts`（198） | get 去契约规定的地方（本机/子/父），调用方永不命名 host；serve 拒绝本 host 不是其位置的类型（reload 时也拒）；host 拒绝自己契约放到别处的 hold，relay 拒绝目标不一致的 hold；host 由规范化后的 input 决定（未知字段与默认值不改变它）；placement 属于契约，移动类型是破坏性变更、绝不由 adapter 桥接；类型：family 的字段必须是 input 的必填字符串，remote 契约必须有 host |
| `principals.test.ts`（136） | factory/method/ctx 看到应用 principal 类型；host 自己的 principal 按 key 识别（相等对象命中同一批对象）；子的父以子给 `connect` 的 principal 行事、relay 的调用者保持自己的；拒绝没有 principal 的 host/父链路/route |
| `cluster.test.ts`（644） | 跨 host 重载：每台 host 都 prepare、server 先 commit、worker 后 commit、code version 同步；任一 vote 失败则全员 abort，错误保留 code 并点名 host；worker gate 超时使整个 reload 中止；移除 facet 并在 server 与 worker 生效、移除 tombstone 在重新加入时清除；只有 server 协调重载；browser 不是参与者（只收 shape/reset 推送，或 `upgrade_required`）；worker 仍在 prepare 时 abort：迟到的 open 自行关闭、worker 可再次 reload；reload 期间 hello 的子在其后被 reconcile、绝不与旧代码同步；子保留上一 code version 直到父退休它；父已退休旧版本的子可立即退休自己的；reload 开始时正在跑的 reconcile 只在该 reload 结束后同步到已提交版本；落后子的 hello 在运行中的 reload 期间到达则立刻被 fence（其向上的调用按父版本接纳）；重连的落后子仍保留上一版本时无需等完 retire 期限；强制退休会在 server 关闭旧代码资源前取消经 relay 到 worker 的 browser 调用 |
| `links-review.test.ts`（553） | 接受方决定对端是什么（自称 host 的 client 被切断）；`root_end` 只从 root 来过的链路被采纳；route 仍在解析时 revoke 会关闭 relay、不产生 worker 对象；畸形帧与重复 id 关闭链路；browser 断开会取消其下游调用；route 解析期间 state 被停则转发为无状态 hold；解析期间 release 会结算等待中的调用且不向下游转发；origin 断开清理其 relay 的 hold 与订阅；两个 browser 共享一条 worker 连接、一个离开只释放自己的 hold；经 accepts adapter 的 snapshot 按消费者 schema 校验；子仍加载模块时 abort 会释放 reload 槽位、之后可再 reload；子 prepare 时父链路死亡会 unwind 子；commit 启动的 task 不能在该子 commit 前到达子；错过 commit 的子重连时被 reconcile、绝不重贴标签；经 worker 往返的 server root 正常结束（不泄漏 lease） |
| `manager.test.ts`（183） | `processLock` 在临时目录过深时仍允许一个持有者；真实子进程：无锁子不继承父的锁；启动子 host、连上、优雅停止；子拥有的内核锁只允许一个活进程、replace 等退出后才启动下一个；优雅退出也带走子进程的后代；`idleFor` 从 busy→idle 转变起算；子无视 shutdown 超过 grace 则杀进程树；fatal 子被报告为启动失败；keepAlive 说停就停空闲子 |
| `loader.test.ts`（21） | node loader 加载 facet 模块并在 reload 时用相对导入的新副本重载；模块需要配置 loader |
| `websocket.test.ts`（158） | 通过真实 socket 服务 browser 风格客户端、由认证器盖章；大 snapshot/result 以二进制消息到达客户端；客户端不得发送打包帧 |
| `boundary.test.ts`（41） | 包边界：除 kiana-telemetry 外不依赖其它 Kiana 包、不引用 Chord 之外的文件 |
| `types.test.ts`（41） | 类型层面：state 成员是 T、其它 variant 成员需要收窄、get 是 await 的 |
| `tutorial.test.ts`（517） | 教程 01–11 端到端：对象在 host 内、第一个契约/factory/引用/root、cell 发布值与派生、hold/异步资源/release、principal 选择同一 store 的视图、远程方法与经 memory link 的实时状态、监听器反应与 task 发起新调用、插件贡献注册项与暴露回调、兼容 reload/迁移/旧契约 adapter、用 fake facet 打桩 provider 并检查错误码、短 root 释放临时 hold |

### 7.2 其它单元（不赘述，但 CI 需全绿）

`delta*.test.ts`（顶层 **7** 个：`delta`、`delta-append-text`、`delta-apply-immutable`、`delta-clone`、`delta-diff`、`delta-op-text-retention`、`delta-pending`；另 `delta-traversal.bench.ts` 为非测试基准）、`state*.test.ts`（**6** 个：`state`、`state-diff`、`state-draft`、`state-fuzz`、`state-init`、`state-value`；后两个的断言见 §7.1）、`replicated-state*.test.ts`（2 个）、`output*.test.ts`（2 个）、`context.test.ts`、`contract.test.ts`、`json.test.ts`、`watch.test.ts`、`telemetry-sqlite*.test.ts`(2)、子目录 `delta-benchmark/`（6 个 bench/worker 脚本，无 `.test.ts`）、`delta-tracker/`（`tracker.test.ts` 1507 行 + `retention.test.ts` 35 行：后者用 `spawnSync --expose-gc` 跑 `retention.worker.ts` 的 **15 个场景**——`aborted-payload`、`unadopted-prepared`、`draft-proxies`、`settled-lifecycle`、`obsolete-revisions`、`retained-settled-prepared`/`-change`/`-proxy`、`retained-large-settled-proxy`、`retained-large-placement-proxies`、`stale-unprepared-change`、`same-job-fast-cleanup`、`same-job-folded-ops-cleanup`、`lifecycle-churn`、`adopted-changes-flat`——逐条断言 GC 后对象可回收；`retention.worker.ts` 331 行不是 `.test.ts`）、`sqlite/`（`cloudflare`、`migrations`、`node` 三个 `.test.ts`）、`fixtures/`。**这些测试对 Rust 侧的启示**：delta 的 `track`/`beginChange`/`prepare`/`adopt` 生命周期与"旧 revision 可回收"是硬约束（§7.1 的 `state-value.test.ts` 是同一族），Rust 用 `Arc`/`Weak` 时不能引入隐藏的强引用环（尤其是 `Derived` ↔ `Cell`、`RemoteBinding` ↔ `RemoteRef` 之间的反向引用）。

**测试设施**（`test/fixtures/`，先前只记录了 `counting.ts`，此处补齐其余三个）：

- `counting.ts`：`CountingLink implements Link`，按方向记录 `sent`/`received` 帧与字节数（`JSON.stringify(frame).length`），可 `paused` 扣住自己发出的 ack 以填满对端窗口，`release()` 放行；`count(direction, t)`、`traffic(direction)`（滤掉 ping/pong/ack）；`stateFrames(frames)` 过滤 `t === "state"`。
- `echo.ts`：远程契约 `t.echo@1`（`host: hostNamed("w1")`，variant `main`，方法 `echo(string)->string`、`pid()->number`），供 manager fixture 使用。
- `worker.ts`：manager 测试的**真实子 host 入口**（`FIXTURE_MODE = normal | ignore-shutdown | fatal`）：先 `childControl().lock()`（拥有数据之前，失败则退出）、`createHost({ upstream: { link: ipcLink(process), ... } })`、`await control.init` 拿 `{greeting}`、serve `Echo`、可选 `GRANDCHILD_FILE`（spawn 一个 `setInterval` 孙进程并写 pid，用于验证**优雅退出也带走后代**）、`onShutdown`（`ignore-shutdown` 模式下不退出，用于验证 grace 后 `killTree`）、`ready()`。
- `loader.ts`：**不在 vitest 内运行的脚本**（vitest 的 module runner 会绕过 Node resolve hooks）：写出临时 plugin 目录并用 `nodeLoader` 加载，断言 reload 后相对导入拿到新副本（`v1` → `v2`），并断言"先列出的 facet 若 import 后一个 facet 所在目录，二者共享同一份模块副本"（`ordered.load([a, b])` 后 `same()` 为 true）。`loader.test.ts` 用 `spawnSync` 跑它并额外断言无 loader 时 `load(["./facet.ts"])` 报 `createHost({ loader })`。

所有测试用 `until(check, attempts = 200)` 轮询（每 2 ms），Rust 侧需等价 helper。

## 8. 建议的 Rust 模块布局与最难复刻的部分

### 8.1 建议布局（与 TS 文件一一对应，便于逐行核对）

```
crates/chord/src/
  lib.rs                 // 公开 prelude：re-export 与 TS index.ts 对应
  errors.rs              // ChordError, codes, RETRYABLE, forbidden/invalid/as_chord_error/run_all
  contract/
    mod.rs               // method/opaque/hostNamed/hostFamily/remote/local/contractFromDescription
    describe.rs          // DescriptionSchema（serde 结构 + 校验）、describe_member、fingerprint
    coverage.rs          // coverage/bind/takesNoArgs、RESERVED_MEMBERS、LOCAL_VARIANT
  schema.rs              // canonical_json/hash64/deep_equal/assert_valid/normalize/canonical_identity/fits（其他单元）
  json.rs                // copy_json/is_json_value（其他单元）
  publish.rs             // 进程级 FIFO enqueue/hold_publications（其他单元）
  cell.rs                // Cell/Derived/HYDRATION（其他单元）
  delta/                 // Op/Encoder/Decoder/apply/apply_immutable（其他单元）
  registry.rs            // Registry/RegistryTable/RegistryView（其他单元）
  declare.rs, types.rs   // Subscribable/Resource/ListenerDecl；Ctx/Readable/StateEvent/Shape/RefStatus（其他单元）
  wire.rs                // 帧 enum + per-frame serde + is_frame + to/from_wire_error（其他单元）
  link.rs                // Link trait, MemoryLink, IpcLink, WebSocketLink, packing（其他单元）
  websocket.rs           // accept_web_sockets（其他单元）
  loader.rs              // nodeLoader 等价（其他单元）
  object/
    mod.rs               // Service/Factory/Self/Decls/Run/Installed/HostCore/CtxImpl/linked_signal/await_ready/race_signal
    resource.rs          // ResourceImpl/ResourceFacade/open/close/adopt/markUnavailable
    refs.rs              // RefCore trait, RefImpl, MemberReadable, REF_BASE, notify_status
    object_impl.rs       // ObjectImpl 状态机（start/run/open/initCells/unwind/install/... ）
    declarations.rs      // stage_listeners/start_runtime/start_tasks/start_task/registries
    invoke.rs            // call/invoke/lease/guard/rootEnded/admit/dispose/unwind
  connection/
    mod.rs               // Connection 主体 + dispatch + flow control
    relay.rs             // RelayRef/RelayCall/#admitRoot
    feed.rs              // Slot/Overflow/Feed/#flush/#makeRoom/#onAck
  remote.rs              // RemoteBinding/RemoteRef/ViewReadable/StateIn
  host/
    mod.rs               // HostImpl + 公开 Host trait
    roots.rs             // root/joinRoot/endRoot/foreignRootEnded/retire/allowRetire/watchRetirement
    tables.rs            // buildTables/registriesChanged/installTables/sameTable
    routes.rs            // #routes/#routeFor/#resolve/#childNamed/#hold/#acquireRemote/relay
    facets.rs            // FacetApi/defineFacet/install/load/resolve/facetRunChanged/serveInto/startHostObject
  reload/
    mod.rs               // Reload 类 + runReload/beginReload/endReload/reloadError/summary
    prepare.rs           // waitForRetirement/load/checkProviders/prepareAll/prepareObject/checkRefs/entries
    gate.rs              // gate/migrate/dryRun/candidateValue
    commit.rs            // commit/#publish/install/rollback
  cluster.rs             // run_cluster_reload/reconcile_child/ChildReloads（其他单元）
  manager.rs             // processLock/forkDriver/createHostManager/childControl（Node only feature）
  // 测试：crates/chord/tests/ 按 §7 的文件逐一对应
```

- `Host`、`Facet`、`Ctx` 等公开 trait 用 `async_trait` 或手写 `Pin<Box<dyn Future>>`；`HostCore`（object 需要的宿主视图）保持为内部 trait，避免公开面膨胀。
- **没有 JS Proxy**：`Ref` 改为显式 handle 类型（`Ref<C>` 结构体，提供 `state_member(name)`、`call(name, args, ctx)`、`on_shape/on_status`、`release`）。`REF_BASE` 的成员名（见 §2.3）仍需在 `member(name)` 分发里保留同名访问器语义（`then` 一律 `None` 以保持"引用不是 thenable"）。
- `AbortSignal` → `CancellationToken` + `Arc<Mutex<Option<ChordError>>>`（原因）。`linkedSignal` 的"释放监听器"语义在 Rust 里用 drop 取消注册即可，反而更自然。
- `Promise` 的微任务顺序 → 明确用 `tokio::spawn`/`futures::join_all` 并逐处对照 TS 的 `await` 顺序；凡是"同步执行后再 await"的地方（如 `install()` 先同步 setup 再 `#hostObjectsReady`），Rust 里必须同样是同步段 + 异步段两段。
- `Map` 插入序 → `indexmap::IndexMap`；`Set` 插入序 → `IndexSet`。
- `WeakMap`/GC → `Arc`/`Weak`；注意 TS 依赖 GC 的"不保留"断言（generations.test.ts）在 Rust 里可用 `Weak::strong_count` 断言等价性质。

### 8.2 最难复刻的部分（按风险排序）

1. **类型系统与 schema 校验（TypeBox 语义）**：`Value.Check/Errors/Clean/Default/Clone` 与 `fits` 的精确行为，尤其是错误消息文本（会直接出现在用户可见的 `invalid/args: ...` 里）、`default` 与 `Clean` 的交互、`anyOf` 重排算作变化的 canonical 比较，以及 `fits` 与 `Value.Check` 无假阳性（review #11 用 40k 随机反例校验）。这是整个移植的地基。
2. **`Math.imul` 双路 FNV-1a + UTF-16 code unit**：`hash64` 必须逐位一致，否则指纹变化会让**所有**跨版本契约匹配失败（包括浏览器缓存、`shape.contract`、远程 key）。
3. **reload 的阶段顺序与 gate 语义**：prepare（recording 模式）→ gate（drain lease、grandfather、replace exclusive、migrate）→ commit（holdPublications 内同步发布 + `beforeStart` 排队 commit 帧 + `#starts` 顺序）。任何一步顺序错都会让"旧代码不跑在已迁移状态上""task 不能在子 commit 前到达子"这类不变量失效。`review.test.ts` 与 `cluster.test.ts` 会逐条抓住。
4. **流控与帧顺序**：只窗口化 state、首 unit 不等待、每 (hold, member) 槽合并、溢出替换为 reset（relay 改为 `resync`）、`ack` 累计计数、`#makeRoom`/`#flush`/`#drop` 的精确时序，以及"result 可越过等待中的 state"这条 FIFO 例外。`flow.test.ts`/`flow-units.test.ts` 用 `CountingLink.paused` 精确构造窗口满场景。
5. **relay 的所有权与生命周期**：`RelayRef`/`RelayCall` 必须"恰好一次"地撤销（取消下游调用、丢弃等待、释放下游 hold、拒绝等待者），并且 `origin`/`principal` 的盖章规则（下游一律用 accept 的 principal；origin 用本连接的 id；父链路才采纳转发值）不能有偏差。
6. **reload 与 cluster 的交叉**：participants 在 reload 开始时快照、fenced child 的 `#admitRoot`、`deferredHellos`、`whenReloadEnded`、reconcile 在 reload 期间的重试、`announceRetired` 在 `prepare` 之前、vote 超时的 `vote_deadline`。`cluster.test.ts` 的 **14 个** `it` 几乎每条都针对一个时序坑。
7. **进程锁的平台差异**：Linux 抽象 socket（`\0` 前缀、不落盘、无文件清理）vs Windows 命名管道 vs 其它平台的 socket 文件 + 103 字节限制 + `/tmp/chord2-<uid>` 0700 回退 + 陈旧文件探测。Rust 里需要 `socket2`/`interprocess`，且 `EADDRINUSE` 语义要对齐。
8. **子进程管理与 IPC**：`detached` 进程组 + `kill(-pid, SIGKILL)` + fork 后立即发送 init + 继承的 `CHORD2_LOCK` 必须被删除 + 写失败（EPIPE）→ `ended("ipc send failed: ...")` 而不是 panic + `disconnect` 事件。Rust 没有 `node:child_process` 的 IPC 通道，需要自建（stdio 长度前缀 JSON 或 Unix socketpair），并保持 `{chord2}`/`{chord2control}` 信封与"IPC 不窗口化"的性质。
9. **`nodeLoader`**：动态 `import()` + `registerHooks` 的 `?chord2-reload=n` 传播。Rust 没有模块热替换；需要设计插件 ABI（dylib + 版本化符号，或内嵌 JS 运行时），并保持"每次 reload 得到新副本、facet 名 = 模块 URL、default 导出必须是 facet"的可见契约（含错误消息）。
10. **通知顺序**：进程级 `enqueue` FIFO + `holdPublications`。Rust 里若用多线程/多 runtime，`process-wide` 的语义需要显式设计（全局单队列 + 单消费者，或每 host 一个但保持跨 host 的顺序？TS 是**进程级**单队列，spec §12 明确说"a slow observer delays every later notification in the process"）。
11. **`document`/`structuredClone` 与 `migrate`**：迁移函数是用户代码（JS 闭包），Rust 侧要定义等价的迁移回调签名（`fn(old_value, from_version) -> new_value`），并保持"先克隆再交给 migrate"（防止 migrate 原地改旧值）。
12. **字符串/数字的 JSON 表示**：`canonicalJson` 与 `JSON.stringify` 的数字格式、字符串转义（包括 lone surrogate）、`Math.imul` 溢出，都必须与 JS 完全一致，否则指纹/identity/帧字节都会不同。

### 8.3 移植顺序建议

1. `errors` + `schema`（canonicalJson/hash64/normalize/assertValid/fits）+ `json` + `contract`（先用一个 JSON Schema 校验器打通）。
2. `publish` + `cell` + `delta` + `registry` + `types`/`declare`。
3. `object`（Self/Decls/Resource/RefImpl/ObjectImpl/CtxImpl）——本单元最大一块（1932 行）。
4. `wire` + `link`（memory 先，IPC/WebSocket 后）+ `connection`（含流控与 relay）。
5. `remote` + `host`（roots/retire/tables/routes/facets）。
6. `reload` + `cluster`。
7. `manager` + `loader` + `websocket`（Node only feature）。
8. 按 §7 逐文件移植测试；优先 `host`/`reload`/`flow`/`links-review`/`cluster`，它们覆盖最多的时序不变量。

## Audit additions

本节的目的是把本轮对抗式完整性审计中发现的、**不适合直接插进上文结构**的补充集中记录在此。上文已就地修正的错误条目只在 A1 里做索引、不重复正文。所有路径均相对 `kiana/packages/chord`（只读的 TS 参考树）。**阅读顺序建议**：Rust 实现者先按 §1–§8 读主 spec，再用 A2–A11 逐条自查；A12 列出本轮已确认无问题的部分，避免重复审计。

### A1. 就地修正的错误陈述（索引）

| 位置 | 原文（错误） | 事实 |
| --- | --- | --- |
| §2.5.1 | `defineFacet = <P>(definition: FacetDefinition<P>) => FacetDefinition<P> => definition` | 实际签名是 `defineFacet(setup: (api: FacetApi<P>) => void, options: { readonly uses?: readonly AnyContract[] } = {}): FacetDefinition<P> => ({ setup, uses: options.uses })`；`FacetDefinition` 是**对象**而非函数，模块 default export 的是它的返回值。§2.5.1 已改 |
| §1.2 / §3.1 | `MemoryLink`/`IpcLink`/`WebSocketLink` 是 index.ts 的值导出 | 只有 `memoryLinkPair`/`ipcLink`/`webSocketLink`（以及 `PACK_THRESHOLD`、`inflateMessage`）是；三个类与 `Packed`/`unpack` 只在 `src/link.ts` 导出，包入口不再导出。已改 |
| §1.2 | `MemberDescription` 从 index.ts 导出 | `contract.ts` 导出它，但 `index.ts` **没有**再导出（`grep -c MemberDescription src/index.ts` = 0）。已改 |
| §1.2 | `CHORD_ERROR_CODES` 从 index.ts 导出 | 只在 `errors.ts` 导出（`index.ts` 仅导出 `ChordErrorCode` 类型与 `ChordError`/`isChordError`/`forbidden`/`invalid`）。已改 |
| §5.2 | `describeMember` 是 contract 的导出 | `contract.ts:192` 是**私有** `function`。已改 |
| §5.2 | `MemberReadable`、`Gate` 是 object.ts 的导出 | 二者都是私有（`class MemberReadable`、`interface Gate`），连同 `Subscription`、`ResourceFacade`、`facades`、`resourceOf`、`isReadableLike`、`validateImplementation` 均无 `export`。已改，并补上真正导出的 `RefOwner`、`StatusListener`、`ResourceImpl`、`ObjectIdentity`、`StagedListeners`。 |
| §5.4 | "运行期导出 52 个" 的口径 | 52 是**行数**；`contract.ts` 的 19 行里有 3 行是同一 `method` 的重载签名，按符号去重是 16，7 文件合计 49。已加口径说明 |
| §2.5.3 | `EMPTY_TABLE` 写作 `new Map()` | 实际 `new Map<string, never>()`。已改 |
| §2.5.3 | `HostImpl` 字段表缺 `upstream` | `upstream: Connection \| undefined` 是**公开字段**。`isSelf` 是公开方法。已改 |
| §3.4 | 标题声称"全部可见错误消息"但缺约 40 条 | 已补齐 object/host/reload/connection 四组遗漏条目（含全部 `self` API 的 TypeError、`validateImplementation` 的 7 条、协议违规 9 条） |
| §3.9 | 把 `a task must be a function` 等误列为链路/解包错误 | 它们属于 `self` API 的 TypeError；已移出并注明。解包错误补全为 7 条 |
| §7 | "共 48 个 `*.test.ts`" | 实际 53（顶层 48 + 子目录 5） |
| §7.2 | "`delta*.test.ts`（8 个）" | 顶层 7 个 + 1 个 `delta-traversal.bench.ts`（非 test） |
| §7.1 | 若干行数混用了字节（`24.8K`/`18.8K`/`120+`/`300+`/`180`/`250`/`160`/`857B`/`6.0K`/`1.8K`/`17K`） | 已全部改为真实行数 |
| §7.1 | 测试清单缺 `provider-cost.test.ts`、`state-init.test.ts`、`state-value.test.ts` | 已补入（见 A2） |
| §6 | 未列 `node:fs`、`node:fs/promises`、`node:os`、`node:path`、`node:url`、`Buffer`、`TextEncoder`/`TextDecoder`、`getBuiltinModule` 探测、`Math.random` base36、`unref`、`FinalizationRegistry` | 已补（见 A3） |
| §3.6 | 环境变量只列 `CHORD2_LOCK` | `os.tmpdir()` 会读 `TMPDIR`/`TMP`/`TEMP`，决定锁 socket 路径；已补说明与"本单元只读这些变量"的边界 |
| §2.7.4 | `RemoteRef` 的 Proxy "与 RefImpl 相同" | `RemoteRef` **只有 `get` 陷阱**，没有 `has`（RefImpl 有）；已改 |
| §2.5.4 `bindingIdle` | "否则 `lingerTimer = ...`；`#lingering.set`" 缺 `!binding.applied` 与复活路径 | 已补：从未 applied 的绑定不 linger；`#acquireRemote` 先 `#lingering.delete(key)` 再 `addView`，复活后不再占 cap |
| §2.5.4 `announceRetired` | 只写在 `peerHello` 里调用 | 实际有 3 处调用点，其中 `cluster.reconcileChild` 在发 `prepare` **之前**调用；已补 |
| §3.1 | "`isFrame`/`toWireError`/`fromWireError` 未在 index.ts 再导出" 与 `PROTOCOL_VERSION` 并列，含义可读成"全都未导出" | **只有 `PROTOCOL_VERSION` 在 index.ts 再导出**；另外三个只在 `wire.ts` 内导出。已改 |
| §2.5.3 | `#incarnation` 是"**6 个** base36 字符" | `Math.random().toString(36).slice(2,8)` 得到 **至多** 6 个字符，常常更短（`0.5` → `"i"`）；协议不校验长度。已改 |
| §3.4 `reloadError` | 只写"若已是则不再包" | 实际判定是**先剥一层 `code[/reason]: ` 前缀**再看是否以 `reload aborted` 开头；且 code/reason/details **继承**。已改 |

### A2. 遗漏的测试与夹具（已补入 §7，这里给出对 Rust 移植的额外要求）

- `test/provider-cost.test.ts`（83 行）：**不是**可选的性能测试，而是行为契约——"一次变化对 K 个客户端 = K 次 encode（每 feed 一次）+ 每链路 1 帧 + 所有链路 ops 相同"。Rust 侧如果为了省 CPU 把 diff 结果共享给多个 feed，会破坏每个 feed 的**有状态 encoder**（路径字典随帧累积，见 §5.1 的 `delta` 行），必须保持每 feed 独立 `encoder`。
- `test/state-init.test.ts`（232 行）：`state(..., {init})` 的完整生命周期（资源 open 后、live 前；root 中运行；失败关闭资源并可重试；reload 时保留 cell 不重跑 init、新增 cell 在 prepare/gate 阶段跑 init 且失败则拒绝 reload）。§2.3.17 只写了 `initCells` 的并行/abort 语义，**未**写"call 在 init 期间等待 live"与"live 前成员读到 undefined"这两条可观察行为，Rust 侧以本测试为准。
- `test/state-value.test.ts`（37 行）：`track` 的就地接管/不冻结/结构共享/去别名，`applyImmutable` 的语义依赖它。
- `test/delta-tracker/retention.test.ts` + `retention.worker.ts`（35 + 331 行，15 个 GC 场景）：旧 revision 必须可回收；Rust 的 `Arc` 引用图不能有隐藏环（尤其 `Derived`↔`Cell`、`Binding`↔`View`、`Installed`↔`Decls`）。
- 夹具 `echo.ts`/`worker.ts`/`loader.ts`：见 §7.2 的逐条说明。`worker.ts` 与 `loader.ts` 是**子进程脚本**而非 vitest 用例，Rust 侧要写等价的集成测试二进制。

### A3. §6 未列出的 Node API（行为差异补充）

已并入 §6 表格，这里只强调三条最容易漏的：

1. **`Buffer.byteLength` vs `PACK_THRESHOLD`**：锁 socket 路径用 **UTF-8 字节数**（103 上限），打包阈值用 **JS 字符串长度（UTF-16 code unit 数）**。两者口径不同，Rust 侧不能都写成 `len()`（Rust `str::len()` 是 UTF-8 字节数、`chars().count()` 是 Unicode 标量数，都不等于 UTF-16 code unit 数——需要 `.encode_utf16().count()`）。
2. **`TextDecoder` 非 fatal**：非法 UTF-8 → U+FFFD 而不是错误；`JSON.parse` 随后失败才变成 `protocol error`。
3. **base64 解码宽松**：`Buffer.from(s, "base64")` 忽略非法字符与缺失 padding；Rust `base64` crate 默认严格，必须显式选择宽松配置，否则同一帧在 Rust 侧会被判 `bad packed payload`。

### A4. §4 帧格式的补充校验点（逐字核对自 `src/wire.ts`）

- `FrameSchema` 是 TypeBox `Type.Union`，**按 `t` 判别**；`isFrame` 用 `Value.Check`。每个分支都是 `Type.Object({...})` 且**都没有**写 `additionalProperties: false`（`src/wire.ts` 里 `additionalProperties` 出现次数为 0）——按 JSON Schema 默认语义，多余字段**被接受**。因此 Rust 侧给帧结构加 `deny_unknown_fields` 会**过严**（TS 能接受的帧会被判 malformed）；应忽略未知字段。反过来说，未知的 `t` 会让所有分支都失败 → `#violation("malformed frame")`，这一点必须保留。
- **但** `DescriptionSchema`（`hold.token` 的内容）里有两个分支**确实**写了 `additionalProperties: false`：`PlacementSchema` 的 `{name}` 与 `{prefix,key}`（`src/contract.ts:140,143`）。其余 `DescriptionSchema` 字段（`name`/`scope`/`version`/`input`/`host`/`variants`）以及 `MemberDescriptionSchema` 的三个分支都**没有**。Rust 的 `Placement` 枚举反序列化若拒绝未知字段，就与 TS 一致——这是本协议里**唯一**需要 `deny_unknown_fields` 的地方（placement 带多余字段在 TS 侧会让整个 `hold` 帧 `isFrame` 失败）。
- `WireError.message` **带** `code[/reason]: ` 前缀（`toWireError` 直接取 `ChordError.message`）；`fromWireError` 只剥**一层**且正则限定 `[a-z_]+`（见 §3.3）。
- `hello.codeVersion` 的 schema 是 `Integer{minimum:0}`，而 `RootRef.codeVersion`、`prepare.codeVersion`、`retire.codeVersion` 都是 `minimum:1`——**初始 code version 是 1**（§2.5.3 已写明），只有 hello 允许 0。
- `ack.n` 是 `Integer{minimum:1}`。`#scheduleAck` 只有 `#receive` 末尾一个调用点，且在 `#received++` 之后，所以正常路径永远不会发出 `n = 0`；Rust 侧不要构造 `n = 0` 的 ack（schema 会拒），并且要保证“一个突发只调度一次 ack”与 `#received` 的递增次序一致（先计数、后调度）。
- `state.as` 是枚举 `"snap" | "reset" | "ops" | "error"`；`ops` 帧的 `ops` 字段是 `List<List<Unknown>>`（**数组的数组**），`snap`/`reset` 的 `value` 可缺省（缺省即 `undefined`）——Rust 的 `Option<Value>` 要区分"字段缺省"与"值为 null"。

### A5. §2.3 / §2.7 补充的可观察行为

- `ObjectImpl.call`（§2.3 第 709 行附近）在 `installed === undefined` 时抛的是 **`superseded` + `${address} has no implementation for code version ${ctx.codeVersion}`**，而 `ctx.codeVersion` 版本已不在 `codeVersions` 时抛的是 `code version ${ctx.codeVersion} is retired`——两条消息不同、code 相同，测试会分别断言。
- `ctx.codeVersion < minCodeVersion` → `${address} was upgraded in code version ${minCodeVersion}`（`upgrade_conflict`），与 gate 的 `${address} is being upgraded`（同样 `upgrade_conflict`）是两条不同消息：前者是"重载后旧 root 已过期"，后者是"gate 期间新 root 不能进入"。
- `withinReadyTimeout` 的超时消息在**存在等待者**时会追加 `, waiting on ${list.join(", ")} (a reference cycle across hosts?)`；`hydration.test.ts` 的跨 host 环测试断言的正是这个后缀。
- `taskStopDeadline` 超时时上报的消息是 `${address}: aborted tasks still running after ${deadline} ms; starting their replacements anyway`（**只上报，不阻塞 commit**）。
- `retireImplementation` 的 `runAll` 标签是 `${address}: retiring code version ${codeVersion} failed`（§2.5 只写了"runAll(...).catch(report)"，未给出标签文本）。
- `ResourceImpl.value` 在 factory 中的 facade 版本抛 `not_ready: resource ${key} (inside a factory)`（**带括号后缀**），与 `ResourceImpl.value` 的 `not_ready: resource ${key} is not ready` 不同。

### A6. §2.4 connection 补充

- `#onHold` 的**校验顺序**（可观察，因为失败时回 `gone` 的 error 不同）：① `served.has(id)` → violation `duplicate hold id`；② `frame.at !== host.name` → relay（upstream 则回 `does not relay`）；③ `contractFromDescription` 抛错（非法 token）；④ `token.scope !== "remote"` → `invalid/token`；⑤ `resolveHost(...) !== host.name` → `invalid/placement`；⑥ `acquire`（可能 `contract_mismatch`/`placement`/`unknown_type`）；⑦ 非 `RefImpl` → `unavailable: not a local reference`。⑤ 在 ⑥ **之前**，所以"input 指向别处"的 hold 得到 placement 错误而不是 provider 的 placement 错误。
- `#principalOf`/`#originOf` 的分支：`role === "downstream"` 时**永远**用连接自己的 `principal`/`originId`（帧里的字段被忽略）；`role === "upstream"` 时才用 `frame.principal ?? this.principal` 与 `frame.origin`。注意 upstream 的缺省是 `this.principal`（`connect` 给的），不是 `undefined`。
- `#protocolMismatch` 的幂等标志 `#mismatched`：一旦触发，后续不再回调 `onProtocolMismatch`，也不再关闭链路。`#closed(reason)` 里对 `protocol-version N` 用 `#protocolMismatch(n, false)`（**不关闭**，因为对端已经关了）。
- `revoke(next)`：先 `generation++`，再关闭所有 relay（`releaseDown: true, notify: true`），最后 `next === undefined` 才 `link.close("principal invalidated")`，否则**只换 principal 不关链路**。`invalidatePrincipal` 的 restamp 语义依赖这一点（`link.test.ts` 的 "re-stamps the link" 用例）。
- `Connection.send` 在 `#peerAcks === false` 时**直接返回且不计数**（帧仍然发出）；`#peerAcks === undefined`（还没收到 hello）时**照常计数**，收到 hello 后若对方不说 acks 则清空计数——这决定了"hello 之前发出的帧不被窗口化"。
- `#scheduleAck` 用 `setTimeout(0)` 合并一个突发内的所有帧为**一个** ack；`unref()`。Rust 侧用 `tokio::task::yield_now` + 一个 dirty flag 即可，但不能每帧发一个 ack（`flow.test.ts` 断言突发只产生一个 ack）。**ack 的 `n` 是 `#received`**（已处理的非 ack 帧数，含 dispatch 抛错的帧；`#received++` 在 dispatch 之前，见 §2.4 收帧段）。`hello` 自己**也计入** `#received`（`#received++` 在 dispatch 之前），且 `#onHello` 先把 `#peerAcks` 设为 `frame.acks === true`，随后 `#receive` 末尾的 `#scheduleAck()` 才决定是否发 ack——所以若 hello 是收到的第一帧，`acks: true` 的对端收到的第一个 ack 就是 `n = 1`；若 hello 之前还收到过别的帧（不常见），第一个 ack 的 `n` 是截至 hello 处理完的 `#received`（那些帧同样计入，尽管当时 `#peerAcks` 还是 undefined）。`acks` 缺失/false 时本端永不发 ack。

### A7. §2.5 host 补充

- `HostImpl.close()` 的顺序是**先对象后链路**：`disposeHoldersFirst(objects, gone("host ${name} closed"), "host close: disposal failed")` 在 `finally` 里才对所有 connection `link.close("host closed")`。Rust 侧若先关链路，资源的 `close` 里再发起远程调用就会失败（`tests/requests.test.ts` 的 "host close disposes holders first" 断言）。
- `hostObjects.clear()` 发生在 dispose **之前**；`#facetRuns` 的 abort 也在之前（facet 的 `signal` 先 abort）。
- `disposeHoldersFirst` 的 wave 算法：每轮取"不被任何剩余对象持有的对象"；若剩余集合成环（wave 为空）则**整批**一起 dispose。错误聚合：1 个 → 原样抛；>1 → `AggregateError(message)`。Rust 侧要用等价的错误聚合类型。
- `announceRetired(connection)`：`retired = (previous ?? current).id - 1`，仅 `> 0` 时发送 `{t:"retire", codeVersion}`——即"我允许你退休到哪一版"。调用点有 **3 处**：① `peerHello` 的 `codeVersion === current.id` 分支里、`markSynced` 之后；② `cluster.ts` 的 `reconcileChild` 里"已同步"分支 `markSynced` 之后；③ `reconcileChild` 里**发 `prepare` 之前**（子进程的 prepare 会先退休自己的 previous code version，而那个动作要等这个许可）。③ 是 §8.2 第 6 条"announceRetired 在 prepare 之前"的出处。
- `peerHello` 的 `codeVersion > current.id`：先 `report(invalid)` 再 `link.close("code version ahead of parent")`，**不**进入 reconcile。`reloading` 时则只记 `peerCodeVersion`/`fencedCodeVersions`/`deferredHellos` 并返回。
- `#childNamed(name)` 只匹配 `open && role === "downstream" && peer.kind === "host" && peer.name === name`——**peer 自称的名字来自 `accept` 配置**，不是 hello 里的 `frame.name`（hello 只做一致性校验）。所以 `route` 的目标名字必须与 `accept` 时给的名字一致。
- `#routeFor(hostName)`：最长前缀胜出，**严格更长才替换**（同长度时**先注册的**胜出）。Rust 侧不要用 `>=`。

### A8. §2.6 reload 补充

- `Reload.prepare` 只在 `candidate.reasons.length === 0` 时调用 `object.initCells(run, abort.signal)` 再 `dryRun`（`src/reload.ts:418-421`）；有 reasons（state 升版、exclusive 资源被替换）的 candidate 推迟到 **gate**：`open(candidate.atGate, ...)` 之后 `initCells(candidate.run, ...)`，然后才 `migrate`（`src/reload.ts:497-502`）。因此"新增 cell 的 init 在 prepare 还是 gate 运行"取决于**这个对象是否被 gate**，不是全局阶段；`state-init.test.ts` 的第三个用例断言的正是 gate 阶段（`report.gated.length === 1` 且 `inits === ["notes","more"]`——只跑一次，不是两处都跑）。init 抛错时：无 reasons → prepare 失败；有 reasons → gate 失败并 rollback。两种情况都不改变"旧代码继续运行"。
- `migrate` 的调用签名是 `fn(structuredClone(cell.value), from)`（`src/reload.ts:514`）——**先克隆**再交给用户函数，防止原地修改旧值；Rust 侧必须显式 clone（`Value::clone`）。
- `ReloadOptions.voteDeadline` 的默认值是 `gateDeadline + retireDeadline + 5000`（**不是**固定 15000），且默认值在 `cluster` 单元计算（`runClusterReload`），本单元只透传。
- rollback 时 abort 所有 candidate 的 `run`：`abort.abort(ChordError("aborted", "reload aborted"))` 且对每个 facet run 同样 abort（`src/reload.ts:526-527`）；`facet` 的 `signal` 因此必须在 rollback 时 abort。
- `waitForRetirement` 有 `ABORTED_ROOTS_GRACE = 1000` ms 的宽限（§2.6 已写）；强制退休时先 abort 遗留 root 的 `codeVersion.abort`，再关资源。

### A9. §2.7 remote 补充

- `RemoteBinding.state(name, frame)` 的 `ops` 分支里，`target = pending?.values.get(name) ?? members.get(name)`；若 target 恰好是 `members` 里的对象，事件是 `{type:"ops", ops, value}`；若在 pending 中则**不**派发（等 `#apply` 的 reset）。Rust 侧不要统一按"总是派发"实现。
- `RemoteRef.shapeChanged()` 对 shape 监听器是**同步**调用（§5.3 已列为两处同步之一），而本地 `RefImpl` 的 shape 通知经 `enqueue`——这个不对称是可观察的，`variants.test.ts` 的 `onShape` 用例依赖同步性。
- `RemoteRef.update()` 的 live 条件包含 `!wantsState || binding.hydrated`——`state: false` 的方法引用在对象 live 后**不等 hydration** 就 ready（`bindings.test.ts` 的"仅方法引用"用例）。
- `ViewReadable.deliver` 对 `unseen` 监听者的特殊处理：`ops` 事件被**转换成 snap**（`{type:"snap", value: event.value}`），其它事件原样；这是"订阅者加入前错过的变化合并为首个 snap"的实现方式。

### A10. §2.8 manager 补充

- `processLock` 在 `EADDRINUSE` 且 **file 平台**时的判定顺序：先 `accepting(address)`（能连上 = 有人持有 → 报 `locked`），连不上（ECONNREFUSED/ENOENT）才 `rm` 重试。非 file 平台（抽象 socket / 命名管道）直接报 `locked`，**永不** rm。
- `forkDriver` 的 `send` 只检查 `child.connected`；`child.send(..., ignoreSendError)` 吞掉 EPIPE。`killTree` 先尝试 `process.kill(-pid)`，失败才 `child.kill()`（pid 可能是 -1，此时 `-pid = 1` 会误杀——TS 侧有 `pid ?? -1` 的隐患；Rust 侧应显式处理 `pid` 未知的情况，不要照抄）。
- `createHostManager` 的 `serialized` 用 `queue.then(step, step)`——**前一步失败也继续执行下一步**（`stop` 在 `start` 失败后仍可调用），且 `queue = next.catch(() => {})` 避免未处理拒绝。Rust 侧用一个串行任务队列即可，但要保持"失败不阻塞后续"。
- `doStart` 的 ready 等待里，`exited` 的 `then` 与 `onControl` 的 `settle` 都可能 resolve/reject，`settle` 清 timeout 且只生效一次（Promise 语义）；**ready 之后**到达的 `fatal` 不再影响 start（但 `onControl` 仍会更新 report）。Rust 侧用一个 oneshot 即可。
- `idleSince` 的更新**只在 `status` 控制消息**里发生（`ready` 时不重置），且 `doStart` 成功后发现总活动为 0 才 `idleSince ??= Date.now()`。
- `keepAlive` 的 interval 是 `checkInterval ?? 1000`，且 `timer.unref?.()`——不阻止进程退出。
- `childControl().onShutdown` 的 handler **不 await**（`void handler()`），因此 `worker.ts` 里 async handler 里的 `await host.close()` 与 `process.exit` 之间没有任何顺序保证——Rust 侧若想 await 会改变行为（`manager.test.ts` 的"优雅停止"依赖 handler 里自己 `process.exit`）。

### A11. 交叉引用与实现风险（新增/修正）

- **`install()` 的同步段**：`facet.setup(...)` 是**同步**调用（`FacetApi.serve` 也是同步），`#hostObjectsReady` 才 await。Rust 里不要把 `setup` 变成 async。**`host.serve`（非 facet 路径）没有 `awaitHostObjects` 选项**：它在 `serveInto` 之后对 `lifetime: "host"` 立即 `startHostObject`（同步 acquire，返回 `RefCore`），错误只经 `ref.ready.catch(report)` 上报——`readiness.test.ts` 的"`ready` 覆盖直接的 host serve"用例依赖这个先后：先 `serve` 再调 `ready()` 会等到它，先调 `ready()` 再 `serve` 不会。
- **`load()` 的 `Promise.all`**：所有 facet 的 `install` 在**同一个微任务批次**里启动（`.map` 同步执行），因此 `setup` 顺序 = 数组顺序，但 `hostObjectsReady` 的 await 是并行的。Rust 侧若要复刻"后一个 facet 的类型可被前一个 host object 工厂消费"，必须先把所有 `install` 同步跑完再 await。
- **`registry().subscribe` 的立即同步回调**：首次回调发生在 `subscribe` 调用**栈内**，且用**当前**表与 `current.id`；后续通知经 `enqueue`。Rust 侧用一个"立即调用 + 队列"的组合。
- **`Map` 迭代顺序**在 `retainedCodeVersions()`、`#foreign`、`#rootLinks`、`#order` 上都可观察（§5.3 已列一部分），Rust 用 `IndexMap`/`IndexSet`。
- **`ObjectImpl` 的 `implementations: Map<codeVersion, Installed>`** 的插入顺序决定 `allDecls()` 的合并顺序（后插入覆盖先插入）；`currentInstalled()` 取 `implementations.get(current.codeVersion)`。
- **`RefImpl`/`RemoteRef` 的 `proxy`**：`REF_BASE` 里的键走原方法（`this` 绑定），其它字符串键走 `member(name)`，`"then"` 显式返回 `undefined`（避免被当成 thenable）——Rust 的显式 handle 必须提供等价的 `"then"` 语义（否则 `await ref` 会挂起）。

### A12. 本轮确认**无**问题的部分（供后续审计参考）

以下内容逐字核对后与源码一致，不需要改动：§2.1 errors（12 个错误码顺序、`retryable` 集合、`fromWireError` 正则）、§2.2 contract 的 `opaque` 正则/reserved 成员顺序/`coverage`/`bind`/`takesNoArgs`、§2.3 的 activation/unwind/lease/gate/refs 主要流程、§2.4 的流控（window/pendingCap/slot/overflow/ack）与 relay 生命周期、§2.5 的 roots/retire/tables/routes/facets、§2.6 的 reload 三段式、§2.7 的 binding/hydration、§2.8 的 processLock 平台分支与 manager 状态机、§3.2 错误码表（与 `docs/guide.md:786-799` 逐字一致）、§3.7 锁地址格式、§4.1 的 hash64 双路 FNV-1a 与地址格式、§4.2 的帧 schema、§4.4 的 IPC 信封、§4.6 的 ReloadReport。

已知的**文档级**不一致（不在本单元源码里，仅提醒）：`docs/guide.md:206` 与 `docs/spec.md:104` 的 `self.get` 示例仍带一个 `{ at? }` 选项，而 `GetOptions`（`src/types.ts:8-16`）只有 `principal`/`state`——移植时以源码为准，不要实现 `at`。
