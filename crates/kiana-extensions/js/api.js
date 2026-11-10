// `core/extensions/loader.ts` 的 `createExtensionAPI` 的 JS 投影（`kiana` 对象 + `events`）。
//
// 状态机（loading / active / failed、pending flag、pending runtime changes、loading 期订阅）
// 在 Rust 侧（`loader/mod.rs` 的 `ApiEntry`）实现，因为同一套语义也要服务 Rust 定义的扩展
// （`kiana-ext-api` 的 `ExtensionApi` trait）。本文件只负责：
//   1. 把 41 个 `on()` 重载折叠成一个 `on(event, handler)`；
//   2. 暴露 31 个非 `on` 方法与 `events` 字段，逐项转发到宿主原语；
//   3. 两个【逐字】TS 特例：`registerProvider(name)` 缺 config、`setModel` 的 rejected promise。
//
// 宿主对象（`host`）是每个扩展一个的 Rust 闭包集合，见 `build_host_object`。
(function () {
  if (globalThis.__kiana_api) return;

  function makeApi(host, extensionPath) {
    const api = {
      // --- 事件订阅（41 个重载 → 一个方法） --------------------------------
      on(event, handler) {
        host.checkActive();
        const id = host.on(String(event), handler);
        return function unsubscribe() {
          host.off(id);
        };
      },

      // --- 工具 -------------------------------------------------------------
      registerTool(tool) {
        host.checkActive();
        host.registerTool(tool);
      },

      // --- 命令 / 快捷键 / flag --------------------------------------------
      registerCommand(name, options) {
        host.checkActive();
        host.registerCommand(name, options);
      },
      registerShortcut(shortcut, options) {
        host.checkActive();
        host.registerShortcut(String(shortcut), options);
      },
      registerFlag(name, options) {
        host.checkActive();
        // TS 的 Map 键不做字符串化：`registerFlag(5, …)` 与 `registerFlag("5", …)` 是两个键。
        // 宿主按同一规则（SameValueZero）建键，见 loader/mod.rs 的 `js_map_key`。
        host.registerFlag(name, options);
      },
      getFlag(name) {
        host.checkActive();
        return host.getFlag(name);
      },

      // --- 渲染器 -----------------------------------------------------------
      registerMessageRenderer(customType, renderer) {
        host.checkActive();
        host.registerMessageRenderer(String(customType), renderer);
      },
      registerMarkdownTransformer(transformer) {
        host.checkActive();
        host.registerMarkdownTransformer(transformer);
      },
      registerEntryRenderer(customType, renderer) {
        host.checkActive();
        host.registerEntryRenderer(String(customType), renderer);
      },
      registerToolRenderer(resolver) {
        host.checkActive();
        host.registerToolRenderer(resolver);
      },

      // --- 动作 -------------------------------------------------------------
      sendMessage(message, options) {
        host.checkActive();
        host.action("sendMessage", [message, options === undefined ? null : options]);
      },
      sendUserMessage(content, options) {
        host.checkActive();
        host.action("sendUserMessage", [content, options === undefined ? null : options]);
      },
      appendEntry(customType, data) {
        host.checkActive();
        host.action("appendEntry", [String(customType), data === undefined ? null : data]);
      },
      setSessionName(name) {
        host.checkActive();
        host.action("setSessionName", [String(name)]);
      },
      getSessionName() {
        host.checkActive();
        const value = host.action("getSessionName", []);
        return value === null ? undefined : value;
      },
      setLabel(entryId, label) {
        host.checkActive();
        host.action("setLabel", [String(entryId), label === undefined ? null : label]);
      },
      exec(command, args, options) {
        host.checkActive();
        try {
          const result = host.exec(String(command), args, options);
          return Promise.resolve(result);
        } catch (error) {
          return Promise.reject(error);
        }
      },
      getActiveTools() {
        host.checkActive();
        return host.action("getActiveTools", []);
      },
      getAllTools() {
        host.checkActive();
        return host.action("getAllTools", []);
      },
      getSettings() {
        host.checkActive();
        return host.action("getSettings", []);
      },
      setActiveTools(toolNames) {
        host.checkActive();
        host.action("setActiveTools", [toolNames]);
      },
      getCommands() {
        host.checkActive();
        return host.action("getCommands", []);
      },
      setModel(model) {
        host.checkActive();
        if (!host.isBound("setModel") || typeof host.setModel !== "function") {
          // 【逐字】TS 的 `() => Promise.reject(new Error("Extension runtime not initialized"))`。
          return Promise.reject(new Error("Extension runtime not initialized"));
        }
        return host.setModel(model);
      },
      getThinkingLevel() {
        host.checkActive();
        return host.action("getThinkingLevel", []);
      },
      setThinkingLevel(level) {
        host.checkActive();
        host.action("setThinkingLevel", [level]);
      },

      // --- provider ---------------------------------------------------------
      registerProvider(providerOrName, config) {
        host.checkActive();
        if (typeof providerOrName === "string") {
          if (!config) {
            // 【逐字】`loader.ts:433`。
            throw new Error("Provider config is required when registering by name");
          }
          host.registerProvider(providerOrName, config);
          return;
        }
        host.registerProvider(providerOrName, undefined);
      },
      unregisterProvider(name) {
        host.checkActive();
        host.unregisterProvider(String(name));
      },

      // --- MCP servers ------------------------------------------------------
      registerMcpServer(name, config) {
        host.checkActive();
        host.registerMcpServer(String(name), config);
      },
      unregisterMcpServer(name) {
        host.checkActive();
        host.unregisterMcpServer(String(name));
      },
      getMcpServers() {
        host.checkActive();
        return host.getMcpServers();
      },

      // --- 虚拟模型 ---------------------------------------------------------
      registerVirtualModel(model) {
        host.checkActive();
        host.registerVirtualModel(model);
      },
      unregisterVirtualModel(provider, id) {
        host.checkActive();
        host.unregisterVirtualModel(String(provider), String(id));
      },

      // --- 事件总线 ---------------------------------------------------------
      events: {
        emit(channel, data) {
          host.checkActive();
          host.eventsEmit(String(channel), data);
        },
        on(channel, handler) {
          host.checkActive();
          const name = String(channel);
          // TS: `createEventBus().on` 把 handler 包进 async try/catch，错误以
          // `Event handler error (<channel>):` + 错误对象落到 console.error；同步抛出与
          // Promise rejection 都按同一落点上报（Rust 适配器只看得到 settle 后的 Promise）。
          const safeHandler = async (data) => {
            try {
              await handler(data);
            } catch (err) {
              console.error(`Event handler error (${name}):`, err);
            }
          };
          const id = host.eventsOn(name, safeHandler);
          return function unsubscribe() {
            host.eventsOff(id);
          };
        },
      },
    };
    Object.defineProperty(api, "__extensionPath", { value: extensionPath, enumerable: false });
    return api;
  }

  globalThis.__kiana_api = { makeApi };
})();
