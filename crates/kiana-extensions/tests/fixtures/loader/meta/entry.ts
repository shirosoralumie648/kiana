export const keys: string = Object.keys(import.meta).sort().join(",");
export const main: unknown = (import.meta as { main?: unknown }).main;
export const mainType: string = typeof (import.meta as { main?: unknown }).main;
export const resolveType: string = typeof (import.meta as { resolve?: unknown }).resolve;
export const dirname: unknown = (import.meta as { dirname?: unknown }).dirname;
export const filename: unknown = (import.meta as { filename?: unknown }).filename;
export const url: string = import.meta.url;
export default url;
