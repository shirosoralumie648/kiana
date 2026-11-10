import a, { x as ax } from "./a.ts";
import b, { x as bx, foo } from "./b.ts";
import c, { x as cx, foo as cfoo } from "./c.ts";

export const aType = typeof a;
export const aValue = typeof a === "function" ? (a as () => string)() : "not-a-function";
export const aNamed = String(ax);
export const bDefault = b;
export const bxValue = bx;
export const fooValue = foo;
export const cDefault = c;
export const cxValue = cx;
export const cfooValue = cfoo;
export default "consumer";
