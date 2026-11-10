import named from "./named.cjs";

export const mode: string = named.mode;
export const quoted: string = named.quote("hello");
export default named.mode;
