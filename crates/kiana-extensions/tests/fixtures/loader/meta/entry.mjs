export const keys = Object.keys(import.meta).sort().join(",");
export const main = import.meta.main;
export const mainType = typeof import.meta.main;
export const resolveType = typeof import.meta.resolve;
export const dirname = import.meta.dirname;
export const filename = import.meta.filename;
export const url = import.meta.url;
export default url;
