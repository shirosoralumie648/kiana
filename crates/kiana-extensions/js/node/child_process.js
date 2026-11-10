// node:child_process — spawn/exec/execFile and their sync variants.
//
// Async spawn is `__kiana.promise("child.spawn", ...)`: Rust starts the process with
// `kiana_core::child_process::spawn_process` and streams stdout/stderr/close events.
// `stdio: "ignore"` maps to Stdio::null() on the Rust side, so a child never
// inherits kiana's stdin (design §6.3; overlay-qa-tests spawns kiana itself).
(function () {
  "use strict";

  var I = globalThis.__kiana_node_internal;
  var host = I.host;
  var shims = globalThis.__kiana_node_shims;
  var EventEmitter = shims.events.EventEmitter;
  var Readable = shims.stream.Readable;
  var Writable = shims.stream.Writable;
  var promisifyCustom = Symbol.for("nodejs.util.promisify.custom");

  function optionsOrCallback(options, callback) {
    if (typeof options === "function") return { options: {}, callback: options };
    return { options: options || {}, callback: callback };
  }

  /// Node's `child_process` inherits the *live* `process.env` when no `env`
  /// option is passed: snapshot it (including mutations) at spawn time.
  function inheritedEnv() {
    if (typeof process === "undefined" || !process.env) return undefined;
    var env = {};
    var keys = Object.keys(process.env);
    for (var i = 0; i < keys.length; i++) {
      var value = process.env[keys[i]];
      if (value !== undefined) env[keys[i]] = String(value);
    }
    return env;
  }

  /// Resolve the `env` option the way Node does: `undefined`/`null` inherit the
  /// live `process.env` (including mutations), other primitives yield an empty
  /// environment, and object values are stringified (skipping `undefined`).
  function envOption(env) {
    if (env === undefined || env === null) return inheritedEnv();
    var result = {};
    if (typeof env === "object" || typeof env === "function") {
      for (var key in env) {
        var value = env[key];
        if (value !== undefined) result[key] = String(value);
      }
    }
    return result;
  }

  function commandFailedMessage(spawnargs, stderrText) {
    // Node always appends the newline, even when stderr is empty.
    return "Command failed: " + spawnargs.join(" ") + "\n" +
      (stderrText === undefined || stderrText === null ? "" : stderrText);
  }

  class ChildProcess extends EventEmitter {
    constructor() {
      super();
      this.pid = undefined;
      this.killed = false;
      this.exitCode = null;
      this.signalCode = null;
      this.connected = false;
      this._handle = null;
      this._stdinQueue = [];
      this._stdinEnded = false;
      this.spawnargs = [];
      this.spawnfile = "";
      // `child.stdin` is a Writable only for stdio[0] === "pipe" (Node sets it
      // to null for "ignore"/"inherit").
      this.stdin = null;
      this.stdout = new Readable();
      this.stderr = new Readable();
      this.stdio = [];
    }

    _bindStdin() {
      var child = this;
      var writable = new Writable({
        write: function (chunk, encoding, callback) {
          var bytes = I.toBytes(typeof chunk === "string" ? I.stringToBytes(chunk, encoding) : chunk);
          if (child._handle === null) child._stdinQueue.push(bytes);
          else host.childWrite(child._handle, I.base64Encode(bytes));
          callback();
        },
        final: function (callback) {
          if (child._handle === null) child._stdinEnded = true;
          else host.childEnd(child._handle);
          callback();
        },
      });
      writable.destroy = function () { this.end(); };
      this.stdin = writable;
      this.stdio = [writable, this.stdout, this.stderr];
    }

    _flushStdin() {
      var queued = this._stdinQueue;
      this._stdinQueue = [];
      for (var i = 0; i < queued.length; i++) {
        host.childWrite(this._handle, I.base64Encode(queued[i]));
      }
      if (this._stdinEnded) host.childEnd(this._handle);
    }

    kill(signal) {
      if (this._handle === null) return false;
      var ok = host.spawnKill(this._handle, signal === undefined ? "SIGTERM" : String(signal));
      if (ok) this.killed = true;
      return ok;
    }

    ref() { return this; }
    unref() { return this; }
    disconnect() { throw new Error("child.disconnect is not supported"); }
  }

  function spawn(command, args, options, callback) {
    if (typeof args === "function") {
      callback = args;
      args = [];
      options = {};
    } else if (!Array.isArray(args)) {
      callback = typeof options === "function" ? options : callback;
      options = args || {};
      args = [];
    }
    options = options || {};
    var child = new ChildProcess();
    child.spawnfile = String(command);
    child.spawnargs = [String(command)].concat(args.map(String));
    var stdioOption = options.stdio;
    var stdinMode = "pipe";
    if (Array.isArray(stdioOption)) {
      if (stdioOption.length > 0) stdinMode = stdioOption[0];
    } else if (typeof stdioOption === "string") {
      stdinMode = stdioOption;
    }
    if (stdinMode === "pipe" || stdinMode === undefined || stdinMode === null) {
      child._bindStdin();
    } else {
      child.stdin = null;
      child.stdio = [null, child.stdout, child.stderr];
    }
    if (typeof callback === "function") child.on("error", function () {});

    function onEvent(event) {
      if (!event || typeof event !== "object") return;
      if (event.type === "stdout") {
        child.stdout.push(Buffer.from(event.b64 || "", "base64"));
      } else if (event.type === "stderr") {
        child.stderr.push(Buffer.from(event.b64 || "", "base64"));
      } else if (event.type === "stderrError") {
        // Internal diagnostic only; Node has no equivalent event.
      } else if (event.type === "error") {
        child.emit("error", I.nodeError(event));
      } else if (event.type === "close") {
        child.exitCode = event.code === undefined ? null : event.code;
        child.signalCode = event.signal === undefined ? null : event.signal;
        child.stdout.push(null);
        child.stderr.push(null);
        child.emit("exit", child.exitCode, child.signalCode);
        child.emit("close", child.exitCode, child.signalCode);
      }
    }

    var payload = {
      command: String(command),
      args: args.map(String),
      options: {
        cwd: options.cwd === undefined ? undefined : String(options.cwd),
        env: envOption(options.env),
        stdio: options.stdio,
        shell: options.shell,
        detached: !!options.detached,
        windowsHide: options.windowsHide === undefined ? true : !!options.windowsHide,
      },
      onEvent: onEvent,
    };

    I.promise("child.spawn", payload).then(function (meta) {
      child._handle = meta.handle;
      child.pid = meta.pid;
      child._flushStdin();
      child.emit("spawn");
      child.emit("ready");
    }, function (error) {
      child.pid = undefined;
      child.exitCode = null;
      child.signalCode = null;
      child.emit("error", error);
      child.stdout.push(null);
      child.stderr.push(null);
      // Node does not emit 'exit' when the process never started; the close
      // code is the (negative) errno.
      var closeCode = error && typeof error.errno === "number" ? error.errno : -2;
      child.emit("close", closeCode, null);
    });
    return child;
  }

  function collect(child, stdoutParts, stderrParts, limit, decode, callback) {
    var done = false;
    var length = 0;
    function finish(error) {
      if (done) return;
      done = true;
      callback(error);
    }
    child.stdout.on("data", function (chunk) {
      length += chunk.length;
      if (length > limit) {
        var error = new Error("stdout maxBuffer length exceeded");
        error.code = "ERR_CHILD_PROCESS_STDIO_MAXBUFFER";
        child.kill("SIGTERM");
        finish(error);
        return;
      }
      stdoutParts.push(chunk);
    });
    child.stderr.on("data", function (chunk) { stderrParts.push(chunk); });
    child.on("error", function (error) { finish(error); });
    child.on("close", function (code, signal) {
      if (done) return;
      var stderrBuffer = Buffer.concat(stderrParts);
      var error = null;
      if (code !== 0) {
        // Node appends the (decoded) stderr to the message: `Command failed: <cmd>\n<stderr>`.
        error = new Error(commandFailedMessage(child.spawnargs, decode(stderrBuffer)));
        error.code = code;
        error.signal = signal;
        error.killed = child.killed;
      }
      done = true;
      callback(error, Buffer.concat(stdoutParts), stderrBuffer, code, signal);
    });
  }

  function execFile(file, args, options, callback) {
    var normalized = optionsOrCallback(options, callback);
    options = normalized.options;
    callback = normalized.callback;
    if (typeof args === "function") {
      callback = args;
      args = [];
      options = {};
    } else if (!Array.isArray(args)) {
      callback = typeof options === "function" ? options : callback;
      options = args || {};
      args = [];
    }
    var encoding = options.encoding === undefined ? "utf8" : options.encoding;
    var limit = options.maxBuffer === undefined ? 1024 * 1024 : options.maxBuffer;
    var output = function (buffer) {
      return encoding === null || encoding === "buffer" ? buffer : buffer.toString(encoding || "utf8");
    };
    var child = spawn(file, args, options);
    collect(child, [], [], limit, output, function (error, stdout, stderr, code, signal) {
      if (error) {
        // Node's callback error has `cmd` but no stdout/stderr (the
        // promisified variant adds them later).
        error.cmd = [String(file)].concat(args.map(String)).join(" ");
      }
      if (typeof callback === "function") {
        callback(error, output(stdout), output(stderr));
      }
      void code;
      void signal;
    });
    return child;
  }

  function exec(command, options, callback) {
    if (typeof options === "function") {
      callback = options;
      options = {};
    }
    options = options || {};
    return execFile(String(command), [], Object.assign({}, options, { shell: true }), callback);
  }

  function promisifyExec(original) {
    return function (command, options) {
      return new Promise(function (resolve, reject) {
        var opts = typeof options === "object" && options !== null ? options : {};
        original(command, opts, function (error, stdout, stderr) {
          if (error) {
            error.stdout = stdout;
            error.stderr = stderr;
            reject(error);
            return;
          }
          resolve({ stdout: stdout, stderr: stderr });
        });
      });
    };
  }
  exec[promisifyCustom] = promisifyExec(exec);
  var execFilePromisified = promisifyExec(execFile);
  execFile[promisifyCustom] = execFilePromisified;

  function syncOptions(options) {
    options = options || {};
    var stdio = options.stdio;
    if (stdio === undefined) stdio = ["pipe", "pipe", "pipe"];
    return {
      cwd: options.cwd === undefined ? undefined : String(options.cwd),
      env: envOption(options.env),
      stdio: stdio,
      shell: options.shell,
      timeout: options.timeout,
      windowsHide: options.windowsHide === undefined ? true : !!options.windowsHide,
    };
  }

  function decodeSync(value, options) {
    var encoding = options && options.encoding;
    var bytes = Buffer.from(value, "base64");
    if (encoding === undefined || encoding === null || encoding === "buffer") return bytes;
    return bytes.toString(encoding);
  }

  function spawnSyncRaw(command, args, options) {
    var result = JSON.parse(host.spawnSync(JSON.stringify({
      command: String(command),
      args: args.map(String),
      options: syncOptions(options),
    })));
    var error = null;
    if (result.error) {
      // `spawnSync().error` has no status/signal (only the throwing
      // `execSync`/`execFileSync` variants add them).
      error = I.nodeError(result.error);
    }
    return { result: result, error: error };
  }

  function spawnSync(command, args, options) {
    if (!Array.isArray(args)) {
      options = args || {};
      args = [];
    }
    options = options || {};
    var raw = spawnSyncRaw(command, args, options);
    var result = raw.result;
    var stdout = decodeSync(result.stdout, options);
    var stderr = decodeSync(result.stderr, options);
    return {
      pid: result.pid === undefined || result.pid === null ? 0 : result.pid,
      output: result.error ? null : [null, stdout, stderr],
      stdout: stdout,
      stderr: stderr,
      status: result.status,
      signal: result.signal,
      error: raw.error,
    };
  }

  function throwSyncError(raw, command, args, options) {
    var error = raw.error;
    if (!error && raw.result.status !== 0 && raw.result.status !== null && raw.result.status !== undefined) {
      // Node: `execSync`/`execFileSync` 在非零退出时抛 Error（status/signal/stdout/stderr 齐备），
      // 而 `spawnSync` 只把同样的信息放进返回值。Node 不在这个错误上设数值 `code`
      // （只有 `status`/`signal`），消息末尾附上 stderr。
      var stderrText = decodeSync(raw.result.stderr, options);
      var message = "Command failed: " + [String(command)].concat(args.map(String)).join(" ") +
        "\n" + stderrText;
      error = new Error(message);
    }
    if (!error) return;
    error.status = raw.result.status;
    error.signal = raw.result.signal;
    error.stdout = decodeSync(raw.result.stdout, options);
    error.stderr = decodeSync(raw.result.stderr, options);
    error.output = [null, error.stdout, error.stderr];
    error.pid = undefined;
    throw error;
  }

  function execSync(command, options) {
    options = Object.assign({}, options || {}, { shell: true });
    var raw = spawnSyncRaw(command, [], options);
    throwSyncError(raw, command, [], options);
    return decodeSync(raw.result.stdout, options);
  }

  function execFileSync(file, args, options) {
    if (!Array.isArray(args)) {
      options = args || {};
      args = [];
    }
    options = Object.assign({}, options || {}, { shell: false });
    var raw = spawnSyncRaw(file, args, options);
    throwSyncError(raw, file, args, options);
    return decodeSync(raw.result.stdout, options);
  }

  function fork() {
    var error = new Error("child_process.fork is not supported by the kiana extension runtime");
    error.code = "ERR_UNSUPPORTED";
    throw error;
  }

  var childProcessObject = {
    ChildProcess: ChildProcess,
    spawn: spawn,
    spawnSync: spawnSync,
    exec: exec,
    execSync: execSync,
    execFile: execFile,
    execFileSync: execFileSync,
    fork: fork,
  };

  shims.child_process = childProcessObject;
  shims["node:child_process"] = childProcessObject;
})();
