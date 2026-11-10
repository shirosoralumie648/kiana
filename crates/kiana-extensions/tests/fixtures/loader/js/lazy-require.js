export function getDep() {
	return require("./dep.cjs").value;
}
export default typeof require;
