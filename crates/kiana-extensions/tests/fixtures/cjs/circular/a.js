exports.name = "a";
var b = require("./b");
exports.seenFromB = b.seenFromA;
exports.bName = b.name;
