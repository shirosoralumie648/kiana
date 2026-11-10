class Temporary {
	dispose(): void {}
}

using shortLived = new Temporary();
await using awaited = new Temporary();

export { shortLived, awaited };
