import type { Foo } from "./types.ts";
import { type Bar, baz } from "./other.ts";

export type { Foo } from "./types.ts";
export { type Bar } from "./other.ts";

export const value: Baz = baz;
