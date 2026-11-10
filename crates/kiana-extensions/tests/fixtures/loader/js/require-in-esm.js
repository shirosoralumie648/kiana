import dep from "./dep.cjs";

const viaRequire = require("./dep.cjs");

export const sameInstance = dep === viaRequire;
export const sum = dep.value + viaRequire.value;
export default sum;
