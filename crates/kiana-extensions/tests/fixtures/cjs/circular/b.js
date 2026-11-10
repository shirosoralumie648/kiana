exports.name = "b";
var a = require("./a");
exports.seenFromA = a.name; // partial exports: a.name is already set
