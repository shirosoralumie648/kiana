import dep from "./dep.cjs";

export const viaRequire = require("./dep.cjs").value;
export const keys = Object.keys(import.meta).sort().join(",");
export default dep.value + viaRequire;
