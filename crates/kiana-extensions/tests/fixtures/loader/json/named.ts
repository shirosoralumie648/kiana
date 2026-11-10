import data, { name, n } from "./data.json";
import * as namespace from "./data.json";
import * as weird from "./weird.json";

export const named: string = name;
export const num: number = n;
export const defaultName: string = data.name;
export const keys: string = Object.keys(namespace).sort().join(",");
export const weirdKeys: string = Object.keys(weird).sort().join(",");
export const dashed: number = (weird as { "a-b": number })["a-b"];
export default { name, n };
