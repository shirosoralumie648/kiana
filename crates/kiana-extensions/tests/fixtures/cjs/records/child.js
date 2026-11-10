var entry = require("./entry.js");
exports.name = "child";
exports.moduleRecord = module;
exports.parentIsEntry = module.parent !== undefined && module.parent.filename === entry.file;
exports.sameExportsObject = entry === require.cache[require.resolve("./entry.js")].exports;
