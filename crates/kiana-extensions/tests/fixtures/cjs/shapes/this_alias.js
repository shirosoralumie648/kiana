// Node: at CJS top level `this === module.exports === exports`.
module.exports = { sameThis: this === module.exports, sameExports: this === exports };
