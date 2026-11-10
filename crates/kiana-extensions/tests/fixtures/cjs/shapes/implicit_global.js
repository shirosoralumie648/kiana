// Sloppy-mode CJS: a bare assignment creates a global (Node runs CJS in sloppy mode).
implicitGlobalVar = 42;
exports.value = implicitGlobalVar;
