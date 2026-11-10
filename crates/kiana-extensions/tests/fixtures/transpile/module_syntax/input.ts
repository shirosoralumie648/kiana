const meta = import.meta;
const lazy = () => import("./lazy.ts");
export const topLevel = await Promise.resolve(1);
export { meta, lazy };
