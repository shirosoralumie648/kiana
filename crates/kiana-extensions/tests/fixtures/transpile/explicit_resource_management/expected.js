class Temporary {
	dispose() {}
}
using shortLived = new Temporary();
await using awaited = new Temporary();
export { shortLived, awaited };
