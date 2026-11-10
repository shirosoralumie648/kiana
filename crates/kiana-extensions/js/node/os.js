// node:os — std::env backed (design §6.1).
(function () {
  "use strict";

  var I = globalThis.__kiana_node_internal;
  var host = I.host;
  var shims = globalThis.__kiana_node_shims;

  function env(name, fallback) {
    var value = globalThis.process && globalThis.process.env ? globalThis.process.env[name] : undefined;
    return value === undefined ? fallback : value;
  }

  var isWindows = host.platform() === "win32";

  var osObject = {
    EOL: isWindows ? "\r\n" : "\n",
    devNull: isWindows ? "\\\\.\\nul" : "/dev/null",
    homedir: function () { return host.homedir(); },
    tmpdir: function () { return host.tmpdir(); },
    platform: function () { return host.platform(); },
    arch: function () { return host.arch(); },
    type: function () {
      var platform = host.platform();
      if (platform === "linux") return "Linux";
      if (platform === "darwin") return "Darwin";
      if (platform === "win32") return "Windows_NT";
      return platform;
    },
    release: function () { return host.osRelease(); },
    hostname: function () { return host.hostname(); },
    machine: function () { return host.machine(); },
    cpus: function () {
      var count = Math.max(1, host.cpuCount());
      var model = host.cpuModel();
      var list = [];
      for (var i = 0; i < count; i++) {
        list.push({ model: model, speed: 0, times: { user: 0, nice: 0, sys: 0, idle: 0, irq: 0 } });
      }
      return list;
    },
    totalmem: function () { return host.totalMemory(); },
    freemem: function () { return host.freeMemory(); },
    uptime: function () { return host.now() / 1000; },
    loadavg: function () { return [0, 0, 0]; },
    networkInterfaces: function () { return {}; },
    userInfo: function () {
      return {
        uid: 0,
        gid: 0,
        username: env("USER", env("USERNAME", "")),
        homedir: host.homedir(),
        shell: env("SHELL", isWindows ? "cmd.exe" : "/bin/sh"),
      };
    },
    endianness: function () { return "LE"; },
    getPriority: function () { return 0; },
    setPriority: function () {},
    constants: {
      SIGHUP: 1, SIGINT: 2, SIGQUIT: 3, SIGILL: 4, SIGTRAP: 5, SIGABRT: 6,
      SIGBUS: 7, SIGFPE: 8, SIGKILL: 9, SIGUSR1: 10, SIGSEGV: 11, SIGUSR2: 12,
      SIGPIPE: 13, SIGALRM: 14, SIGTERM: 15, SIGCONT: 18, SIGSTOP: 19,
      UV_UDP_REUSEADDR: 4,
      errno: {},
      signals: {
        SIGHUP: 1, SIGINT: 2, SIGQUIT: 3, SIGILL: 4, SIGTRAP: 5, SIGABRT: 6,
        SIGBUS: 7, SIGFPE: 8, SIGKILL: 9, SIGUSR1: 10, SIGSEGV: 11, SIGUSR2: 12,
        SIGPIPE: 13, SIGALRM: 14, SIGTERM: 15, SIGCONT: 18, SIGSTOP: 19,
      },
      priority: {},
    },
  };

  shims.os = osObject;
  shims["node:os"] = osObject;
})();
