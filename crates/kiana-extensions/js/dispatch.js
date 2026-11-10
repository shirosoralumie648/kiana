// `core/extensions/runner.ts` 的 13 个 emitter 的逐行移植（design §4.6）。
//
// 合并语义**只**在这里实现：引用相等（`sameMessages`）、`undefined` 键存在性、原地修改、
// 每个 handler 后的 `buildContext(entries)` 都依赖 JS 的对象语义。Rust 侧只负责：
//   - 构造每次派发的 `state`（token、事件、快照、路径）；
//   - 通过 `__kiana_dispatch_host` 调用 handler（host 桥）；
//   - 把结果按事件类型反序列化成 `kiana-ext-api` 的类型。
//
// 宿主对象 `globalThis.__kiana_dispatch_host` 由 Rust 安装（`runner/dispatch.rs`）：
//   - `reportError({extensionPath, event, error, stack})`     同步；扩展错误落点。
//   - `setSystemPrompt(token, prompt)`                       同步；before_agent_start 的
//     `ctx.getSystemPrompt()` 覆盖值（原生 handler 的 ctx）。
//   - `call(token, hid, event) -> Promise<{hasValue, value, event?}>` 可选；缺省时使用
//     `__kiana.promise("kiana_dispatch_call", …)`。返回值用信封包住，区分
//     `undefined`（hasValue=false）与显式 `null`（hasValue=true, value=null）。
//     `event` 是 JS handler 调用后读回的完整事件对象（Rust 适配器提供）；`callHandler`
//     把它按 TS 的对象语义写回本地事件对象，恢复 `headers`/`input`/`entries`/
//     `systemPromptOptions`/`payload`/`messages` 的共享可变语义。
//   - `buildContext(token, entries) -> Promise<context>`       可选；缺省时使用
//     `__kiana.promise("kiana_dispatch_build", …)`。
//   - `renderSystemPrompt(token, options) -> string | undefined` 可选；before_agent_start
//     按当前选项渲染系统提示（未安装渲染器时返回 undefined，退回缓存文本）。
//   - `snapshotContextWithSystem(token) -> {paths, snapshot} | undefined` 可选；emitContext
//     的 phase 2 在 phase 1 结束后才取快照（TS 的调用时机）。
//
// `state` 的形状（由 Rust 构造）：
//   { token, type, paths: string[], snapshot: number[][], event, ... }
// `snapshot[i]` 与 `paths[i]` 平行：第 i 个扩展在本事件上的 handler id 列表（注册顺序）。
// 快照在每次派发开始时取一次；嵌套派发重新取（Rust 每次 emit 新建表）。

(function () {
  "use strict";

  if (globalThis.__kiana_dispatch) return;

  // ---------------------------------------------------------------------
  // 宿主桥
  // ---------------------------------------------------------------------

  function dispatchHost() {
    const host = globalThis.__kiana_dispatch_host;
    if (host === undefined || host === null) {
      throw new Error("__kiana_dispatch_host is not installed");
    }
    return host;
  }

  // 结果信封：`{ hasValue, value, event? }`。`hasValue === false` 表示 handler 返回
  // undefined（JSON 里没有 undefined，必须用信封把它与显式 null 区分开）；`event` 是
  // handler 调用后读回的事件对象。fake host 可以直接返回裸值。
  function unwrapEnvelope(result) {
    if (result !== null && typeof result === "object" && typeof result.hasValue === "boolean") {
      return result;
    }
    return { hasValue: result !== undefined, value: result };
  }

  // TS 的 handler 拿到的是 runner 构造的**共享**事件对象：`before_provider_headers.headers`、
  // `tool_call.input`、`before_agent_start.systemPromptOptions`、边界事件的 `event.entries`
  // 的原地修改对后续 handler 与最终返回值可见。宿主桥（Rust 适配器）在每次调用后读回事件
  // JSON；这里按 TS 的对象语义写回本地事件对象（源里没有的键删除）。
  // `systemPrompt` 是 dispatch 侧定义的 getter（按当前选项重渲染），不能被快照覆盖。
  function isPlainObjectValue(value) {
    return value !== null && typeof value === "object" && !Array.isArray(value);
  }

  // JSON 往返还原会把未修改的数组/对象换成新引用，而 TS 的 `sameMessages` /
  // `sameMessages(returned, visible)` 依赖引用相等。`sameValue` 做结构化比较，
  // `reconcileValue` 在内容未变时保留原引用、内容变化时**就地**合并，从而在
  // “handler 没动 / 原地修改 / 只替换属性” 三种情形下复现 TS 的可观察结果。
  function sameValue(left, right) {
    if (left === right) return true;
    if (Array.isArray(left) || Array.isArray(right)) {
      if (!Array.isArray(left) || !Array.isArray(right) || left.length !== right.length) return false;
      for (let index = 0; index < left.length; index++) {
        if (!sameValue(left[index], right[index])) return false;
      }
      return true;
    }
    if (isPlainObjectValue(left) && isPlainObjectValue(right)) {
      const leftKeys = Object.keys(left);
      const rightKeys = Object.keys(right);
      if (leftKeys.length !== rightKeys.length) return false;
      for (const key of leftKeys) {
        if (!Object.prototype.hasOwnProperty.call(right, key)) return false;
        if (!sameValue(left[key], right[key])) return false;
      }
      return true;
    }
    return false;
  }

  function reconcileValue(target, source) {
    if (sameValue(target, source)) return target;
    if (Array.isArray(target) && Array.isArray(source) && target.length === source.length) {
      for (let index = 0; index < source.length; index++) {
        target[index] = reconcileValue(target[index], source[index]);
      }
      return target;
    }
    if (isPlainObjectValue(target) && isPlainObjectValue(source)) {
      for (const key of Object.keys(target)) {
        if (!Object.prototype.hasOwnProperty.call(source, key)) delete target[key];
      }
      for (const key of Object.keys(source)) {
        target[key] = reconcileValue(target[key], source[key]);
      }
      return target;
    }
    return source;
  }

  function applyEventMutation(target, source) {
    if (target === null || typeof target !== "object") return;
    if (source === null || typeof source !== "object" || Array.isArray(source)) return;
    for (const key of Object.keys(target)) {
      if (key === "systemPrompt") continue;
      if (!Object.prototype.hasOwnProperty.call(source, key)) delete target[key];
    }
    for (const key of Object.keys(source)) {
      if (key === "systemPrompt") continue;
      target[key] = reconcileValue(target[key], source[key]);
    }
  }

  // TS 逐 handler 重建事件对象（`{ ...event, message: currentMessage }`、`{ type, headers }`、
  // `{ type, messages }` …）时，只有**嵌套共享对象**的原地修改可见；handler 对事件属性的
  // 重新赋值只改它自己的临时对象，runner 读不到。`refs[key]` 由 loader 适配器在 JS 运行时
  // 里逐属性比较调用前后的引用得出：`"same"` 表示仍是同一个 JS 值（可能是原地修改），
  // 其余（`"changed"`/缺失/新增）是重新赋值，忽略。宿主不提供 refs（fake host）时退回
  // 旧的「全部采纳」行为。
  function applyIsolatedMutation(target, source, refs) {
    if (target === null || typeof target !== "object") return;
    if (source === null || typeof source !== "object" || Array.isArray(source)) return;
    for (const key of Object.keys(source)) {
      if (key === "systemPrompt") continue;
      if (refs !== undefined && refs !== null && refs[key] !== "same") continue;
      target[key] = reconcileValue(target[key], source[key]);
    }
  }

  function applyCallMutation(target, source, refs, mode) {
    if (mode === "isolated") applyIsolatedMutation(target, source, refs);
    else applyEventMutation(target, source);
  }

  // 最近一次 handler 调用是否按**引用**改动了共享事件对象的 `messages` 数组
  // （`undefined` 表示宿主没有提供该信息，例如测试里的 fake host）。
  // TS 的 `sameMessages` 比较数组元素引用：只有 splice/push/替换元素才算改动，消息对象
  // 的**原地内容修改**不算（它本来就与 runner 共享同一个对象，内容修改自然可见）。
  // 跨 JSON 桥后引用信息只能由 loader 适配器在 JS 运行时里测出，经宿主桥带回。
  let lastCallMessagesChanged;
  // `input` handler 返回的 `images` 是否与事件的 `images` 是同一个引用（TS 的
  // `currentImages !== images` 判定；由 loader 适配器在 JS 运行时里测出）。
  let lastCallInputImagesSameRef;

  function takeLastCallMessagesChanged() {
    const changed = lastCallMessagesChanged;
    lastCallMessagesChanged = undefined;
    return changed;
  }

  function takeLastCallInputImagesSameRef() {
    const same = lastCallInputImagesSameRef;
    lastCallInputImagesSameRef = undefined;
    return same;
  }

  // 宿主拒绝值：`{ __kianaDispatchError: true, name, message, stack, event?, messagesChanged? }`
  // （`runner/dispatch.rs` 的 `handle_call` 构造）。失败前对共享事件对象的就地修改在
  // TS 里已经生效（handler 与 runner 是同一个对象），这里先应用再抛出，保持同样语义。
  function normalizeRejection(err, event, mode) {
    if (err !== null && typeof err === "object" && err.__kianaDispatchError === true) {
      if (err.event !== undefined) applyCallMutation(event, err.event, err.eventRefs, mode);
      lastCallMessagesChanged = err.messagesChanged === true;
      const error = new Error(typeof err.message === "string" ? err.message : String(err.message));
      if (typeof err.name === "string" && err.name.length > 0) error.name = err.name;
      // 原始异常的 stack（TS 的 `err instanceof Error ? err.stack : undefined`）；交互式
      // 错误面板逐行显示它。QuickJS 的 `Error.prototype.stack` setter 只接受字符串，
      // 原始抛出没有 stack（非 Error 值）时用不可枚举标记表达 `undefined`，
      // `errorStack` 优先读它，避免把宿主桥新建 `Error` 的栈当成扩展的栈上报。
      Object.defineProperty(error, "__kianaStack", {
        value: typeof err.stack === "string" ? err.stack : undefined,
        configurable: true,
        writable: true,
        enumerable: false,
      });
      return error;
    }
    lastCallMessagesChanged = undefined;
    return err;
  }

  async function callHandler(token, hid, event, mode) {
    const host = dispatchHost();
    let raw;
    try {
      raw =
        typeof host.call === "function"
          ? await host.call(token, hid, event)
          : await globalThis.__kiana.promise("kiana_dispatch_call", { token: token, hid: hid, event: event });
    } catch (err) {
      throw normalizeRejection(err, event, mode);
    }
    const envelope = unwrapEnvelope(raw);
    if (envelope.event !== undefined) {
      applyCallMutation(event, envelope.event, envelope.eventRefs, mode);
    }
    lastCallMessagesChanged = envelope.messagesChanged === true;
    lastCallInputImagesSameRef =
      typeof envelope.inputImagesSameRef === "boolean" ? envelope.inputImagesSameRef : undefined;
    return envelope.hasValue ? envelope.value : undefined;
  }

  function buildContext(token, entries) {
    const host = dispatchHost();
    if (typeof host.buildContext === "function") {
      return Promise.resolve(host.buildContext(token, entries));
    }
    return globalThis.__kiana.promise("kiana_dispatch_build", { token: token, entries: entries });
  }

  // TS: `err instanceof Error ? err.message : String(err)`。
  function errorMessage(err) {
    return err instanceof Error ? err.message : String(err);
  }

  // TS: `err instanceof Error ? err.stack : undefined`。
  function errorStack(err) {
    if (!(err instanceof Error)) return undefined;
    // 宿主拒绝值还原出的 Error 用 `__kianaStack` 携带原始异常的 stack（含 `undefined`）。
    if (Object.prototype.hasOwnProperty.call(err, "__kianaStack")) return err.__kianaStack;
    return err.stack;
  }

  function reportRaw(token, payload) {
    payload.token = token;
    dispatchHost().reportError(payload);
  }

  function report(token, extensionPath, event, err) {
    reportRaw(token, {
      extensionPath: extensionPath,
      event: event,
      error: errorMessage(err),
      stack: errorStack(err),
    });
  }

  function setSystemPrompt(token, prompt) {
    const host = dispatchHost();
    if (typeof host.setSystemPrompt === "function") host.setSystemPrompt(token, prompt);
  }

  // `snapshotEventHandlers`：extensions.map((ext) => ({ ext, handlers }))。
  function handlerSnapshot(state) {
    const entries = [];
    const paths = state.paths === undefined ? [] : state.paths;
    const snapshot = state.snapshot === undefined ? [] : state.snapshot;
    for (let index = 0; index < paths.length; index++) {
      entries.push({ path: paths[index], handlers: snapshot[index] === undefined ? [] : snapshot[index] });
    }
    return entries;
  }

  function sameMessages(left, right) {
    if (left.length !== right.length) return false;
    for (let index = 0; index < left.length; index++) {
      if (left[index] !== right[index]) return false;
    }
    return true;
  }

  // TS: `contentText(content, separator = "\n")`（packages/ai/src/utils/text.ts）。
  function contentText(content, separator) {
    const sep = separator === undefined ? "\n" : separator;
    if (typeof content === "string") return content;
    if (!Array.isArray(content)) return "";
    return content
      .filter((block) => block && block.type === "text")
      .map((block) => block.text)
      .join(sep);
  }

  // TS: `getCurrentTools(messages)`——按 transcript delta 顺序解析 toolsAdded/toolsRemoved。
  function getCurrentTools(messages) {
    const tools = new Map();
    for (const message of messages) {
      if (message === null || message === undefined || message.role !== "system") continue;
      for (const tool of message.toolsRemoved === undefined ? [] : message.toolsRemoved) {
        tools.delete(tool.name);
      }
      for (const tool of message.toolsAdded === undefined ? [] : message.toolsAdded) {
        tools.set(tool.name, tool);
      }
    }
    return [...tools.values()];
  }

  // TS: `getCurrentSystemMessage(messages)`——把每条 system 消息回放成一条首部 system 消息。
  // 后续 content 以 `"\n\n"` 追加，sections 按名打补丁（`null` 删除），工具用
  // `getCurrentTools` 解析。键顺序与 TS 的对象字面量一致：role、content、sections?、
  // toolsAdded?、timestamp。
  function getCurrentSystemMessage(messages) {
    const content = [];
    const sections = new Map();
    let timestamp;
    for (const message of messages) {
      if (message === null || message === undefined || message.role !== "system") continue;
      if (timestamp === undefined) timestamp = message.timestamp;
      const text = contentText(message.content, "\n");
      if (text.length > 0) content.push(text);
      const messageSections = message.sections === undefined ? {} : message.sections;
      for (const name of Object.keys(messageSections)) {
        const value = messageSections[name];
        if (value === null) sections.delete(name);
        else sections.set(name, value);
      }
    }
    const tools = getCurrentTools(messages);
    if (timestamp === undefined && tools.length === 0) return undefined;
    const head = { role: "system", content: content.join("\n\n") };
    if (sections.size > 0) head.sections = Object.fromEntries(sections);
    if (tools.length > 0) head.toolsAdded = tools;
    head.timestamp = timestamp === undefined ? 0 : timestamp;
    return head;
  }

  function isSessionBeforeEvent(type) {
    return (
      type === "session_before_switch" ||
      type === "session_before_fork" ||
      type === "session_before_compact" ||
      type === "session_before_tree"
    );
  }

  // TS: `isUserBashEventResult` 的 XOR 校验。
  //
  // `operations` 来自 Rust 原生 handler 时无法跨 JSON 传递函数；host 桥用
  // `{ operations: { __kianaOperations: true } }` 标记，并把这个结果留在 Rust 侧的
  // 派发表里（emitUserBash 返回时按标记取回）。
  function isUserBashEventResult(value) {
    if (typeof value !== "object" || value === null) return false;
    const hasOperations = value.operations !== undefined;
    const hasResult = value.result !== undefined;
    if (hasOperations === hasResult) return false;

    if (hasOperations) {
      const operations = value.operations;
      if (typeof operations !== "object" || operations === null) return false;
      if (operations.__kianaOperations === true) return true;
      return typeof operations.exec === "function";
    }

    const result = value.result;
    if (typeof result !== "object" || result === null) return false;
    // Rust 原生 handler 的 `BashResult` 是类型化构造的（合法）：`exitCode` 的 `None`
    // 对应 TS 的属性 `undefined`，JSON 无法表达（键被省略），因此由 `__kianaNative` 标记
    // 跳过 `"exitCode" in result` 的字面检查。JS handler 的结果已在 loader 里按 TS 的
    // `isUserBashEventResult` 逐字校验，不会带这个标记。
    if (value.__kianaNative === true) return true;
    // TS: `exitCode` 必须是存在的键，值为 number 或 undefined；`null` 非法。
    return (
      typeof result.output === "string" &&
      "exitCode" in result &&
      (result.exitCode === undefined || typeof result.exitCode === "number") &&
      typeof result.cancelled === "boolean" &&
      typeof result.truncated === "boolean" &&
      (result.fullOutputPath === undefined || typeof result.fullOutputPath === "string")
    );
  }

  // ---------------------------------------------------------------------
  // 1. 泛型 emit（仅 session_before_* 取结果；cancel 短路）
  // ---------------------------------------------------------------------

  async function emit(state) {
    let result;
    for (const entry of handlerSnapshot(state)) {
      for (const hid of entry.handlers) {
        try {
          const handlerResult = await callHandler(state.token, hid, state.event);
          if (isSessionBeforeEvent(state.type) && handlerResult) {
            result = handlerResult;
            if (result.cancel) return result;
          }
        } catch (err) {
          report(state.token, entry.path, state.type, err);
        }
      }
    }
    return result === undefined ? null : result;
  }

  // ---------------------------------------------------------------------
  // 2. emitBoundary（turn_end / agent_before_settle）
  // ---------------------------------------------------------------------

  async function emitBoundary(state) {
    let entries = state.entries === undefined ? [] : state.entries;
    let shouldContinue = state.continue === true;
    // TS: `let context = await buildContext(entries)` 在循环之前；初始构建失败会让整个
    // emit 直接 reject（不在 try/catch 内）。
    let context = await buildContext(state.token, entries);
    let valid = true;

    for (const entry of handlerSnapshot(state)) {
      for (const hid of entry.handlers) {
        const event = {
          ...state.base,
          entries: entries,
          continue: shouldContinue,
          context: context,
        };
        let handlerResult;
        try {
          handlerResult = await callHandler(state.token, hid, event, "isolated");
        } catch (err) {
          // TS 的 handler 与 runner 共享事件对象：抛错前对 `event.entries` 的修改
          // **不会**回滚（`callHandler` 已把读回的事件对象写回本地 `event`）。
          report(state.token, entry.path, state.type, err);
        }
        let tookResultEntries = false;
        if (handlerResult !== null && handlerResult !== undefined) {
          if (handlerResult.entries !== undefined) {
            entries = handlerResult.entries;
            tookResultEntries = true;
          }
          if (handlerResult.continue !== undefined) shouldContinue = handlerResult.continue;
        }
        // TS 的 `event.entries` 是共享草稿数组：handler 只 `push(...)` 并返回 undefined
        // （或抛错）时修改同样生效。读回后数组可能被换成新引用（长度变化），此时采用它。
        if (
          !tookResultEntries &&
          Array.isArray(event.entries) &&
          !sameMessages(event.entries, entries)
        ) {
          entries = event.entries;
        }

        try {
          context = await buildContext(state.token, entries);
          valid = true;
        } catch (err) {
          valid = false;
          reportRaw(state.token, {
            extensionPath: entry.path,
            event: state.type,
            error: "Invalid boundary entries: " + errorMessage(err),
            stack: errorStack(err),
          });
        }
      }
    }

    return valid
      ? { entries: entries, continue: shouldContinue, context: context, valid: true }
      : { entries: [], continue: false, context: context, valid: false };
  }

  // ---------------------------------------------------------------------
  // 3. emitCacheWarmingDecision（最后一次 override 获胜）
  // ---------------------------------------------------------------------

  async function emitCacheWarmingDecision(state) {
    let action = state.event.action;
    for (const entry of handlerSnapshot(state)) {
      for (const hid of entry.handlers) {
        try {
          const result = await callHandler(state.token, hid, state.event);
          if (result !== null && result !== undefined && result.action !== undefined) {
            action = result.action;
          }
        } catch (err) {
          report(state.token, entry.path, state.type, err);
        }
      }
    }
    return action;
  }

  // ---------------------------------------------------------------------
  // 4. emitMessageEnd（逐 handler 换 currentMessage；role 必须一致）
  // ---------------------------------------------------------------------

  async function emitMessageEnd(state) {
    let currentMessage = state.event.message;
    let modified = false;

    for (const entry of handlerSnapshot(state)) {
      for (const hid of entry.handlers) {
        try {
          const currentEvent = { ...state.event, message: currentMessage };
          const handlerResult = await callHandler(state.token, hid, currentEvent, "isolated");
          if (!handlerResult || !handlerResult.message) continue;

          if (handlerResult.message.role !== currentMessage.role) {
            reportRaw(state.token, {
              extensionPath: entry.path,
              event: "message_end",
              error: "message_end handlers must return a message with the same role",
            });
            continue;
          }

          currentMessage = handlerResult.message;
          modified = true;
        } catch (err) {
          report(state.token, entry.path, "message_end", err);
        }
      }
    }

    return modified ? currentMessage : null;
  }

  // ---------------------------------------------------------------------
  // 5. emitToolResult（单一被改事件；content 替换时删除不匹配的 structuredContent）
  // ---------------------------------------------------------------------

  async function emitToolResult(state) {
    const currentEvent = { ...state.event };
    let modified = false;

    for (const entry of handlerSnapshot(state)) {
      for (const hid of entry.handlers) {
        try {
          const handlerResult = await callHandler(state.token, hid, currentEvent);
          if (!handlerResult) continue;

          if (handlerResult.content !== undefined) {
            currentEvent.content = handlerResult.content;
            // Structured content that is not replaced along with the content may no longer match it.
            if (handlerResult.structuredContent === undefined) delete currentEvent.structuredContent;
            modified = true;
          }
          if (handlerResult.details !== undefined) {
            currentEvent.details = handlerResult.details;
            modified = true;
          }
          if (handlerResult.structuredContent !== undefined) {
            currentEvent.structuredContent = handlerResult.structuredContent;
            modified = true;
          }
          if (handlerResult.isError !== undefined) {
            currentEvent.isError = handlerResult.isError;
            modified = true;
          }
          if (handlerResult.usage !== undefined) {
            currentEvent.usage = handlerResult.usage;
            modified = true;
          }
        } catch (err) {
          report(state.token, entry.path, "tool_result", err);
        }
      }
    }

    if (!modified) return null;

    return {
      content: currentEvent.content,
      details: currentEvent.details,
      structuredContent: currentEvent.structuredContent,
      isError: currentEvent.isError,
      usage: currentEvent.usage,
    };
  }

  // ---------------------------------------------------------------------
  // 6. emitToolCall（无 try/catch：错误向上穿透；block 短路；最后结果获胜）
  // ---------------------------------------------------------------------

  async function emitToolCall(state) {
    let result;
    let hasResult = false;
    for (const entry of handlerSnapshot(state)) {
      for (const hid of entry.handlers) {
        const handlerResult = await callHandler(state.token, hid, state.event);
        if (handlerResult) {
          result = handlerResult;
          hasResult = true;
          if (result.block) return { hasResult: true, result: result, input: state.event.input };
        }
      }
    }
    return { hasResult: hasResult, result: hasResult ? result : null, input: state.event.input };
  }

  // ---------------------------------------------------------------------
  // 7. emitUserBash（唯一“捕获后重新抛出”的 emitter）
  // ---------------------------------------------------------------------

  const INVALID_USER_BASH_RESULT =
    "Invalid user_bash handler result: return undefined for local execution or exactly one valid { operations } or { result } object";

  async function emitUserBash(state) {
    for (const entry of handlerSnapshot(state)) {
      for (const hid of entry.handlers) {
        try {
          const handlerResult = await callHandler(state.token, hid, state.event);
          // TS 只跳过 `undefined`；`null` 是非法结果（下面抛出 canonical 文案）。
          if (handlerResult === undefined) continue;
          if (!isUserBashEventResult(handlerResult)) {
            throw new Error(INVALID_USER_BASH_RESULT);
          }
          return handlerResult;
        } catch (err) {
          report(state.token, entry.path, "user_bash", err);
          throw err;
        }
      }
    }
    return null;
  }

  // ---------------------------------------------------------------------
  // 8. emitContext（两阶段；sameMessages 引用相等）
  // ---------------------------------------------------------------------

  // TS 的 `restoreSystemMessages` 在**返回数组与调用前的可见数组逐元素不同**时重建首部
  // system 消息。跨 JSON 桥后 `sameMessages` 的引用比较不可用，调用方已经用
  // `sameValue`（结构化比较）或 loader 带回的引用变更标志判定“是否改动”，因此这里直接
  // 执行重建分支（与 TS 的 `[head, ...returned]` 一致）。
  function rebuildLeadingSystemMessage(current, messages) {
    const head = getCurrentSystemMessage(current);
    return head ? [head, ...messages] : messages;
  }

  async function emitContext(state) {
    let currentMessages =
      typeof structuredClone === "function"
        ? structuredClone(state.messages === undefined ? [] : state.messages)
        : JSON.parse(JSON.stringify(state.messages === undefined ? [] : state.messages));

    for (const entry of handlerSnapshot(state)) {
      for (const hid of entry.handlers) {
        try {
          const visibleMessages = currentMessages.filter((message) => message.role !== "system");
          const visibleSnapshot = visibleMessages.slice();
          const event = { type: "context", messages: visibleMessages };
          const handlerResult = await callHandler(state.token, hid, event, "isolated");
          const messagesChanged = takeLastCallMessagesChanged();

          // Handlers may return a new list or edit event.messages in place.
          const returned =
            handlerResult === null || handlerResult === undefined ? undefined : handlerResult.messages;
          const currentVisible = Array.isArray(event.messages) ? event.messages : visibleMessages;
          if (returned !== undefined && returned !== null) {
            // TS 的 `sameMessages(returned, visibleSnapshot)` 是引用比较：返回 `event.messages`
            // 本身（或同元素的副本）时 `currentMessages` 原样返回，保留中间位置的 system
            // 消息。跨 JSON 桥后按内容比较；`messagesChanged`（数组引用被 splice/push/
            // 替换元素）优先判定为“改动了”。
            if (messagesChanged !== true && sameValue(returned, currentVisible)) continue;
            currentMessages = rebuildLeadingSystemMessage(currentMessages, returned);
            continue;
          }
          // loader 带回的引用变更标志（splice/push/替换元素）或 fake host 直接改动共享数组。
          // 消息对象的原地内容修改两边都不算“改动”：它与 currentMessages 共享对象，内容
          // 修改已经生效，且 TS 不会因此重建首部 system 消息。
          if (messagesChanged === true || !sameMessages(currentVisible, visibleSnapshot)) {
            currentMessages = rebuildLeadingSystemMessage(currentMessages, currentVisible);
          }
        } catch (err) {
          report(state.token, entry.path, "context", err);
        }
      }
    }

    // TS 在 phase 1 结束后才 `snapshotEventHandlers("context_with_system")`：phase 1 里
    // 动态 `kiana.on()/off()` 的结果参与本次派发。宿主提供晚快照回调；fake host 退回
    // state 自带的 `paths2`/`snapshot2`。
    const host = dispatchHost();
    let late =
      typeof host.snapshotContextWithSystem === "function"
        ? host.snapshotContextWithSystem(state.token)
        : undefined;
    if (late === undefined || late === null) {
      late = { paths: state.paths2, snapshot: state.snapshot2 };
    }
    const secondPhase = handlerSnapshot(late);
    for (const entry of secondPhase) {
      for (const hid of entry.handlers) {
        try {
          const hadLeadingSystemMessage =
            currentMessages[0] !== undefined && currentMessages[0] !== null && currentMessages[0].role === "system";
          const event = { type: "context_with_system", messages: currentMessages };
          const handlerResult = await callHandler(state.token, hid, event, "isolated");
          const returned =
            handlerResult === null || handlerResult === undefined ? undefined : handlerResult.messages;
          // TS 的 phase-2 handler 拿到 `currentMessages` 数组本身；就地修改（push/splice）
          // 通过宿主读回体现在 event.messages 上。
          const mutated = Array.isArray(event.messages) ? event.messages : currentMessages;
          currentMessages = returned === undefined || returned === null ? mutated : returned;
          if (
            hadLeadingSystemMessage &&
            (currentMessages[0] === undefined || currentMessages[0] === null || currentMessages[0].role !== "system")
          ) {
            reportRaw(state.token, {
              extensionPath: entry.path,
              event: "context_with_system",
              error:
                "Handler removed the leading system message; the request has no prompt or initial tool declarations. Keep it at index 0 or replace a dropped prefix with getCurrentSystemMessage().",
            });
          }
        } catch (err) {
          report(state.token, entry.path, "context_with_system", err);
        }
      }
    }

    return currentMessages;
  }

  // ---------------------------------------------------------------------
  // 9. emitBeforeProviderRequest（undefined 保持当前 payload）
  // ---------------------------------------------------------------------

  async function emitBeforeProviderRequest(state) {
    let currentPayload = state.event.payload;
    let replaced = false;

    for (const entry of handlerSnapshot(state)) {
      for (const hid of entry.handlers) {
        const event = { type: "before_provider_request", payload: currentPayload };
        const before = JSON.stringify(currentPayload);
        let handlerResult;
        let failed = false;
        try {
          handlerResult = await callHandler(state.token, hid, event, "isolated");
        } catch (err) {
          failed = true;
          report(state.token, entry.path, "before_provider_request", err);
        }
        // `event.payload` 与 `currentPayload` 在 TS 里是同一个对象（就地修改由共享引用
        // 生效，重新赋值 `event.payload` 不被采纳——TS 只读返回值）。抛错前的就地修改
        // 同样保留。
        if (!failed && handlerResult !== undefined) {
          currentPayload = handlerResult;
          replaced = true;
        } else if (JSON.stringify(currentPayload) !== before) {
          replaced = true;
        }
      }
    }

    return { payload: currentPayload === undefined ? null : currentPayload, replaced: replaced };
  }

  // ---------------------------------------------------------------------
  // 10. emitBeforeProviderHeaders（原地修改，返回值忽略）
  // ---------------------------------------------------------------------

  async function emitBeforeProviderHeaders(state) {
    let headers = state.event.headers;

    for (const entry of handlerSnapshot(state)) {
      for (const hid of entry.handlers) {
        try {
          // Handlers mutate `headers` in place; the return value is ignored.
          const event = { type: "before_provider_headers", headers: headers };
          await callHandler(state.token, hid, event, "isolated");
          // 读回同一事件对象的 headers（Rust 原生 handler 没有就地修改通道，值不变）。
          headers = event.headers;
        } catch (err) {
          report(state.token, entry.path, "before_provider_headers", err);
        }
      }
    }

    return { headers: headers };
  }

  // ---------------------------------------------------------------------
  // 11. emitBeforeAgentStart（chained forceSystemPrompt、per-handler message、
  //     event.systemPrompt getter、ctx.getSystemPrompt 覆盖）
  // ---------------------------------------------------------------------

  async function emitBeforeAgentStart(state) {
    let currentOptions = state.systemPromptOptions;
    let currentPrompt = state.systemPrompt === undefined ? "" : state.systemPrompt;
    const messages = [];
    const host = dispatchHost();

    // TS: `renderCurrentSystemPrompt = () => buildSystemPrompt(currentOptions)`；每次读取
    // `event.systemPrompt` / `ctx.getSystemPrompt()` 都按**当前**选项重新渲染。Rust 侧注入
    // 的渲染器返回 undefined 时（未注入）退回缓存的提示文本。
    function renderCurrentSystemPrompt(options) {
      // handler 内的读取（`event.systemPrompt` / `ctx.getSystemPrompt()`）传入**事件对象上
      // 当前的**选项（可能已被本 handler 原地修改）；不传时用 dispatch 侧的共享对象。
      const current = options === undefined || options === null ? currentOptions : options;
      if (typeof host.renderSystemPrompt === "function") {
        const rendered = host.renderSystemPrompt(state.token, current);
        if (rendered !== undefined && rendered !== null) return rendered;
      }
      // `buildSystemPrompt` 对 `forceSystemPrompt !== undefined` 直接返回它（不渲染段落）；
      // 未注入渲染器时至少复现该分支，让 handler 原地设置/清除 `forceSystemPrompt` 之后
      // 的读取（以及后续 handler 的 `ctx.getSystemPrompt()`）看到当前值。
      if (
        current !== null &&
        typeof current === "object" &&
        current.forceSystemPrompt !== undefined
      ) {
        return current.forceSystemPrompt;
      }
      return currentPrompt;
    }

    for (const entry of handlerSnapshot(state)) {
      for (const hid of entry.handlers) {
        try {
          // `ctx.getSystemPrompt()` 在 handler 执行期间由 runner 上下文实现读取；把当前
          // 渲染结果放进覆盖槽，与 TS 的 `ctx.getSystemPrompt = renderCurrentSystemPrompt`
          // 保持同一语义（handler 之间的链式更新可见）。
          setSystemPrompt(state.token, renderCurrentSystemPrompt());
          const event = {
            ...state.trigger,
            type: "before_agent_start",
            get systemPrompt() {
              return renderCurrentSystemPrompt(this.systemPromptOptions);
            },
            systemPromptOptions: currentOptions,
          };
          // 适配器构造的事件对象把 `systemPrompt` 定义成访问器：每次读取都调用这里安装的
          // 渲染器（闭包持有活的 `currentOptions`），复现 TS 的
          // `get systemPrompt() { return buildSystemPrompt(currentOptions); }`。
          // `ctx.getSystemPrompt()`（context.js / Rust 桥）读同一个全局。
          const previousRender = globalThis.__kiana_before_agent_start_render;
          globalThis.__kiana_before_agent_start_render = renderCurrentSystemPrompt;
          let handlerResult;
          try {
            handlerResult = await callHandler(state.token, hid, event, "isolated");
          } finally {
            globalThis.__kiana_before_agent_start_render = previousRender;
          }

          // `event.systemPromptOptions` 是共享可变对象：handler 的原地修改让后续
          // handler 与最终结果看到（宿主桥读回 event 后这里采用其最新值）。
          if (event.systemPromptOptions !== undefined && event.systemPromptOptions !== null) {
            currentOptions = event.systemPromptOptions;
          }
          if (handlerResult) {
            if (handlerResult.message) messages.push(handlerResult.message);
            if (handlerResult.systemPrompt !== undefined) {
              currentOptions.forceSystemPrompt = handlerResult.systemPrompt;
              currentPrompt = handlerResult.systemPrompt;
              setSystemPrompt(state.token, renderCurrentSystemPrompt());
            }
          }
        } catch (err) {
          report(state.token, entry.path, "before_agent_start", err);
        }
      }
    }

    return { messages: messages, systemPromptOptions: currentOptions };
  }

  // ---------------------------------------------------------------------
  // 12. emitResourcesDiscover（三类路径各自带 extensionPath）
  // ---------------------------------------------------------------------

  async function emitResourcesDiscover(state) {
    const skillPaths = [];
    const promptPaths = [];
    const themePaths = [];

    for (const entry of handlerSnapshot(state)) {
      for (const hid of entry.handlers) {
        try {
          // TS 在循环内构造新的事件对象（同一 cwd/reason，但 handler 之间的就地修改
          // 不共享）。
          const event = {
            type: "resources_discover",
            cwd: state.event.cwd,
            reason: state.event.reason,
          };
          const handlerResult = await callHandler(state.token, hid, event, "isolated");
          if (!handlerResult) continue;
          if (handlerResult.skillPaths && handlerResult.skillPaths.length) {
            for (const path of handlerResult.skillPaths) {
              skillPaths.push({ path: path, extensionPath: entry.path });
            }
          }
          if (handlerResult.promptPaths && handlerResult.promptPaths.length) {
            for (const path of handlerResult.promptPaths) {
              promptPaths.push({ path: path, extensionPath: entry.path });
            }
          }
          if (handlerResult.themePaths && handlerResult.themePaths.length) {
            for (const path of handlerResult.themePaths) {
              themePaths.push({ path: path, extensionPath: entry.path });
            }
          }
        } catch (err) {
          report(state.token, entry.path, "resources_discover", err);
        }
      }
    }

    return { skillPaths: skillPaths, promptPaths: promptPaths, themePaths: themePaths };
  }

  // ---------------------------------------------------------------------
  // 13. emitInput（handled 短路；transform 链式；引用比较决定最终动作）
  // ---------------------------------------------------------------------

  async function emitInput(state) {
    let currentText = state.text;
    let currentImages = state.images;
    // 调用方数组的初始内容：原位修改（`event.images[0].data = …`，共享数组）在 TS 里同样
    // 改变调用方持有的数组；Rust 的 `InputEventResult::Continue` 不携带图像，因此内容有变
    // 时必须返回 `transform` 才能把修改交回调用方。
    const initialImages =
      currentImages === undefined ? undefined : JSON.parse(JSON.stringify(currentImages));
    let imagesReplaced = false;

    for (const entry of handlerSnapshot(state)) {
      for (const hid of entry.handlers) {
        try {
          const event = {
            type: "input",
            text: currentText,
            images: currentImages,
            source: state.source,
            streamingBehavior: state.streamingBehavior,
          };
          const result = await callHandler(state.token, hid, event, "isolated");
          const imagesSameRef = takeLastCallInputImagesSameRef();
          if (result !== null && result !== undefined && result.action === "handled") return result;
          if (result !== null && result !== undefined && result.action === "transform") {
            currentText = result.text;
            // TS 的 `result.images ?? currentImages` 按**引用**判断：只有 handler 返回的
            // 正是事件里的原数组（同一个引用）才视为未替换；新数组（哪怕内容相同）都采用。
            if (result.images !== undefined && result.images !== null) {
              if (imagesSameRef !== true) {
                currentImages = result.images;
                imagesReplaced = true;
              }
            }
          }
        } catch (err) {
          report(state.token, entry.path, "input", err);
        }
      }
    }
    return currentText !== state.text ||
      imagesReplaced ||
      !sameValue(currentImages, initialImages)
      ? { action: "transform", text: currentText, images: currentImages }
      : { action: "continue" };
  }

  // ---------------------------------------------------------------------
  // project_trust（第一个非 undecided 获胜）
  // ---------------------------------------------------------------------

  async function emitProjectTrust(state) {
    for (const entry of handlerSnapshot(state)) {
      for (const hid of entry.handlers) {
        try {
          const handlerResult = await callHandler(state.token, hid, state.event);
          // TS 直接读 `handlerResult.trusted`：null/undefined 会抛 TypeError，被 catch
          // 收进 errors 后继续下一个 handler（fail-soft）。
          if (handlerResult === null) {
            throw new TypeError("Cannot read properties of null (reading 'trusted')");
          }
          if (handlerResult === undefined) {
            throw new TypeError("Cannot read properties of undefined (reading 'trusted')");
          }
          if (handlerResult.trusted === "undecided") continue;
          return handlerResult;
        } catch (err) {
          report(state.token, entry.path, "project_trust", err);
        }
      }
    }
    return null;
  }

  globalThis.__kiana_dispatch = {
    emit: emit,
    emitBoundary: emitBoundary,
    emitCacheWarmingDecision: emitCacheWarmingDecision,
    emitMessageEnd: emitMessageEnd,
    emitToolResult: emitToolResult,
    emitToolCall: emitToolCall,
    emitUserBash: emitUserBash,
    emitContext: emitContext,
    emitBeforeProviderRequest: emitBeforeProviderRequest,
    emitBeforeProviderHeaders: emitBeforeProviderHeaders,
    emitBeforeAgentStart: emitBeforeAgentStart,
    emitResourcesDiscover: emitResourcesDiscover,
    emitInput: emitInput,
    emitProjectTrust: emitProjectTrust,
  };
})();
