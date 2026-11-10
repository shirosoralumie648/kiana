// 真 CJS 加载器（spec/design-extension-runtime.md §3.5）。
//
// Rust 侧 loader/cjs.rs 把一个 CJS 文件包装成 ESM 模块，其中的 require 来自
// globalThis.__kiana_make_require(name, dir)。递归的 require 在这里用 new Function 求值：
// 每个 CJS 文件自动获得独立函数作用域、this === module.exports、arguments 与 __filename/__dirname，
// 不需要改写源码（sloppy 语义，与 Node 的 CJS 一致）。
//
// 只依赖两个同步宿主函数（由 loader-host 单元安装）：
//   __kiana_host.resolve(from, spec) -> string   绝对路径；解析失败抛错
//   __kiana_host.read(path)          -> string | null   ESM 文件返回 null，触发 ERR_REQUIRE_ESM
// 文件读取在 JS 线程直接进行（std::fs），不经过 tokio。
//
// ## require 的非文件目标（跨单元契约，均不增加编译期依赖）
//
// - Node 内建（`net`、`node:events`、`fs/promises` …）：`globalThis.__kiana_node_shims` 是
//   「shim 键 → exports 对象」的普通对象（`node:fs` 与 `fs` 两种拼写都查）；loader-host 在加载
//   扩展前填充它。回退 `__kiana_host.requireBuiltin(key)`（若宿主提供函数），再回退
//   `__kiana_host.read("node:<name>")` 返回的 CJS 源码（宿主合成文本的形态）。都没有时抛
//   `ERR_UNSUPPORTED`（语料只需 §6.1 列出的那些）。内建导出对象在 JS 侧缓存以保证单实例。
// - 22 个虚拟模块：`globalThis.__kiana_virtual`（设计 §3.3）里同名的 namespace 直接返回。
// - JSON：读到源码后在 JS 侧 JSON.parse（Node 行为），缓存与 CJS 模块共用。
//
// ## Node 错误形状（review finding 6/8）
//
// - 解析失败：`Error` + `code = "MODULE_NOT_FOUND"`，消息
//   `Cannot find module '<spec>'\nRequire stack:\n- <importer>`（Node 的文案；spec 原样回显）。
// - 参数非字符串：`TypeError` + `code = "ERR_INVALID_ARG_TYPE"`，require 的参数名是 `id`、
//   require.resolve 的是 `request`，收到值按 Node 的 `determineSpecificType` 格式描述。
// - `require("node:notreal")`：`ERR_UNKNOWN_BUILTIN_MODULE`。
// - loader-host 没装 `__kiana_host.resolve` / `__kiana_host.read`：第一次调用时报
//   `ERR_PI_HOST_MISSING`（消息点名缺失的原语），不伪装成 MODULE_NOT_FOUND/ERR_REQUIRE_ESM。
// - `require.resolve.paths` 与 Node 26 一致：内建 → null；`.`/`..`/`./x`/`../x` →
//   `[dirname(from)]`；绝对路径、裸包名与未知 `node:` 名 → `nodeModulePaths(from)` +
//   `globalThis.__kiana_module_global_paths`（Rust 侧注入的 `Module.globalPaths`）。
//
// CJS 缓存以绝对路径为键，放在 JS 侧（Rust 侧 Persistent<Value> 会遇到 'js 生命周期逃逸）。
//
// 包装器（Rust 生成的 ESM）使用三个全局函数：
//   __kiana_cjs_acquire(path, dir, fresh) -> [module, evaluate]
//       预登记模块记录到 __kiana_cjs[path]。fresh=true（入口，模块名带 ?v=）时总是新建记录，
//       对应 jiti 的 moduleCache:false；否则复用已有记录，保证 require 与 import 的单一实例。
//   __kiana_cjs_delete(path, module)
//       求值抛错时移除缓存项（Node 语义），且只删除同一个记录。
//   __kiana_make_require(from, dir)
//       供 ESM 侧 TS 模块的 require 绑定注入与包装器使用。
(function () {
	"use strict";

	var cjsCache = Object.create(null);
	globalThis.__kiana_cjs = cjsCache;

	function stripBomAndShebang(source) {
		if (source.charCodeAt(0) === 0xfeff) source = source.slice(1);
		if (source.charAt(0) === "#" && source.charAt(1) === "!") {
			var newline = source.indexOf("\n");
			source = newline === -1 ? "" : source.slice(newline);
		}
		return source;
	}

	// node:path.dirname 的最小实现（同时接受 / 与 \，与 Node 的跨平台行为一致）。
	function dirname(path) {
		var index = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
		if (index === -1) return ".";
		if (index === 0) return path.slice(0, 1);
		var parent = path.slice(0, index);
		if (/^[A-Za-z]:$/.test(parent)) return parent + path.charAt(index);
		return parent;
	}

	function joinPath(dir, name) {
		if (!dir) return name;
		var last = dir.charAt(dir.length - 1);
		if (last === "/" || last === "\\") return dir + name;
		return dir + "/" + name;
	}

	// node:module 的 Module._nodeModulePaths(from)（不含 Node 的全局目录）。
	function nodeModulePaths(from) {
		var dir = dirname(from).replace(/\\/g, "/");
		if (dir === "/") return ["/node_modules"];
		var parts = dir.split("/");
		var paths = [];
		for (var i = parts.length; i > 0; i--) {
			var prefix = parts.slice(0, i).join("/");
			if (prefix === "") continue;
			var base = parts[i - 1];
			if (base === "node_modules") continue;
			paths.push(prefix === "" ? "/node_modules" : prefix + "/node_modules");
		}
		if (paths.indexOf("/node_modules") === -1) paths.push("/node_modules");
		return paths;
	}

	// require("node:module").builtinModules 的裸名部分（Node 22；顺序与
	// loader/resolver.rs 的 NODE_BUILTINS 一致）。
	var builtinModules = {
		_http_agent: 1,
		_http_client: 1,
		_http_common: 1,
		_http_incoming: 1,
		_http_outgoing: 1,
		_http_server: 1,
		_tls_common: 1,
		_tls_wrap: 1,
		assert: 1,
		"assert/strict": 1,
		async_hooks: 1,
		buffer: 1,
		child_process: 1,
		cluster: 1,
		console: 1,
		constants: 1,
		crypto: 1,
		dgram: 1,
		diagnostics_channel: 1,
		dns: 1,
		"dns/promises": 1,
		domain: 1,
		events: 1,
		fs: 1,
		"fs/promises": 1,
		http: 1,
		http2: 1,
		https: 1,
		inspector: 1,
		"inspector/promises": 1,
		module: 1,
		net: 1,
		os: 1,
		path: 1,
		"path/posix": 1,
		"path/win32": 1,
		perf_hooks: 1,
		process: 1,
		punycode: 1,
		querystring: 1,
		readline: 1,
		"readline/promises": 1,
		repl: 1,
		stream: 1,
		"stream/consumers": 1,
		"stream/promises": 1,
		"stream/web": 1,
		string_decoder: 1,
		sys: 1,
		timers: 1,
		"timers/promises": 1,
		tls: 1,
		trace_events: 1,
		tty: 1,
		url: 1,
		util: 1,
		"util/types": 1,
		v8: 1,
		vm: 1,
		wasi: 1,
		worker_threads: 1,
		zlib: 1,
	};

	// node: 前缀独有的内建（Node 22 的 module.builtinModules 里以 node: 开头的项）。
	var nodeOnlyBuiltins = {
		test: 1,
		"test/reporters": 1,
		sea: 1,
		sqlite: 1,
	};

	var hasOwnProperty = Object.prototype.hasOwnProperty;

	function hasOwn(object, key) {
		return object !== null && object !== undefined && hasOwnProperty.call(object, key);
	}

	// 已知内建 → "node:<name>" 键；未知 → null。`node:notreal` 由调用方区分处理。
	function builtinKey(name) {
		if (name.slice(0, 5) === "node:") {
			var bare = name.slice(5);
			return builtinModules[bare] === 1 || nodeOnlyBuiltins[bare] === 1 ? name : null;
		}
		return builtinModules[name] === 1 ? "node:" + name : null;
	}

	function isNodePrefixed(name) {
		return name.slice(0, 5) === "node:";
	}

	function isJsonFile(path) {
		return path.length >= 5 && path.slice(-5) === ".json";
	}

	// Node 的 determineSpecificType 文案：`Received type number (5)` /
	// `Received an instance of Object` / `Received function f` / `Received null` …
	function describeReceived(value) {
		if (value === null) return "Received null";
		if (value === undefined) return "Received undefined";
		var type = typeof value;
		if (type === "function") return "Received function " + (value.name || "");
		if (type === "object") {
			var prototype = Object.getPrototypeOf(value);
			var constructor = prototype === null ? undefined : prototype.constructor;
			var name =
				typeof constructor === "function" && constructor.name ? constructor.name : "Object";
			return "Received an instance of " + name;
		}
		if (type === "bigint") return "Received type bigint (" + String(value) + "n)";
		return "Received type " + type + " (" + String(value) + ")";
	}

	// Node 的 ERR_INVALID_ARG_TYPE（require 用 "id"，require.resolve 用 "request"）。
	function typeErrorMessage(argument, value) {
		var error = new TypeError(
			'The "' +
				argument +
				'" argument must be of type string. ' +
				describeReceived(value),
		);
		error.code = "ERR_INVALID_ARG_TYPE";
		return error;
	}

	// Node 的 ERR_UNKNOWN_BUILTIN_MODULE（仅 require，require.resolve 用 MODULE_NOT_FOUND）。
	function unknownBuiltinError(name) {
		var error = new Error("No such built-in module: " + name);
		error.code = "ERR_UNKNOWN_BUILTIN_MODULE";
		return error;
	}

	// Node 的 MODULE_NOT_FOUND：消息带 Require stack（从发起模块沿 parent 链）。
	function requireStack(from) {
		var lines = [];
		var seen = Object.create(null);
		var current = from;
		while (typeof current === "string" && !seen[current]) {
			seen[current] = true;
			lines.push("- " + current);
			var record = cjsCache[current];
			current = record && record.parent ? record.parent.filename : undefined;
		}
		return lines.length === 0 ? "" : "\nRequire stack:\n" + lines.join("\n");
	}

	function moduleNotFoundError(spec, from) {
		var error = new Error("Cannot find module '" + spec + "'" + requireStack(from));
		error.code = "MODULE_NOT_FOUND";
		return error;
	}

	// 宿主原语缺失（loader-host 单元没装 __kiana_host）时的明确错误。planner 契约：不能伪装成
	// MODULE_NOT_FOUND / ERR_REQUIRE_ESM —— 那会把「宿主没装」误报成「文件不存在」。
	function hostMissing(primitive, operation) {
		var error = new Error(
			operation +
				" requires the __kiana_host." +
				primitive +
				" host primitive (loader-host primitives are not installed)",
		);
		error.code = "ERR_PI_HOST_MISSING";
		return error;
	}

	function hostResolve(from, spec) {
		if (
			typeof __kiana_host === "undefined" ||
			__kiana_host === null ||
			typeof __kiana_host.resolve !== "function"
		) {
			throw hostMissing("resolve", "The CJS require bridge");
		}
		return __kiana_host.resolve(from, spec);
	}

	function hostRead(path) {
		if (
			typeof __kiana_host === "undefined" ||
			__kiana_host === null ||
			typeof __kiana_host.read !== "function"
		) {
			throw hostMissing("read", "The CJS require bridge");
		}
		return __kiana_host.read(path);
	}

	// 宿主解析失败（rquickjs 的 loading 文案）统一转成 Node 的 MODULE_NOT_FOUND。
	function resolveFrom(from, spec) {
		try {
			return hostResolve(from, spec);
		} catch (error) {
			if (
				error &&
				(error.code === "MODULE_NOT_FOUND" ||
					error.code === "ERR_MODULE_NOT_FOUND" ||
					error.code === "ERR_PI_HOST_MISSING")
			) {
				throw error;
			}
			throw moduleNotFoundError(spec, from);
		}
	}

	// 内建 shim 的导出对象缓存：Node 不把 builtin 放进 require.cache，但同一内建必须单实例。
	var builtinCache = Object.create(null);

	// Node 内建 shim，按顺序：
	// 1. globalThis.__kiana_node_shims（node:fs / fs 两种键都认）；
	// 2. 宿主函数 __kiana_host.requireBuiltin（若提供）；
	// 3. __kiana_host.read("node:<name>") 返回的 CJS 源码（宿主合成文本的形态，见文件头）；
	// 4. 都没有 → ERR_UNSUPPORTED（设计 §6.1：未提供的内建）。
	function requireBuiltin(key) {
		if (hasOwn(builtinCache, key)) return builtinCache[key];

		var shims = globalThis.__kiana_node_shims;
		if (shims !== null && shims !== undefined) {
			if (hasOwn(shims, key)) return (builtinCache[key] = shims[key]);
			var bare = key.slice(5);
			if (hasOwn(shims, bare)) return (builtinCache[key] = shims[bare]);
		}
		if (
			typeof __kiana_host !== "undefined" &&
			__kiana_host !== null &&
			typeof __kiana_host.requireBuiltin === "function"
		) {
			return (builtinCache[key] = __kiana_host.requireBuiltin(key));
		}
		var source = null;
		try {
			source = hostRead(key);
		} catch (error) {
			if (error && error.code === "ERR_PI_HOST_MISSING") throw error;
			source = null;
		}
		if (source !== null && source !== undefined) {
			var dir = dirname(key);
			var module = createModule(key, dir);
			var evaluate = new Function(
				"module",
				"exports",
				"require",
				"__filename",
				"__dirname",
				String(source),
			);
			evaluate.call(module.exports, module, module.exports, module.require, key, dir);
			module.loaded = true;
			return (builtinCache[key] = module.exports);
		}
		var error = new Error('Node builtin module "' + key + '" is not supported in this runtime.');
		error.code = "ERR_UNSUPPORTED";
		throw error;
	}

	// Node 的 ERR_REQUIRE_ESM 文案（Node 22+ 的 require(esm) 第一版不做；设计 §3.5 第 2 条）。
	function esmRequireError(path, from) {
		var error = new Error(
			"require() of ES Module " +
				path +
				" from " +
				from +
				" not supported.\n" +
				"Instead change the require of " +
				path +
				" in " +
				from +
				" to a dynamic import() which is available in all CommonJS modules.",
		);
		error.code = "ERR_REQUIRE_ESM";
		return error;
	}

	function createModule(path, dir) {
		var module = {
			id: path,
			path: dir,
			filename: path,
			exports: {},
			loaded: false,
			children: [],
			paths: nodeModulePaths(path),
			parent: undefined,
		};
		module.require = makeRequire(path, dir);
		return module;
	}

	function linkParent(module, from) {
		var parent = cjsCache[from];
		if (parent === undefined || parent === module) return;
		module.parent = parent;
		parent.children.push(module);
	}

	// Node 的 `require.resolve.paths` 请求分类：`_resolveLookupPaths` 只看首字符是否 `.`、以及
	// 第二个字符是 `.` 还是 `/`（所以 `.`、`..`、`..foo`、`./x`、`../x` 是相对请求，`.foo` 不是）。
	function isRelativeRequest(spec) {
		if (spec.charAt(0) !== ".") return false;
		if (spec.length === 1) return true;
		var second = spec.charAt(1);
		return second === "." || second === "/";
	}

	// Node 的全局模块目录（`Module.globalPaths`；由 Rust 侧 install_cjs_bridge 计算并注入，
	// 因为 QuickJS 里没有 process.execPath）。
	function globalModulePaths() {
		var paths = globalThis.__kiana_module_global_paths;
		return Array.isArray(paths) ? paths.slice() : [];
	}

	// from = 发起 require 的模块名（真实绝对路径）；dir 是 §3.5 契约的一部分，
	// 解析实际以绝对路径 from 为准（宿主据此推导目录，见 loader/resolver.rs）。
	function makeRequire(from, dir) {
		function require(spec) {
			if (typeof spec !== "string") {
				throw typeErrorMessage("id", spec);
			}
			var builtin = builtinKey(spec);
			if (builtin !== null) return requireBuiltin(builtin);
			if (isNodePrefixed(spec)) throw unknownBuiltinError(spec);
			return loadModule(resolveFrom(from, spec), from);
		}

		require.resolve = function resolve(spec, options) {
			if (typeof spec !== "string") {
				throw typeErrorMessage("request", spec);
			}
			// Node returns builtin requests as written (`fs`, not `node:fs`); an unknown
			// `node:` name is MODULE_NOT_FOUND here (unlike require(), which reports
			// ERR_UNKNOWN_BUILTIN_MODULE).
			if (builtinKey(spec) !== null) return spec;
			if (isNodePrefixed(spec)) throw moduleNotFoundError(spec, from);
			var paths = options && options.paths;
			var resolved;
			if (Array.isArray(paths) && paths.length > 0) {
				var lastError;
				for (var i = 0; i < paths.length; i++) {
					try {
						resolved = resolveFrom(joinPath(String(paths[i]), "_index.js"), spec);
						break;
					} catch (error) {
						lastError = error;
					}
				}
				if (resolved === undefined) {
					throw lastError || moduleNotFoundError(spec, from);
				}
			} else {
				resolved = resolveFrom(from, spec);
			}
			// The host resolves builtin names to `node:<name>` shim keys; Node's
			// require.resolve returns the request as written.
			if (resolved.slice(0, 5) === "node:" && !isNodePrefixed(spec)) return spec;
			return resolved;
		};

		require.resolve.paths = function paths(spec) {
			if (typeof spec !== "string") {
				throw typeErrorMessage("request", spec);
			}
			// Node: core modules (and `node:` spellings of them) have no lookup paths.
			if (builtinKey(spec) !== null) return null;
			// Node's `_resolveLookupPaths`: `.`/`..`/`./x`/`../x` resolve against the requesting
			// module's directory only; absolute, bare and unknown `node:` requests use the
			// parent's node_modules chain plus the global folders.
			if (isRelativeRequest(spec)) return [dirname(from)];
			return nodeModulePaths(from).concat(globalModulePaths());
		};

		require.cache = cjsCache;
		require.main = undefined;
		return require;
	}

	function loadModule(path, from) {
		var cached = cjsCache[path];
		if (cached !== undefined) return cached.exports;

		// Node 内建 shim 键（防御性：makeRequire 已提前分派）。
		if (isNodePrefixed(path)) return requireBuiltin(path);
		// 虚拟模块（设计 §3.3）：返回预置 namespace，未注册时继续走文件读取。
		var virtual = globalThis.__kiana_virtual;
		if (hasOwn(virtual, path)) return virtual[path];

		var source = hostRead(path);
		// null/undefined = 该路径是 ESM 文件（或宿主拒绝按 CJS 读取）。
		if (source === null || source === undefined) throw esmRequireError(path, from);

		var dir = dirname(path);
		var module = createModule(path, dir);
		linkParent(module, from);
		// 先入缓存：循环 require 拿到的是半成品 exports（Node 语义）。
		cjsCache[path] = module;

		try {
			if (isJsonFile(path)) {
				module.exports = JSON.parse(stripBomAndShebang(String(source)));
			} else {
				var evaluate = new Function("module", "exports", "require", "__filename", "__dirname", String(source));
				evaluate.call(module.exports, module, module.exports, module.require, path, dir);
			}
			module.loaded = true;
		} catch (error) {
			// Node 在求值抛错时移除缓存项，下次 require 重新执行。
			delete cjsCache[path];
			throw error;
		}

		return module.exports;
	}

	// 包装器预登记：fresh=true 表示入口（模块名带 ?v=，对应 jiti moduleCache:false）。
	function acquireModule(path, dir, fresh) {
		var existing = cjsCache[path];
		if (!fresh && existing !== undefined) return [existing, false];
		var module = createModule(path, dir);
		cjsCache[path] = module;
		return [module, true];
	}

	function deleteModule(path, module) {
		if (cjsCache[path] === module) delete cjsCache[path];
	}

	globalThis.__kiana_make_require = makeRequire;
	globalThis.__kiana_cjs_acquire = acquireModule;
	globalThis.__kiana_cjs_delete = deleteModule;
})();
